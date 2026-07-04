//! User-space virtio-blk transaction program.
//!
//! The kernel maps only the granted virtio MMIO page and one DMA/bounce window
//! into this process. All disk I/O in the milestone flows through this U-mode
//! program, not through a kernel-resident block driver.

#![no_std]
#![no_main]

use core::arch::asm;

const SYS_EXIT: usize = 0;
const SYS_PRINT: usize = 1;
const SYS_SEND: usize = 6;
const SYS_RECV: usize = 7;
const SYS_PRINTNUM: usize = 8;

const OP_DISK: usize = 1;
const OP_BWRITE: usize = 2;
const OP_BREAD: usize = 3;
const OP_PSET: usize = 4;
const OP_PGET: usize = 5;
const OP_PROLLBACK: usize = 6;
const OP_NO_GRANT_PROBE: usize = 7;
const OP_DAEMON: usize = 8;
const OP_CLIENT_DEMO: usize = 9;
const OP_CLIENT_REQ: usize = 10;

const REQ_PROBE: usize = 1;
const REQ_BWRITE: usize = 2;
const REQ_BREAD: usize = 3;
const REQ_PSET: usize = 4;
const REQ_PGET: usize = 5;
const REQ_PROLLBACK: usize = 6;
const REQ_STOP: usize = 7;
const REQ_INSTALL_CHECK: usize = 8;
const REQ_INSTALL_INIT: usize = 9;
const REQ_ROOT_STATUS: usize = 10;
const REQ_APP_AVAILABLE: usize = 11;
const REQ_APP_INSTALLED: usize = 12;
const REQ_APP_INFO: usize = 13;
const REQ_APP_INSTALL_NOTE: usize = 14;
const REQ_APP_REQUIRE_NOTE: usize = 15;
const REQ_APP_REMOVE_NOTE: usize = 16;
const REQ_NOTE_SET: usize = 17;
const REQ_NOTE_GET: usize = 18;
const REQ_APP_INSTALL_LAB: usize = 19;
const REQ_APP_REQUIRE_LAB: usize = 20;
const REQ_APP_REMOVE_LAB: usize = 21;
const REQ_LAB_SET: usize = 22;
const REQ_LAB_GET: usize = 23;
const REQ_FAULT_DEMO: usize = 24;
const REQ_APP_INSTALL_CALC: usize = 25;
const REQ_APP_REQUIRE_CALC: usize = 26;
const REQ_APP_REMOVE_CALC: usize = 27;
const REQ_CALC_SET: usize = 28;
const REQ_CALC_GET: usize = 29;
const REQ_APP_INSTALL_VAULT: usize = 30;
const REQ_APP_REQUIRE_VAULT: usize = 31;
const REQ_APP_REMOVE_VAULT: usize = 32;
const REQ_VAULT_SET: usize = 33;
const REQ_VAULT_GET: usize = 34;
const REQ_PKG_STORE_INIT: usize = 35;
const REQ_PKG_REGISTRY_READ: usize = 36;
const REQ_PKG_REGISTRY_WRITE: usize = 37;
const REQ_PKG_BLOB_READ: usize = 38;
const REQ_PKG_BLOB_WRITE: usize = 39;
const REQ_PKG_JOURNAL_READ: usize = 40;
const REQ_PKG_JOURNAL_WRITE: usize = 41;
const REQ_CAIRN_INIT: usize = 42;
const REQ_CAIRN_COMMIT: usize = 43;
const REQ_CAIRN_GET: usize = 44;
const REQ_CAIRN_LOG: usize = 45;
const REQ_CAIRN_ROLLBACK: usize = 46;
const REQ_CAIRN_VERIFY: usize = 47;
const REQ_CAIRN_STATUS: usize = 48;

const IPC_PROTO_V1: usize = 0xd1;
const IPC_SERVICE_VIRTIO_BLOCK: usize = 1;
const IPC_STATUS_OK: usize = 0;
const IPC_STATUS_DENIED: usize = 1;
const IPC_STATUS_UNAVAILABLE: usize = 2;
const IPC_STATUS_TIMEOUT: usize = 3;
const IPC_STATUS_BAD_REQUEST: usize = 4;
const IPC_STATUS_IO_FAILURE: usize = 5;
const IPC_STATUS_FAULTED: usize = 6;

static mut MMIO_BASE: usize = 0x5000_0000;
const MMIO_WINDOW: usize = 0x5000_0000;
const MMIO_STRIDE: usize = 0x1000;
const MMIO_COUNT: usize = 8;
const DMA_VA: usize = 0x5100_0000;
const VIRTIO_MAGIC: u32 = 0x7472_6976;
const VIRTIO_ID_BLOCK: u32 = 2;

const VR_MAGIC: usize = 0x000;
const VR_VERSION: usize = 0x004;
const VR_DEVICE_ID: usize = 0x008;
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
const AVAIL_OFF: usize = 128;
const USED_OFF: usize = 4096;
const REQ_OFF: usize = 8192;
const INPUT_OFF: usize = 12288;
const SECTOR_SIZE: usize = 512;
const TEST_SECTOR: u64 = 8;
const CAIRN_CURRENT_SECTOR: u64 = 2;
const CAIRN_PREVIOUS_SECTOR: u64 = 3;
const INSTALL_MARKER_SECTOR: u64 = 0;
const ROOT_METADATA_SECTOR: u64 = 4;
const APP_REGISTRY_SECTOR: u64 = 5;
const APP_REGISTRY_PREVIOUS_SECTOR: u64 = 6;
const LAB_REGISTRY_SECTOR: u64 = 7;
const CALC_REGISTRY_SECTOR: u64 = 9;
const VAULT_REGISTRY_SECTOR: u64 = 10;
const NOTE_PRIVATE_ROOT_SECTOR: u64 = 16;
const LAB_PRIVATE_ROOT_SECTOR: u64 = 17;
const CALC_PRIVATE_ROOT_SECTOR: u64 = 18;
const VAULT_PRIVATE_ROOT_SECTOR: u64 = 19;
const PKG_STORE_MARKER_SECTOR: u64 = 24;
const PKG_REGISTRY_FIRST_SECTOR: u64 = 25;
const PKG_REGISTRY_LAST_SECTOR: u64 = 31;
const PKG_JOURNAL_FIRST_SECTOR: u64 = 32;
const PKG_JOURNAL_LAST_SECTOR: u64 = 39;
const PKG_BLOB_FIRST_SECTOR: u64 = 64;
const PKG_BLOB_LAST_SECTOR: u64 = 1599;
// --- Cairn v1: ref/commit-log store (sectors 1600..1855) ---------------------
// Superblock (1600) holds the namespace table (name -> head commit + count)
// and the next free commit slot. Commits (1601..1855) are append-only records:
// header + inline value object with an FNV-1a hash. Rollback moves a
// namespace's head ref back along the parent chain; history is never erased.
const CAIRN1_SUPER_SECTOR: u64 = 1600;
const CAIRN1_COMMIT_FIRST_SECTOR: u64 = 1601;
const CAIRN1_COMMIT_SLOTS: u32 = 255;
const CAIRN1_NS_MAX: usize = 8;
const CAIRN1_NONE: u32 = 0xffff_ffff;
const CAIRN1_VALUE_OFF: usize = 64;
const CAIRN1_VALUE_MAX: usize = SECTOR_SIZE - CAIRN1_VALUE_OFF;
// Kernel task-capability bits 8..15 gate access to Cairn namespace 0..7. The
// kernel attests the sender's caps on every recv; the daemon checks the bit
// for the requested namespace, so a client cannot reach another app's data.
const TASK_CAIRN_NS_BASE: usize = 8;
const CAIRN1_NS_NAMES: [&[u8]; 5] = [b"note", b"lab", b"calc", b"vault", b"agent"];
const VIRTQ_DESC_F_NEXT: u16 = 1;
const VIRTQ_DESC_F_WRITE: u16 = 2;

#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    loop {}
}

#[no_mangle]
#[link_section = ".text._start"]
extern "C" fn _start() -> ! {
    unsafe {
        asm!(
            "li sp, 0x40700000",
            "j {main}",
            main = sym main,
            options(noreturn)
        )
    }
}

fn sys_print(s: &[u8]) {
    unsafe {
        asm!("ecall",
            in("a0") s.as_ptr() as usize, in("a1") s.len(), in("a7") SYS_PRINT,
            lateout("a0") _, lateout("a1") _);
    }
}

fn sys_printnum(v: usize) {
    unsafe { asm!("ecall", inout("a0") v => _, in("a7") SYS_PRINTNUM) }
}

fn sys_exit(code: usize) -> ! {
    unsafe { asm!("ecall", in("a0") code, in("a7") SYS_EXIT, options(noreturn)) }
}

fn sys_send(to: usize, word: usize) -> usize {
    let rc: usize;
    unsafe {
        asm!("ecall",
            inout("a0") to => rc,
            in("a1") 0usize,
            in("a2") 0usize,
            in("a3") 0usize,
            in("a4") word,
            in("a7") SYS_SEND)
    };
    rc
}

fn sys_recv() -> (usize, usize, usize) {
    let from: usize;
    let word: usize;
    let sender_caps: usize;
    unsafe {
        asm!("ecall",
            inout("a0") 0usize => _,
            inout("a1") 0usize => from,
            out("a2") word,
            out("a3") sender_caps,
            in("a7") SYS_RECV)
    };
    (word, from, sender_caps)
}

fn r32(off: usize) -> u32 {
    unsafe { core::ptr::read_volatile((MMIO_BASE + off) as *const u32) }
}

fn w32(off: usize, val: u32) {
    unsafe { core::ptr::write_volatile((MMIO_BASE + off) as *mut u32, val) }
}

fn dma(off: usize) -> usize {
    DMA_VA + off
}

fn dma_pa(base: usize, off: usize) -> u64 {
    (base + off) as u64
}

fn clear_dma() {
    let mut i = 0usize;
    while i < INPUT_OFF {
        unsafe { core::ptr::write_volatile((DMA_VA + i) as *mut u8, 0) };
        i += 1;
    }
}

