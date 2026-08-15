//! Dezh on x86_64 — milestone 1: boot via Multiboot, climb to 64-bit long mode,
//! and talk to the COM1 serial port. QEMU loads this ELF directly with `-kernel`
//! (Multiboot1), entering in 32-bit protected mode; `arch::boot`'s trampoline
//! sets up identity paging + long mode, then calls `kmain`.
//!
//! The architecture-independent Dezh logic (capabilities, Cairn, IPC, the IR
//! engine) is shared later; this crate is the x86 hardware layer (boot, paging,
//! traps, context switch) — the only part that must be written per ISA.

#![no_std]
#![no_main]

mod arch;
mod console;
mod dev;
mod global;
mod io;

use arch::timer;
use console::{print, print_i64, putb};
use core::arch::asm;
use core::panic::PanicInfo;

// The x86 implementation of the shared Dezh-core Host: capability checks + the
// actual side effect (serial output). The Dezh-IR engine itself is shared.
struct SerialHost {
    cap: bool,
}
impl dezh_core::ir::Host for SerialHost {
    fn can(&self, cap: u32) -> bool {
        self.cap && cap == dezh_core::ir::CAP_PRINT
    }
    fn print_num(&mut self, v: i64) {
        print("  [ir] => ");
        print_i64(v);
        print("\n");
    }
    fn print_str(&mut self, s: &[u8]) {
        print("  [ir] ");
        for &b in s {
            putb(b);
        }
        putb(b'\n');
    }
    // No block device on x86 yet (M2/M3); Cairn host calls are unavailable.
    fn cairn_put(&mut self, _data: &[u8]) -> bool {
        false
    }
    fn cairn_get(&mut self, _buf: &mut [u8]) -> Option<usize> {
        None
    }
}

/// A deterministic unit of work, run under the armed timer.
///
/// Its running total and loop counter are live in registers across whatever
/// interrupt lands in the middle of it, so an entry path that lost a register
/// shows up here as a wrong sum rather than as a plausible-looking number. The
/// `black_box` is what keeps the sum from being folded away at compile time in
/// the release profile, where there would otherwise be no loop left to interrupt.
fn work_chunk() -> u64 {
    let mut sum: u64 = 0;
    let mut i: u64 = 1;
    while i <= 1000 {
        sum = sum.wrapping_add(core::hint::black_box(i));
        i += 1;
    }
    sum
}
const WORK_CHUNK_SUM: u64 = 500_500;

/// Arms the timer and then refuses to stop working while it fires.
///
/// The exception path in this kernel ends every trap in `hlt`, which is only
/// ever an admission that the machine cannot go on. This is the other kind of
/// trap: the CPU is taken away from a loop that was in the middle of a sum and
/// has to be handed back to it. The output below is what distinguishes the two —
/// a tick count that grew, a work loop that kept its arithmetic intact across
/// every one of those ticks, and, after the timer is masked, a tick count that
/// stops while the same loop keeps running.
fn timer_demo() {
    print("\nTimer: arming the Local APIC timer and staying at work through it.\n");
    if !timer::lapic_enable() {
        print("  [timer] Local APIC is not at the mapped window; timer not armed.\n");
        return;
    }
    print("  [timer] Local APIC enabled, id=");
    print_i64(timer::lapic_id() as i64);
    print("\n");

    let counted = match timer::calibrate() {
        None => {
            print("  [timer] PIT calibration returned no counts; timer not armed.\n");
            return;
        }
        Some(c) => c,
    };
    let per_second = counted as u64 * (1000 / timer::CALIBRATE_MS);
    print("  [timer] measured ");
    print_i64(counted as i64);
    print(" LAPIC counts in ");
    print_i64(timer::CALIBRATE_MS as i64);
    print(" ms at divide-16 (APIC bus ");
    print_i64((per_second * timer::LAPIC_DIVISOR) as i64);
    print(" Hz)\n");

    let per_tick = per_second / timer::TIMER_HZ;
    if per_tick == 0 || per_tick > u32::MAX as u64 {
        print("  [timer] measured rate will not fit the requested tick; timer not armed.\n");
        return;
    }

    print("  [timer] armed: vector 0x30, periodic, ");
    print_i64(timer::TIMER_HZ as i64);
    print(" Hz\n");
    timer::arm_periodic(per_tick as u32);
    timer::sti();

    const TARGET_TICKS: u64 = 10;
    let mut rounds: u64 = 0;
    let mut wrong: u64 = 0;
    while timer::ticks() < TARGET_TICKS {
        if work_chunk() != WORK_CHUNK_SUM {
            wrong += 1;
        }
        rounds += 1;
    }
    let ticks = timer::ticks();

    // Mask the timer and keep the same loop running with interrupts still on.
    // If the ticks had been coming from anything other than the source just
    // armed, the count would carry on climbing here.
    timer::mask();
    for _ in 0..64 {
        // Drain anything the APIC had already begun delivering when the mask
        // landed, so the frozen reading below is taken after the last of them.
        let _ = work_chunk();
    }
    let frozen = timer::ticks();
    for _ in 0..2000 {
        if work_chunk() != WORK_CHUNK_SUM {
            wrong += 1;
        }
        rounds += 1;
    }
    let after = timer::ticks();
    timer::cli();

    print("  [timer] took ");
    print_i64(ticks as i64);
    print(" ticks; the work loop completed ");
    print_i64(rounds as i64);
    print(" rounds\n");
    if wrong == 0 && ticks >= TARGET_TICKS && rounds > 0 {
        print("  [timer] interrupts returned: work resumed after every tick, checksum OK\n");
    } else {
        print("  [timer] FAILED: ");
        print_i64(wrong as i64);
        print(" corrupted rounds\n");
    }
    if after == frozen {
        print("  [timer] masked: tick count frozen at ");
        print_i64(after as i64);
        print(" while the work loop kept running\n");
    } else {
        print("  [timer] FAILED: ticks kept arriving after the timer was masked\n");
    }
    let spurious = timer::spurious();
    if spurious != 0 {
        print("  [timer] spurious APIC deliveries: ");
        print_i64(spurious as i64);
        print("\n");
    }
}

