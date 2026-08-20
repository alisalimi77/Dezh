//! Marz — the guarded egress boundary daemon (M1: the device).
//!
//! A separate U-mode ELF that owns the NIC. It receives exactly two grants from
//! the kernel: the **single** virtio-net MMIO page (the kernel found the device;
//! this daemon never scans the window) and a DMA window. It holds no block
//! authority and no other device.
//!
//! M1 transmits one raw frame so the device path is proven end to end; the
//! authority gate (per-destination capability + DIFC declassification) and the
//! effect record land in M2/M3. See `docs/SUBSYSTEMS.md (Marz)`.

#![no_std]
#![no_main]

use core::arch::asm;

const SYS_EXIT: usize = 0;
const SYS_PRINT: usize = 1;
const SYS_IRQ_WAIT: usize = 10;

/// The granted NIC page. One device, mapped by the kernel at a fixed VA.
const NIC_VA: usize = 0x5002_0000;
/// Marz's OWN DMA window (virtual); its physical base arrives in a register.
/// It is not shared with the block daemon - two devices, two grants.
const DMA_VA: usize = 0x5200_0000;

const VIRTIO_MAGIC: u32 = 0x7472_6976;
const VIRTIO_ID_NET: u32 = 1;

const VR_MAGIC: usize = 0x000;
const VR_DEVICE_ID: usize = 0x008;
const VR_HOST_FEATURES: usize = 0x010;
const VR_HOST_FEATURES_SEL: usize = 0x014;
const VR_GUEST_FEATURES: usize = 0x020;
const VR_GUEST_FEATURES_SEL: usize = 0x024;
const VR_GUEST_PAGE_SIZE: usize = 0x028;
const VR_QUEUE_SEL: usize = 0x030;
const VR_QUEUE_NUM_MAX: usize = 0x034;
const VR_QUEUE_NUM: usize = 0x038;
const VR_QUEUE_ALIGN: usize = 0x03c;
const VR_QUEUE_PFN: usize = 0x040;
const VR_QUEUE_NOTIFY: usize = 0x050;
const VR_STATUS: usize = 0x070;

const ST_ACK: u32 = 1;
const ST_DRIVER: u32 = 2;
const ST_DRIVER_OK: u32 = 4;

const VQ_SIZE: usize = 8;
const VIRTQ_DESC_F_NEXT: u16 = 1;

// virtio-net queues: 0 = receive, 1 = transmit. M1 is transmit-only.
const Q_RX: u32 = 0;
const Q_TX: u32 = 1;

// DMA layout. The TX virtqueue lives on the first page (desc | avail | used at
// the 4 KiB alignment the legacy transport requires); RX gets its own page so
// the device sees a valid PFN for every queue. Frame staging sits past both.
const TX_RING_OFF: usize = 0;
const RX_RING_OFF: usize = 0x2000;
const DESC_OFF: usize = 0;
const AVAIL_OFF: usize = 128;
const USED_OFF: usize = 4096;
// The granted DMA window is 16 KiB. TX ring occupies 0..0x1046 (used ring sits at
// the 4 KiB alignment) and RX ring 0x2000..0x3046, so staging goes above both and
// still inside the window — writing past it would fault the daemon.
const HDR_OFF: usize = 0x3100;
/// Request/response staging, in the gap the 10-byte net header leaves before the
/// frame buffer. The kernel writes the effect request here BEFORE launching this
/// daemon, and the daemon overwrites it with the gateway's reply before exiting
/// - the same shared-window handoff the block daemon uses, and the reason no new
/// grant is needed for an effect that talks both ways.
const REQ_OFF: usize = 0x3120;
const REQ_MAX: usize = 0xE0;
const FRAME_OFF: usize = 0x3200;
/// Receive buffers. Two of 1536 bytes each — enough for a full Ethernet frame plus
/// the virtio header — placed last so they end exactly on the 16 KiB grant
/// boundary. The device WRITES here, which is why these descriptors carry
/// `VIRTQ_DESC_F_WRITE`.
const RX_BUF0_OFF: usize = 0x3400;
const RX_BUF_SZ: usize = 0x600;
const RX_NBUF: usize = 2;