fn init(dma_base: usize) -> bool {
    let mut found = 0usize;
    let mut i = 0usize;
    while i < MMIO_COUNT {
        let base = MMIO_WINDOW + i * MMIO_STRIDE;
        let magic = unsafe { core::ptr::read_volatile((base + VR_MAGIC) as *const u32) };
        let dev = unsafe { core::ptr::read_volatile((base + VR_DEVICE_ID) as *const u32) };
        if magic == VIRTIO_MAGIC && dev == VIRTIO_ID_BLOCK {
            found = base;
            break;
        }
        i += 1;
    }
    if found == 0 {
        return false;
    }
    unsafe { MMIO_BASE = found };
    w32(VR_STATUS, 0);
    w32(VR_STATUS, ST_ACK);
    w32(VR_STATUS, ST_ACK | ST_DRIVER);
    w32(VR_HOST_FEATURES_SEL, 0);
    let _ = r32(VR_HOST_FEATURES);
    w32(VR_GUEST_FEATURES_SEL, 0);
    w32(VR_GUEST_FEATURES, 0);
    w32(VR_GUEST_PAGE_SIZE, 4096);
    w32(VR_QUEUE_SEL, 0);
    if r32(VR_QUEUE_NUM_MAX) == 0 {
        return false;
    }
    w32(VR_QUEUE_NUM, VQ_SIZE as u32);
    w32(VR_QUEUE_ALIGN, 4096);
    w32(VR_QUEUE_PFN, (dma_base >> 12) as u32);
    w32(VR_STATUS, ST_ACK | ST_DRIVER | ST_DRIVER_OK);
    true
}

fn put_desc(i: usize, addr: u64, len: u32, flags: u16, next: u16) {
    let e = dma(DESC_OFF + i * 16);
    unsafe {
        core::ptr::write_volatile(e as *mut u64, addr);
        core::ptr::write_volatile((e + 8) as *mut u32, len);
        core::ptr::write_volatile((e + 12) as *mut u16, flags);
        core::ptr::write_volatile((e + 14) as *mut u16, next);
    }
}

fn rw(dma_base: usize, sector: u64, write: bool) -> u8 {
    let hdr = dma(REQ_OFF);
    let data = dma(REQ_OFF + 16);
    let status = dma(REQ_OFF + 16 + SECTOR_SIZE);
    unsafe {
        core::ptr::write_volatile(hdr as *mut u32, if write { 1 } else { 0 });
        core::ptr::write_volatile((hdr + 4) as *mut u32, 0);
        core::ptr::write_volatile((hdr + 8) as *mut u64, sector);
        core::ptr::write_volatile(status as *mut u8, 0xff);
    }
    put_desc(0, dma_pa(dma_base, REQ_OFF), 16, VIRTQ_DESC_F_NEXT, 1);
    let flags = VIRTQ_DESC_F_NEXT | if write { 0 } else { VIRTQ_DESC_F_WRITE };
    put_desc(1, dma_pa(dma_base, REQ_OFF + 16), SECTOR_SIZE as u32, flags, 2);
    put_desc(2, dma_pa(dma_base, REQ_OFF + 16 + SECTOR_SIZE), 1, VIRTQ_DESC_F_WRITE, 0);

    let a = dma(AVAIL_OFF);
    unsafe {
        let avail_idx = core::ptr::read_volatile((a + 2) as *const u16);
        core::ptr::write_volatile((a + 4 + (avail_idx as usize % VQ_SIZE) * 2) as *mut u16, 0);
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        core::ptr::write_volatile((a + 2) as *mut u16, avail_idx.wrapping_add(1));
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        let used = dma(USED_OFF);
        let before = core::ptr::read_volatile((used + 2) as *const u16);
        w32(VR_QUEUE_NOTIFY, 0);
        while core::ptr::read_volatile((used + 2) as *const u16) == before {
            core::hint::spin_loop();
        }
        core::ptr::read_volatile(status as *const u8)
    }
}

fn data_ptr() -> *mut u8 {
    dma(REQ_OFF + 16) as *mut u8
}

fn set_data(s: &[u8]) {
    unsafe {
        core::ptr::write_bytes(data_ptr(), 0, SECTOR_SIZE);
        let n = s.len().min(SECTOR_SIZE - 1);
        core::ptr::copy_nonoverlapping(s.as_ptr(), data_ptr(), n);
    }
}

fn data_starts_with(s: &[u8]) -> bool {
    let p = data_ptr();
    let mut i = 0usize;
    while i < s.len() {
        let b = unsafe { core::ptr::read_volatile(p.add(i)) };
        if b != s[i] {
            return false;
        }
        i += 1;
    }
    true
}

fn data_contains(needle: &[u8]) -> bool {
    let p = data_ptr();
    let mut hay_len = 0usize;
    while hay_len < SECTOR_SIZE {
        let b = unsafe { core::ptr::read_volatile(p.add(hay_len)) };
        if b == 0 {
            break;
        }
        hay_len += 1;
    }
    if needle.is_empty() || needle.len() > hay_len {
        return false;
    }
    let mut start = 0usize;
    while start + needle.len() <= hay_len {
        let mut ok = true;
        let mut j = 0usize;
        while j < needle.len() {
            let b = unsafe { core::ptr::read_volatile(p.add(start + j)) };
            if b != needle[j] {
                ok = false;
                break;
            }
            j += 1;
        }
        if ok {
            return true;
        }
        start += 1;
    }
    false
}

fn copy_input(len: usize) {
    unsafe {
        core::ptr::write_bytes(data_ptr(), 0, SECTOR_SIZE);
        // Full sector: blob writes carry 512 payload bytes. String users
        // (Cairn values, notes) pass len <= 511 themselves, so termination
        // stays intact for them.
        let n = len.min(SECTOR_SIZE);
        core::ptr::copy_nonoverlapping((DMA_VA + INPUT_OFF) as *const u8, data_ptr(), n);
    }
}

fn print_data(prefix: &[u8]) {
    sys_print(prefix);
    let p = data_ptr();
    let mut n = 0usize;
    while n < 64 {
        let b = unsafe { core::ptr::read_volatile(p.add(n)) };
        if b == 0 {
            break;
        }
        n += 1;
    }
    let s = unsafe { core::slice::from_raw_parts(p as *const u8, n) };
    sys_print(s);
    sys_print(b"\n");
}

// --- Cairn v1 helpers ---------------------------------------------------------

fn d_read_u32(off: usize) -> u32 {
    let p = data_ptr();
    let mut v: u32 = 0;
    let mut i = 0usize;
    while i < 4 {
        let b = unsafe { core::ptr::read_volatile(p.add(off + i)) };
        v |= (b as u32) << (8 * i);
        i += 1;
    }
    v
}

fn d_write_u32(off: usize, v: u32) {
    let p = data_ptr();
    let mut i = 0usize;
    while i < 4 {
        unsafe { core::ptr::write_volatile(p.add(off + i), (v >> (8 * i)) as u8) };
        i += 1;
    }
}

fn d_read_u64(off: usize) -> u64 {
    (d_read_u32(off) as u64) | ((d_read_u32(off + 4) as u64) << 32)
}

fn d_write_u64(off: usize, v: u64) {
    d_write_u32(off, v as u32);
    d_write_u32(off + 4, (v >> 32) as u32);
}

fn zero_data() {
    unsafe { core::ptr::write_bytes(data_ptr(), 0, SECTOR_SIZE) };
}

/// Decimal print WITHOUT the trailing newline that SYS_PRINTNUM appends —
/// keeps Cairn log/status lines on one line.
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

fn fnv64(ptr: *const u8, len: usize) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0usize;
    while i < len {
        let b = unsafe { core::ptr::read_volatile(ptr.add(i)) };
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    h
}

fn print_hex(v: u64) {
    let mut buf = [0u8; 18];
    buf[0] = b'0';
    buf[1] = b'x';
    let mut i = 0usize;
    while i < 16 {
        let nib = ((v >> (60 - 4 * i)) & 0xf) as u8;
        buf[2 + i] = if nib < 10 { b'0' + nib } else { b'a' + nib - 10 };
        i += 1;
    }
    sys_print(&buf);
}

fn print_ns_name(ns: usize) {
    if ns < CAIRN1_NS_NAMES.len() {
        sys_print(CAIRN1_NS_NAMES[ns]);
    } else {
        sys_print(b"ns#");
        print_num(ns);
    }
}

fn cairn_ns_entry_off(ns: usize) -> usize {
    16 + ns * 32
}

/// Read the superblock into the data buffer; format it fresh if the magic is
/// missing (first boot on a blank disk). Returns false on I/O failure.
fn cairn_load_super(dma_base: usize) -> bool {
    let st = rw(dma_base, CAIRN1_SUPER_SECTOR, false);
    if st != 0 {
        return false;
    }
    if data_starts_with(b"DZCAIRN1") {
        return true;
    }
    zero_data();
    set_data(b"DZCAIRN1");
    d_write_u32(8, 0); // next free commit slot
    let mut ns = 0usize;
    while ns < CAIRN1_NS_MAX {
        let e = cairn_ns_entry_off(ns);
        if ns < CAIRN1_NS_NAMES.len() {
            let name = CAIRN1_NS_NAMES[ns];
            let p = data_ptr();
            let mut i = 0usize;
            while i < name.len() && i < 16 {
                unsafe { core::ptr::write_volatile(p.add(e + i), name[i]) };
                i += 1;
            }
        }
        d_write_u32(e + 16, CAIRN1_NONE); // head commit
        d_write_u32(e + 20, 0); // commit count on the current chain
        ns += 1;
    }
    let st = rw(dma_base, CAIRN1_SUPER_SECTOR, true);
    if st != 0 {
        return false;
    }
    sys_print(b"  [cairn] v1 store formatted: superblock sector 1600, 255 commit slots\n");
    rw(dma_base, CAIRN1_SUPER_SECTOR, false) == 0
}

