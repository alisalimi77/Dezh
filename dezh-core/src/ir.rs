//! Dezh-IR — a small, verifiable, capability-gated stack machine (agent runtime).
//!
//! Dezh's own agent execution substrate (D003/D016): a portable bytecode the
//! kernel interprets, instead of running native code or embedding a large engine
//! in the trusted core. It is **architecture-independent** — it lives in
//! `dezh-core` and runs identically on every ISA Dezh is ported to (RISC-V, x86,
//! …). Four properties matter:
//!   * **portable** — the same bytecode runs on any kernel.
//!   * **sandboxed** — only its own operand stack + a small linear memory; every
//!     access is bounds-checked.
//!   * **verifiable** — [`verify`] rejects malformed programs (unknown opcode,
//!     truncated immediate, a branch/call target that isn't an instruction
//!     boundary) before they run.
//!   * **no ambient authority** — the only way to touch the outside world is a
//!     host call, and every host call is gated by a capability the [`Host`]
//!     holds. The kernel supplies the `Host`; `dezh-core` never touches hardware.
//!
//! `alloc`-free on purpose, so kernels without a heap (e.g. the early x86 port)
//! can run it too.

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
const MAX_PROG: usize = 4096;

/// The kernel-provided boundary: capability checks + the actual side effects.
/// `dezh-core` calls through this and never touches hardware itself.
pub trait Host {
    /// Does the current principal hold the capability bit(s) in `cap`?
    fn can(&self, cap: u32) -> bool;
    /// Emit a value (engine has already checked `CAP_PRINT`).
    fn print_num(&mut self, v: i64);
    /// Emit bytes (engine has already checked `CAP_PRINT`).
    fn print_str(&mut self, s: &[u8]);
    /// Persist bytes to the durable store; `false` if unavailable (no disk).
    fn cairn_put(&mut self, data: &[u8]) -> bool;
    /// Read the durable store into `buf`; returns byte count, or `None`.
    fn cairn_get(&mut self, buf: &mut [u8]) -> Option<usize>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
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
    TooLong,
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
            Trap::TooLong => "program too long",
            Trap::MissingCapability => "missing required capability for this host call",
            Trap::NoDisk => "no disk for the Cairn host call",
        }
    }
}

/// Size in bytes of the instruction starting with `op` (immediate included).
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

