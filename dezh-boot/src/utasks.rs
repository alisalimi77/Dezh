//! The U-mode task bodies.
//!
//! Every function here runs in user mode with no ambient authority, reaching
//! the kernel only through `ecall`. They are linked into `.user.text` and must
//! stay free of anything that would emit a call into kernel text - no buffers,
//! no compiler-generated memcpy, scalars only.
//!
//! The Cairn service and its client, the preemption pair, the isolation
//! victim and forger, the IPC workers, the benchmark tasks and the Linux-ABI
//! app. They were scattered across six banners in main.rs; what makes them one
//! module is the link section, which is a hard constraint rather than a
//! taxonomy.

use core::arch::asm;

use crate::abi::*;
use crate::sched::{LINUX_EXIT, LINUX_WRITE};
use crate::{
    sys_exit, sys_print, SYS_DENIED, SYS_NULL, SYS_PRINTNUM, SYS_RECV, SYS_RECV_TIMEOUT,
    SYS_PRINT, SYS_REPORT, SYS_SEND, SYS_YIELD, TASK_PRINT,
};

#[link_section = ".user.text"]
#[inline(never)]
pub(crate) fn sys_yield() {
    unsafe { asm!("ecall", in("a7") SYS_YIELD, lateout("a0") _, lateout("a1") _) };
}

#[link_section = ".user.text"]
#[no_mangle]
pub(crate) extern "C" fn worker_a() -> ! {
    sys_print(b"    [task A] step 1\n");
    sys_yield();
    sys_print(b"    [task A] step 2\n");
    sys_yield();
    sys_print(b"    [task A] finished\n");
    sys_exit(0)
}

#[link_section = ".user.text"]
#[no_mangle]
pub(crate) extern "C" fn worker_b() -> ! {
    sys_print(b"    [task B] step 1\n");
    sys_yield();
    sys_print(b"    [task B] step 2\n");
    sys_yield();
    sys_print(b"    [task B] finished\n");
    sys_exit(0)
}

#[link_section = ".user.text"]
#[no_mangle]
pub(crate) extern "C" fn worker_c() -> ! {
    sys_print(b"    [task C] step 1\n");
    sys_yield();
    sys_print(b"    [task C] step 2\n");
    sys_yield();
    sys_print(b"    [task C] finished\n");
    sys_exit(0)
}

// --- Cairn-style store as a user-space service, reached over IPC. -------------
// The agent never touches the store's memory; it sends requests and the service
// replies, all via capability-mediated IPC. The store keeps a current value and
// one previous value, so an action can be *rolled back* — the agent-OS
// differentiator (rollbackable actions, D013/D004), now on the kernel. (v0:
// 1-deep history, ≤63-byte values; full content-addressing/provenance is the
// dezh-cairn crate.)
pub(crate) const OP_SET: usize = 0;
pub(crate) const OP_GET: usize = 1;
pub(crate) const OP_ROLLBACK: usize = 2;
pub(crate) const OP_STOP: usize = 3;

// Value-IPC: pass a request as a single register word, encoded (op << 32 | value).
// No buffers means no compiler-emitted memcpy/memset — which a U-mode task cannot
// call (those live in kernel text). Everything here is scalar.
#[inline(always)]
pub(crate) fn enc(op: usize, val: usize) -> usize {
    (op << 32) | (val & 0xFFFF_FFFF)
}

#[link_section = ".user.text"]
#[inline(never)]
pub(crate) fn vsend(to: usize, word: usize) {
    unsafe {
        asm!("ecall", inout("a0") to => _, in("a1") 0usize, in("a2") 0usize, in("a3") 0usize, in("a4") word, in("a7") SYS_SEND)
    };
}

#[link_section = ".user.text"]
#[inline(never)]
pub(crate) fn vrecv() -> (usize, usize) {
    let word: usize;
    let from: usize;
    unsafe {
        asm!("ecall", inout("a0") 0usize => _, inout("a1") 0usize => from, out("a2") word, lateout("a3") _, in("a7") SYS_RECV)
    };
    (word, from)
}