/// The sender must hold the kernel-attested capability bit for namespace `ns`.
/// Explainable denial (D020): say which capability was missing, for whom.
fn cairn_ns_allowed(ns: usize, sender_caps: usize, from: usize) -> bool {
    if ns < CAIRN1_NS_MAX && sender_caps & (1usize << (TASK_CAIRN_NS_BASE + ns)) != 0 {
        return true;
    }
    sys_print(b"  [cairn] DENIED: ns=");
    print_ns_name(ns);
    sys_print(b" requires capability CAIRN_NS_");
    print_num(ns);
    sys_print(b" sender task=");
    print_num(from);
    sys_print(b" holds caps=");
    print_hex(sender_caps as u64);
    sys_print(b"\n  [cairn] hint: grant the namespace in the app manifest / launch caps\n");
    false
}

fn cairn_commit(dma_base: usize, ns: usize, len: usize, from: usize) -> usize {
    if !cairn_load_super(dma_base) {
        return IPC_STATUS_IO_FAILURE;
    }
    let next = d_read_u32(8);
    if next >= CAIRN1_COMMIT_SLOTS {
        sys_print(b"  [cairn] commit log full (255 slots); GC is future work\n");
        return IPC_STATUS_IO_FAILURE;
    }
    let e = cairn_ns_entry_off(ns);
    let head = d_read_u32(e + 16);
    let count = d_read_u32(e + 20);
    let len = len.min(CAIRN1_VALUE_MAX - 1);
    let hash = fnv64((DMA_VA + INPUT_OFF) as *const u8, len);
    // Build the commit record in the data buffer, then persist it.
    zero_data();
    set_data(b"DZC1");
    d_write_u32(4, next);
    d_write_u32(8, head); // parent
    d_write_u32(12, ns as u32);
    d_write_u64(16, hash);
    d_write_u32(24, from as u32); // actor (kernel task id)
    d_write_u32(28, 1); // flags bit0: reversible
    d_write_u32(32, len as u32);
    unsafe {
        core::ptr::copy_nonoverlapping(
            (DMA_VA + INPUT_OFF) as *const u8,
            data_ptr().add(CAIRN1_VALUE_OFF),
            len,
        );
    }
    let st = rw(dma_base, CAIRN1_COMMIT_FIRST_SECTOR + next as u64, true);
    if st != 0 {
        return IPC_STATUS_IO_FAILURE;
    }
    // Advance the namespace ref and the free slot pointer.
    if !cairn_load_super(dma_base) {
        return IPC_STATUS_IO_FAILURE;
    }
    d_write_u32(8, next + 1);
    d_write_u32(e + 16, next);
    d_write_u32(e + 20, count + 1);
    let st = rw(dma_base, CAIRN1_SUPER_SECTOR, true);
    if st != 0 {
        return IPC_STATUS_IO_FAILURE;
    }
    sys_print(b"  [cairn] commit ns=");
    print_ns_name(ns);
    sys_print(b" slot=");
    print_num(next as usize);
    sys_print(b" parent=");
    if head == CAIRN1_NONE {
        sys_print(b"none");
    } else {
        print_num(head as usize);
    }
    sys_print(b" len=");
    print_num(len);
    sys_print(b" hash=");
    print_hex(hash);
    sys_print(b" actor=task");
    print_num(from);
    sys_print(b"\n");
    IPC_STATUS_OK
}

/// Load the head commit of `ns` into the data buffer. Returns the head slot
/// or CAIRN1_NONE (with status left for the caller to map).
fn cairn_load_head(dma_base: usize, ns: usize) -> Option<u32> {
    if !cairn_load_super(dma_base) {
        return None;
    }
    let head = d_read_u32(cairn_ns_entry_off(ns) + 16);
    if head == CAIRN1_NONE {
        return Some(CAIRN1_NONE);
    }
    let st = rw(dma_base, CAIRN1_COMMIT_FIRST_SECTOR + head as u64, false);
    if st != 0 || !data_starts_with(b"DZC1") {
        return None;
    }
    Some(head)
}

fn cairn_get(dma_base: usize, ns: usize) -> usize {
    match cairn_load_head(dma_base, ns) {
        None => IPC_STATUS_IO_FAILURE,
        Some(CAIRN1_NONE) => {
            sys_print(b"  [cairn] ns=");
            print_ns_name(ns);
            sys_print(b" is empty (no commits)\n");
            IPC_STATUS_UNAVAILABLE
        }
        Some(head) => {
            let len = d_read_u32(32) as usize;
            sys_print(b"  [cairn] get ns=");
            print_ns_name(ns);
            sys_print(b" head=");
            print_num(head as usize);
            sys_print(b" len=");
            print_num(len);
            sys_print(b"\n");
            // Move the value to the front of the shared data window so the
            // client's print_data / console read sees a plain string.
            let p = data_ptr();
            let mut i = 0usize;
            while i < len {
                let b = unsafe { core::ptr::read_volatile(p.add(CAIRN1_VALUE_OFF + i)) };
                unsafe { core::ptr::write_volatile(p.add(i), b) };
                i += 1;
            }
            while i < SECTOR_SIZE {
                unsafe { core::ptr::write_volatile(p.add(i), 0) };
                i += 1;
            }
            IPC_STATUS_OK
        }
    }
}

fn cairn_log(dma_base: usize, ns: usize) -> usize {
    if !cairn_load_super(dma_base) {
        return IPC_STATUS_IO_FAILURE;
    }
    let mut cur = d_read_u32(cairn_ns_entry_off(ns) + 16);
    sys_print(b"  [cairn] log ns=");
    print_ns_name(ns);
    sys_print(b" (newest first):\n");
    if cur == CAIRN1_NONE {
        sys_print(b"    (empty)\n");
        return IPC_STATUS_OK;
    }
    let mut shown = 0usize;
    while cur != CAIRN1_NONE && shown < 8 {
        let st = rw(dma_base, CAIRN1_COMMIT_FIRST_SECTOR + cur as u64, false);
        if st != 0 || !data_starts_with(b"DZC1") {
            sys_print(b"    [cairn] log walk failed: unreadable commit slot\n");
            return IPC_STATUS_IO_FAILURE;
        }
        let parent = d_read_u32(8);
        sys_print(b"    slot=");
        print_num(cur as usize);
        sys_print(b" parent=");
        if parent == CAIRN1_NONE {
            sys_print(b"none");
        } else {
            print_num(parent as usize);
        }
        sys_print(b" len=");
        print_num(d_read_u32(32) as usize);
        sys_print(b" hash=");
        print_hex(d_read_u64(16));
        sys_print(b" actor=task");
        print_num(d_read_u32(24) as usize);
        sys_print(b" reversible=yes\n");
        cur = parent;
        shown += 1;
    }
    IPC_STATUS_OK
}

fn cairn_rollback(dma_base: usize, ns: usize, steps: usize) -> usize {
    if !cairn_load_super(dma_base) {
        return IPC_STATUS_IO_FAILURE;
    }
    let e = cairn_ns_entry_off(ns);
    let head = d_read_u32(e + 16);
    let count = d_read_u32(e + 20);
    if head == CAIRN1_NONE {
        sys_print(b"  [cairn] rollback: namespace is already empty\n");
        return IPC_STATUS_UNAVAILABLE;
    }
    let mut cur = head;
    let mut walked = 0usize;
    while walked < steps && cur != CAIRN1_NONE {
        let st = rw(dma_base, CAIRN1_COMMIT_FIRST_SECTOR + cur as u64, false);
        if st != 0 || !data_starts_with(b"DZC1") {
            return IPC_STATUS_IO_FAILURE;
        }
        cur = d_read_u32(8); // parent
        walked += 1;
    }
    if !cairn_load_super(dma_base) {
        return IPC_STATUS_IO_FAILURE;
    }
    d_write_u32(e + 16, cur);
    d_write_u32(e + 20, count.saturating_sub(walked as u32));
    let st = rw(dma_base, CAIRN1_SUPER_SECTOR, true);
    if st != 0 {
        return IPC_STATUS_IO_FAILURE;
    }
    sys_print(b"  [cairn] rollback ns=");
    print_ns_name(ns);
    sys_print(b" steps=");
    print_num(walked);
    sys_print(b" head ");
    print_num(head as usize);
    sys_print(b" -> ");
    if cur == CAIRN1_NONE {
        sys_print(b"none (empty)");
    } else {
        print_num(cur as usize);
    }
    sys_print(b"\n  [cairn] history preserved: rollback moves the ref, commits stay on disk\n");
    IPC_STATUS_OK
}

fn cairn_verify(dma_base: usize, ns: usize) -> usize {
    match cairn_load_head(dma_base, ns) {
        None => IPC_STATUS_IO_FAILURE,
        Some(CAIRN1_NONE) => {
            sys_print(b"  [cairn] verify: namespace is empty\n");
            IPC_STATUS_UNAVAILABLE
        }
        Some(head) => {
            let len = d_read_u32(32) as usize;
            let stored = d_read_u64(16);
            let actual = fnv64(unsafe { data_ptr().add(CAIRN1_VALUE_OFF) }, len.min(CAIRN1_VALUE_MAX));
            sys_print(b"  [cairn] verify ns=");
            print_ns_name(ns);
            sys_print(b" slot=");
            print_num(head as usize);
            if stored == actual {
                sys_print(b" hash MATCH ");
                print_hex(stored);
                sys_print(b"\n");
                IPC_STATUS_OK
            } else {
                sys_print(b" hash MISMATCH stored=");
                print_hex(stored);
                sys_print(b" actual=");
                print_hex(actual);
                sys_print(b"\n");
                IPC_STATUS_IO_FAILURE
            }
        }
    }
}

fn cairn_status(dma_base: usize) -> usize {
    if !cairn_load_super(dma_base) {
        return IPC_STATUS_IO_FAILURE;
    }
    sys_print(b"  [cairn] v1 store: commit slots used=");
    print_num(d_read_u32(8) as usize);
    sys_print(b"/255 region=sectors 1600..1855\n");
    let mut ns = 0usize;
    while ns < CAIRN1_NS_NAMES.len() {
        let e = cairn_ns_entry_off(ns);
        let head = d_read_u32(e + 16);
        sys_print(b"    ns=");
        print_ns_name(ns);
        sys_print(b" cap=CAIRN_NS_");
        print_num(ns);
        sys_print(b" head=");
        if head == CAIRN1_NONE {
            sys_print(b"none");
        } else {
            print_num(head as usize);
        }
        sys_print(b" commits=");
        print_num(d_read_u32(e + 20) as usize);
        sys_print(b"\n");
        ns += 1;
    }
    IPC_STATUS_OK
}

