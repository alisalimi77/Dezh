//! Dezh-IR: a small, verifiable, capability-gated stack machine (agent runtime).
//!
//! This is Dezh's own agent execution substrate (D003/D016): a portable bytecode
//! the KERNEL interprets, instead of running native code or embedding a large
//! external engine in the trusted core. Three properties matter:
//!   * **sandboxed** — the program only sees its own operand stack and a small
//!     linear memory; every memory access is bounds-checked.
//!   * **verifiable** — `verify` rejects a malformed program (unknown opcode,
//!     truncated immediate, a branch/call target that isn't an instruction
//!     boundary) BEFORE it runs, so traps during execution are about data, not
//!     a broken program.
//!   * **no ambient authority** — the only way to affect the outside world is a
//!     host call, and every host call is gated by an explicit capability bit.
//!
//! A real wasm frontend can later compile to this IR; keeping the in-kernel
//! interpreter tiny keeps the trusted core small and reviewable.

use crate::{blk, kprintln};
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write;

// Opcodes ---------------------------------------------------------------------
const HALT: u8 = 0x00;
const PUSH: u8 = 0x01; // + i64 little-endian immediate
const POP: u8 = 0x02;
const DUP: u8 = 0x03;
const SWAP: u8 = 0x04;
const ADD: u8 = 0x10;
const SUB: u8 = 0x11;
const MUL: u8 = 0x12;
const DIV: u8 = 0x13;
const MOD: u8 = 0x14;
const LT: u8 = 0x18; // pop b, a; push (a < b) as 1/0
const GT: u8 = 0x19;
const EQ: u8 = 0x1a;
const LOAD8: u8 = 0x20; // pop addr; push mem[addr]
const STORE8: u8 = 0x21; // pop val; pop addr; mem[addr] = val
const LOAD64: u8 = 0x22; // pop addr; push i64 LE at mem[addr..]
const STORE64: u8 = 0x23; // pop val; pop addr; write i64 LE
const JMP: u8 = 0x30; // + u16 LE target
const JZ: u8 = 0x31; // pop; if 0 jump
const JNZ: u8 = 0x32; // pop; if != 0 jump
const CALL: u8 = 0x40; // + u16 LE target; push return addr
const RET: u8 = 0x41;
const HOSTCALL: u8 = 0x50; // + u8 function id (capability-gated)

// Host-call ids + the capability each one requires.
const HC_PRINT_NUM: u8 = 0; // CAP_PRINT
const HC_PRINT_STR: u8 = 1; // CAP_PRINT
const HC_CAIRN_PUT: u8 = 2; // CAP_WRITE
const HC_CAIRN_GET: u8 = 3; // CAP_READ

pub const CAP_PRINT: u32 = 1;
pub const CAP_WRITE: u32 = 2;
pub const CAP_READ: u32 = 4;

const MEM_SIZE: usize = 256;
const STACK_SIZE: usize = 64;
const CALL_SIZE: usize = 16;

/// Size in bytes of the instruction starting with `op` (immediate included),
/// or `None` if the opcode is unknown.
fn op_size(op: u8) -> Option<usize> {
    Some(match op {
        PUSH => 9,
        JMP | JZ | JNZ | CALL => 3,
        HOSTCALL => 2,
        HALT | POP | DUP | SWAP | ADD | SUB | MUL | DIV | MOD | LT | GT | EQ | LOAD8 | STORE8
        | LOAD64 | STORE64 | RET => 1,
        _ => return None,
    })
}

#[derive(Clone, Copy)]
pub enum Trap {
    StackOverflow,
    StackUnderflow,
    CallOverflow,
    CallUnderflow,
    BadOpcode,
    MemOob,
    DivZero,
    BadTarget,
    Truncated,
    MissingCapability,
    NoDisk,
}

