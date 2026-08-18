//! The PLIC: real device interrupts.
//!
//! Until this existed every device path was a busy-wait, so I/O and compute
//! could never overlap and a task could not block on I/O. The PLIC is what
//! turns polled drivers into event-driven ones: a driver blocks on
//! `sys_irq_wait`, and the external-interrupt handler wakes it.
//!
//! Step 3 recorded that this is NOT a leaf, against how it looked in the module
//! table: `plic_handle` reaches into the scheduler's task table to wake blocked
//! drivers, so it had to follow `sched` rather than lead the other devices.
//! `sched` split in step 16, which is why this could move now.

use core::arch::asm;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::dev::virtio::{VIRTIO_BLK_MMIO_PA, VIRTIO_MMIO_COUNT, VIRTIO_MMIO_STRIDE};
use crate::sched::{MAX_TASKS, TIRQ_WAITING, TSTATE};
use crate::{kprintln, TaskState};

//
// QEMU `virt` layout: hart h has PLIC context 2h (M-mode) and 2h+1 (S-mode);
// per-context enable bits start at +0x2000 with stride 0x80, and per-context
// threshold/claim at +0x20_0000 with stride 0x1000. virtio-mmio slot i raises
// IRQ (1 + i). The boot hart is chosen by the firmware and is NOT always hart 0
// under -smp, so the S-mode context we program is derived from the boot hart id
// at init time rather than hardcoded - otherwise device interrupts get routed to
// a hart that is not running the kernel and every driver blocks forever.
pub(crate) const PLIC_BASE: usize = 0x0c00_0000;
pub(crate) const PLIC_ENABLE_BASE: usize = PLIC_BASE + 0x2000;
pub(crate) const PLIC_ENABLE_STRIDE: usize = 0x80;
pub(crate) const PLIC_CONTEXT_BASE: usize = PLIC_BASE + 0x0020_0000;
pub(crate) const PLIC_CONTEXT_STRIDE: usize = 0x1000;
/// Claim/complete register for the boot hart's S-mode context. Set by plic_init;
/// read by plic_handle. Defaults to context 1 (hart 0) until init runs.
pub(crate) static PLIC_S_CLAIM: AtomicUsize = AtomicUsize::new(PLIC_CONTEXT_BASE + 0x1000 + 4);
pub(crate) const VIRTIO_IRQ_BASE: u32 = 1;
/// UART0 on the QEMU `virt` board. It went unrouted for as long as only the
/// virtio slots were enabled here, which is why the console had to poll for
/// input and lost whatever arrived while it was busy - see `dev::uart`.
pub(crate) const UART_IRQ: u32 = 10;
pub(crate) const SEIE: usize = 1 << 9;
pub(crate) const VR_INTERRUPT_STATUS: usize = 0x060;
pub(crate) const VR_INTERRUPT_ACK: usize = 0x064;
/// Supervisor external interrupt (`scause` code with the interrupt bit set).
pub(crate) const SCAUSE_EXTERNAL: usize = 9;

pub(crate) static EXT_IRQS: AtomicU64 = AtomicU64::new(0);
/// Tasks woken by a device interrupt rather than by spinning.
pub(crate) static IRQ_WAKEUPS: AtomicU64 = AtomicU64::new(0);

/// Route the virtio device interrupts to this hart's S-mode context and unmask
/// external interrupts. Devices assert their own line; the PLIC arbitrates.
pub(crate) fn plic_init(boot_hart: usize) {
    // S-mode context of the boot hart. Under -smp the boot hart is not always 0.
    let ctx = 2 * boot_hart + 1;
    let enable = PLIC_ENABLE_BASE + ctx * PLIC_ENABLE_STRIDE;
    let threshold = PLIC_CONTEXT_BASE + ctx * PLIC_CONTEXT_STRIDE;
    let claim = threshold + 4;
    PLIC_S_CLAIM.store(claim, Ordering::Relaxed);
    unsafe {
        let mut irq = VIRTIO_IRQ_BASE;
        while irq < VIRTIO_IRQ_BASE + VIRTIO_MMIO_COUNT as u32 {
            write_volatile((PLIC_BASE + irq as usize * 4) as *mut u32, 1);
            irq += 1;
        }
        write_volatile((PLIC_BASE + UART_IRQ as usize * 4) as *mut u32, 1);
        let mask: u32 =
            (((1u32 << VIRTIO_MMIO_COUNT) - 1) << VIRTIO_IRQ_BASE) | (1u32 << UART_IRQ);
        write_volatile(enable as *mut u32, mask);
        write_volatile(threshold as *mut u32, 0);
        asm!("csrs sie, {}", in(reg) SEIE);
    }
}