fn registry_is_active(dma_base: usize) -> bool {
    let st = rw(dma_base, APP_REGISTRY_SECTOR, false);
    st == 0 && data_starts_with(b"DEZHAPPREG") && data_contains(b"state=Active")
}

fn registry_is_removed(dma_base: usize) -> bool {
    let st = rw(dma_base, APP_REGISTRY_SECTOR, false);
    st == 0 && data_starts_with(b"DEZHAPPREG") && data_contains(b"state=Removed")
}

fn lab_registry_is_active(dma_base: usize) -> bool {
    let st = rw(dma_base, LAB_REGISTRY_SECTOR, false);
    st == 0 && data_starts_with(b"DEZHLABREG") && data_contains(b"state=Active")
}

fn lab_registry_is_removed(dma_base: usize) -> bool {
    let st = rw(dma_base, LAB_REGISTRY_SECTOR, false);
    st == 0 && data_starts_with(b"DEZHLABREG") && data_contains(b"state=Removed")
}

fn calc_registry_is_active(dma_base: usize) -> bool {
    let st = rw(dma_base, CALC_REGISTRY_SECTOR, false);
    st == 0 && data_starts_with(b"DEZHCALCREG") && data_contains(b"state=Active")
}

fn calc_registry_is_removed(dma_base: usize) -> bool {
    let st = rw(dma_base, CALC_REGISTRY_SECTOR, false);
    st == 0 && data_starts_with(b"DEZHCALCREG") && data_contains(b"state=Removed")
}

fn vault_registry_is_active(dma_base: usize) -> bool {
    let st = rw(dma_base, VAULT_REGISTRY_SECTOR, false);
    st == 0 && data_starts_with(b"DEZHVAULTREG") && data_contains(b"state=Active")
}

fn vault_registry_is_removed(dma_base: usize) -> bool {
    let st = rw(dma_base, VAULT_REGISTRY_SECTOR, false);
    st == 0 && data_starts_with(b"DEZHVAULTREG") && data_contains(b"state=Removed")
}

fn set_registry_pending() {
    set_data(
        b"DEZHAPPREG v0 app=note version=0.1.0 state=Pending caps=PRINT,IPC code_hash=note-elf-v0 manifest_hash=note-manifest-v0 private_root=16 previous_registry_sector=6",
    );
}

fn set_registry_active() {
    set_data(
        b"DEZHAPPREG v0 app=note version=0.1.0 state=Active caps=PRINT,IPC code_hash=note-elf-v0 manifest_hash=note-manifest-v0 private_root=16 previous_registry_sector=6",
    );
}

fn set_registry_removed() {
    set_data(
        b"DEZHAPPREG v0 app=note version=0.1.0 state=Removed caps=PRINT,IPC code_hash=note-elf-v0 manifest_hash=note-manifest-v0 private_root=16 previous_registry_sector=6",
    );
}

fn set_lab_registry_pending() {
    set_data(
        b"DEZHLABREG v0 app=lab version=0.1.0 state=Pending caps=PRINT,IPC code_hash=lab-elf-v0 manifest_hash=lab-manifest-v0 private_root=17 previous_registry_sector=6",
    );
}

fn set_lab_registry_active() {
    set_data(
        b"DEZHLABREG v0 app=lab version=0.1.0 state=Active caps=PRINT,IPC code_hash=lab-elf-v0 manifest_hash=lab-manifest-v0 private_root=17 previous_registry_sector=6",
    );
}

fn set_lab_registry_removed() {
    set_data(
        b"DEZHLABREG v0 app=lab version=0.1.0 state=Removed caps=PRINT,IPC code_hash=lab-elf-v0 manifest_hash=lab-manifest-v0 private_root=17 previous_registry_sector=6",
    );
}

fn set_calc_registry_pending() {
    set_data(
        b"DEZHCALCREG v0 app=calc version=0.1.0 state=Pending caps=PRINT,IPC code_hash=calc-elf-v0 manifest_hash=calc-manifest-v0 private_root=18 previous_registry_sector=6",
    );
}

fn set_calc_registry_active() {
    set_data(
        b"DEZHCALCREG v0 app=calc version=0.1.0 state=Active caps=PRINT,IPC code_hash=calc-elf-v0 manifest_hash=calc-manifest-v0 private_root=18 previous_registry_sector=6",
    );
}

fn set_calc_registry_removed() {
    set_data(
        b"DEZHCALCREG v0 app=calc version=0.1.0 state=Removed caps=PRINT,IPC code_hash=calc-elf-v0 manifest_hash=calc-manifest-v0 private_root=18 previous_registry_sector=6",
    );
}

fn set_vault_registry_pending() {
    set_data(
        b"DEZHVAULTREG v0 app=vault version=0.1.0 state=Pending caps=PRINT,IPC code_hash=vault-elf-v0 manifest_hash=vault-manifest-v0 private_root=19 previous_registry_sector=6",
    );
}

fn set_vault_registry_active() {
    set_data(
        b"DEZHVAULTREG v0 app=vault version=0.1.0 state=Active caps=PRINT,IPC code_hash=vault-elf-v0 manifest_hash=vault-manifest-v0 private_root=19 previous_registry_sector=6",
    );
}

fn set_vault_registry_removed() {
    set_data(
        b"DEZHVAULTREG v0 app=vault version=0.1.0 state=Removed caps=PRINT,IPC code_hash=vault-elf-v0 manifest_hash=vault-manifest-v0 private_root=19 previous_registry_sector=6",
    );
}

fn typed_word(op: usize, request_id: usize, status: usize, arg: usize) -> usize {
    (IPC_PROTO_V1 << 56)
        | ((IPC_SERVICE_VIRTIO_BLOCK & 0xff) << 48)
        | ((op & 0xff) << 40)
        | ((request_id & 0xffff) << 24)
        | ((status & 0xff) << 16)
        | (arg & 0xffff)
}

fn request_word(op: usize, arg: usize) -> usize {
    typed_word(op, 1, IPC_STATUS_OK, arg)
}

fn request_proto(word: usize) -> usize {
    (word >> 56) & 0xff
}

fn request_service(word: usize) -> usize {
    (word >> 48) & 0xff
}

fn request_op(word: usize) -> usize {
    (word >> 40) & 0xff
}

fn request_id(word: usize) -> usize {
    (word >> 24) & 0xffff
}

fn request_arg(word: usize) -> usize {
    word & 0xffff
}

fn response_status(word: usize) -> usize {
    if request_proto(word) == IPC_PROTO_V1 && request_service(word) == IPC_SERVICE_VIRTIO_BLOCK {
        (word >> 16) & 0xff
    } else {
        IPC_STATUS_BAD_REQUEST
    }
}

fn reply_word(req: usize, status: usize) -> usize {
    typed_word(request_op(req), request_id(req), status, 0)
}

fn send_status(to: usize, req: usize, status: usize) {
    let _ = sys_send(to, reply_word(req, status));
}

fn status_from_io(st: u8) -> usize {
    if st == 0 {
        IPC_STATUS_OK
    } else {
        IPC_STATUS_IO_FAILURE
    }
}

