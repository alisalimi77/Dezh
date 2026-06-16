//! # dezh-boot — Step 10: bare-metal kernel boot, interrupts, and a console
//!
//! This is the first Dezh code that runs on bare metal (QEMU `virt`, RISC-V 64).
//! It crosses the simulation → hardware boundary that every earlier spike ran
//! around. The boot flow:
//!
//!   1. come up in S-mode after OpenSBI, zero `.bss`, set the stack;
//!   2. build the kernel boot description and run it through the *validated*
//!      `dezh-kernel` boot contract, printing the banner + init service plan;
//!   3. install an S-mode trap vector and arm the SBI timer (a silent background
//!      uptime tick — the kernel's first hardware event source);
//!   4. run **Dezh's own console** over the UART: a real read/eval/print loop
//!      where every command is gated by an explicit capability. The console
//!      holds a fixed capability set; a command whose capability it was not
//!      granted is denied — no-ambient-authority, now interactive on bare metal.

#![no_std]
#![no_main]

extern crate alloc;

use core::alloc::{GlobalAlloc, Layout};
use core::arch::{asm, global_asm};
use core::cell::UnsafeCell;
use core::fmt::{self, Write};
use core::panic::PanicInfo;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use alloc::vec;
use dezh_kernel::{boot_banner, plan_boot, BootInfo, KernelPlan, MemoryKind, MemoryRegion};

// --- Boot entry: zero .bss, set the stack, jump to Rust. -------------------
// --- Trap entry: save caller-saved regs, call the handler, restore, sret. --
global_asm!(
    r#"
    .section .text.entry
    .globl _start
_start:
    la      t0, __bss_start
    la      t1, __bss_end
0:
    bgeu    t0, t1, 1f
    sd      zero, 0(t0)
    addi    t0, t0, 8
    j       0b
1:
    la      sp, __stack_top
    call    kmain
2:
    wfi
    j       2b

    .section .text
    .align 4
    .globl trap_entry
trap_entry:
    addi    sp, sp, -128
    sd      ra,   0(sp)
    sd      t0,   8(sp)
    sd      t1,  16(sp)
    sd      t2,  24(sp)
    sd      t3,  32(sp)
    sd      t4,  40(sp)
    sd      t5,  48(sp)
    sd      t6,  56(sp)
    sd      a0,  64(sp)
    sd      a1,  72(sp)
    sd      a2,  80(sp)
    sd      a3,  88(sp)
    sd      a4,  96(sp)
    sd      a5, 104(sp)
    sd      a6, 112(sp)
    sd      a7, 120(sp)
    call    trap_handler
    ld      ra,   0(sp)
    ld      t0,   8(sp)
    ld      t1,  16(sp)
    ld      t2,  24(sp)
    ld      t3,  32(sp)
    ld      t4,  40(sp)
    ld      t5,  48(sp)
    ld      t6,  56(sp)
    ld      a0,  64(sp)
    ld      a1,  72(sp)
    ld      a2,  80(sp)
    ld      a3,  88(sp)
    ld      a4,  96(sp)
    ld      a5, 104(sp)
    ld      a6, 112(sp)
    ld      a7, 120(sp)
    addi    sp, sp, 128
    sret
"#
);

extern "C" {
    fn trap_entry();
}

// --- NS16550 UART on the QEMU `virt` board. --------------------------------
const UART_BASE: *mut u8 = 0x1000_0000 as *mut u8;
const UART_RBR: usize = 0; // receive buffer (read)
const UART_THR: usize = 0; // transmit holding (write)
const UART_FCR: usize = 2; // FIFO control (write)
const UART_LSR: usize = 5; // line status: bit0 = data ready, bit5 = THR empty

struct Uart;

impl Uart {
    /// Enable the 16550 RX/TX FIFOs so bursts of input are buffered (16 bytes)
    /// instead of overrunning the single RBR between polls.
    fn init(&self) {
        unsafe { write_volatile(UART_BASE.add(UART_FCR), 0x07) } // enable + clear FIFOs
    }