/// Legacy `virtio_net_hdr` (no MRG_RXBUF negotiated) is 10 bytes, all zero for a
/// plain frame with no offload.
const NET_HDR_LEN: usize = 10;

const VIRTQ_DESC_F_WRITE: u16 = 2;
const ETHERTYPE_IPV4: u16 = 0x0800;
const ETHERTYPE_ARP: u16 = 0x0806;
const IP_PROTO_ICMP: u8 = 1;
const OP_SEND: usize = 0;
const OP_PING: usize = 1;
/// A real external effect: send the staged request, then WAIT for the reply and
/// hand it back. The difference from OP_SEND is not the wire format, it is that
/// the outcome is observed rather than assumed.
const OP_EFFECT: usize = 2;
const IP_PROTO_UDP: u8 = 17;
const SRC_PORT: u16 = 12345;
/// Our address and the QEMU user-net gateway, which answers ARP and ICMP.
const SRC_IP: [u8; 4] = [10, 0, 2, 15];
const SRC_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
const PING_ID: u16 = 0xDE20;
const PING_SEQ: u16 = 1;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

#[unsafe(no_mangle)]
#[unsafe(link_section = ".text._start")]
extern "C" fn _start() -> ! {
    unsafe {
        asm!("li sp, 0x40700000", "j {main}", main = sym main, options(noreturn))
    }
}

fn sys_print(s: &[u8]) {
    unsafe {
        asm!("ecall",
            in("a0") s.as_ptr() as usize, in("a1") s.len(), in("a7") SYS_PRINT,
            lateout("a0") _, lateout("a1") _);
    }
}

/// Sleep until a device interrupt is serviced (see the block daemon).
fn sys_irq_wait(prev: usize) -> usize {
    let out: usize;
    unsafe { asm!("ecall", inout("a0") prev => out, in("a7") SYS_IRQ_WAIT) };
    out
}

fn sys_exit(code: usize) -> ! {
    unsafe { asm!("ecall", in("a0") code, in("a7") SYS_EXIT, options(noreturn)) }
}

fn print_num(mut v: usize) {
    let mut buf = [0u8; 20];
    let mut i = buf.len();
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    sys_print(&buf[i..]);
}

fn r32(off: usize) -> u32 {
    unsafe { core::ptr::read_volatile((NIC_VA + off) as *const u32) }
}

fn w32(off: usize, val: u32) {
    unsafe { core::ptr::write_volatile((NIC_VA + off) as *mut u32, val) }
}

fn wr8(off: usize, v: u8) {
    unsafe { core::ptr::write_volatile((DMA_VA + off) as *mut u8, v) }
}

fn rd16(off: usize) -> u16 {
    unsafe { core::ptr::read_volatile((DMA_VA + off) as *const u16) }
}

fn wr16(off: usize, v: u16) {
    unsafe { core::ptr::write_volatile((DMA_VA + off) as *mut u16, v) }
}

fn wr32(off: usize, v: u32) {
    unsafe { core::ptr::write_volatile((DMA_VA + off) as *mut u32, v) }
}

fn wr64(off: usize, v: u64) {
    unsafe { core::ptr::write_volatile((DMA_VA + off) as *mut u64, v) }
}

/// Place a descriptor in the TX ring.
fn put_desc(i: usize, addr: u64, len: u32, flags: u16, next: u16) {
    let e = TX_RING_OFF + DESC_OFF + i * 16;
    wr64(e, addr);
    wr32(e + 8, len);
    wr16(e + 12, flags);
    wr16(e + 14, next);
}

/// Clear a virtqueue's memory. The DMA window is reused across daemon launches,
/// so a fresh device init must start from a fresh ring — otherwise the device
/// resets its own index to zero, sees a stale avail index, and processes buffers
/// that were never offered.
fn zero_ring(base: usize) {
    let mut i = 0usize;
    while i < 256 {
        wr8(base + i, 0);
        i += 1;
    }
    i = 0;
    while i < 16 {
        wr8(base + USED_OFF + i, 0);
        i += 1;
    }
}