#[link_section = ".user.text"]
#[inline(never)]
pub(crate) fn vrecv_timeout(timeout_ticks: usize) -> (usize, usize, usize) {
    let rc: usize;
    let from: usize;
    let word: usize;
    unsafe {
        asm!(
            "ecall",
            inout("a0") 0usize => rc,
            inout("a1") 0usize => from,
            inout("a2") timeout_ticks => word,
            lateout("a3") _,
            in("a7") SYS_RECV_TIMEOUT
        )
    };
    (rc, from, word)
}

#[link_section = ".user.text"]
#[inline(always)]
pub(crate) fn utyped_word(service: usize, op: usize, request_id: usize, status: usize, arg: usize) -> usize {
    (IPC_PROTO_V1 << 56)
        | ((service & 0xff) << 48)
        | ((op & 0xff) << 40)
        | ((request_id & 0xffff) << 24)
        | ((status & 0xff) << 16)
        | (arg & 0xffff)
}

#[link_section = ".user.text"]
#[inline(always)]
pub(crate) fn utyped_op(word: usize) -> usize {
    (word >> 40) & 0xff
}

#[link_section = ".user.text"]
#[inline(always)]
pub(crate) fn utyped_status(word: usize) -> usize {
    (word >> 16) & 0xff
}

#[link_section = ".user.text"]
#[inline(never)]
pub(crate) fn sys_printnum(v: usize) {
    unsafe { asm!("ecall", inout("a0") v => _, in("a7") SYS_PRINTNUM) };
}

#[link_section = ".user.text"]
#[no_mangle]
pub(crate) extern "C" fn cairn_service() -> ! {
    let mut cur: usize = 0;
    let mut prev: usize = 0;
    loop {
        let (word, from) = vrecv();
        let op = word >> 32;
        let val = word & 0xFFFF_FFFF;
        if op == OP_SET {
            prev = cur; // keep one step of history so the action is rollbackable
            cur = val;
            vsend(from, 0);
        } else if op == OP_GET {
            vsend(from, cur);
        } else if op == OP_ROLLBACK {
            cur = prev;
            vsend(from, 0);
        } else {
            vsend(from, 0);
            sys_exit(0);
        }
    }
}

#[link_section = ".user.text"]
#[no_mangle]
pub(crate) extern "C" fn agent_cairn() -> ! {
    let svc = 0usize; // the Cairn store service is task 0

    sys_print(b"    [agent] set value to 100\n");
    vsend(svc, enc(OP_SET, 100));
    vrecv();

    sys_print(b"    [agent] set value to 999 (a bad edit)\n");
    vsend(svc, enc(OP_SET, 999));
    vrecv();

    vsend(svc, enc(OP_GET, 0));
    let (v, _) = vrecv();
    sys_print(b"    [agent] get -> ");
    sys_printnum(v);

    sys_print(b"    [agent] rolling back the bad edit\n");
    vsend(svc, enc(OP_ROLLBACK, 0));
    vrecv();

    vsend(svc, enc(OP_GET, 0));
    let (v2, _) = vrecv();
    sys_print(b"    [agent] get -> ");
    sys_printnum(v2);
    sys_print(b"    [agent] (value restored by rollback) done\n");

    vsend(svc, enc(OP_STOP, 0));
    vrecv();
    sys_exit(0)
}

// --- Preemption demo: CPU-bound tasks that never yield still interleave. ------
// With cooperative scheduling, "A start, A end, B start, B end" (A hogs the CPU).
// With timer preemption, "B start" appears before "A end" — the timer forces a
// switch mid-loop, so one task can no longer monopolize the CPU (the safety
// property needed before running untrusted agents).
#[link_section = ".user.text"]
#[inline(never)]
pub(crate) fn busy(n: usize) {
    let mut i = 0usize;
    while i < n {
        unsafe { asm!("nop") };
        i += 1;
    }
}

#[link_section = ".user.text"]
#[no_mangle]
pub(crate) extern "C" fn preempt_a() -> ! {
    sys_print(b"    [A] start (busy loop, never yields)\n");
    busy(8_000_000);
    sys_print(b"    [A] end\n");
    sys_exit(0)
}

#[link_section = ".user.text"]
#[no_mangle]
pub(crate) extern "C" fn preempt_b() -> ! {
    sys_print(b"    [B] start (busy loop, never yields)\n");
    busy(8_000_000);
    sys_print(b"    [B] end\n");
    sys_exit(0)
}