    fn putc(&self, byte: u8) {
        unsafe { write_volatile(UART_BASE.add(UART_THR), byte) }
    }

    /// Blocking read of one byte from the UART (polls the Data-Ready bit).
    fn getc(&self) -> u8 {
        loop {
            let lsr = unsafe { read_volatile(UART_BASE.add(UART_LSR)) };
            if lsr & 0x01 != 0 {
                return unsafe { read_volatile(UART_BASE.add(UART_RBR)) };
            }
        }
    }
}

impl Write for Uart {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            self.putc(b);
        }
        Ok(())
    }
}

macro_rules! kprint {
    ($($arg:tt)*) => {{ let _ = core::write!(Uart, $($arg)*); }};
}
macro_rules! kprintln {
    ($($arg:tt)*) => {{ let _ = core::writeln!(Uart, $($arg)*); }};
}

// --- Minimal bump allocator (alloc is needed by dezh-kernel's Vec/String). --
const HEAP_SIZE: usize = 1 << 20; // 1 MiB, lives in .bss (zeroed by _start)

struct BumpHeap {
    arena: UnsafeCell<[u8; HEAP_SIZE]>,
    next: AtomicUsize,
}

unsafe impl Sync for BumpHeap {}

unsafe impl GlobalAlloc for BumpHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let base = self.arena.get() as usize;
        loop {
            let cur = self.next.load(Ordering::Relaxed);
            let aligned = (base + cur + layout.align() - 1) & !(layout.align() - 1);
            let new_next = aligned - base + layout.size();
            if new_next > HEAP_SIZE {
                return core::ptr::null_mut();
            }
            if self
                .next
                .compare_exchange(cur, new_next, Ordering::SeqCst, Ordering::Relaxed)
                .is_ok()
            {
                return aligned as *mut u8;
            }
        }
    }

    unsafe fn dealloc(&self, _ptr: *mut u8, _layout: Layout) {}
}

#[global_allocator]
static HEAP: BumpHeap = BumpHeap {
    arena: UnsafeCell::new([0; HEAP_SIZE]),
    next: AtomicUsize::new(0),
};

// --- QEMU `virt` SiFive test finisher: cleanly exit the emulator. ----------
const TEST_FINISHER: *mut u32 = 0x10_0000 as *mut u32;
const FINISH_PASS: u32 = 0x5555;
const FINISH_FAIL: u32 = 0x3333;

fn shutdown(code: u32) -> ! {
    unsafe { write_volatile(TEST_FINISHER, code) }
    loop {
        unsafe { asm!("wfi") }
    }
}

// --- Timer (silent background uptime tick) ---------------------------------
// QEMU `virt` mtimer runs at 10 MHz, so 1_000_000 ticks ≈ 0.1 s.
const TIMER_DELTA: u64 = 1_000_000;
const TIMER_HZ: u64 = 10; // ticks per second (0.1 s period)
static TICKS: AtomicU64 = AtomicU64::new(0);

fn rdtime() -> u64 {
    let t: u64;
    unsafe { asm!("rdtime {}", out(reg) t) };
    t
}

/// Program the next timer interrupt via the SBI legacy `set_timer` call, which
/// also clears the pending timer interrupt.
fn sbi_set_timer(stime: u64) {
    unsafe {
        asm!(
            "ecall",
            in("a0") stime,
            in("a7") 0usize, // legacy SBI_SET_TIMER
            lateout("a0") _,
            lateout("a1") _,
        );
    }
}

#[no_mangle]
extern "C" fn trap_handler() {
    let scause: usize;
    unsafe { asm!("csrr {}, scause", out(reg) scause) };

    let interrupt = scause >> (usize::BITS - 1) == 1;
    let code = scause & (!0 >> 1);

    if interrupt && code == 5 {
        // Supervisor timer: bump uptime silently, re-arm.
        TICKS.fetch_add(1, Ordering::Relaxed);
        sbi_set_timer(rdtime() + TIMER_DELTA);
    } else {
        kprintln!("\n[dezh-boot] unexpected trap scause={scause:#x} — halting");
        shutdown(FINISH_FAIL);
    }
}