/// Bring the NIC up: acknowledge, negotiate no features (legacy header, no
/// offload), give both queues a valid ring, then DRIVER_OK.
fn nic_init(dma_pa: usize) -> bool {
    if r32(VR_MAGIC) != VIRTIO_MAGIC || r32(VR_DEVICE_ID) != VIRTIO_ID_NET {
        return false;
    }
    w32(VR_STATUS, 0);
    w32(VR_STATUS, ST_ACK);
    w32(VR_STATUS, ST_ACK | ST_DRIVER);
    // Negotiate nothing: a 10-byte legacy header and no checksum/GSO offload.
    w32(VR_HOST_FEATURES_SEL, 0);
    let _ = r32(VR_HOST_FEATURES);
    w32(VR_GUEST_FEATURES_SEL, 0);
    w32(VR_GUEST_FEATURES, 0);
    w32(VR_GUEST_PAGE_SIZE, 4096);

    zero_ring(TX_RING_OFF);
    zero_ring(RX_RING_OFF);

    // Receive queue: a valid ring so the device sees every queue configured,
    // with no buffers offered (M1 is transmit-only).
    w32(VR_QUEUE_SEL, Q_RX);
    if r32(VR_QUEUE_NUM_MAX) == 0 {
        return false;
    }
    w32(VR_QUEUE_NUM, VQ_SIZE as u32);
    w32(VR_QUEUE_ALIGN, 4096);
    w32(VR_QUEUE_PFN, ((dma_pa + RX_RING_OFF) >> 12) as u32);

    // Transmit queue.
    w32(VR_QUEUE_SEL, Q_TX);
    if r32(VR_QUEUE_NUM_MAX) == 0 {
        return false;
    }
    w32(VR_QUEUE_NUM, VQ_SIZE as u32);
    w32(VR_QUEUE_ALIGN, 4096);
    w32(VR_QUEUE_PFN, ((dma_pa + TX_RING_OFF) >> 12) as u32);

    w32(VR_STATUS, ST_ACK | ST_DRIVER | ST_DRIVER_OK);
    true
}

/// The standard internet checksum over `len` bytes of the DMA window at `off`.
///
/// The arithmetic is `dezh_core::net::internet_checksum`, and it is there rather
/// than here for a reason this function is the example of. It used to do the
/// summing itself, reading through `DMA_VA` — so it had no input a test could
/// supply, and the only way to exercise it was to boot a machine and send a
/// packet. An IPv4 header is always even-length, but ICMP covers header +
/// payload and can be odd; the final byte must be padded with a zero, not
/// dropped, and dropping it produced a checksum the host rejected *silently* —
/// the echo went out and no reply ever came back.
///
/// What stays here is the reading. Handing `dezh-core` an accessor keeps the
/// volatile loads at the address that needs them; building a `&[u8]` over a DMA
/// window instead would be the aliasing `Global<T>` exists to prevent.
fn ip_checksum(off: usize, len: usize) -> u16 {
    dezh_core::net::internet_checksum(len, |i| unsafe {
        core::ptr::read_volatile((DMA_VA + off + i) as *const u8)
    })
}

/// Build one Ethernet + IPv4 + UDP frame carrying `payload` into the DMA window.
/// Returns the frame length. Broadcast destination so the frame is unambiguous
/// in a capture; source is QEMU user-net's guest address.
fn build_frame(payload: &[u8], dst_ip: [u8; 4], dst_port: u16) -> usize {
    build_frame_to([0xff; 6], payload, dst_ip, dst_port)
}

