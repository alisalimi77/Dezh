//! Dezh-core — architecture-independent Dezh logic, shared by every ISA kernel.
//!
//! Today this holds the **Dezh-IR engine**: a small, verifiable, capability-gated
//! stack machine that is the portable agent substrate (D003/D016). It depends
//! only on `core`; all side effects go through the [`Host`] trait that each
//! kernel (RISC-V, x86, …) implements. The *same* agent bytecode therefore runs
//! unchanged on any architecture Dezh has been ported to.

#![no_std]

pub mod b64;
pub mod dzp;
pub mod ir;
