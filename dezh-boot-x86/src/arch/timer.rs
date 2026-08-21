//! The Local APIC timer, and the rate that had to be measured to arm it.
//!
//! The choice between the Local APIC timer and the legacy 8259 + PIT was decided
//! by where this is going: a scheduler needs a timer per CPU, and the LAPIC is
//! per CPU while the PIT is one global counter shared by all of them. The RISC-V
//! side already has a per-hart timer, so this keeps the two kernels the same
//! shape. Every 64-bit CPU has a LAPIC; the PIT and the 8259 are legacy parts
//! that modern platforms are removing.
//!
//! The two costs of that choice are paid here. The APIC register window is at
//! 0xFEE00000, outside the 2 MiB the trampoline identity-maps, so the trampoline
//! maps it. And unlike the PIT the LAPIC timer runs at a frequency nobody tells
//! you, so it has to be measured — against the PIT, which is still the one clock
//! on the machine with a frequency fixed by hardware. Every rate this kernel
//! prints is that measurement, not a datasheet number.

use crate::io::{inb, outb};
use core::arch::asm;
use core::sync::atomic::{AtomicU32, AtomicU64, Ordering};

const LAPIC_BASE: usize = 0xFEE0_0000;
const LAPIC_ID: usize = 0x020;
const LAPIC_TPR: usize = 0x080;
const LAPIC_EOI: usize = 0x0B0;
const LAPIC_SVR: usize = 0x0F0;
const LAPIC_LVT_TIMER: usize = 0x320;
const LAPIC_TIMER_INIT: usize = 0x380;
const LAPIC_TIMER_CUR: usize = 0x390;
const LAPIC_TIMER_DIV: usize = 0x3E0;

const LVT_MASKED: u32 = 1 << 16;
const LVT_PERIODIC: u32 = 1 << 17;
const SVR_ENABLE: u32 = 1 << 8;
const LAPIC_DIV_16: u32 = 0x3; // divide configuration register encoding
pub(crate) const LAPIC_DIVISOR: u64 = 16;

const IA32_APIC_BASE: u32 = 0x1B;
const APIC_BASE_GLOBAL_ENABLE: u64 = 1 << 11;

/// Timer vector, chosen above the 0x20..0x2F block the 8259s were remapped into
/// so the two can never be confused for one another.
pub(crate) const VEC_TIMER: u8 = 0x30;
/// The APIC's own "never mind" vector, given a number of its own so a spurious
/// delivery is countable instead of landing on some unrelated handler.
pub(crate) const VEC_SPURIOUS: u8 = 0xFF;

pub(crate) const TIMER_HZ: u64 = 100;
const PIT_HZ: u64 = 1_193_182;
pub(crate) const CALIBRATE_MS: u64 = 10;

/// Incremented by the timer handler, read by ordinary kernel code. Relaxed is
/// enough: nothing is published alongside it, and the handler runs on the same
/// CPU as the reader, so the only thing being protected is the increment itself.
static TICKS: AtomicU64 = AtomicU64::new(0);
/// Spurious APIC deliveries. Expected to stay zero; counted rather than ignored
/// so that "zero" is an observation instead of an assumption.
static SPURIOUS: AtomicU64 = AtomicU64::new(0);

fn lapic_read(reg: usize) -> u32 {
    unsafe { core::ptr::read_volatile((LAPIC_BASE + reg) as *const u32) }
}
fn lapic_write(reg: usize, val: u32) {
    unsafe { core::ptr::write_volatile((LAPIC_BASE + reg) as *mut u32, val) };
}

unsafe fn rdmsr(msr: u32) -> u64 {
    unsafe {
        let (lo, hi): (u32, u32);
        asm!("rdmsr", in("ecx") msr, out("eax") lo, out("edx") hi, options(nomem, nostack));
        ((hi as u64) << 32) | lo as u64
    }
}
unsafe fn wrmsr(msr: u32, val: u64) {
    unsafe {
        asm!(
            "wrmsr",
            in("ecx") msr,
            in("eax") val as u32,
            in("edx") (val >> 32) as u32,
            options(nomem, nostack),
        );
    }
}

pub(crate) fn sti() {
    unsafe { asm!("sti", options(nomem, nostack)) };
}
pub(crate) fn cli() {
    unsafe { asm!("cli", options(nomem, nostack)) };
}

pub(crate) fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}
pub(crate) fn spurious() -> u64 {
    SPURIOUS.load(Ordering::Relaxed)
}

