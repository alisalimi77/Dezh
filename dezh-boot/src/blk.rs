//! Legacy virtio-blk driver + a durable rollbackable store (persistence).
//!
//! QEMU `virt` exposes virtio transports as MMIO at 0x1000_1000 (8 slots, stride
//! 0x1000) and defaults to the legacy (version 1) layout: one split virtqueue at
//! QueuePFN, polled completion. The durable store keeps the current value in
//! sector 0 and the previous in sector 1, so a change can be rolled back — and
//! it survives reboot. (The driver runs in the kernel for now; relocating it to
//! a user-space process holding the device's MMIO + DMA capabilities is the next
//! step toward drivers fully out of the kernel.)

use crate::kprintln;
use core::fmt::Write;
use core::sync::atomic::Ordering;

const VIRTIO_MMIO_BASE: usize = 0x1000_1000;
const VIRTIO_MMIO_STRIDE: usize = 0x1000;
const VIRTIO_MMIO_COUNT: usize = 8;
const VIRTIO_MAGIC: u32 = 0x7472_6976; // "virt"
const VIRTIO_ID_BLOCK: u32 = 2;

fn mmio_r32(addr: usize) -> u32 {
    unsafe { core::ptr::read_volatile(addr as *const u32) }
}
fn mmio_w32(addr: usize, val: u32) {
    unsafe { core::ptr::write_volatile(addr as *mut u32, val) }
}

fn find_block() -> Option<usize> {
    for i in 0..VIRTIO_MMIO_COUNT {
        let base = VIRTIO_MMIO_BASE + i * VIRTIO_MMIO_STRIDE;
        if mmio_r32(base) == VIRTIO_MAGIC && mmio_r32(base + 0x008) == VIRTIO_ID_BLOCK {
            return Some(base);
        }
    }
    None
}

// virtio-mmio legacy registers / status bits.
const VR_HOST_FEATURES_SEL: usize = 0x014;
const VR_HOST_FEATURES: usize = 0x010;
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
const DESC_OFF: usize = 0;
const AVAIL_OFF: usize = 128; // 16 * VQ_SIZE
const USED_OFF: usize = 4096; // QueueAlign
const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;
const SECTOR_SIZE: usize = 512;

#[repr(align(4096))]
#[allow(dead_code)]
struct Virtq([u8; 8192]);
static mut VIRTQ: Virtq = Virtq([0; 8192]);

#[repr(align(16))]
struct BlkReq {
    hdr: [u8; 16], // type:u32, reserved:u32, sector:u64
    data: [u8; SECTOR_SIZE],
    status: u8,
}
static mut BLKREQ: BlkReq = BlkReq {
    hdr: [0; 16],
    data: [0; SECTOR_SIZE],
    status: 0,
};

fn vq(off: usize) -> usize {
    core::ptr::addr_of_mut!(VIRTQ) as usize + off
}

fn data_ptr() -> *mut u8 {
    unsafe { core::ptr::addr_of_mut!(BLKREQ.data) as *mut u8 }
}

/// The data buffer as text, up to the first NUL (capped for display).
fn data_str() -> &'static str {
    unsafe {
        let d = &(*core::ptr::addr_of!(BLKREQ.data))[..];
        let n = d.iter().position(|&b| b == 0).unwrap_or(64).min(64);
        core::str::from_utf8(&d[..n]).unwrap_or("<non-utf8>")
    }
}

fn set_data(bytes: &[u8]) {
    unsafe {
        core::ptr::write_bytes(data_ptr(), 0, SECTOR_SIZE);
        let n = bytes.len().min(SECTOR_SIZE - 1);
        core::ptr::copy_nonoverlapping(bytes.as_ptr(), data_ptr(), n);
    }
}

fn init(base: usize) -> bool {
    mmio_w32(base + VR_STATUS, 0); // reset
    mmio_w32(base + VR_STATUS, ST_ACK);
    mmio_w32(base + VR_STATUS, ST_ACK | ST_DRIVER);
    mmio_w32(base + VR_HOST_FEATURES_SEL, 0);
    let _ = mmio_r32(base + VR_HOST_FEATURES);
    mmio_w32(base + VR_GUEST_FEATURES_SEL, 0);
    mmio_w32(base + VR_GUEST_FEATURES, 0);
    mmio_w32(base + VR_GUEST_PAGE_SIZE, 4096);
    mmio_w32(base + VR_QUEUE_SEL, 0);
    if mmio_r32(base + VR_QUEUE_NUM_MAX) == 0 {
        return false;
    }
    mmio_w32(base + VR_QUEUE_NUM, VQ_SIZE as u32);
    mmio_w32(base + VR_QUEUE_ALIGN, 4096);
    let pfn = (core::ptr::addr_of!(VIRTQ) as usize >> 12) as u32;
    mmio_w32(base + VR_QUEUE_PFN, pfn);
    mmio_w32(base + VR_STATUS, ST_ACK | ST_DRIVER | ST_DRIVER_OK);
    true
}