/// Static verifier: reject malformed programs before they ever run.
pub fn verify(code: &[u8]) -> Result<(), Trap> {
    if code.len() > MAX_PROG {
        return Err(Trap::TooLong);
    }
    // Pass 1: walk instructions, recording valid start offsets.
    let mut starts = [false; MAX_PROG + 1];
    let mut pc = 0usize;
    while pc < code.len() {
        starts[pc] = true;
        let size = op_size(code[pc]).ok_or(Trap::BadOpcode)?;
        if pc + size > code.len() {
            return Err(Trap::Truncated);
        }
        pc += size;
    }
    starts[code.len()] = true; // falling off the end is a valid boundary

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

/// Interpret a Dezh-IR program against `host`. The engine is internally guarded
/// (bounds-checked) regardless of verification, but callers should [`verify`]
/// first so a fault means bad data, not a malformed program.
pub fn run(code: &[u8], host: &mut dyn Host) -> Result<(), Trap> {
    let mut vm = Vm {
        stack: [0; STACK_SIZE],
        sp: 0,
        calls: [0; CALL_SIZE],
        csp: 0,
        mem: [0; MEM_SIZE],
    };
    let mut pc = 0usize;
    while pc < code.len() {
        let op = code[pc];
        pc += 1;
        match op {
            HALT => return Ok(()),
            PUSH => {
                if pc + 8 > code.len() {
                    return Err(Trap::Truncated);
                }
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
            JMP => pc = target(code, pc)?,
            JZ => {
                let t = target(code, pc)?;
                pc += 2;
                if vm.pop()? == 0 {
                    pc = t;
                }
            }
            JNZ => {
                let t = target(code, pc)?;
                pc += 2;
                if vm.pop()? != 0 {
                    pc = t;
                }
            }
            CALL => {
                let t = target(code, pc)?;
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
                if pc >= code.len() {
                    return Err(Trap::Truncated);
                }
                let f = code[pc];
                pc += 1;
                hostcall(&mut vm, f, host)?;
            }
            _ => return Err(Trap::BadOpcode),
        }
    }
    Ok(())
}

fn target(code: &[u8], pc: usize) -> Result<usize, Trap> {
    if pc + 2 > code.len() {
        return Err(Trap::Truncated);
    }
    Ok(u16::from_le_bytes([code[pc], code[pc + 1]]) as usize)
}

fn hostcall(vm: &mut Vm, f: u8, host: &mut dyn Host) -> Result<(), Trap> {
    match f {
        HC_PRINT_NUM => {
            if !host.can(CAP_PRINT) {
                return Err(Trap::MissingCapability);
            }
            let v = vm.pop()?;
            host.print_num(v);
        }
        HC_PRINT_STR => {
            if !host.can(CAP_PRINT) {
                return Err(Trap::MissingCapability);
            }
            let len = vm.pop()? as usize;
            let addr = vm.pop()?;
            let (a, e) = vm.range(addr, len)?;
            host.print_str(&vm.mem[a..e]);
        }
        HC_CAIRN_PUT => {
            if !host.can(CAP_WRITE) {
                return Err(Trap::MissingCapability);
            }
            let len = vm.pop()? as usize;
            let addr = vm.pop()?;
            let (a, e) = vm.range(addr, len)?;
            if !host.cairn_put(&vm.mem[a..e]) {
                return Err(Trap::NoDisk);
            }
        }
        HC_CAIRN_GET => {
            if !host.can(CAP_READ) {
                return Err(Trap::MissingCapability);
            }
            let max = vm.pop()? as usize;
            let addr = vm.pop()?;
            let (a, e) = vm.range(addr, max)?;
            let n = host.cairn_get(&mut vm.mem[a..e]).ok_or(Trap::NoDisk)?;
            vm.push(n as i64)?;
        }
        _ => return Err(Trap::BadOpcode),
    }
    Ok(())
}

// --- A tiny assembler (writes into a caller buffer; alloc-free) ---------------

pub struct Asm<'a> {
    buf: &'a mut [u8],
    len: usize,
}

impl<'a> Asm<'a> {
    pub fn new(buf: &'a mut [u8]) -> Self {
        Asm { buf, len: 0 }
    }
    pub fn here(&self) -> u16 {
        self.len as u16
    }
    fn put(&mut self, b: u8) {
        self.buf[self.len] = b;
        self.len += 1;
    }
    pub fn push(&mut self, v: i64) {
        self.put(PUSH);
        for b in v.to_le_bytes() {
            self.put(b);
        }
    }
    pub fn op(&mut self, o: u8) {
        self.put(o);
    }
    pub fn hostcall(&mut self, f: u8) {
        self.put(HOSTCALL);
        self.put(f);
    }
    pub fn jmp(&mut self, target: u16) {
        self.put(JMP);
        let b = target.to_le_bytes();
        self.put(b[0]);
        self.put(b[1]);
    }
    /// Emit a JZ with a placeholder target; returns the patch offset.
    pub fn jz_fwd(&mut self) -> usize {
        self.put(JZ);
        let at = self.len;
        self.put(0);
        self.put(0);
        at
    }
    pub fn patch(&mut self, at: usize, target: u16) {
        let b = target.to_le_bytes();
        self.buf[at] = b[0];
        self.buf[at + 1] = b[1];
    }
    pub fn finish(self) -> &'a [u8] {
        &self.buf[..self.len]
    }
}