/// Called from the interrupt dispatcher, on the path that returns.
pub(crate) fn on_tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
    lapic_write(LAPIC_EOI, 0);
}

/// A spurious vector is the APIC withdrawing an interrupt it had already started
/// to deliver. It takes no EOI — sending one would acknowledge an interrupt that
/// is still in service.
pub(crate) fn on_spurious() {
    SPURIOUS.fetch_add(1, Ordering::Relaxed);
}

/// The Local APIC's own id, which is only readable once the window is mapped and
/// the APIC is on — so a sane value here is evidence both of those happened.
pub(crate) fn lapic_id() -> u32 {
    lapic_read(LAPIC_ID) >> 24
}

/// Turns the Local APIC on. Returns false if firmware relocated the register
/// window away from the address the trampoline mapped, in which case none of the
/// register accesses here would mean anything and the caller must not proceed.
pub(crate) fn lapic_enable() -> bool {
    let base = unsafe { rdmsr(IA32_APIC_BASE) };
    if (base & 0xFFFF_F000) as usize != LAPIC_BASE {
        return false;
    }
    unsafe { wrmsr(IA32_APIC_BASE, base | APIC_BASE_GLOBAL_ENABLE) };
    lapic_write(LAPIC_TPR, 0); // accept every priority; firmware may have raised it
    lapic_write(LAPIC_SVR, SVR_ENABLE | VEC_SPURIOUS as u32);
    true
}

/// Counts how far the LAPIC timer gets while the PIT measures `CALIBRATE_MS`.
///
/// PIT channel 2 is used because it is the one channel wired to no interrupt
/// line and whose output is readable from port 0x61 bit 5 — it can be timed
/// against without an IRQ path existing yet. Returns `None` rather than
/// spinning forever on a machine that has no PIT.
pub(crate) fn calibrate() -> Option<u32> {
    let reload = (PIT_HZ * CALIBRATE_MS / 1000) as u16;
    unsafe {
        let p61 = inb(0x61);
        outb(0x61, (p61 & 0xFD) | 0x01); // speaker off, gate on
        outb(0x43, 0xB2); // channel 2, lo/hi byte, mode 1 one-shot, binary
        outb(0x42, reload as u8);
        outb(0x80, 0); // settle between the two halves of the reload value
        outb(0x42, (reload >> 8) as u8);

        // Dropping the gate and raising it retriggers the one-shot: t = 0 is
        // here, and channel 2's output stays low until the count runs out.
        let gate = inb(0x61) & 0xFE;
        outb(0x61, gate);
        outb(0x61, gate | 1);

        lapic_write(LAPIC_TIMER_DIV, LAPIC_DIV_16);
        lapic_write(LAPIC_LVT_TIMER, LVT_MASKED); // count, but deliver nothing
        lapic_write(LAPIC_TIMER_INIT, u32::MAX);

        let mut spins: u64 = 0;
        while inb(0x61) & 0x20 == 0 {
            spins += 1;
            if spins > 200_000_000 {
                lapic_write(LAPIC_TIMER_INIT, 0);
                return None;
            }
        }
        let remaining = lapic_read(LAPIC_TIMER_CUR);
        lapic_write(LAPIC_TIMER_INIT, 0);
        match u32::MAX - remaining {
            0 => None,
            counted => Some(counted),
        }
    }
}

/// The last count `arm_periodic` was given, so a later caller can restart the
/// timer at the same measured rate without calibrating a second time.
static INITIAL: AtomicU32 = AtomicU32::new(0);

pub(crate) fn arm_periodic(initial: u32) {
    INITIAL.store(initial, Ordering::Relaxed);
    lapic_write(LAPIC_TIMER_DIV, LAPIC_DIV_16);
    lapic_write(LAPIC_LVT_TIMER, LVT_PERIODIC | VEC_TIMER as u32);
    lapic_write(LAPIC_TIMER_INIT, initial);
}

/// Restarts the timer at the rate already measured. Returns false if it was
/// never armed, in which case there is no measured rate to restart at.
pub(crate) fn rearm() -> bool {
    match INITIAL.load(Ordering::Relaxed) {
        0 => false,
        initial => {
            arm_periodic(initial);
            true
        }
    }
}

pub(crate) fn mask() {
    lapic_write(LAPIC_LVT_TIMER, LVT_MASKED);
    lapic_write(LAPIC_TIMER_INIT, 0);
}