/// As `build_frame`, but to a known MAC. A one-way send can broadcast; a request
/// whose reply we intend to WAIT for goes to the address ARP actually resolved,
/// so the answer comes back to us rather than to whoever else is listening.
fn build_frame_to(dst_mac: [u8; 6], payload: &[u8], dst_ip: [u8; 4], dst_port: u16) -> usize {
    let mut o = FRAME_OFF;
    // Ethernet: src 52:54:00:12:34:56, ethertype IPv4.
    let mut i = 0;
    while i < 6 {
        wr8(o + i, dst_mac[i]);
        i += 1;
    }
    let src_mac = [0x52u8, 0x54, 0x00, 0x12, 0x34, 0x56];
    i = 0;
    while i < 6 {
        wr8(o + 6 + i, src_mac[i]);
        i += 1;
    }
    wr8(o + 12, 0x08);
    wr8(o + 13, 0x00);
    o += 14;

    let udp_len = 8 + payload.len();
    let ip_len = 20 + udp_len;
    // IPv4 header.
    wr8(o, 0x45); // v4, IHL 5
    wr8(o + 1, 0x00);
    wr8(o + 2, (ip_len >> 8) as u8);
    wr8(o + 3, ip_len as u8);
    wr8(o + 4, 0x4d);
    wr8(o + 5, 0x5a); // id
    wr8(o + 6, 0x00);
    wr8(o + 7, 0x00); // no fragment
    wr8(o + 8, 64); // TTL
    wr8(o + 9, 17); // UDP
    wr8(o + 10, 0);
    wr8(o + 11, 0); // checksum placeholder
    let src_ip = [10u8, 0, 2, 15];
    i = 0;
    while i < 4 {
        wr8(o + 12 + i, src_ip[i]);
        wr8(o + 16 + i, dst_ip[i]);
        i += 1;
    }
    let csum = ip_checksum(o, 20);
    wr8(o + 10, (csum >> 8) as u8);
    wr8(o + 11, csum as u8);
    o += 20;

    // UDP header: checksum 0 is legal over IPv4.
    wr8(o, 0x30);
    wr8(o + 1, 0x39); // src port 12345
    wr8(o + 2, (dst_port >> 8) as u8);
    wr8(o + 3, dst_port as u8);
    wr8(o + 4, (udp_len >> 8) as u8);
    wr8(o + 5, udp_len as u8);
    wr8(o + 6, 0);
    wr8(o + 7, 0);
    o += 8;

    i = 0;
    while i < payload.len() {
        wr8(o + i, payload[i]);
        i += 1;
    }
    14 + ip_len
}

fn rd8(off: usize) -> u8 {
    unsafe { core::ptr::read_volatile((DMA_VA + off) as *const u8) }
}

fn rd32(off: usize) -> u32 {
    unsafe { core::ptr::read_volatile((DMA_VA + off) as *const u32) }
}

/// Place a device-writable descriptor in the RX ring.
fn put_rx_desc(i: usize, addr: u64, len: u32) {
    let e = RX_RING_OFF + DESC_OFF + i * 16;
    wr64(e, addr);
    wr32(e + 8, len);
    wr16(e + 12, VIRTQ_DESC_F_WRITE);
    wr16(e + 14, 0);
}

/// Offer one RX buffer to the device.
fn rx_offer(dma_pa: usize, id: usize) {
    put_rx_desc(
        id,
        (dma_pa + RX_BUF0_OFF + id * RX_BUF_SZ) as u64,
        RX_BUF_SZ as u32,
    );
    let avail = RX_RING_OFF + AVAIL_OFF;
    let idx = rd16(avail + 2);
    wr16(avail + 4 + (idx as usize % VQ_SIZE) * 2, id as u16);
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    wr16(avail + 2, idx.wrapping_add(1));
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    w32(VR_QUEUE_NOTIFY, Q_RX);
}

/// Offer every RX buffer. Until this runs the NIC has nowhere to put an incoming
/// frame and silently drops it — this is what makes the daemon able to receive.
fn rx_arm(dma_pa: usize) {
    let mut i = 0usize;
    while i < RX_NBUF {
        rx_offer(dma_pa, i);
        i += 1;
    }
}

/// Block until the device delivers a frame. Returns (buffer id, payload offset,
/// payload length) with the virtio header already skipped, or None on timeout.
fn rx_wait(last: &mut u16) -> Option<(usize, usize, usize)> {
    let used = RX_RING_OFF + USED_OFF;
    let mut seen = sys_irq_wait(usize::MAX);
    let mut waits = 0u32;
    while rd16(used + 2) == *last {
        seen = sys_irq_wait(seen);
        waits += 1;
        if waits > 20_000 {
            return None;
        }
    }
    // Used ring: flags(2) idx(2) then elements of (id: u32, len: u32).
    let slot = (*last as usize) % VQ_SIZE;
    let id = rd32(used + 4 + slot * 8) as usize;
    let len = rd32(used + 4 + slot * 8 + 4) as usize;
    *last = last.wrapping_add(1);
    if id >= RX_NBUF || len <= NET_HDR_LEN {
        return None;
    }
    Some((
        id,
        RX_BUF0_OFF + id * RX_BUF_SZ + NET_HDR_LEN,
        len - NET_HDR_LEN,
    ))
}

