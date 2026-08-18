//! Proof that an interrupt gives the CPU back.
//!
//! The exception path in this kernel ends every trap in `hlt`, which is only
//! ever an admission that the machine cannot go on. This is the other kind of
//! trap: the CPU is taken away from a loop that was in the middle of a sum and
//! has to be handed back to it. Three things distinguish the two in the output —
//! a tick count that grew, a work loop that kept its arithmetic intact across
//! every one of those ticks, and, once the timer is masked, a tick count that
//! stops while the same loop keeps running.

use crate::arch::timer;
use crate::console::{print, print_i64};

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
const TARGET_TICKS: u64 = 10;

pub(crate) fn run() {
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