// --- Console capabilities ---------------------------------------------------
// The console is NOT ambient: it holds an explicit capability set, and each
// command requires a specific capability. A command whose capability the
// console was never granted is denied — the Step 1 thesis, now at the console.
mod cap {
    pub const INSPECT: u32 = 1 << 0; // read boot/memory/cap state
    pub const TIME: u32 = 1 << 1; // read uptime
    pub const ECHO: u32 = 1 << 2; // echo text
    pub const HALT: u32 = 1 << 3; // power off
    pub const SECRET: u32 = 1 << 4; // deliberately never granted (demo)
}

struct Command {
    name: &'static str,
    cap: u32,
    cap_name: &'static str,
    help: &'static str,
}

const COMMANDS: &[Command] = &[
    Command { name: "help", cap: 0, cap_name: "-", help: "list commands" },
    Command { name: "caps", cap: cap::INSPECT, cap_name: "INSPECT", help: "show the console's capabilities" },
    Command { name: "mem", cap: cap::INSPECT, cap_name: "INSPECT", help: "show the memory map" },
    Command { name: "services", cap: cap::INSPECT, cap_name: "INSPECT", help: "list init services" },
    Command { name: "uptime", cap: cap::TIME, cap_name: "TIME", help: "show timer uptime" },
    Command { name: "echo", cap: cap::ECHO, cap_name: "ECHO", help: "echo <text>" },
    Command { name: "secret", cap: cap::SECRET, cap_name: "SECRET", help: "(needs a cap the console lacks)" },
    Command { name: "halt", cap: cap::HALT, cap_name: "HALT", help: "power off the machine" },
];

fn cap_names(set: u32) -> &'static str {
    // Compact printable summary for the held set used in this demo.
    match set {
        s if s == cap::INSPECT | cap::TIME | cap::ECHO | cap::HALT => "INSPECT TIME ECHO HALT",
        _ => "(custom set)",
    }
}

/// The interactive console. Never returns except via the `halt` command.
fn console(plan: &KernelPlan, memory: &[MemoryRegion], held: u32) -> ! {
    kprintln!();
    kprintln!("Dezh console. Every command requires an explicit capability.");
    kprintln!("Type 'help'. The console holds: {}", cap_names(held));

    let mut buf = [0u8; 128];
    loop {
        kprint!("dezh> ");
        let len = read_line(&mut buf);
        let line = core::str::from_utf8(&buf[..len]).unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let (cmd, arg) = match line.split_once(' ') {
            Some((c, a)) => (c, a.trim()),
            None => (line, ""),
        };
        dispatch(cmd, arg, plan, memory, held);
    }
}

fn dispatch(cmd: &str, arg: &str, plan: &KernelPlan, memory: &[MemoryRegion], held: u32) {
    let spec = COMMANDS.iter().find(|c| c.name == cmd);
    let spec = match spec {
        Some(s) => s,
        None => {
            kprintln!("unknown command: {cmd} (try 'help')");
            return;
        }
    };

    // THE CONSOLE ENFORCEMENT POINT: deny anything we lack the capability for.
    if spec.cap != 0 && held & spec.cap != spec.cap {
        kprintln!("denied: '{}' requires capability {} (not held)", cmd, spec.cap_name);
        return;
    }

    match cmd {
        "help" => {
            kprintln!("commands (cap required → held?):");
            for c in COMMANDS {
                let ok = if c.cap == 0 || held & c.cap == c.cap {
                    "yes"
                } else {
                    "DENIED"
                };
                kprintln!("  {:<9} {:<8} [{}]  {}", c.name, c.cap_name, ok, c.help);
            }
        }
        "caps" => kprintln!("console capabilities: {}", cap_names(held)),
        "mem" => {
            kprintln!("usable memory: {} bytes", plan.usable_bytes);
            for r in memory {
                let end = r.start + r.len;
                kprintln!("  {:#012x}..{:#012x}  {:?}", r.start, end, r.kind);
            }
        }
        "services" => {
            kprintln!("init services ({} total):", plan.services.len());
            for s in &plan.services {
                kprintln!("  - {:<13} {:?}", s.name, s.kind);
            }
        }
        "uptime" => {
            let t = TICKS.load(Ordering::Relaxed);
            kprintln!("uptime: {} ticks (~{}.{} s)", t, t / TIMER_HZ, t % TIMER_HZ);
        }
        "echo" => kprintln!("{arg}"),
        "halt" => {
            kprintln!("halting.");
            shutdown(FINISH_PASS);
        }
        // `secret` would only reach here if it were granted; it never is.
        other => kprintln!("'{other}' has no handler"),
    }
}

