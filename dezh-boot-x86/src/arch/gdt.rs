//! A real GDT, and the TSS that makes leaving ring 0 survivable.
//!
//! The boot trampoline builds the smallest GDT that can enter long mode: a null
//! descriptor and one 64-bit code segment. That is enough for a kernel that
//! never leaves ring 0, which is what this kernel was until now.
//!
//! Running a task at CPL3 needs three more things. Descriptors the CPU will
//! accept in `cs` and `ss` at CPL3, so `iretq` can drop into ring 3 at all. A
//! kernel data descriptor, because a ring-3 `ss` cannot be reused on the way
//! back. And a TSS holding `rsp0`: when an interrupt arrives while the CPU is at
//! CPL3 it switches to the stack named there before pushing anything, so
//! without it a user task's interrupt would be taken on the user task's own
//! stack — which is the stack the user task controls.
//!
//! Index 1 keeps the same kernel-code descriptor the trampoline installed, so
//! `cs` stays valid across the `lgdt` and no far jump is needed to reload it.

use crate::global::Global;
use core::arch::asm;

pub(crate) const KERNEL_CS: u16 = 0x08;
pub(crate) const KERNEL_DS: u16 = 0x10;
/// Selectors handed to a ring-3 task carry RPL 3; the CPU refuses an `iretq`
/// into user mode whose `cs` and `ss` do not.
pub(crate) const USER_DS: u16 = 0x18 | 3;
pub(crate) const USER_CS: u16 = 0x20 | 3;
const TSS_SEL: u16 = 0x28;

/// null, kernel code, kernel data, user data, user code, then the TSS — which is
/// a system descriptor and takes two slots.
const GDT_LEN: usize = 7;

const KERNEL_CODE: u64 = 0x00AF_9A00_0000_FFFF;
const KERNEL_DATA: u64 = 0x00CF_9200_0000_FFFF;
const USER_DATA: u64 = 0x00CF_F200_0000_FFFF;
const USER_CODE: u64 = 0x00AF_FA00_0000_FFFF;

#[repr(C, packed)]
struct Gdtr {
    limit: u16,
    base: u64,
}

/// The 64-bit TSS. Almost all of it is unused here: no ring 1 or 2, no IST
/// stacks yet, and `iomap_base` past the end of the segment so the I/O
/// permission bitmap is absent and every port access from CPL3 faults.
#[repr(C, packed)]
struct Tss {
    reserved0: u32,
    rsp: [u64; 3],
    reserved1: u64,
    ist: [u64; 7],
    reserved2: u64,
    reserved3: u16,
    iomap_base: u16,
}

/// Written once by `init` before any task runs, and thereafter only by
/// `set_kernel_stack` from the scheduler's switch, which runs in the timer
/// interrupt with interrupts off on the one CPU this kernel uses.
static TSS: Global<Tss> = Global::new(Tss {
    reserved0: 0,
    rsp: [0; 3],
    reserved1: 0,
    ist: [0; 7],
    reserved2: 0,
    reserved3: 0,
    iomap_base: core::mem::size_of::<Tss>() as u16,
});

/// Written once by `init`, read by the CPU thereafter.
static GDT: Global<[u64; GDT_LEN]> = Global::new([0; GDT_LEN]);

fn tss_descriptor(base: u64, limit: u32) -> (u64, u64) {
    let low = (limit as u64 & 0xFFFF)
        | ((base & 0x00FF_FFFF) << 16)
        | (0x89 << 40) // present, DPL0, 64-bit available TSS
        | (((limit as u64 >> 16) & 0xF) << 48)
        | (((base >> 24) & 0xFF) << 56);
    (low, base >> 32)
}

pub(crate) fn init() {
    unsafe {
        let tss_base = TSS.get() as u64;
        let (tss_lo, tss_hi) = tss_descriptor(tss_base, core::mem::size_of::<Tss>() as u32 - 1);
        let gdt = GDT.get() as *mut u64;
        core::ptr::write(gdt.add(0), 0);
        core::ptr::write(gdt.add(1), KERNEL_CODE);
        core::ptr::write(gdt.add(2), KERNEL_DATA);
        core::ptr::write(gdt.add(3), USER_DATA);
        core::ptr::write(gdt.add(4), USER_CODE);
        core::ptr::write(gdt.add(5), tss_lo);
        core::ptr::write(gdt.add(6), tss_hi);

        let gdtr = Gdtr {
            limit: (GDT_LEN * 8 - 1) as u16,
            base: gdt as u64,
        };
        asm!("lgdt [{}]", in(reg) &gdtr, options(nostack));

        // `cs` still names the same descriptor it did under the boot GDT, so it
        // needs no reload. The data selectors do get loaded: a ring-3 task will
        // put its own selector in `ss`, and the kernel needs a descriptor of its
        // own to go back to.
        asm!(
            "mov ds, {0:x}",
            "mov es, {0:x}",
            "mov ss, {0:x}",
            in(reg) KERNEL_DS,
            options(nostack, preserves_flags),
        );
        asm!("ltr {0:x}", in(reg) TSS_SEL, options(nostack, preserves_flags));
    }
}

/// Names the stack the CPU switches to when an interrupt arrives while a task is
/// at CPL3. The scheduler sets this to the incoming task's kernel stack on every
/// switch, because the frame the interrupt pushes must land somewhere the task
/// being interrupted cannot write.
pub(crate) fn set_kernel_stack(top: u64) {
    unsafe { (*TSS.get()).rsp[0] = top };
}