// --- Isolation demo: one task cannot read another task's private memory. ------
// task0 (victim) owns its stack region; task1 (spy) tries to read it directly.
// While the spy runs, the victim's region is U=0, so the load page-faults and the
// kernel kills only the spy — inter-task no-ambient-authority at the hardware
// memory boundary, which is what makes the IPC layer the *only* way to share.
#[link_section = ".user.text"]
#[no_mangle]
pub(crate) extern "C" fn victim_task() -> ! {
    sys_print(b"    [task0] my stack is private; only I can touch my region\n");
    sys_yield(); // let the spy try
    sys_print(b"    [task0] still alive after the spy was killed\n");
    sys_exit(0)
}

// A zero-authority task that tries to WIELD a capability it was never granted:
// it calls the privileged PRINT syscall directly. There is no ambient authority
// to inherit and no way to forge or amplify a capability, so the kernel denies
// the syscall at the capability check and the task prints nothing.
#[link_section = ".user.text"]
#[no_mangle]
pub(crate) extern "C" fn forge_task() -> ! {
    let msg = b"    [forge] (BUG) I printed without holding the PRINT capability!\n";
    sys_write(msg.as_ptr(), msg.len());
    sys_exit(0)
}

#[link_section = ".user.text"]
#[no_mangle]
pub(crate) extern "C" fn spy_task() -> ! {
    // Read straight into task0's stack region (base = stack_base(); see the
    // kernel log). It is U=0 while we run, so this load faults and we are killed.
    let v: u64;
    unsafe { asm!("ld {0}, 0({1})", out(reg) v, in(reg) 0x8060_0800usize) };
    let _ = v;
    let msg = b"    [spy] (BUG) I read another task's memory!\n";
    sys_write(msg.as_ptr(), msg.len());
    sys_exit(0)
}

// --- IPC demo: an agent delegates a capability to a service over a message. ---
// The service starts with NO authority; it cannot print until the agent sends it
// a message that *delegates* the PRINT capability. The kernel enforces that the
// agent can only delegate what it holds (attenuation, never widening) — the
// microkernel keystone for agents calling services and spawning sub-agents.
#[link_section = ".user.text"]
#[inline(never)]
pub(crate) fn sys_send(to: usize, s: &[u8], grant: usize) -> usize {
    let mut a0 = to;
    unsafe {
        asm!("ecall", inout("a0") a0, in("a1") s.as_ptr() as usize, in("a2") s.len(), in("a3") grant, in("a7") SYS_SEND)
    };
    a0
}

#[link_section = ".user.text"]
#[inline(never)]
pub(crate) fn sys_recv(buf: &mut [u8]) -> usize {
    let mut a0 = buf.as_mut_ptr() as usize;
    unsafe {
        asm!("ecall", inout("a0") a0, in("a1") buf.len(), lateout("a2") _, lateout("a3") _, in("a7") SYS_RECV)
    };
    a0 // bytes received
}

// Raw write wrapper: takes ptr+len so user code never calls a (non-inlined,
// kernel-resident) core slicing helper — which a U-mode task cannot fetch.
#[link_section = ".user.text"]
#[inline(never)]
pub(crate) fn sys_write(ptr: *const u8, len: usize) -> usize {
    let mut a0 = ptr as usize;
    unsafe { asm!("ecall", inout("a0") a0, in("a1") len, in("a7") SYS_PRINT) };
    a0
}

#[link_section = ".user.text"]
#[no_mangle]
pub(crate) extern "C" fn service_task() -> ! {
    // No authority yet: this print is denied by the kernel.
    sys_print(b"    [service] (pre-IPC) I have no capabilities; this print is denied\n");
    let mut buf = [0u8; 64];
    let n = sys_recv(&mut buf); // blocks until the agent delegates a capability
    sys_print(b"    [service] received a delegated PRINT capability via IPC; now I can print:\n");
    sys_write(buf.as_ptr(), n); // echo the payload (no slice indexing)
    sys_exit(0)
}

#[link_section = ".user.text"]
#[no_mangle]
pub(crate) extern "C" fn agent_task() -> ! {
    sys_print(b"    [agent] delegating my PRINT capability to the service over IPC\n");
    sys_send(
        0,
        b"    [service] <payload delivered with a delegated PRINT cap>\n",
        TASK_PRINT,
    );
    sys_exit(0)
}