fn daemon(dma_base: usize) -> ! {
    clear_dma();
    sys_print(b"  [virtio-blk-daemon] started as a long-lived U-mode driver service\n");
    if !init(dma_base) {
        sys_print(b"  [virtio-blk-daemon] no virtio block device found\n");
        sys_exit(1);
    }
    sys_print(b"  [virtio-blk-daemon] device + DMA capabilities accepted\n");
    loop {
        let (word, from, sender_caps) = sys_recv();
        if request_proto(word) != IPC_PROTO_V1 || request_service(word) != IPC_SERVICE_VIRTIO_BLOCK {
            sys_print(b"  [virtio-blk-daemon] typed IPC BAD_REQUEST: malformed envelope\n");
            let _ = sys_send(from, typed_word(0, 0, IPC_STATUS_BAD_REQUEST, 0));
            continue;
        }
        let op = request_op(word);
        let sector = request_arg(word) as u64;
        // Cairn v1 encodes namespace in the top 4 bits of the 16-bit arg;
        // the low 12 bits carry the value length or the rollback step count.
        let cairn_ns = (request_arg(word) >> 12) & 0xf;
        let cairn_aux = request_arg(word) & 0xfff;
        if op == REQ_PROBE {
            sys_print(b"  [virtio-blk-daemon] PROBE over IPC\n");
            send_status(from, word, IPC_STATUS_OK);
        } else if op == REQ_BWRITE {
            set_data(b"DEZH-DAEMON-BLOCK-OK");
            let st = rw(dma_base, sector, true);
            sys_print(b"  [virtio-blk-daemon] WRITE sector via IPC status=");
            sys_printnum(st as usize);
            send_status(from, word, status_from_io(st));
        } else if op == REQ_BREAD {
            set_data(b"");
            let st = rw(dma_base, sector, false);
            sys_print(b"  [virtio-blk-daemon] READ sector via IPC status=");
            sys_printnum(st as usize);
            send_status(from, word, status_from_io(st));
        } else if op == REQ_PSET {
            let _ = rw(dma_base, CAIRN_CURRENT_SECTOR, false);
            let _ = rw(dma_base, CAIRN_PREVIOUS_SECTOR, true);
            copy_input(sector as usize);
            let st = rw(dma_base, CAIRN_CURRENT_SECTOR, true);
            sys_print(b"  [virtio-blk-daemon] CAIRN SET via IPC status=");
            sys_printnum(st as usize);
            send_status(from, word, status_from_io(st));
        } else if op == REQ_PGET {
            let st = rw(dma_base, CAIRN_CURRENT_SECTOR, false);
            sys_print(b"  [virtio-blk-daemon] CAIRN GET via IPC status=");
            sys_printnum(st as usize);
            send_status(from, word, status_from_io(st));
        } else if op == REQ_PROLLBACK {
            let _ = rw(dma_base, CAIRN_PREVIOUS_SECTOR, false);
            let st = rw(dma_base, CAIRN_CURRENT_SECTOR, true);
            sys_print(b"  [virtio-blk-daemon] CAIRN ROLLBACK via IPC status=");
            sys_printnum(st as usize);
            send_status(from, word, status_from_io(st));
        } else if op == REQ_STOP {
            sys_print(b"  [virtio-blk-daemon] STOP received; exiting cleanly\n");
            send_status(from, word, IPC_STATUS_OK);
            sys_exit(0);
        } else if op == REQ_INSTALL_CHECK {
            let st = rw(dma_base, INSTALL_MARKER_SECTOR, false);
            if st == 0 && data_starts_with(b"DEZHINST") {
                sys_print(b"  [virtio-blk-daemon] install-check: installed root marker found\n");
                send_status(from, word, IPC_STATUS_OK);
            } else {
                sys_print(b"  [virtio-blk-daemon] install-check: no Dezh root marker yet\n");
                send_status(from, word, IPC_STATUS_UNAVAILABLE);
            }
        } else if op == REQ_INSTALL_INIT {
            set_data(b"DEZHINST v0 target=riscv64 root=cairn block=virtio-block");
            let st0 = rw(dma_base, INSTALL_MARKER_SECTOR, true);
            set_data(b"DEZHROOT v0 cairn_current=2 cairn_previous=3 metadata_sector=4");
            let st1 = rw(dma_base, ROOT_METADATA_SECTOR, true);
            let st = if st0 == 0 { st1 } else { st0 };
            sys_print(b"  [virtio-blk-daemon] install-init: wrote marker/root metadata status=");
            sys_printnum(st as usize);
            send_status(from, word, status_from_io(st));
        } else if op == REQ_ROOT_STATUS {
            let st = rw(dma_base, ROOT_METADATA_SECTOR, false);
            sys_print(b"  [virtio-blk-daemon] root-status: metadata read status=");
            sys_printnum(st as usize);
            send_status(from, word, status_from_io(st));
        } else if op == REQ_PKG_STORE_INIT {
            set_data(
                b"DEZHPKGS v0 slots=8 registry=25..31 blob=64..575 slot_sectors=64",
            );
            let st = rw(dma_base, PKG_STORE_MARKER_SECTOR, true);
            sys_print(b"  [virtio-blk-daemon] pkg-store-init status=");
            sys_printnum(st as usize);
            send_status(from, word, status_from_io(st));
        } else if op == REQ_PKG_REGISTRY_READ {
            if sector < PKG_REGISTRY_FIRST_SECTOR || sector > PKG_REGISTRY_LAST_SECTOR {
                sys_print(b"  [virtio-blk-daemon] pkg-registry-read denied: sector out of range\n");
                send_status(from, word, IPC_STATUS_DENIED);
            } else {
                let st = rw(dma_base, sector, false);
                send_status(from, word, status_from_io(st));
            }
        } else if op == REQ_PKG_REGISTRY_WRITE {
            if sector < PKG_REGISTRY_FIRST_SECTOR || sector > PKG_REGISTRY_LAST_SECTOR {
                sys_print(b"  [virtio-blk-daemon] pkg-registry-write denied: sector out of range\n");
                send_status(from, word, IPC_STATUS_DENIED);
            } else {
                copy_input(SECTOR_SIZE);
                let st = rw(dma_base, sector, true);
                send_status(from, word, status_from_io(st));
            }
        } else if op == REQ_PKG_BLOB_READ {
            if sector < PKG_BLOB_FIRST_SECTOR || sector > PKG_BLOB_LAST_SECTOR {
                sys_print(b"  [virtio-blk-daemon] pkg-blob-read denied: sector out of range\n");
                send_status(from, word, IPC_STATUS_DENIED);
            } else {
                let st = rw(dma_base, sector, false);
                send_status(from, word, status_from_io(st));
            }
        } else if op == REQ_PKG_BLOB_WRITE {
            if sector < PKG_BLOB_FIRST_SECTOR || sector > PKG_BLOB_LAST_SECTOR {
                sys_print(b"  [virtio-blk-daemon] pkg-blob-write denied: sector out of range\n");
                send_status(from, word, IPC_STATUS_DENIED);
            } else {
                copy_input(SECTOR_SIZE);
                let st = rw(dma_base, sector, true);
                send_status(from, word, status_from_io(st));
            }
        } else if op == REQ_PKG_JOURNAL_READ {
            if sector < PKG_JOURNAL_FIRST_SECTOR || sector > PKG_JOURNAL_LAST_SECTOR {
                sys_print(b"  [virtio-blk-daemon] pkg-journal-read denied: sector out of range\n");
                send_status(from, word, IPC_STATUS_DENIED);
            } else {
                let st = rw(dma_base, sector, false);
                send_status(from, word, status_from_io(st));
            }
        } else if op == REQ_PKG_JOURNAL_WRITE {
            if sector < PKG_JOURNAL_FIRST_SECTOR || sector > PKG_JOURNAL_LAST_SECTOR {
                sys_print(b"  [virtio-blk-daemon] pkg-journal-write denied: sector out of range\n");
                send_status(from, word, IPC_STATUS_DENIED);
            } else {
                copy_input(SECTOR_SIZE);
                let st = rw(dma_base, sector, true);
                send_status(from, word, status_from_io(st));
            }
        } else if op == REQ_APP_AVAILABLE {
            sys_print(
                b"  \x1b[36m[available] note\x1b[0m version=0.1.0 caps=PRINT,IPC storage=PrivateRoot\n",
            );
            sys_print(
                b"  \x1b[36m[available] lab\x1b[0m version=0.1.0 caps=PRINT,IPC ui=terminal tasks=3 storage=PrivateRoot\n",
            );
            sys_print(
                b"  \x1b[36m[available] calc\x1b[0m version=0.1.0 caps=PRINT,IPC compute=integer storage=LastResult\n",
            );
            sys_print(
                b"  \x1b[36m[available] vault\x1b[0m version=0.1.0 caps=PRINT,IPC storage=PrivateValue\n",
            );
            send_status(from, word, IPC_STATUS_OK);
        } else if op == REQ_APP_INSTALLED {
            let mut shown = false;
            if registry_is_active(dma_base) {
                sys_print(
                    b"  \x1b[32m[installed] note\x1b[0m version=0.1.0 state=Active caps=PRINT,IPC root=sector:16\n",
                );
                shown = true;
            } else if registry_is_removed(dma_base) {
                sys_print(
                    b"  \x1b[33m[removed] note\x1b[0m version=0.1.0 state=Removed execution=denied\n",
                );
                shown = true;
            }
            if lab_registry_is_active(dma_base) {
                sys_print(
                    b"  \x1b[32m[installed] lab\x1b[0m version=0.1.0 state=Active caps=PRINT,IPC root=sector:17 ui=terminal\n",
                );
                shown = true;
            } else if lab_registry_is_removed(dma_base) {
                sys_print(
                    b"  \x1b[33m[removed] lab\x1b[0m version=0.1.0 state=Removed execution=denied\n",
                );
                shown = true;
            }
            if calc_registry_is_active(dma_base) {
                sys_print(
                    b"  \x1b[32m[installed] calc\x1b[0m version=0.1.0 state=Active caps=PRINT,IPC root=sector:18 compute=integer\n",
                );
                shown = true;
            } else if calc_registry_is_removed(dma_base) {
                sys_print(
                    b"  \x1b[33m[removed] calc\x1b[0m version=0.1.0 state=Removed execution=denied\n",
                );
                shown = true;
            }
            if vault_registry_is_active(dma_base) {
                sys_print(
                    b"  \x1b[32m[installed] vault\x1b[0m version=0.1.0 state=Active caps=PRINT,IPC root=sector:19 storage=PrivateValue\n",
                );
                shown = true;
            } else if vault_registry_is_removed(dma_base) {
                sys_print(
                    b"  \x1b[33m[removed] vault\x1b[0m version=0.1.0 state=Removed execution=denied\n",
                );
                shown = true;
            }
            if shown {
                send_status(from, word, IPC_STATUS_OK);
            } else {
                sys_print(b"  [installed] none\n");
                send_status(from, word, IPC_STATUS_UNAVAILABLE);
            }
        } else if op == REQ_APP_INFO {
            sys_print(
                b"  [app-info] note bundle=available version=0.1.0 requested_caps=PRINT,IPC denied_caps=DEVICE_VIRTIO_BLK,DMA,BLOCK_DIRECT\n",
            );
            sys_print(
                b"  [app-info] lab bundle=available version=0.1.0 requested_caps=PRINT,IPC ui=terminal workers=2 denied_caps=DEVICE_VIRTIO_BLK,DMA,BLOCK_DIRECT\n",
            );
            sys_print(
                b"  [app-info] calc bundle=available version=0.1.0 requested_caps=PRINT,IPC integer_ops=+,-,*,/ denied_caps=DEVICE_VIRTIO_BLK,DMA,BLOCK_DIRECT\n",
            );
            sys_print(
                b"  [app-info] vault bundle=available version=0.1.0 requested_caps=PRINT,IPC private_value=true denied_caps=DEVICE_VIRTIO_BLK,DMA,BLOCK_DIRECT\n",
            );
            if registry_is_active(dma_base) {
                sys_print(b"  [app-info] note install_state=Active private_root=sector:16\n");
            } else if registry_is_removed(dma_base) {
                sys_print(b"  [app-info] note install_state=Removed execution=denied\n");
            } else {
                sys_print(b"  [app-info] note install_state=NotInstalled\n");
            }
            if lab_registry_is_active(dma_base) {
                sys_print(b"  [app-info] lab install_state=Active private_root=sector:17\n");
            } else if lab_registry_is_removed(dma_base) {
                sys_print(b"  [app-info] lab install_state=Removed execution=denied\n");
            } else {
                sys_print(b"  [app-info] lab install_state=NotInstalled\n");
            }
            if calc_registry_is_active(dma_base) {
                sys_print(b"  [app-info] calc install_state=Active private_root=sector:18\n");
            } else if calc_registry_is_removed(dma_base) {
                sys_print(b"  [app-info] calc install_state=Removed execution=denied\n");
            } else {
                sys_print(b"  [app-info] calc install_state=NotInstalled\n");
            }
            if vault_registry_is_active(dma_base) {
                sys_print(b"  [app-info] vault install_state=Active private_root=sector:19\n");
            } else if vault_registry_is_removed(dma_base) {
                sys_print(b"  [app-info] vault install_state=Removed execution=denied\n");
            } else {
                sys_print(b"  [app-info] vault install_state=NotInstalled\n");
            }
            send_status(from, word, IPC_STATUS_OK);
        } else if op == REQ_APP_INSTALL_NOTE {
            if registry_is_active(dma_base) {
                sys_print(
                    b"  [installer] already installed note version=0.1.0 state=Active\n",
                );
                send_status(from, word, IPC_STATUS_OK);
            } else {
                let _ = rw(dma_base, APP_REGISTRY_SECTOR, false);
                let _ = rw(dma_base, APP_REGISTRY_PREVIOUS_SECTOR, true);
                set_registry_pending();
                let st0 = rw(dma_base, APP_REGISTRY_SECTOR, true);
                set_registry_active();
                let st1 = rw(dma_base, APP_REGISTRY_SECTOR, true);
                let ok = registry_is_active(dma_base);
                if st0 == 0 && st1 == 0 && ok {
                    sys_print(
                        b"  [installer] installed note version=0.1.0 state=Active caps=PRINT,IPC root=sector:16\n",
                    );
                    send_status(from, word, IPC_STATUS_OK);
                } else {
                    sys_print(b"  [installer] install failed: registry verify failed\n");
                    send_status(from, word, IPC_STATUS_IO_FAILURE);
                }
            }
        } else if op == REQ_APP_REQUIRE_NOTE {
            if registry_is_active(dma_base) {
                sys_print(b"  [installer] note is installed state=Active\n");
                send_status(from, word, IPC_STATUS_OK);
            } else if registry_is_removed(dma_base) {
                sys_print(b"  [installer] note not active: state=Removed\n");
                send_status(from, word, IPC_STATUS_FAULTED);
            } else {
                sys_print(b"  [installer] note not installed\n");
                send_status(from, word, IPC_STATUS_UNAVAILABLE);
            }
        } else if op == REQ_APP_REMOVE_NOTE {
            if registry_is_active(dma_base) || registry_is_removed(dma_base) {
                let _ = rw(dma_base, APP_REGISTRY_SECTOR, false);
                let _ = rw(dma_base, APP_REGISTRY_PREVIOUS_SECTOR, true);
                set_registry_removed();
                let st = rw(dma_base, APP_REGISTRY_SECTOR, true);
                sys_print(b"  [installer] removed note state=Removed status=");
                sys_printnum(st as usize);
                send_status(from, word, status_from_io(st));
            } else {
                sys_print(b"  [installer] remove skipped: note not installed\n");
                send_status(from, word, IPC_STATUS_UNAVAILABLE);
            }
        } else if op == REQ_NOTE_SET {
            if registry_is_active(dma_base) {
                copy_input(sector as usize);
                let st = rw(dma_base, NOTE_PRIVATE_ROOT_SECTOR, true);
                sys_print(b"  [note-storage] note-set status=");
                sys_printnum(st as usize);
                send_status(from, word, status_from_io(st));
            } else {
                sys_print(b"  [note-storage] note-set denied: note not installed\n");
                send_status(from, word, IPC_STATUS_UNAVAILABLE);
            }
        } else if op == REQ_NOTE_GET {
            if registry_is_active(dma_base) {
                let st = rw(dma_base, NOTE_PRIVATE_ROOT_SECTOR, false);
                sys_print(b"  [note-storage] note-get status=");
                sys_printnum(st as usize);
                send_status(from, word, status_from_io(st));
            } else {
                sys_print(b"  [note-storage] note-get denied: note not installed\n");
                send_status(from, word, IPC_STATUS_UNAVAILABLE);
            }
        } else if op == REQ_APP_INSTALL_LAB {
            if lab_registry_is_active(dma_base) {
                sys_print(b"  [installer] already installed lab version=0.1.0 state=Active\n");
                send_status(from, word, IPC_STATUS_OK);
            } else {
                let _ = rw(dma_base, LAB_REGISTRY_SECTOR, false);
                let _ = rw(dma_base, APP_REGISTRY_PREVIOUS_SECTOR, true);
                set_lab_registry_pending();
                let st0 = rw(dma_base, LAB_REGISTRY_SECTOR, true);
                set_lab_registry_active();
                let st1 = rw(dma_base, LAB_REGISTRY_SECTOR, true);
                let ok = lab_registry_is_active(dma_base);
                if st0 == 0 && st1 == 0 && ok {
                    sys_print(
                        b"  [installer] installed lab version=0.1.0 state=Active caps=PRINT,IPC root=sector:17 ui=terminal workers=2\n",
                    );
                    send_status(from, word, IPC_STATUS_OK);
                } else {
                    sys_print(b"  [installer] install failed: lab registry verify failed\n");
                    send_status(from, word, IPC_STATUS_IO_FAILURE);
                }
            }
        } else if op == REQ_APP_REQUIRE_LAB {
            if lab_registry_is_active(dma_base) {
                sys_print(b"  [installer] lab is installed state=Active\n");
                send_status(from, word, IPC_STATUS_OK);
            } else if lab_registry_is_removed(dma_base) {
                sys_print(b"  [installer] lab not active: state=Removed\n");
                send_status(from, word, IPC_STATUS_FAULTED);
            } else {
                sys_print(b"  [installer] lab not installed\n");
                send_status(from, word, IPC_STATUS_UNAVAILABLE);
            }
        } else if op == REQ_APP_REMOVE_LAB {
            if lab_registry_is_active(dma_base) || lab_registry_is_removed(dma_base) {
                let _ = rw(dma_base, LAB_REGISTRY_SECTOR, false);
                let _ = rw(dma_base, APP_REGISTRY_PREVIOUS_SECTOR, true);
                set_lab_registry_removed();
                let st = rw(dma_base, LAB_REGISTRY_SECTOR, true);
                sys_print(b"  [installer] removed lab state=Removed status=");
                sys_printnum(st as usize);
                send_status(from, word, status_from_io(st));
            } else {
                sys_print(b"  [installer] remove skipped: lab not installed\n");
                send_status(from, word, IPC_STATUS_UNAVAILABLE);
            }
        } else if op == REQ_LAB_SET {
            if lab_registry_is_active(dma_base) {
                copy_input(sector as usize);
                let st = rw(dma_base, LAB_PRIVATE_ROOT_SECTOR, true);
                sys_print(b"  [lab-storage] lab-set status=");
                sys_printnum(st as usize);
                send_status(from, word, status_from_io(st));
            } else {
                sys_print(b"  [lab-storage] lab-set denied: lab not installed\n");
                send_status(from, word, IPC_STATUS_UNAVAILABLE);
            }
        } else if op == REQ_LAB_GET {
            if lab_registry_is_active(dma_base) {
                let st = rw(dma_base, LAB_PRIVATE_ROOT_SECTOR, false);
                sys_print(b"  [lab-storage] lab-get status=");
                sys_printnum(st as usize);
                send_status(from, word, status_from_io(st));
            } else {
                sys_print(b"  [lab-storage] lab-get denied: lab not installed\n");
                send_status(from, word, IPC_STATUS_UNAVAILABLE);
            }
        } else if op == REQ_APP_INSTALL_CALC {
            if calc_registry_is_active(dma_base) {
                sys_print(b"  [installer] already installed calc version=0.1.0 state=Active\n");
                send_status(from, word, IPC_STATUS_OK);
            } else {
                let _ = rw(dma_base, CALC_REGISTRY_SECTOR, false);
                let _ = rw(dma_base, APP_REGISTRY_PREVIOUS_SECTOR, true);
                set_calc_registry_pending();
                let st0 = rw(dma_base, CALC_REGISTRY_SECTOR, true);
                set_calc_registry_active();
                let st1 = rw(dma_base, CALC_REGISTRY_SECTOR, true);
                let ok = calc_registry_is_active(dma_base);
                if st0 == 0 && st1 == 0 && ok {
                    sys_print(
                        b"  [installer] installed calc version=0.1.0 state=Active caps=PRINT,IPC root=sector:18 compute=integer\n",
                    );
                    send_status(from, word, IPC_STATUS_OK);
                } else {
                    sys_print(b"  [installer] install failed: calc registry verify failed\n");
                    send_status(from, word, IPC_STATUS_IO_FAILURE);
                }
            }
        } else if op == REQ_APP_REQUIRE_CALC {
            if calc_registry_is_active(dma_base) {
                sys_print(b"  [installer] calc is installed state=Active\n");
                send_status(from, word, IPC_STATUS_OK);
            } else if calc_registry_is_removed(dma_base) {
                sys_print(b"  [installer] calc not active: state=Removed\n");
                send_status(from, word, IPC_STATUS_FAULTED);
            } else {
                sys_print(b"  [installer] calc not installed\n");
                send_status(from, word, IPC_STATUS_UNAVAILABLE);
            }
        } else if op == REQ_APP_REMOVE_CALC {
            if calc_registry_is_active(dma_base) || calc_registry_is_removed(dma_base) {
                let _ = rw(dma_base, CALC_REGISTRY_SECTOR, false);
                let _ = rw(dma_base, APP_REGISTRY_PREVIOUS_SECTOR, true);
                set_calc_registry_removed();
                let st = rw(dma_base, CALC_REGISTRY_SECTOR, true);
                sys_print(b"  [installer] removed calc state=Removed status=");
                sys_printnum(st as usize);
                send_status(from, word, status_from_io(st));
            } else {
                sys_print(b"  [installer] remove skipped: calc not installed\n");
                send_status(from, word, IPC_STATUS_UNAVAILABLE);
            }
        } else if op == REQ_CALC_SET {
            if calc_registry_is_active(dma_base) {
                copy_input(sector as usize);
                let st = rw(dma_base, CALC_PRIVATE_ROOT_SECTOR, true);
                sys_print(b"  [calc-storage] calc-set status=");
                sys_printnum(st as usize);
                send_status(from, word, status_from_io(st));
            } else {
                sys_print(b"  [calc-storage] calc-set denied: calc not installed\n");
                send_status(from, word, IPC_STATUS_UNAVAILABLE);
            }
        } else if op == REQ_CALC_GET {
            if calc_registry_is_active(dma_base) {
                let st = rw(dma_base, CALC_PRIVATE_ROOT_SECTOR, false);
                sys_print(b"  [calc-storage] calc-get status=");
                sys_printnum(st as usize);
                send_status(from, word, status_from_io(st));
            } else {
                sys_print(b"  [calc-storage] calc-get denied: calc not installed\n");
                send_status(from, word, IPC_STATUS_UNAVAILABLE);
            }
        } else if op == REQ_APP_INSTALL_VAULT {
            if vault_registry_is_active(dma_base) {
                sys_print(b"  [installer] already installed vault version=0.1.0 state=Active\n");
                send_status(from, word, IPC_STATUS_OK);
            } else {
                let _ = rw(dma_base, VAULT_REGISTRY_SECTOR, false);
                let _ = rw(dma_base, APP_REGISTRY_PREVIOUS_SECTOR, true);
                set_vault_registry_pending();
                let st0 = rw(dma_base, VAULT_REGISTRY_SECTOR, true);
                set_vault_registry_active();
                let st1 = rw(dma_base, VAULT_REGISTRY_SECTOR, true);
                let ok = vault_registry_is_active(dma_base);
                if st0 == 0 && st1 == 0 && ok {
                    sys_print(
                        b"  [installer] installed vault version=0.1.0 state=Active caps=PRINT,IPC root=sector:19 storage=PrivateValue\n",
                    );
                    send_status(from, word, IPC_STATUS_OK);
                } else {
                    sys_print(b"  [installer] install failed: vault registry verify failed\n");
                    send_status(from, word, IPC_STATUS_IO_FAILURE);
                }
            }
        } else if op == REQ_APP_REQUIRE_VAULT {
            if vault_registry_is_active(dma_base) {
                sys_print(b"  [installer] vault is installed state=Active\n");
                send_status(from, word, IPC_STATUS_OK);
            } else if vault_registry_is_removed(dma_base) {
                sys_print(b"  [installer] vault not active: state=Removed\n");
                send_status(from, word, IPC_STATUS_FAULTED);
            } else {
                sys_print(b"  [installer] vault not installed\n");
                send_status(from, word, IPC_STATUS_UNAVAILABLE);
            }
        } else if op == REQ_APP_REMOVE_VAULT {
            if vault_registry_is_active(dma_base) || vault_registry_is_removed(dma_base) {
                let _ = rw(dma_base, VAULT_REGISTRY_SECTOR, false);
                let _ = rw(dma_base, APP_REGISTRY_PREVIOUS_SECTOR, true);
                set_vault_registry_removed();
                let st = rw(dma_base, VAULT_REGISTRY_SECTOR, true);
                sys_print(b"  [installer] removed vault state=Removed status=");
                sys_printnum(st as usize);
                send_status(from, word, status_from_io(st));
            } else {
                sys_print(b"  [installer] remove skipped: vault not installed\n");
                send_status(from, word, IPC_STATUS_UNAVAILABLE);
            }
        } else if op == REQ_VAULT_SET {
            if vault_registry_is_active(dma_base) {
                copy_input(sector as usize);
                let st = rw(dma_base, VAULT_PRIVATE_ROOT_SECTOR, true);
                sys_print(b"  [vault-storage] vault-put status=");
                sys_printnum(st as usize);
                send_status(from, word, status_from_io(st));
            } else {
                sys_print(b"  [vault-storage] vault-put denied: vault not installed\n");
                send_status(from, word, IPC_STATUS_UNAVAILABLE);
            }
        } else if op == REQ_VAULT_GET {
            if vault_registry_is_active(dma_base) {
                let st = rw(dma_base, VAULT_PRIVATE_ROOT_SECTOR, false);
                sys_print(b"  [vault-storage] vault-get status=");
                sys_printnum(st as usize);
                send_status(from, word, status_from_io(st));
            } else {
                sys_print(b"  [vault-storage] vault-get denied: vault not installed\n");
                send_status(from, word, IPC_STATUS_UNAVAILABLE);
            }
        } else if op == REQ_CAIRN_INIT {
            let st = if cairn_load_super(dma_base) {
                sys_print(b"  [cairn] v1 store ready (superblock verified)\n");
                IPC_STATUS_OK
            } else {
                IPC_STATUS_IO_FAILURE
            };
            send_status(from, word, st);
        } else if op == REQ_CAIRN_STATUS {
            send_status(from, word, cairn_status(dma_base));
        } else if op == REQ_CAIRN_COMMIT {
            let st = if cairn_ns_allowed(cairn_ns, sender_caps, from) {
                cairn_commit(dma_base, cairn_ns, cairn_aux, from)
            } else {
                IPC_STATUS_DENIED
            };
            send_status(from, word, st);
        } else if op == REQ_CAIRN_GET {
            let st = if cairn_ns_allowed(cairn_ns, sender_caps, from) {
                cairn_get(dma_base, cairn_ns)
            } else {
                IPC_STATUS_DENIED
            };
            send_status(from, word, st);
        } else if op == REQ_CAIRN_LOG {
            let st = if cairn_ns_allowed(cairn_ns, sender_caps, from) {
                cairn_log(dma_base, cairn_ns)
            } else {
                IPC_STATUS_DENIED
            };
            send_status(from, word, st);
        } else if op == REQ_CAIRN_ROLLBACK {
            let st = if cairn_ns_allowed(cairn_ns, sender_caps, from) {
                cairn_rollback(dma_base, cairn_ns, cairn_aux.max(1))
            } else {
                IPC_STATUS_DENIED
            };
            send_status(from, word, st);
        } else if op == REQ_CAIRN_VERIFY {
            let st = if cairn_ns_allowed(cairn_ns, sender_caps, from) {
                cairn_verify(dma_base, cairn_ns)
            } else {
                IPC_STATUS_DENIED
            };
            send_status(from, word, st);
        } else if op == REQ_FAULT_DEMO {
            sys_print(b"  [virtio-blk-daemon] FAULT-DEMO received; exiting with fault code\n");
            send_status(from, word, IPC_STATUS_OK);
            sys_exit(99);
        } else {
            sys_print(b"  [virtio-blk-daemon] typed IPC BAD_REQUEST: unknown op\n");
            send_status(from, word, IPC_STATUS_BAD_REQUEST);
        }
    }
}

