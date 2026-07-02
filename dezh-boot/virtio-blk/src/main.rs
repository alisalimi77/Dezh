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

const REQ_PROBE: usize = 1;
const REQ_BWRITE: usize = 2;
const REQ_BREAD: usize = 3;
const REQ_PSET: usize = 4;
const REQ_PGET: usize = 5;
const REQ_PROLLBACK: usize = 6;
const REQ_STOP: usize = 7;

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

fn sys_send(to: usize, word: usize) {
    unsafe {
        asm!("ecall",
            inout("a0") to => _,
            in("a1") 0usize,
            in("a2") 0usize,
            in("a3") 0usize,
            in("a4") word,
            in("a7") SYS_SEND)
    };
}

fn sys_recv() -> (usize, usize) {
    let from: usize;
    let word: usize;
    unsafe {
        asm!("ecall",
            inout("a0") 0usize => _,
            inout("a1") 0usize => from,
            out("a2") word,
            in("a7") SYS_RECV)
    };
    (word, from)
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

fn copy_input(len: usize) {
    unsafe {
        core::ptr::write_bytes(data_ptr(), 0, SECTOR_SIZE);
        let n = len.min(SECTOR_SIZE - 1);
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

fn request_word(op: usize, sector: usize) -> usize {
    (op << 56) | (sector & 0x00ff_ffff_ffff_ffff)
}

fn request_op(word: usize) -> usize {
    word >> 56
}

fn request_sector(word: usize) -> u64 {
    (word & 0x00ff_ffff_ffff_ffff) as u64
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
        let (word, from) = sys_recv();
        let op = request_op(word);
        let sector = request_sector(word);
        if op == REQ_PROBE {
            sys_print(b"  [virtio-blk-daemon] PROBE over IPC\n");
            sys_send(from, 0);
        } else if op == REQ_BWRITE {
            set_data(b"DEZH-DAEMON-BLOCK-OK");
            let st = rw(dma_base, sector, true);
            sys_print(b"  [virtio-blk-daemon] WRITE sector via IPC status=");
            sys_printnum(st as usize);
            sys_send(from, st as usize);
        } else if op == REQ_BREAD {
            set_data(b"");
            let st = rw(dma_base, sector, false);
            sys_print(b"  [virtio-blk-daemon] READ sector via IPC status=");
            sys_printnum(st as usize);
            sys_send(from, st as usize);
        } else if op == REQ_PSET {
            let _ = rw(dma_base, 0, false);
            let _ = rw(dma_base, 1, true);
            copy_input(sector as usize);
            let st = rw(dma_base, 0, true);
            sys_print(b"  [virtio-blk-daemon] CAIRN SET via IPC status=");
            sys_printnum(st as usize);
            sys_send(from, st as usize);
        } else if op == REQ_PGET {
            let st = rw(dma_base, 0, false);
            sys_print(b"  [virtio-blk-daemon] CAIRN GET via IPC status=");
            sys_printnum(st as usize);
            sys_send(from, st as usize);
        } else if op == REQ_PROLLBACK {
            let _ = rw(dma_base, 1, false);
            let st = rw(dma_base, 0, true);
            sys_print(b"  [virtio-blk-daemon] CAIRN ROLLBACK via IPC status=");
            sys_printnum(st as usize);
            sys_send(from, st as usize);
        } else if op == REQ_STOP {
            sys_print(b"  [virtio-blk-daemon] STOP received; exiting cleanly\n");
            sys_send(from, 0);
            sys_exit(0);
        } else {
            sys_send(from, 2);
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

fn client_send(op: usize, sector_or_len: usize) -> usize {
    sys_send(0, request_word(op, sector_or_len));
    let (reply, _) = sys_recv();
    reply
}

fn client_demo() -> ! {
    sys_print(b"  [vblk-client] talking to long-lived virtio-blk daemon over IPC\n");
    let _ = client_send(REQ_PROBE, 0);
    let _ = client_send(REQ_BWRITE, 0);
    let st = client_send(REQ_BREAD, 0);
    sys_print(b"  [vblk-client] read reply status=");
    sys_printnum(st);
    print_data(b"  [vblk-client] sector0 via daemon = \"");

    let n = client_set_input(b"daemon-ci-value");
    let _ = client_send(REQ_PSET, n);
    let _ = client_send(REQ_PGET, 0);
    print_data(b"  [vblk-client] cairn current via daemon = \"");

    let n = client_set_input(b"daemon-bad-edit");
    let _ = client_send(REQ_PSET, n);
    let _ = client_send(REQ_PROLLBACK, 0);
    let _ = client_send(REQ_PGET, 0);
    print_data(b"  [vblk-client] rollback via daemon restored = \"");

    let _ = shared_text_len();
    let _ = client_send(REQ_STOP, 0);
    sys_print(b"  [vblk-client] daemon workflow complete\n");
    sys_exit(0)
}

extern "C" fn main(op: usize, dma_base: usize, input_len: usize) -> ! {
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
        client_demo();
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
        let st = rw(dma_base, 0, true);
        sys_print(b"  [virtio-blk] bwrite via user-space driver status=");
        sys_printnum(st as usize);
        sys_exit(st as usize);
    } else if op == OP_BREAD {
        set_data(b"");
        let st = rw(dma_base, 0, false);
        sys_print(b"  [virtio-blk] bread via user-space driver status=");
        sys_printnum(st as usize);
        print_data(b"  [virtio-blk] sector0 = \"");
        sys_exit(st as usize);
    } else if op == OP_PSET {
        let _ = rw(dma_base, 0, false);
        let _ = rw(dma_base, 1, true);
        copy_input(input_len);
        let st = rw(dma_base, 0, true);
        sys_print(b"  [virtio-blk] cairn set via user-space driver status=");
        sys_printnum(st as usize);
        sys_exit(st as usize);
    } else if op == OP_PGET {
        let st = rw(dma_base, 0, false);
        sys_print(b"  [virtio-blk] cairn get via user-space driver status=");
        sys_printnum(st as usize);
        print_data(b"  [virtio-blk] cairn current = \"");
        sys_exit(st as usize);
    } else if op == OP_PROLLBACK {
        let _ = rw(dma_base, 1, false);
        let st = rw(dma_base, 0, true);
        sys_print(b"  [virtio-blk] rollback via user-space driver status=");
        sys_printnum(st as usize);
        print_data(b"  [virtio-blk] rollback restored current = \"");
        sys_exit(st as usize);
    }

    sys_print(b"  [virtio-blk] unknown transaction\n");
    sys_exit(2)
}