#[no_mangle]
pub extern "C" fn kmain() -> ! {
    use dezh_core::ir;
    console::init();
    arch::trap::init();
    arch::pic::remap_and_mask();
    print("\n");
    print("Dezh x86_64 - long mode reached. 64-bit kernel running.\n");
    print("IDT installed: 32 CPU-exception vectors (faults are reported, not silent)\n");
    print("  plus 224 interrupt vectors on a path that saves state and returns.\n");
    print("Legacy 8259 PICs remapped to 0x20..0x2F and fully masked.\n");

    // Install and run a real .dzp package (F3, D003/D016): the SAME Dezh-IR bytes
    // the RISC-V kernel runs, wrapped in the SAME architecture-independent .dzp
    // format the SDK builds. We pack it, then parse it back exactly as an install
    // flow would (magic + version + CRC + manifest checks) and run the payload.
    // The bytes are pinned byte-identical by dezh-core's `demo_sum_bytes_are_pinned`
    // test, so what installs on one ISA is exactly what runs on the other.
    use dezh_core::dzp;
    print("Dezh .dzp agent package (sum 1..=5 with a loop) on x86_64:\n");
    let mut prog_buf = [0u8; 256];
    let prog = ir::demo_sum(&mut prog_buf);
    let manifest = "name = \"agent-sum\"\nversion = \"0.1.0\"\ncaps = [\"print\"]\n";
    let mut pkg = [0u8; 512];
    let n = dzp::pack(dzp::KIND_DEZH_IR, manifest, prog, &mut pkg);
    match dzp::parse(&pkg[..n]) {
        Err(e) => {
            print("  .dzp parse failed: ");
            print(e.msg());
            print("\n");
        }
        Ok(p) => {
            print("  .dzp verified: kind=");
            print(dzp::kind_name(p.kind));
            print(", name=");
            print(dzp::manifest_str(p.manifest, "name").unwrap_or("?"));
            print("\n");
            match ir::verify(p.payload) {
                Err(_) => print("  IR verify failed\n"),
                Ok(()) => {
                    print("  with PRINT capability:\n");
                    let mut h = SerialHost { cap: true };
                    let _ = ir::run(p.payload, &mut h);
                    print("  without PRINT capability:\n");
                    let mut h = SerialHost { cap: false };
                    if ir::run(p.payload, &mut h) == Err(ir::Trap::MissingCapability) {
                        print("  [ir] DENIED: agent holds no PRINT capability\n");
                    }
                }
            }
        }
    }

    timer_demo();

    // Prove the IDT works: deliberately raise a breakpoint (vector 3). Without an
    // IDT this would triple-fault and reset the machine; instead the handler
    // catches it, reports it, and halts cleanly.
    print("\nTrap demo: deliberately raising a breakpoint (int3) to prove the handler catches it...\n");
    unsafe { asm!("int3") };

    // The breakpoint handler halts, so this is unreachable; kept for totality.
    loop {
        unsafe { asm!("hlt") };
    }
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    loop {
        unsafe { asm!("hlt") };
    }
}