fn shared_text_len() -> usize {
    let p = data_ptr();
    let mut n = 0usize;
    while n < 64 {
        let b = unsafe { core::ptr::read_volatile(p.add(n)) };
        if b == 0 {
            break;
        }
        n += 1;
    }
    n
}

fn client_set_input(s: &[u8]) -> usize {
    let n = s.len().min(SECTOR_SIZE - 1);
    unsafe {
        core::ptr::write_bytes((DMA_VA + INPUT_OFF) as *mut u8, 0, SECTOR_SIZE);
        core::ptr::copy_nonoverlapping(s.as_ptr(), (DMA_VA + INPUT_OFF) as *mut u8, n);
    }
    n
}

fn client_send(to: usize, op: usize, sector_or_len: usize) -> usize {
    let rc = sys_send(to, request_word(op, sector_or_len));
    if rc != 0 {
        sys_print(b"  [vblk-client] service unavailable or IPC denied\n");
        return IPC_STATUS_UNAVAILABLE;
    }
    let (reply, _, _) = sys_recv();
    response_status(reply)
}

fn client_demo(daemon: usize) -> ! {
    sys_print(b"  [vblk-client] talking to long-lived virtio-blk daemon over IPC\n");
    let _ = client_send(daemon, REQ_PROBE, 0);
    let _ = client_send(daemon, REQ_BWRITE, TEST_SECTOR as usize);
    let st = client_send(daemon, REQ_BREAD, TEST_SECTOR as usize);
    sys_print(b"  [vblk-client] read reply status=");
    sys_printnum(st);
    print_data(b"  [vblk-client] test sector via daemon = \"");

    let n = client_set_input(b"daemon-ci-value");
    let _ = client_send(daemon, REQ_PSET, n);
    let _ = client_send(daemon, REQ_PGET, 0);
    print_data(b"  [vblk-client] cairn current via daemon = \"");

    let n = client_set_input(b"daemon-bad-edit");
    let _ = client_send(daemon, REQ_PSET, n);
    let _ = client_send(daemon, REQ_PROLLBACK, 0);
    let _ = client_send(daemon, REQ_PGET, 0);
    print_data(b"  [vblk-client] rollback via daemon restored = \"");

    let _ = shared_text_len();
    sys_print(b"  [vblk-client] daemon workflow complete\n");
    sys_exit(0)
}