/// Read or write BLKREQ.data to one 512-byte sector. Returns the device status
/// byte (0 = OK).
fn rw(base: usize, sector: u64, write: bool) -> u8 {
    unsafe {
        let hdr = core::ptr::addr_of_mut!(BLKREQ.hdr) as usize;
        core::ptr::write_volatile(hdr as *mut u32, if write { 1 } else { 0 });
        core::ptr::write_volatile((hdr + 4) as *mut u32, 0);
        core::ptr::write_volatile((hdr + 8) as *mut u64, sector);
        let data = data_ptr() as usize;
        let status = core::ptr::addr_of_mut!(BLKREQ.status) as usize;
        core::ptr::write_volatile(status as *mut u8, 0xff);

        let d = vq(DESC_OFF);
        let put = |i: usize, addr: u64, len: u32, flags: u16, next: u16| {
            let e = d + i * 16;
            core::ptr::write_volatile(e as *mut u64, addr);
            core::ptr::write_volatile((e + 8) as *mut u32, len);
            core::ptr::write_volatile((e + 12) as *mut u16, flags);
            core::ptr::write_volatile((e + 14) as *mut u16, next);
        };
        put(0, hdr as u64, 16, VIRTQ_DESC_F_NEXT, 1);
        let data_flags = VIRTQ_DESC_F_NEXT | if write { 0 } else { VIRTQ_DESC_F_WRITE };
        put(1, data as u64, SECTOR_SIZE as u32, data_flags, 2);
        put(2, status as u64, 1, VIRTQ_DESC_F_WRITE, 0);

        let a = vq(AVAIL_OFF);
        let avail_idx = core::ptr::read_volatile((a + 2) as *const u16);
        core::ptr::write_volatile((a + 4 + (avail_idx as usize % VQ_SIZE) * 2) as *mut u16, 0);
        core::sync::atomic::fence(Ordering::SeqCst);
        core::ptr::write_volatile((a + 2) as *mut u16, avail_idx.wrapping_add(1));
        core::sync::atomic::fence(Ordering::SeqCst);

        let used = vq(USED_OFF);
        let used_before = core::ptr::read_volatile((used + 2) as *const u16);
        mmio_w32(base + VR_QUEUE_NOTIFY, 0);
        while core::ptr::read_volatile((used + 2) as *const u16) == used_before {
            core::hint::spin_loop();
        }
        core::ptr::read_volatile(status as *const u8)
    }
}

// --- Public API used by the console -----------------------------------------

/// Print every virtio device found (for the `disk` command).
pub fn list_devices() {
    let mut found = false;
    for i in 0..VIRTIO_MMIO_COUNT {
        let base = VIRTIO_MMIO_BASE + i * VIRTIO_MMIO_STRIDE;
        if mmio_r32(base) != VIRTIO_MAGIC {
            continue;
        }
        let ver = mmio_r32(base + 0x004);
        let dev = mmio_r32(base + 0x008);
        if dev != 0 {
            let kind = if dev == VIRTIO_ID_BLOCK { " (block)" } else { "" };
            kprintln!("  virtio-mmio[{i}] @ {base:#x}: version={ver} device-id={dev}{kind}");
            found = true;
        }
    }
    if !found {
        kprintln!("  no virtio devices found (start QEMU with a -drive + virtio-blk-device)");
    }
}

/// Write a marker to sector 0; returns the status byte (None if no disk).
pub fn bwrite() -> Option<u8> {
    let base = find_block()?;
    init(base);
    set_data(b"DEZH-PERSISTENT-DISK-OK");
    Some(rw(base, 0, true))
}

/// Read sector 0; returns (status, text) (None if no disk).
pub fn bread() -> Option<(u8, &'static str)> {
    let base = find_block()?;
    init(base);
    set_data(b"");
    let st = rw(base, 0, false);
    Some((st, data_str()))
}

/// Durable Cairn: set the current value (saving the old as previous). Persisted.
pub fn store_set(text: &str) -> Option<()> {
    let base = find_block()?;
    init(base);
    rw(base, 0, false); // read current
    rw(base, 1, true); // persist it as previous
    set_data(text.as_bytes());
    rw(base, 0, true);
    Some(())
}

/// Durable Cairn: read the current value.
pub fn store_get() -> Option<&'static str> {
    let base = find_block()?;
    init(base);
    rw(base, 0, false);
    Some(data_str())
}

/// Durable Cairn: restore the previous value as current. Persisted.
pub fn store_rollback() -> Option<&'static str> {
    let base = find_block()?;
    init(base);
    rw(base, 1, false); // read previous
    rw(base, 0, true); // restore as current
    Some(data_str())
}
