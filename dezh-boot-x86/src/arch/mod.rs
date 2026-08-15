//! The x86_64 hardware layer: the part that must be written per ISA.

pub(crate) mod boot;
pub(crate) mod gdt;
pub(crate) mod pic;
pub(crate) mod timer;
pub(crate) mod trap;