fn client_request(daemon: usize, input_len: usize, req: usize) -> ! {
    // Cairn v1 requests arrive packed: base op | ns << 8 | steps << 12.
    let base = req & 0xff;
    let cairn_ns = (req >> 8) & 0xf;
    let cairn_steps = (req >> 12) & 0xfff;
    if base >= REQ_CAIRN_INIT && base <= REQ_CAIRN_STATUS {
        let arg = if base == REQ_CAIRN_COMMIT {
            (cairn_ns << 12) | input_len.min(0xfff)
        } else if base == REQ_CAIRN_ROLLBACK {
            (cairn_ns << 12) | cairn_steps
        } else {
            cairn_ns << 12
        };
        let st = client_send(daemon, base, arg);
        if base == REQ_CAIRN_COMMIT {
            sys_print(b"  [vblk-client] cairn-commit status=");
        } else if base == REQ_CAIRN_GET {
            sys_print(b"  [vblk-client] cairn-get status=");
        } else if base == REQ_CAIRN_LOG {
            sys_print(b"  [vblk-client] cairn-log status=");
        } else if base == REQ_CAIRN_ROLLBACK {
            sys_print(b"  [vblk-client] cairn-rollback status=");
        } else if base == REQ_CAIRN_VERIFY {
            sys_print(b"  [vblk-client] cairn-verify status=");
        } else if base == REQ_CAIRN_STATUS {
            sys_print(b"  [vblk-client] cairn-status status=");
        } else {
            sys_print(b"  [vblk-client] cairn-init status=");
        }
        sys_printnum(st);
        if base == REQ_CAIRN_GET && st == IPC_STATUS_OK {
            print_data(b"  [vblk-client] cairn value = \"");
        }
        if st == IPC_STATUS_DENIED {
            sys_print(b"  [vblk-client] DENIED by storage service (kernel-attested caps)\n");
        }
        sys_exit(st)
    }
    let sector_or_len = if req == REQ_BWRITE || req == REQ_BREAD {
        TEST_SECTOR as usize
    } else if req == REQ_PSET
        || req == REQ_NOTE_SET
        || req == REQ_LAB_SET
        || req == REQ_CALC_SET
        || req == REQ_VAULT_SET
    {
        input_len
    } else if req == REQ_PKG_REGISTRY_READ
        || req == REQ_PKG_REGISTRY_WRITE
        || req == REQ_PKG_BLOB_READ
        || req == REQ_PKG_BLOB_WRITE
        || req == REQ_PKG_JOURNAL_READ
        || req == REQ_PKG_JOURNAL_WRITE
    {
        input_len
    } else {
        0
    };
    let st = client_send(daemon, req, sector_or_len);
    if req == REQ_PROBE {
        sys_print(b"  [vblk-client] disk probe via registered daemon status=");
        sys_printnum(st);
    } else if req == REQ_BWRITE {
        sys_print(b"  [vblk-client] bwrite via registered daemon status=");
        sys_printnum(st);
    } else if req == REQ_BREAD {
        sys_print(b"  [vblk-client] bread via registered daemon status=");
        sys_printnum(st);
        print_data(b"  [vblk-client] test sector = \"");
    } else if req == REQ_PSET {
        sys_print(b"  [vblk-client] cairn set via registered daemon status=");
        sys_printnum(st);
    } else if req == REQ_PGET {
        sys_print(b"  [vblk-client] cairn get via registered daemon status=");
        sys_printnum(st);
        print_data(b"  [vblk-client] cairn current = \"");
    } else if req == REQ_PROLLBACK {
        sys_print(b"  [vblk-client] rollback via registered daemon status=");
        sys_printnum(st);
        let _ = client_send(daemon, REQ_PGET, 0);
        print_data(b"  [vblk-client] rollback restored current = \"");
    } else if req == REQ_INSTALL_CHECK {
        sys_print(b"  [vblk-client] install-check status=");
        sys_printnum(st);
    } else if req == REQ_INSTALL_INIT {
        sys_print(b"  [vblk-client] install-init status=");
        sys_printnum(st);
    } else if req == REQ_ROOT_STATUS {
        sys_print(b"  [vblk-client] root-status status=");
        sys_printnum(st);
        print_data(b"  [vblk-client] root metadata = \"");
    } else if req == REQ_APP_AVAILABLE {
        sys_print(b"  [vblk-client] apps available status=");
        sys_printnum(st);
    } else if req == REQ_APP_INSTALLED {
        sys_print(b"  [vblk-client] apps installed status=");
        sys_printnum(st);
    } else if req == REQ_APP_INFO {
        sys_print(b"  [vblk-client] app-info status=");
        sys_printnum(st);
    } else if req == REQ_APP_INSTALL_NOTE {
        sys_print(b"  [vblk-client] app-install note status=");
        sys_printnum(st);
    } else if req == REQ_APP_REQUIRE_NOTE {
        sys_print(b"  [vblk-client] app-require note status=");
        sys_printnum(st);
    } else if req == REQ_APP_REMOVE_NOTE {
        sys_print(b"  [vblk-client] app-remove note status=");
        sys_printnum(st);
    } else if req == REQ_NOTE_SET {
        sys_print(b"  [vblk-client] note-set status=");
        sys_printnum(st);
    } else if req == REQ_NOTE_GET {
        sys_print(b"  [vblk-client] note-get status=");
        sys_printnum(st);
        print_data(b"  [vblk-client] note value = \"");
    } else if req == REQ_APP_INSTALL_LAB {
        sys_print(b"  [vblk-client] app-install lab status=");
        sys_printnum(st);
    } else if req == REQ_APP_REQUIRE_LAB {
        sys_print(b"  [vblk-client] app-require lab status=");
        sys_printnum(st);
    } else if req == REQ_APP_REMOVE_LAB {
        sys_print(b"  [vblk-client] app-remove lab status=");
        sys_printnum(st);
    } else if req == REQ_LAB_SET {
        sys_print(b"  [vblk-client] lab-set status=");
        sys_printnum(st);
    } else if req == REQ_LAB_GET {
        sys_print(b"  [vblk-client] lab-get status=");
        sys_printnum(st);
        print_data(b"  [vblk-client] lab value = \"");
    } else if req == REQ_APP_INSTALL_CALC {
        sys_print(b"  [vblk-client] app-install calc status=");
        sys_printnum(st);
    } else if req == REQ_APP_REQUIRE_CALC {
        sys_print(b"  [vblk-client] app-require calc status=");
        sys_printnum(st);
    } else if req == REQ_APP_REMOVE_CALC {
        sys_print(b"  [vblk-client] app-remove calc status=");
        sys_printnum(st);
    } else if req == REQ_CALC_SET {
        sys_print(b"  [vblk-client] calc-set status=");
        sys_printnum(st);
    } else if req == REQ_CALC_GET {
        sys_print(b"  [vblk-client] calc-history status=");
        sys_printnum(st);
        print_data(b"  [vblk-client] calc last = \"");
    } else if req == REQ_APP_INSTALL_VAULT {
        sys_print(b"  [vblk-client] app-install vault status=");
        sys_printnum(st);
    } else if req == REQ_APP_REQUIRE_VAULT {
        sys_print(b"  [vblk-client] app-require vault status=");
        sys_printnum(st);
    } else if req == REQ_APP_REMOVE_VAULT {
        sys_print(b"  [vblk-client] app-remove vault status=");
        sys_printnum(st);
    } else if req == REQ_VAULT_SET {
        sys_print(b"  [vblk-client] vault-put status=");
        sys_printnum(st);
    } else if req == REQ_VAULT_GET {
        sys_print(b"  [vblk-client] vault-get status=");
        sys_printnum(st);
        print_data(b"  [vblk-client] vault value = \"");
    }
    sys_exit(st)
}