impl Trap {
    pub fn msg(self) -> &'static str {
        match self {
            Trap::StackOverflow => "operand stack overflow",
            Trap::StackUnderflow => "operand stack underflow",
            Trap::CallOverflow => "call stack overflow",
            Trap::CallUnderflow => "ret with empty call stack",
            Trap::BadOpcode => "unknown opcode",
            Trap::MemOob => "memory access out of bounds",
            Trap::DivZero => "division by zero",
            Trap::BadTarget => "branch/call target is not an instruction boundary",
            Trap::Truncated => "truncated instruction",
            Trap::MissingCapability => "missing required capability for this host call",
            Trap::NoDisk => "no disk for the Cairn host call",
        }
    }
}

/// Static verifier: reject malformed programs before they ever run.
fn verify(code: &[u8]) -> Result<(), Trap> {
    // Pass 1: walk instructions, recording valid start offsets.
    let mut starts = vec![false; code.len() + 1];
    let mut pc = 0usize;
    while pc < code.len() {
        starts[pc] = true;
        let size = op_size(code[pc]).ok_or(Trap::BadOpcode)?;
        if pc + size > code.len() {
            return Err(Trap::Truncated);
        }
        pc += size;
    }
    starts[code.len()] = true; // falling off the end is a valid target boundary

    // Pass 2: every branch/call target must land on an instruction boundary.
    let mut pc = 0usize;
    while pc < code.len() {
        let op = code[pc];
        if matches!(op, JMP | JZ | JNZ | CALL) {
            let t = u16::from_le_bytes([code[pc + 1], code[pc + 2]]) as usize;
            if t >= code.len() || !starts[t] {
                return Err(Trap::BadTarget);
            }
        }
        pc += op_size(op).unwrap();
    }
    Ok(())
}

struct Vm {
    stack: [i64; STACK_SIZE],
    sp: usize,
    calls: [usize; CALL_SIZE],
    csp: usize,
    mem: [u8; MEM_SIZE],
}

impl Vm {
    fn push(&mut self, v: i64) -> Result<(), Trap> {
        if self.sp >= STACK_SIZE {
            return Err(Trap::StackOverflow);
        }
        self.stack[self.sp] = v;
        self.sp += 1;
        Ok(())
    }
    fn pop(&mut self) -> Result<i64, Trap> {
        if self.sp == 0 {
            return Err(Trap::StackUnderflow);
        }
        self.sp -= 1;
        Ok(self.stack[self.sp])
    }
    fn range(&self, addr: i64, len: usize) -> Result<(usize, usize), Trap> {
        let a = addr as usize;
        if addr < 0 || a + len > MEM_SIZE {
            return Err(Trap::MemOob);
        }
        Ok((a, a + len))
    }
}