/// Write an Ethernet header at `o`; returns the offset just past it.
fn put_eth(o: usize, dst: [u8; 6], ethertype: u16) -> usize {
    let mut i = 0usize;
    while i < 6 {
        wr8(o + i, dst[i]);
        wr8(o + 6 + i, SRC_MAC[i]);
        i += 1;
    }
    wr8(o + 12, (ethertype >> 8) as u8);
    wr8(o + 13, ethertype as u8);
    o + 14
}

/// Build an ARP request asking who owns `target_ip`. Returns the frame length.
fn build_arp_request(target_ip: [u8; 4]) -> usize {
    let o = put_eth(FRAME_OFF, [0xff; 6], ETHERTYPE_ARP);
    wr8(o, 0);
    wr8(o + 1, 1); // hardware type: Ethernet
    wr8(o + 2, 0x08);
    wr8(o + 3, 0x00); // protocol type: IPv4
    wr8(o + 4, 6); // hardware address length
    wr8(o + 5, 4); // protocol address length
    wr8(o + 6, 0);
    wr8(o + 7, 1); // opcode: request
    let mut i = 0usize;
    while i < 6 {
        wr8(o + 8 + i, SRC_MAC[i]); // sender hardware address
        wr8(o + 18 + i, 0); // target hardware address: unknown
        i += 1;
    }
    i = 0;
    while i < 4 {
        wr8(o + 14 + i, SRC_IP[i]);
        wr8(o + 24 + i, target_ip[i]);
        i += 1;
    }
    14 + 28
}

/// If the received frame is an ARP reply from `from_ip`, return its MAC.
fn parse_arp_reply(off: usize, len: usize, from_ip: [u8; 4]) -> Option<[u8; 6]> {
    if len < 42 {
        return None;
    }
    let ethertype = ((rd8(off + 12) as u16) << 8) | rd8(off + 13) as u16;
    if ethertype != ETHERTYPE_ARP {
        return None;
    }
    let a = off + 14;
    let opcode = ((rd8(a + 6) as u16) << 8) | rd8(a + 7) as u16;
    if opcode != 2 {
        return None; // not a reply
    }
    let mut i = 0usize;
    while i < 4 {
        if rd8(a + 14 + i) != from_ip[i] {
            return None; // some other host answered
        }
        i += 1;
    }
    let mut mac = [0u8; 6];
    i = 0;
    while i < 6 {
        mac[i] = rd8(a + 8 + i);
        i += 1;
    }
    Some(mac)
}

/// Build an ICMP echo request to `dst_ip` via `dst_mac`. Returns the frame length.
fn build_icmp_echo(dst_mac: [u8; 6], dst_ip: [u8; 4]) -> usize {
    let payload = b"DEZH-PING";
    let icmp_len = 8 + payload.len();
    let ip_len = 20 + icmp_len;
    let o = put_eth(FRAME_OFF, dst_mac, ETHERTYPE_IPV4);

    wr8(o, 0x45);
    wr8(o + 1, 0x00);
    wr8(o + 2, (ip_len >> 8) as u8);
    wr8(o + 3, ip_len as u8);
    wr8(o + 4, 0x4d);
    wr8(o + 5, 0x5b); // id
    wr8(o + 6, 0x00);
    wr8(o + 7, 0x00);
    wr8(o + 8, 64); // TTL
    wr8(o + 9, IP_PROTO_ICMP);
    wr8(o + 10, 0);
    wr8(o + 11, 0);
    let mut i = 0usize;
    while i < 4 {
        wr8(o + 12 + i, SRC_IP[i]);
        wr8(o + 16 + i, dst_ip[i]);
        i += 1;
    }
    let csum = ip_checksum(o, 20);
    wr8(o + 10, (csum >> 8) as u8);
    wr8(o + 11, csum as u8);

    let c = o + 20;
    wr8(c, 8); // echo request
    wr8(c + 1, 0);
    wr8(c + 2, 0);
    wr8(c + 3, 0); // checksum placeholder
    wr8(c + 4, (PING_ID >> 8) as u8);
    wr8(c + 5, PING_ID as u8);
    wr8(c + 6, (PING_SEQ >> 8) as u8);
    wr8(c + 7, PING_SEQ as u8);
    i = 0;
    while i < payload.len() {
        wr8(c + 8 + i, payload[i]);
        i += 1;
    }
    // ICMP's checksum covers the whole message, not just a header.
    let icsum = ip_checksum(c, icmp_len);
    wr8(c + 2, (icsum >> 8) as u8);
    wr8(c + 3, icsum as u8);

    14 + ip_len
}