/// Read a line from the UART into `buf`, echoing characters, handling backspace.
/// Returns the number of bytes read (line terminator excluded).
fn read_line(buf: &mut [u8]) -> usize {
    let mut len = 0;
    loop {
        let c = Uart.getc();
        match c {
            b'\n' => {
                kprintln!();
                return len;
            }
            b'\r' => {} // ignore CR; treat only LF as the line terminator
            0x7f | 0x08 => {
                if len > 0 {
                    len -= 1;
                    kprint!("\x08 \x08"); // erase the echoed char
                }
            }
            c if (c == b' ' || c.is_ascii_graphic()) && len < buf.len() => {
                buf[len] = c;
                len += 1;
                Uart.putc(c); // local echo (piped/raw input is not echoed by QEMU)
            }
            _ => {} // ignore other control bytes
        }
    }
}

#[no_mangle]
pub extern "C" fn kmain() -> ! {
    Uart.init();
    kprintln!();
    kprintln!("[dezh-boot] alive on bare metal (qemu virt, riscv64, S-mode)");

    // QEMU `virt` physical layout: RAM at 0x8000_0000 (default 128 MiB);
    // OpenSBI occupies the first 2 MiB; the UART is MMIO at 0x1000_0000.
    let memory = vec![
        MemoryRegion::new(0x8000_0000, 0x20_0000, MemoryKind::Reserved), // OpenSBI / firmware
        MemoryRegion::new(0x8020_0000, 0x7E0_0000, MemoryKind::Usable),  // ~126 MiB usable
        MemoryRegion::new(0x1000_0000, 0x1000, MemoryKind::Mmio),        // UART
    ];

    let info = BootInfo::qemu_minimal_riscv(memory.clone());

    let plan = match plan_boot(&info) {
        Ok(plan) => plan,
        Err(e) => {
            kprintln!("[dezh-boot] BOOT CONTRACT VIOLATION: {e:?}");
            shutdown(FINISH_FAIL);
        }
    };

    kprintln!("[dezh-boot] boot contract VALIDATED");
    kprintln!("[dezh-boot] banner: {}", boot_banner(&plan));
    kprintln!("[dezh-boot] no ambient authority: capability seeds bound to declared services only");

    // Install the trap vector and arm the silent background uptime timer.
    kprintln!("[dezh-boot] installing trap vector + supervisor timer...");
    unsafe {
        asm!("csrw stvec, {}", in(reg) trap_entry as usize); // direct mode (low bits 0)
        sbi_set_timer(rdtime() + TIMER_DELTA);
        asm!("csrs sie, {}", in(reg) 1usize << 5); // STIE: supervisor timer enable
        asm!("csrs sstatus, {}", in(reg) 1usize << 1); // SIE: global supervisor interrupts
    }

    // The console holds an explicit, narrow capability set — NOT SECRET.
    let held = cap::INSPECT | cap::TIME | cap::ECHO | cap::HALT;
    console(&plan, &memory, held);
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kprint!("\n[dezh-boot] PANIC: ");
    kprintln!("{info}");
    shutdown(FINISH_FAIL);
}