/// Verify then interpret a Dezh-IR program with the given capability set.
pub fn run(code: &[u8], caps: u32) -> Result<(), Trap> {
    verify(code)?;
    let mut vm = Vm {
        stack: [0; STACK_SIZE],
        sp: 0,
        calls: [0; CALL_SIZE],
        csp: 0,
        mem: [0; MEM_SIZE],
    };
    let mut pc = 0usize;
    loop {
        let op = code[pc];
        pc += 1;
        match op {
            HALT => return Ok(()),
            PUSH => {
                let mut b = [0u8; 8];
                b.copy_from_slice(&code[pc..pc + 8]);
                vm.push(i64::from_le_bytes(b))?;
                pc += 8;
            }
            POP => {
                vm.pop()?;
            }
            DUP => {
                let v = vm.pop()?;
                vm.push(v)?;
                vm.push(v)?;
            }
            SWAP => {
                let a = vm.pop()?;
                let b = vm.pop()?;
                vm.push(a)?;
                vm.push(b)?;
            }
            ADD | SUB | MUL | DIV | MOD => {
                let b = vm.pop()?;
                let a = vm.pop()?;
                let r = match op {
                    ADD => a.wrapping_add(b),
                    SUB => a.wrapping_sub(b),
                    MUL => a.wrapping_mul(b),
                    DIV => {
                        if b == 0 {
                            return Err(Trap::DivZero);
                        }
                        a / b
                    }
                    _ => {
                        if b == 0 {
                            return Err(Trap::DivZero);
                        }
                        a % b
                    }
                };
                vm.push(r)?;
            }
            LT | GT | EQ => {
                let b = vm.pop()?;
                let a = vm.pop()?;
                let r = match op {
                    LT => a < b,
                    GT => a > b,
                    _ => a == b,
                };
                vm.push(r as i64)?;
            }
            LOAD8 => {
                let addr = vm.pop()?;
                let (a, _) = vm.range(addr, 1)?;
                vm.push(vm.mem[a] as i64)?;
            }
            STORE8 => {
                let val = vm.pop()?;
                let addr = vm.pop()?;
                let (a, _) = vm.range(addr, 1)?;
                vm.mem[a] = val as u8;
            }
            LOAD64 => {
                let addr = vm.pop()?;
                let (a, e) = vm.range(addr, 8)?;
                let mut b = [0u8; 8];
                b.copy_from_slice(&vm.mem[a..e]);
                vm.push(i64::from_le_bytes(b))?;
            }
            STORE64 => {
                let val = vm.pop()?;
                let addr = vm.pop()?;
                let (a, e) = vm.range(addr, 8)?;
                vm.mem[a..e].copy_from_slice(&val.to_le_bytes());
            }
            JMP => pc = u16::from_le_bytes([code[pc], code[pc + 1]]) as usize,
            JZ => {
                let t = u16::from_le_bytes([code[pc], code[pc + 1]]) as usize;
                pc += 2;
                if vm.pop()? == 0 {
                    pc = t;
                }
            }
            JNZ => {
                let t = u16::from_le_bytes([code[pc], code[pc + 1]]) as usize;
                pc += 2;
                if vm.pop()? != 0 {
                    pc = t;
                }
            }
            CALL => {
                let t = u16::from_le_bytes([code[pc], code[pc + 1]]) as usize;
                pc += 2;
                if vm.csp >= CALL_SIZE {
                    return Err(Trap::CallOverflow);
                }
                vm.calls[vm.csp] = pc;
                vm.csp += 1;
                pc = t;
            }
            RET => {
                if vm.csp == 0 {
                    return Err(Trap::CallUnderflow);
                }
                vm.csp -= 1;
                pc = vm.calls[vm.csp];
            }
            HOSTCALL => {
                let f = code[pc];
                pc += 1;
                hostcall(&mut vm, f, caps)?;
            }
            _ => return Err(Trap::BadOpcode),
        }
    }
}

fn need(caps: u32, c: u32) -> Result<(), Trap> {
    if caps & c == 0 {
        Err(Trap::MissingCapability)
    } else {
        Ok(())
    }
}

fn hostcall(vm: &mut Vm, f: u8, caps: u32) -> Result<(), Trap> {
    match f {
        HC_PRINT_NUM => {
            need(caps, CAP_PRINT)?;
            let v = vm.pop()?;
            kprintln!("  [ir] print -> {v}");
        }
        HC_PRINT_STR => {
            need(caps, CAP_PRINT)?;
            let len = vm.pop()? as usize;
            let addr = vm.pop()?;
            let (a, e) = vm.range(addr, len)?;
            let s = core::str::from_utf8(&vm.mem[a..e]).unwrap_or("<non-utf8>");
            kprintln!("  [ir] {s}");
        }
        HC_CAIRN_PUT => {
            need(caps, CAP_WRITE)?;
            let len = vm.pop()? as usize;
            let addr = vm.pop()?;
            let (a, e) = vm.range(addr, len)?;
            blk::store_set_bytes(&vm.mem[a..e]).ok_or(Trap::NoDisk)?;
            kprintln!("  [ir] cairn_put ok ({len} bytes, persisted)");
        }
        HC_CAIRN_GET => {
            need(caps, CAP_READ)?;
            let max = vm.pop()? as usize;
            let addr = vm.pop()?;
            let (a, e) = vm.range(addr, max)?;
            let n = blk::store_get_into(&mut vm.mem[a..e]).ok_or(Trap::NoDisk)?;
            vm.push(n as i64)?;
        }
        _ => return Err(Trap::BadOpcode),
    }
    Ok(())
}