/// True if the frame is the ICMP echo REPLY to the request we just sent.
fn is_our_echo_reply(off: usize, len: usize, from_ip: [u8; 4]) -> bool {
    if len < 42 {
        return false;
    }
    let ethertype = ((rd8(off + 12) as u16) << 8) | rd8(off + 13) as u16;
    if ethertype != ETHERTYPE_IPV4 {
        return false;
    }
    let ip = off + 14;
    let ihl = (rd8(ip) & 0x0f) as usize * 4;
    if rd8(ip) >> 4 != 4 || rd8(ip + 9) != IP_PROTO_ICMP {
        return false;
    }
    let mut i = 0usize;
    while i < 4 {
        if rd8(ip + 12 + i) != from_ip[i] {
            return false; // not from the host we pinged
        }
        i += 1;
    }
    let c = ip + ihl;
    if rd8(c) != 0 {
        return false; // type 0 = echo reply
    }
    let id = ((rd8(c + 4) as u16) << 8) | rd8(c + 5) as u16;
    let seq = ((rd8(c + 6) as u16) << 8) | rd8(c + 7) as u16;
    id == PING_ID && seq == PING_SEQ
}

/// Transmit one frame and wait for the device to consume it.
fn transmit(dma_pa: usize, frame_len: usize) -> bool {
    // A zeroed legacy virtio_net_hdr: no offload, no GSO.
    let mut i = 0usize;
    while i < NET_HDR_LEN {
        wr8(HDR_OFF + i, 0);
        i += 1;
    }
    put_desc(0, (dma_pa + HDR_OFF) as u64, NET_HDR_LEN as u32, VIRTQ_DESC_F_NEXT, 1);
    put_desc(1, (dma_pa + FRAME_OFF) as u64, frame_len as u32, 0, 0);

    let avail = TX_RING_OFF + AVAIL_OFF;
    let used = TX_RING_OFF + USED_OFF;
    let idx = rd16(avail + 2);
    wr16(avail + 4 + (idx as usize % VQ_SIZE) * 2, 0);
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
    wr16(avail + 2, idx.wrapping_add(1));
    core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);

    let before = rd16(used + 2);
    let mut seen = sys_irq_wait(usize::MAX);
    w32(VR_QUEUE_NOTIFY, Q_TX);
    // Block on the NIC rather than spinning; bounded so a silent device cannot
    // wedge the daemon forever.
    let mut waits = 0u32;
    while rd16(used + 2) == before {
        seen = sys_irq_wait(seen);
        waits += 1;
        if waits > 10_000 {
            return false;
        }
    }
    true
}

/// Read the request the kernel staged, as a byte slice into the DMA window.
fn staged_request(len: usize) -> &'static [u8] {
    let n = if len > REQ_MAX { REQ_MAX } else { len };
    unsafe { core::slice::from_raw_parts((DMA_VA + REQ_OFF) as *const u8, n) }
}

/// Write the gateway's answer back where the kernel will read it, NUL-terminated
/// so the kernel does not have to be told the length a second time.
fn stage_reply(off: usize, len: usize) {
    let n = if len > REQ_MAX - 1 { REQ_MAX - 1 } else { len };
    let mut i = 0;
    while i < n {
        wr8(REQ_OFF + i, rd8(off + i));
        i += 1;
    }
    wr8(REQ_OFF + n, 0);
}