#[link_section = ".user.text"]
#[no_mangle]
pub(crate) extern "C" fn typed_ipc_service_task() -> ! {
    let (word1, from1) = vrecv();
    if utyped_op(word1) == IPC_OP_PING {
        vsend(
            from1,
            utyped_word(IPC_SERVICE_SYSTEM, IPC_OP_PING, 1, IPC_STATUS_OK, 0),
        );
    } else {
        vsend(
            from1,
            utyped_word(
                IPC_SERVICE_SYSTEM,
                IPC_OP_BADREQ,
                1,
                IPC_STATUS_BAD_REQUEST,
                0,
            ),
        );
    }

    let (word2, from2) = vrecv();
    let status = if utyped_op(word2) == IPC_OP_PING {
        IPC_STATUS_OK
    } else {
        IPC_STATUS_BAD_REQUEST
    };
    vsend(
        from2,
        utyped_word(IPC_SERVICE_SYSTEM, utyped_op(word2), 2, status, 0),
    );
    sys_exit(0)
}

#[link_section = ".user.text"]
#[no_mangle]
pub(crate) extern "C" fn typed_ipc_client_task() -> ! {
    vsend(
        0,
        utyped_word(IPC_SERVICE_SYSTEM, IPC_OP_PING, 1, IPC_STATUS_OK, 0),
    );
    let (ok, _) = vrecv();
    sys_print(b"    [typed-ipc] PING -> ");
    sys_printnum(utyped_status(ok));

    vsend(
        0,
        utyped_word(IPC_SERVICE_SYSTEM, IPC_OP_BADREQ, 2, IPC_STATUS_OK, 0),
    );
    let (bad, _) = vrecv();
    sys_print(b"    [typed-ipc] BADREQ -> ");
    sys_printnum(utyped_status(bad));
    sys_exit(0)
}

#[link_section = ".user.text"]
#[no_mangle]
pub(crate) extern "C" fn typed_ipc_timeout_task() -> ! {
    let (rc, _, word) = vrecv_timeout(0);
    sys_print(b"    [typed-ipc] RECV_TIMEOUT -> ");
    if rc == IPC_STATUS_TIMEOUT {
        sys_printnum(utyped_status(word));
    } else {
        sys_printnum(rc);
    }
    sys_exit(0)
}

#[link_section = ".user.text"]
#[no_mangle]
pub(crate) extern "C" fn typed_ipc_denied_task() -> ! {
    let rc = sys_send(0, b"", 0);
    sys_print(b"    [typed-ipc] no-IPC SEND -> ");
    if rc == SYS_DENIED {
        sys_printnum(IPC_STATUS_DENIED);
    } else {
        sys_printnum(rc);
    }
    sys_exit(0)
}

#[link_section = ".user.text"]
#[no_mangle]
pub(crate) extern "C" fn queue_service_task() -> ! {
    sys_print(b"    [queue-service] delaying receive so two clients enqueue\n");
    sys_yield();
    sys_yield();

    let mut first = [0u8; 64];
    let n1 = sys_recv(&mut first);
    sys_print(b"    [queue-service] recv #1: ");
    sys_write(first.as_ptr(), n1);

    let mut second = [0u8; 64];
    let n2 = sys_recv(&mut second);
    sys_print(b"    [queue-service] recv #2: ");
    sys_write(second.as_ptr(), n2);

    sys_print(b"    [queue-service] FIFO mailbox preserved both client messages\n");
    sys_exit(0)
}

#[link_section = ".user.text"]
#[no_mangle]
pub(crate) extern "C" fn queue_agent_a() -> ! {
    sys_print(b"    [queue-agent-a] enqueue alpha\n");
    sys_send(0, b"alpha\n", 0);
    sys_exit(0)
}

#[link_section = ".user.text"]
#[no_mangle]
pub(crate) extern "C" fn queue_agent_b() -> ! {
    sys_print(b"    [queue-agent-b] enqueue beta\n");
    sys_send(0, b"beta\n", 0);
    sys_exit(0)
}

