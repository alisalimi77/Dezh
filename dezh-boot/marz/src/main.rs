//! Marz — the guarded egress boundary daemon (M1: the device).
//!
//! A separate U-mode ELF that owns the NIC. It receives exactly two grants from
//! the kernel: the **single** virtio-net MMIO page (the kernel found the device;
//! this daemon never scans the window) and a DMA window. It holds no block
//! authority and no other device.
//!
//! M1 transmits one raw frame so the device path is proven end to end; the
//! authority gate (per-destination capability + DIFC declassification) and the
//! effect record land in M2/M3. See `docs/MARZ.md`.

#![no_std]
#![no_main]

use core::arch::asm;

const SYS_EXIT: usize = 0;
const SYS_PRINT: usize = 1;

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
const FRAME_OFF: usize = 0x3200;

/// Legacy `virtio_net_hdr` (no MRG_RXBUF negotiated) is 10 bytes, all zero for a
/// plain frame with no offload.
const NET_HDR_LEN: usize = 10;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

#[no_mangle]
#[link_section = ".text._start"]
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

fn ip_checksum(off: usize, len: usize) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0usize;
    while i + 1 < len {
        let hi = unsafe { core::ptr::read_volatile((DMA_VA + off + i) as *const u8) } as u32;
        let lo = unsafe { core::ptr::read_volatile((DMA_VA + off + i + 1) as *const u8) } as u32;
        sum += (hi << 8) | lo;
        i += 2;
    }
    while sum >> 16 != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

/// Build one Ethernet + IPv4 + UDP frame carrying `payload` into the DMA window.
/// Returns the frame length. Broadcast destination so the frame is unambiguous
/// in a capture; source is QEMU user-net's guest address.
fn build_frame(payload: &[u8], dst_ip: [u8; 4], dst_port: u16) -> usize {
    let mut o = FRAME_OFF;
    // Ethernet: dst broadcast, src 52:54:00:12:34:56, ethertype IPv4.
    let mut i = 0;
    while i < 6 {
        wr8(o + i, 0xff);
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
    w32(VR_QUEUE_NOTIFY, Q_TX);
    // Bounded wait: the device consumes a transmit buffer promptly.
    let mut spins = 0u32;
    while rd16(used + 2) == before {
        core::hint::spin_loop();
        spins += 1;
        if spins > 20_000_000 {
            return false;
        }
    }
    true
}

#[no_mangle]
extern "C" fn main(_op: usize, dma_pa: usize, dest: usize, _a3: usize) -> ! {
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