/// Is this frame a UDP datagram from `src_ip` addressed to our source port?
/// Returns the payload's DMA offset and length.
///
/// Every field is checked rather than assumed. A daemon that trusted the first
/// frame off the wire would let anything on the segment answer for the gateway,
/// and the whole point of the destination capability is that the operator named
/// who they were willing to talk to.
fn parse_udp_reply(off: usize, len: usize, src_ip: [u8; 4]) -> Option<(usize, usize)> {
    if len < 14 + 20 + 8 {
        return None;
    }
    // `rx_wait` has already stepped past the virtio net header and shortened
    // `len` to match, so `off` is the Ethernet header. Adding NET_HDR_LEN again
    // here parses ten bytes into it, which is exactly the bug the first run of
    // the end-to-end test caught: the gateway committed, and Dezh saw nothing.
    let o = off;
    if ((rd8(o + 12) as u16) << 8 | rd8(o + 13) as u16) != ETHERTYPE_IPV4 {
        return None;
    }
    let ip = o + 14;
    let ihl = ((rd8(ip) & 0x0f) as usize) * 4;
    if ihl < 20 || rd8(ip + 9) != IP_PROTO_UDP {
        return None;
    }
    let mut i = 0;
    while i < 4 {
        if rd8(ip + 12 + i) != src_ip[i] {
            return None;
        }
        i += 1;
    }
    let udp = ip + ihl;
    let dport = (rd8(udp + 2) as u16) << 8 | rd8(udp + 3) as u16;
    if dport != SRC_PORT {
        return None;
    }
    let udp_len = ((rd8(udp + 4) as usize) << 8 | rd8(udp + 5) as usize).saturating_sub(8);
    Some((udp + 8, udp_len))
}

/// `marz-effect`: the request/response path. Send the staged request to an
/// authorized destination and WAIT for the answer.
///
/// This is what makes an effect an effect rather than a transmission: the
/// outcome is observed. What it does NOT do is verify the outcome is true - the
/// gateway is outside the TCB and can lie. Dezh proves the request was
/// authorized, left the machine, and was answered.
fn do_effect(dma_pa: usize, dst_ip: [u8; 4], dst_port: u16, req_len: usize) -> ! {
    rx_arm(dma_pa);
    let mut last_used = 0u16;

    let arp_len = build_arp_request(dst_ip);
    if !transmit(dma_pa, arp_len) {
        sys_print(b"  [marz] ARP request transmit timed out\n");
        sys_exit(1);
    }
    // Declared without a value: every path out of the loop below either assigns
    // it or exits the daemon, so a placeholder MAC can never be transmitted.
    let gw_mac;
    let mut tries = 0;
    loop {
        let Some((id, off, len)) = rx_wait(&mut last_used) else {
            sys_print(b"  [marz] no ARP reply arrived\n");
            sys_exit(1);
        };
        let hit = parse_arp_reply(off, len, dst_ip);
        rx_offer(dma_pa, id);
        if let Some(mac) = hit {
            gw_mac = mac;
            break;
        }
        tries += 1;
        if tries > 8 {
            sys_print(b"  [marz] no ARP reply arrived\n");
            sys_exit(1);
        }
    }

    let payload = staged_request(req_len);
    let frame_len = build_frame_to(gw_mac, payload, dst_ip, dst_port);
    if !transmit(dma_pa, frame_len) {
        sys_print(b"  [marz] effect request transmit timed out\n");
        sys_exit(1);
    }
    sys_print(b"  [marz] effect request left the machine; waiting for the outcome\n");

    tries = 0;
    loop {
        let Some((id, off, len)) = rx_wait(&mut last_used) else {
            sys_print(b"  [marz] no reply from the gateway\n");
            sys_exit(2);
        };
        let hit = parse_udp_reply(off, len, dst_ip);
        if let Some((poff, plen)) = hit {
            stage_reply(poff, plen);
            rx_offer(dma_pa, id);
            sys_print(b"  [marz] EFFECT-REPLY: the gateway answered\n");
            sys_exit(0);
        }
        rx_offer(dma_pa, id);
        tries += 1;
        if tries > 8 {
            sys_print(b"  [marz] no reply from the gateway\n");
            sys_exit(2);
        }
    }
}