extern "C" fn main(op: usize, dma_base: usize, input_len: usize, req: usize) -> ! {
    if op == OP_NO_GRANT_PROBE {
        sys_print(b"  [virtio-blk] no-grant probe: touching MMIO without a device capability\n");
        let _ = r32(VR_MAGIC);
        sys_print(b"  [virtio-blk] BUG: no-grant MMIO read succeeded\n");
        sys_exit(2);
    }

    if op == OP_DAEMON {
        daemon(dma_base);
    }
    if op == OP_CLIENT_DEMO {
        client_demo(dma_base);
    }
    if op == OP_CLIENT_REQ {
        client_request(dma_base, input_len, req);
    }

    clear_dma();
    sys_print(b"  [virtio-blk] user-space driver started (U-mode ELF)\n");
    if !init(dma_base) {
        sys_print(b"  [virtio-blk] no virtio block device found\n");
        sys_exit(1);
    }
    sys_print(b"  [virtio-blk] device capability accepted: virtio-blk @ MMIO\n");
    sys_print(b"  [virtio-blk] dma window granted at PA ");
    sys_printnum(dma_base);

    if op == OP_DISK {
        sys_print(b"  [virtio-blk] disk: version=");
        sys_printnum(r32(VR_VERSION) as usize);
        sys_print(b"  [virtio-blk] disk: device-id=");
        sys_printnum(r32(VR_DEVICE_ID) as usize);
        sys_exit(0);
    } else if op == OP_BWRITE {
        set_data(b"DEZH-PERSISTENT-DISK-OK");
        let st = rw(dma_base, TEST_SECTOR, true);
        sys_print(b"  [virtio-blk] bwrite via user-space driver status=");
        sys_printnum(st as usize);
        sys_exit(st as usize);
    } else if op == OP_BREAD {
        set_data(b"");
        let st = rw(dma_base, TEST_SECTOR, false);
        sys_print(b"  [virtio-blk] bread via user-space driver status=");
        sys_printnum(st as usize);
        print_data(b"  [virtio-blk] test sector = \"");
        sys_exit(st as usize);
    } else if op == OP_PSET {
        let _ = rw(dma_base, CAIRN_CURRENT_SECTOR, false);
        let _ = rw(dma_base, CAIRN_PREVIOUS_SECTOR, true);
        copy_input(input_len);
        let st = rw(dma_base, CAIRN_CURRENT_SECTOR, true);
        sys_print(b"  [virtio-blk] cairn set via user-space driver status=");
        sys_printnum(st as usize);
        sys_exit(st as usize);
    } else if op == OP_PGET {
        let st = rw(dma_base, CAIRN_CURRENT_SECTOR, false);
        sys_print(b"  [virtio-blk] cairn get via user-space driver status=");
        sys_printnum(st as usize);
        print_data(b"  [virtio-blk] cairn current = \"");
        sys_exit(st as usize);
    } else if op == OP_PROLLBACK {
        let _ = rw(dma_base, CAIRN_PREVIOUS_SECTOR, false);
        let st = rw(dma_base, CAIRN_CURRENT_SECTOR, true);
        sys_print(b"  [virtio-blk] rollback via user-space driver status=");
        sys_printnum(st as usize);
        print_data(b"  [virtio-blk] rollback restored current = \"");
        sys_exit(st as usize);
    }

    sys_print(b"  [virtio-blk] unknown transaction\n");
    sys_exit(2)
}