// --- A Linux-ABI app, run unmodified through the Pol personality layer. -------
// It speaks the real Linux riscv64 syscall ABI (write=64, exit=93). The kernel's
// Pol layer translates each into a capability-checked Dezh action; an
// unsupported syscall returns ENOSYS. The app has zero ambient authority — it
// only reaches the console because it holds the PRINT capability.
#[link_section = ".user.text"]
#[inline(never)]
pub(crate) fn linux_write(fd: usize, s: &[u8]) -> i64 {
    let mut a0 = fd;
    unsafe {
        asm!("ecall", inout("a0") a0, in("a1") s.as_ptr() as usize, in("a2") s.len(), in("a7") LINUX_WRITE)
    };
    a0 as i64
}

#[link_section = ".user.text"]
#[inline(never)]
pub(crate) fn linux_close(fd: usize) -> i64 {
    let mut a0 = fd;
    // 57 = Linux `close`; the Pol layer does not support it -> ENOSYS.
    unsafe { asm!("ecall", inout("a0") a0, in("a7") 57usize) };
    a0 as i64
}

#[link_section = ".user.text"]
#[inline(never)]
pub(crate) fn linux_exit(code: usize) -> ! {
    unsafe { asm!("ecall", in("a0") code, in("a7") LINUX_EXIT, options(noreturn)) }
}

// --- Benchmark task: measure the cost of a syscall (ecall) round trip. -------
// Times N minimal syscalls with the U-mode-readable `time` CSR and reports the
// per-call cost back to the kernel. (Under QEMU this is an emulated figure; see
// BENCH.md for the real-hardware comparison.)
#[link_section = ".user.text"]
#[inline(never)]
pub(crate) fn sys_null() {
    unsafe { asm!("ecall", in("a7") SYS_NULL, lateout("a0") _, lateout("a1") _) };
}

#[link_section = ".user.text"]
#[inline(never)]
pub(crate) fn rdtime_u() -> usize {
    let t: usize;
    unsafe { asm!("rdtime {}", out(reg) t) };
    t
}

#[link_section = ".user.text"]
#[inline(never)]
pub(crate) fn sys_report(ticks: usize, iters: usize) {
    unsafe { asm!("ecall", inout("a0") ticks => _, in("a1") iters, in("a7") SYS_REPORT) };
}

#[link_section = ".user.text"]
#[no_mangle]
pub(crate) extern "C" fn bench_task() -> ! {
    let n: usize = 500_000;
    let t0 = rdtime_u();
    let mut i = 0;
    while i < n {
        sys_null();
        i += 1;
    }
    let t1 = rdtime_u();
    sys_report(t1.wrapping_sub(t0), n);
    sys_exit(0)
}

// --- Pol translation-overhead benchmark --------------------------------------
// Two U-mode tasks doing the SAME zero-work syscall the same number of times:
// one via the native Dezh `SYS_PRINT` path, one via the Linux `write` ABI routed
// through the Pol personality layer. Both pass a zero-length buffer, so neither
// touches the UART; the only difference on the kernel side is the personality
// branch + Linux-ABI decode. The kernel times each run and reports the delta as
// the per-syscall translation overhead. (QEMU-emulated; the delta is the honest
// number for F4 — see BENCH.md.)
pub(crate) const BENCH_POL_ITERS: usize = 200_000;

#[link_section = ".user.text"]
#[no_mangle]
pub(crate) extern "C" fn bench_native_print_task() -> ! {
    let mut i = 0;
    while i < BENCH_POL_ITERS {
        sys_print(b""); // native SYS_PRINT, zero-length: cap-checked, no output
        i += 1;
    }
    sys_exit(0)
}

#[link_section = ".user.text"]
#[no_mangle]
pub(crate) extern "C" fn bench_pol_write_task() -> ! {
    let mut i = 0;
    while i < BENCH_POL_ITERS {
        linux_write(1, b""); // Linux write(2) ABI, zero-length: serviced by Pol
        i += 1;
    }
    linux_exit(0)
}

#[link_section = ".user.text"]
#[no_mangle]
pub(crate) extern "C" fn linux_app() -> ! {
    linux_write(
        1,
        b"    [linux] hello from a Linux-ABI app, serviced by Pol\n",
    );
    let r = linux_close(3);
    if r == -38 {
        linux_write(
            1,
            b"    [linux] close(3) returned ENOSYS -> unsupported syscall, denied cleanly\n",
        );
    }
    linux_exit(0)
}