// --- A tiny assembler + sample programs --------------------------------------

struct Asm {
    code: Vec<u8>,
}

impl Asm {
    fn new() -> Self {
        Asm { code: Vec::new() }
    }
    fn here(&self) -> u16 {
        self.code.len() as u16
    }
    fn push(&mut self, v: i64) {
        self.code.push(PUSH);
        self.code.extend_from_slice(&v.to_le_bytes());
    }
    fn op(&mut self, o: u8) {
        self.code.push(o);
    }
    fn hostcall(&mut self, f: u8) {
        self.code.push(HOSTCALL);
        self.code.push(f);
    }
    fn jmp(&mut self, target: u16) {
        self.code.push(JMP);
        self.code.extend_from_slice(&target.to_le_bytes());
    }
    /// Emit a JZ with a placeholder target; returns the patch offset.
    fn jz_fwd(&mut self) -> usize {
        self.code.push(JZ);
        let at = self.code.len();
        self.code.extend_from_slice(&0u16.to_le_bytes());
        at
    }
    fn patch(&mut self, at: usize, target: u16) {
        let b = target.to_le_bytes();
        self.code[at] = b[0];
        self.code[at + 1] = b[1];
    }
}

/// Sum 1..=5 with a real loop (memory variables + branch), then print it (15).
/// Exercises arithmetic, linear memory, comparison and control flow.
pub fn demo_sum() -> Vec<u8> {
    let mut a = Asm::new();
    // acc@0 = 0
    a.push(0);
    a.push(0);
    a.op(STORE64);
    // i@8 = 1
    a.push(8);
    a.push(1);
    a.op(STORE64);
    let loop_start = a.here();
    // condition: i < 6
    a.push(8);
    a.op(LOAD64);
    a.push(6);
    a.op(LT);
    let jz = a.jz_fwd();
    // acc = acc + i
    a.push(0); // addr of acc (for STORE64)
    a.push(0);
    a.op(LOAD64);
    a.push(8);
    a.op(LOAD64);
    a.op(ADD);
    a.op(STORE64);
    // i = i + 1
    a.push(8); // addr of i
    a.push(8);
    a.op(LOAD64);
    a.push(1);
    a.op(ADD);
    a.op(STORE64);
    a.jmp(loop_start);
    let end = a.here();
    a.patch(jz, end);
    // print acc
    a.push(0);
    a.op(LOAD64);
    a.hostcall(HC_PRINT_NUM);
    a.op(HALT);
    a.code
}

/// Write a string into Cairn and read it back, both via capability-gated host
/// calls — a sandboxed agent doing a durable, persisted action.
pub fn demo_cairn() -> Vec<u8> {
    let mut a = Asm::new();
    let s = b"ir-wrote-this-durably";
    for (i, &byte) in s.iter().enumerate() {
        a.push(i as i64);
        a.push(byte as i64);
        a.op(STORE8);
    }
    // cairn_put(addr=0, len=s.len())
    a.push(0);
    a.push(s.len() as i64);
    a.hostcall(HC_CAIRN_PUT);
    // cairn_get(addr=64, max=64) -> pushes the byte count
    a.push(64);
    a.push(64);
    a.hostcall(HC_CAIRN_GET);
    // print_str(addr=64, len): we have len on top, push addr then SWAP
    a.push(64);
    a.op(SWAP);
    a.hostcall(HC_PRINT_STR);
    a.op(HALT);
    a.code
}