/// Claim one external interrupt, ACK the device so it stops asserting its line,
/// then complete it at the PLIC. Skipping the device ACK would re-raise the
/// interrupt immediately and livelock the kernel.
pub(crate) fn plic_handle() -> u32 {
    let claim = PLIC_S_CLAIM.load(Ordering::Relaxed);
    unsafe {
        let irq = read_volatile(claim as *const u32);
        if irq == 0 {
            return 0;
        }
        if irq == UART_IRQ {
            // Emptying the receiver is what deasserts the UART's line, so this
            // is the ACK as much as the read, and it has to happen before the
            // `claim` write below or the PLIC re-raises immediately.
            crate::dev::uart::rx_drain();
        }
        if irq >= VIRTIO_IRQ_BASE && irq < VIRTIO_IRQ_BASE + VIRTIO_MMIO_COUNT as u32 {
            let slot = (irq - VIRTIO_IRQ_BASE) as usize;
            let base = VIRTIO_BLK_MMIO_PA + slot * VIRTIO_MMIO_STRIDE;
            let st = read_volatile((base + VR_INTERRUPT_STATUS) as *const u32);
            if st != 0 {
                write_volatile((base + VR_INTERRUPT_ACK) as *mut u32, st);
            }
        }
        write_volatile(claim as *mut u32, irq);
        EXT_IRQS.fetch_add(1, Ordering::Relaxed);
        // Anyone sleeping on a device becomes runnable again.
        let mut i = 0usize;
        while i < MAX_TASKS {
            if (*TIRQ_WAITING.get())[i] {
                (*TIRQ_WAITING.get())[i] = false;
                if (*TSTATE.get())[i] == TaskState::Blocked {
                    (*TSTATE.get())[i] = TaskState::Ready;
                }
                IRQ_WAKEUPS.fetch_add(1, Ordering::Relaxed);
            }
            i += 1;
        }
        irq
    }
}

pub(crate) fn irq_stat() {
    kprintln!(
        "irq: external device interrupts serviced = {}",
        EXT_IRQS.load(Ordering::Relaxed)
    );
    kprintln!(
        "irq: driver waits woken by a device interrupt (not by spinning) = {}",
        IRQ_WAKEUPS.load(Ordering::Relaxed)
    );
    kprintln!(
        "  source: PLIC S-mode context of the boot hart (claim @ {:#x}); virtio slots raise IRQ 1..8, UART0 raises IRQ {}",
        PLIC_S_CLAIM.load(Ordering::Relaxed),
        UART_IRQ
    );
    kprintln!("  before this, every device wait was a busy-loop; devices can now report completion");
    kprintln!(
        "  console input dropped by a full receive ring = {} (non-zero means input outran the console by more than 256 bytes)",
        crate::dev::uart::RX_OVERRUNS.load(Ordering::Relaxed)
    );
    kprintln!(
        "  console input dropped by the UART itself (LSR.OE) = {} (non-zero means the receiver was not read in time)",
        crate::dev::uart::RX_HW_OVERRUNS.load(Ordering::Relaxed)
    );
    kprintln!(
        "  console input bytes received = {} (short of what was sent, with both drop counts zero, means the bytes never reached the device)",
        crate::dev::uart::RX_BYTES.load(Ordering::Relaxed)
    );
}
