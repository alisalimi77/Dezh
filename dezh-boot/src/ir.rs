//! Dezh-IR: a tiny capability-gated bytecode interpreter (agent-runtime v0).
//!
//! The long-term agent substrate is a typed, verifiable IR (D003/D016) so the
//! same agent program runs on any ISA and is sandboxed. This is a minimal v0: a
//! stack machine the KERNEL interprets (portable, not native), where the
//! side-effecting opcode (print) is gated by a capability. A full wasm
//! interpreter (e.g. wasmi, no_std) is the next step.

use crate::kprintln;
use core::fmt::Write;

const IR_HALT: u8 = 0;
const IR_PUSH: u8 = 1; // followed by an i64 little-endian immediate
const IR_ADD: u8 = 2;
const IR_PRINT: u8 = 3; // pop and print — requires the PRINT capability

/// A sample agent program: push 2, push 3, add, print, halt  (=> 5).
pub const AGENT_IR: &[u8] = &[
    IR_PUSH, 2, 0, 0, 0, 0, 0, 0, 0, // push 2
    IR_PUSH, 3, 0, 0, 0, 0, 0, 0, 0, // push 3
    IR_ADD, IR_PRINT, IR_HALT,
];

/// Interpret a Dezh-IR program. `can_print` is the PRINT capability.
pub fn run_ir(prog: &[u8], can_print: bool) {
    let mut stack = [0i64; 32];
    let mut sp = 0usize;
    let mut pc = 0usize;
    while pc < prog.len() {
        match prog[pc] {
            IR_HALT => {
                kprintln!("  [ir] halt");
                return;
            }
            IR_PUSH => {
                let mut b = [0u8; 8];
                let mut i = 0;
                while i < 8 {
                    b[i] = prog[pc + 1 + i];
                    i += 1;
                }
                stack[sp] = i64::from_le_bytes(b);
                sp += 1;
                pc += 9;
            }
            IR_ADD => {
                sp -= 1;
                let b = stack[sp];
                stack[sp - 1] += b;
                pc += 1;
            }
            IR_PRINT => {
                sp -= 1;
                let v = stack[sp];
                if can_print {
                    kprintln!("  [ir] print -> {v}");
                } else {
                    kprintln!("  [ir] DENIED print: agent holds no PRINT capability");
                }
                pc += 1;
            }
            other => {
                kprintln!("  [ir] bad opcode {other}");
                return;
            }
        }
    }
}