/// `marz-ping`: resolve the destination with ARP, then exchange a real ICMP echo.
/// Unlike a send, this needs the RECEIVE path — the daemon must offer the NIC
/// buffers, block on the device's interrupt, and parse what actually came back.
fn do_ping(dma_pa: usize, dst_ip: [u8; 4]) -> ! {
    rx_arm(dma_pa);
    sys_print(b"  [marz] receive queue armed (buffers offered to the NIC)\n");
    let mut last_used = 0u16;

    // 1. Who owns the destination address?
    let arp_len = build_arp_request(dst_ip);
    if !transmit(dma_pa, arp_len) {
        sys_print(b"  [marz] ARP request transmit timed out\n");
        sys_exit(1);
    }
    sys_print(b"  [marz] ARP request sent; waiting for a reply from the wire\n");
    // Declared without a value: every path out of the loop below either assigns
    // it or exits the daemon, so a placeholder MAC can never be transmitted.
    let gw_mac;
    let mut tries = 0;
    loop {
        let Some((id, off, len)) = rx_wait(&mut last_used) else {
            sys_print(b"  [marz] no ARP reply arrived\n");
            sys_exit(1);
        };
        let hit = parse_arp_reply(off, len, dst_ip);
        rx_offer(dma_pa, id); // hand the buffer back to the device
        if let Some(mac) = hit {
            gw_mac = mac;
            break;
        }
        tries += 1;
        if tries > 8 {
            sys_print(b"  [marz] no ARP reply arrived\n");
            sys_exit(1);
        }
    }
    sys_print(b"  [marz] ARP reply received: the destination is reachable\n");

    // 2. A real ICMP echo, and a real reply.
    let icmp_len = build_icmp_echo(gw_mac, dst_ip);
    if !transmit(dma_pa, icmp_len) {
        sys_print(b"  [marz] ICMP echo transmit timed out\n");
        sys_exit(1);
    }
    sys_print(b"  [marz] ICMP echo request sent; waiting for the reply\n");
    tries = 0;
    loop {
        let Some((id, off, len)) = rx_wait(&mut last_used) else {
            sys_print(b"  [marz] no ICMP echo reply arrived\n");
            sys_exit(1);
        };
        let hit = is_our_echo_reply(off, len, dst_ip);
        rx_offer(dma_pa, id);
        if hit {
            sys_print(b"  [marz] PING-OK: ICMP echo reply received and matched (id+seq)\n");
            sys_exit(0);
        }
        tries += 1;
        if tries > 8 {
            sys_print(b"  [marz] no ICMP echo reply arrived\n");
            sys_exit(1);
        }
    }
}

#[unsafe(no_mangle)]
extern "C" fn main(op: usize, dma_pa: usize, dest: usize, _a3: usize) -> ! {
    sys_print(b"  [marz] egress daemon started; holds ONLY the granted NIC page + DMA\n");
    if !nic_init(dma_pa) {
        sys_print(b"  [marz] no virtio-net on the granted page (device init failed)\n");
        sys_exit(1);
    }
    sys_print(b"  [marz] virtio-net ready (no features negotiated, transmit queue armed)\n");

    // The destination is chosen by the kernel gate, not by this daemon: it is
    // part of the capability that authorized the send.
    let dst_ip = [
        (dest >> 24) as u8,
        (dest >> 16) as u8,
        (dest >> 8) as u8,
        dest as u8,
    ];
    let dst_port = (dest >> 32) as u16;
    if op == OP_PING {
        do_ping(dma_pa, dst_ip);
    }
    if op == OP_EFFECT {
        do_effect(dma_pa, dst_ip, dst_port, _a3);
    }
    let _ = OP_SEND;
    let payload = b"DEZH-MARZ-EGRESS-v0";
    let frame_len = build_frame(payload, dst_ip, dst_port);
    sys_print(b"  [marz] frame built: Ethernet+IPv4+UDP len=");
    print_num(frame_len);
    sys_print(b" payload=\"DEZH-MARZ-EGRESS-v0\"\n");

    if transmit(dma_pa, frame_len) {
        sys_print(b"  [marz] EGRESS: frame left the machine (device consumed the buffer)\n");
        sys_exit(0);
    }
    sys_print(b"  [marz] transmit timed out; nothing left the machine\n");
    sys_exit(1)
}