/// Sum 1..=5 with a real loop (memory variables + branch), then print it (15).
pub fn demo_sum(buf: &mut [u8]) -> &[u8] {
    let mut a = Asm::new(buf);
    a.push(0);
    a.push(0);
    a.op(STORE64); // acc@0 = 0
    a.push(8);
    a.push(1);
    a.op(STORE64); // i@8 = 1
    let loop_start = a.here();
    a.push(8);
    a.op(LOAD64);
    a.push(6);
    a.op(LT); // i < 6 ?
    let jz = a.jz_fwd();
    a.push(0);
    a.push(0);
    a.op(LOAD64);
    a.push(8);
    a.op(LOAD64);
    a.op(ADD);
    a.op(STORE64); // acc += i
    a.push(8);
    a.push(8);
    a.op(LOAD64);
    a.push(1);
    a.op(ADD);
    a.op(STORE64); // i += 1
    a.jmp(loop_start);
    let end = a.here();
    a.patch(jz, end);
    a.push(0);
    a.op(LOAD64);
    a.hostcall(HC_PRINT_NUM); // print acc => 15
    a.op(HALT);
    a.finish()
}

/// Write a string into Cairn and read it back, both via capability-gated host
/// calls — a sandboxed agent doing a durable, persisted action.
pub fn demo_cairn(buf: &mut [u8]) -> &[u8] {
    let mut a = Asm::new(buf);
    let s = b"ir-wrote-this-durably";
    for (i, &byte) in s.iter().enumerate() {
        a.push(i as i64);
        a.push(byte as i64);
        a.op(STORE8);
    }
    a.push(0);
    a.push(s.len() as i64);
    a.hostcall(HC_CAIRN_PUT); // cairn_put(addr=0, len)
    a.push(64);
    a.push(64);
    a.hostcall(HC_CAIRN_GET); // cairn_get(addr=64, max=64) -> count on stack
    a.push(64);
    a.op(SWAP);
    a.hostcall(HC_PRINT_STR); // print_str(addr=64, len)
    a.op(HALT);
    a.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    struct T {
        caps: u32,
        last_num: i64,
        store: [u8; 64],
        store_len: usize,
        last_str: [u8; 64],
        last_str_len: usize,
    }
    impl Default for T {
        fn default() -> Self {
            T {
                caps: 0,
                last_num: 0,
                store: [0; 64],
                store_len: 0,
                last_str: [0; 64],
                last_str_len: 0,
            }
        }
    }
    impl Host for T {
        fn can(&self, cap: u32) -> bool {
            self.caps & cap != 0
        }
        fn print_num(&mut self, v: i64) {
            self.last_num = v;
        }
        fn print_str(&mut self, s: &[u8]) {
            let n = s.len().min(64);
            self.last_str[..n].copy_from_slice(&s[..n]);
            self.last_str_len = n;
        }
        fn cairn_put(&mut self, data: &[u8]) -> bool {
            let n = data.len().min(64);
            self.store[..n].copy_from_slice(&data[..n]);
            self.store_len = n;
            true
        }
        fn cairn_get(&mut self, buf: &mut [u8]) -> Option<usize> {
            let n = self.store_len.min(buf.len());
            buf[..n].copy_from_slice(&self.store[..n]);
            Some(n)
        }
    }

    #[test]
    fn demo_sum_is_15() {
        let mut buf = [0u8; 256];
        let prog = demo_sum(&mut buf);
        verify(prog).unwrap();
        let mut h = T {
            caps: CAP_PRINT,
            ..Default::default()
        };
        run(prog, &mut h).unwrap();
        assert_eq!(h.last_num, 15);
    }

    #[test]
    fn print_needs_capability() {
        let mut buf = [0u8; 256];
        let prog = demo_sum(&mut buf);
        let mut h = T::default(); // no caps
        assert_eq!(run(prog, &mut h), Err(Trap::MissingCapability));
    }

    #[test]
    fn cairn_roundtrip() {
        let mut buf = [0u8; 512];
        let prog = demo_cairn(&mut buf);
        verify(prog).unwrap();
        let mut h = T {
            caps: CAP_PRINT | CAP_WRITE | CAP_READ,
            ..Default::default()
        };
        run(prog, &mut h).unwrap();
        assert_eq!(&h.last_str[..h.last_str_len], b"ir-wrote-this-durably");
    }

    #[test]
    fn verify_rejects_bad_jump() {
        assert_eq!(verify(&[JMP, 1, 0]), Err(Trap::BadTarget));
    }

    #[test]
    fn verify_rejects_bad_opcode() {
        assert_eq!(verify(&[0xFE]), Err(Trap::BadOpcode));
    }
}
