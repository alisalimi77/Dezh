//! # dezh-host — Step 1: Capability Core Spike
//!
//! This crate validates Dezh's one non-negotiable thesis: **no ambient
//! authority**. A WebAssembly guest starts with ZERO access to anything and can
//! act on a resource ONLY by presenting an unforgeable capability handle that
//! the host explicitly granted it. Capabilities can be *attenuated* (narrowed)
//! but, by construction, never *widened*.
//!
//! WASI is never enabled. The only imports a guest module can resolve are the
//! four capability-gated host functions registered in [`build_linker`]. There is
//! no filesystem, no clock, no network, no `env` — nothing ambient.
//!
//! ## The enforcement boundary (read this part carefully)
//! Everything that matters lives in three places, all heavily commented below:
//!   * [`Capability::derive`] — the only guest-reachable capability constructor.
//!     It computes `parent.ops ∩ requested`, so it is *structurally incapable*
//!     of producing more authority than its parent.
//!   * [`CapTable::check`] — the single choke point every host function calls
//!     before touching any resource.
//!   * The host functions ([`cap_read`] et al.) — each one calls `check` FIRST.
//!
//! Resources are a fake in-memory file table (`resource_id -> Vec<u8>`). No real
//! I/O happens; persistence is deferred to a later step ("Cairn").

use std::collections::HashMap;
use wasmtime::{Caller, Engine, Extern, Linker, Memory, Module, Store};

/// Identifies an entry in the in-memory resource table.
pub type ResourceId = u32;

/// An opaque token a guest holds. It is just an index into that guest's
/// [`CapTable`]; the guest never sees a host pointer and cannot construct a
/// valid token it was not handed (see [`CapTable::check`]).
pub type Handle = u32;

// ---------------------------------------------------------------------------
// Operation set
// ---------------------------------------------------------------------------

/// The set of operations a capability permits, as a bitset.
///
/// The inner `u32` is private: the only ways to obtain an `Ops` value are the
/// named constants and the set-algebra methods below. None of them can invent a
/// bit that was not already present in an input, which is what makes attenuation
/// safe even when the requested ops come straight off the wasm boundary.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ops(u32);

impl Ops {
    pub const READ: Ops = Ops(1 << 0);
    pub const WRITE: Ops = Ops(1 << 1);
    pub const PRINT: Ops = Ops(1 << 2);
    /// The empty authority set. Used as the "just validate the handle" probe.
    pub const NONE: Ops = Ops(0);

    /// Every bit the system currently defines. Anything else is meaningless.
    const ALL_BITS: u32 = Self::READ.0 | Self::WRITE.0 | Self::PRINT.0;

    /// Build an `Ops` from raw bits supplied by a guest, MASKING OFF any
    /// undefined bit. A guest cannot smuggle authority by setting bit 31.
    pub fn from_bits_truncate(raw: u32) -> Ops {
        Ops(raw & Self::ALL_BITS)
    }

    pub fn bits(self) -> u32 {
        self.0
    }

    /// True if `self` permits everything in `other` (superset test).
    pub fn contains(self, other: Ops) -> bool {
        self.0 & other.0 == other.0
    }

    /// Set intersection. The keystone of attenuation: the result can never
    /// contain a bit that was absent from BOTH inputs.
    pub fn intersect(self, other: Ops) -> Ops {
        Ops(self.0 & other.0)
    }

    /// Set union. **Host-side minting only.** Lets the host grant READ+WRITE in
    /// one call. It is never applied to guest-supplied ops, so it cannot widen a
    /// capability from inside a guest.
    pub fn union(self, other: Ops) -> Ops {
        Ops(self.0 | other.0)
    }

    pub fn is_empty(self) -> bool {
        self.0 == 0
    }
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Why a capability operation was denied. Each maps to a stable negative code
/// returned across the wasm boundary, so guests (and tests) can branch on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CapError {
    /// Handle is out of range for this guest's table (forged / made-up).
    NoSuchHandle,
    /// Handle is valid but does not permit the requested operation.
    OpNotPermitted,
    /// Attenuation requested an op the parent did not hold.
    Widening,
    /// Attenuation requested exactly the parent's ops (nothing narrowed).
    NotNarrower,
    /// The capability points at a resource that does not exist.
    NoResource,
    /// The guest supplied an out-of-bounds pointer/length.
    BadMemory,
}

impl CapError {
    /// Stable wire code. Always negative so a single i32/i64 return can carry
    /// "success value (>= 0)" or "error (< 0)".
    pub fn code(self) -> i32 {
        match self {
            CapError::NoSuchHandle => -1,
            CapError::OpNotPermitted => -2,
            CapError::Widening => -3,
            CapError::NotNarrower => -4,
            CapError::NoResource => -5,
            CapError::BadMemory => -6,
        }
    }
}

// ---------------------------------------------------------------------------
// Capability
// ---------------------------------------------------------------------------

/// An unforgeable, host-side handle to authority over one resource.
///
/// Both fields are private. The ONLY two ways to construct a `Capability` are
/// [`Capability::grant`] (host-trusted minting) and [`Capability::derive`]
/// (attenuation). There is deliberately **no** setter, no `with_ops`, no
/// `&mut` accessor — nothing anywhere can raise a capability's authority after
/// it is built. That absence is the proof behind "capabilities can never be
/// widened"; the exhaustive test in this file's `tests` module checks it holds
/// across the entire op space.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capability {
    resource: ResourceId,
    ops: Ops,
}

impl Capability {
    /// Host-trusted minting of fresh authority. Crate-private so it is reachable
    /// only through [`HostState::grant`] in trusted host code — never from a
    /// guest, which can call nothing but the registered host functions.
    pub(crate) fn grant(resource: ResourceId, ops: Ops) -> Capability {
        Capability { resource, ops }
    }

    pub fn resource(&self) -> ResourceId {
        self.resource
    }

    pub fn ops(&self) -> Ops {
        self.ops
    }

    /// Derive an attenuated child capability — the one capability-producing path
    /// reachable (indirectly, via `cap_attenuate`) from guest code.
    ///
    /// ## Why this cannot widen
    /// The returned ops are `self.ops ∩ requested`. An intersection can never
    /// contain a bit absent from `self.ops`, so **no** input — not even
    /// `0xFFFF_FFFF` — yields more authority than the parent. The two `Err`
    /// branches below make illegitimate requests fail *loudly*, but they are
    /// belt-and-suspenders: even if they were deleted, the intersection alone
    /// would keep the child no wider than its parent.
    pub fn derive(&self, requested: Ops) -> Result<Capability, CapError> {
        // Loud rejection: requested contains a bit the parent never held.
        if !self.ops.contains(requested) {
            return Err(CapError::Widening);
        }
        // Loud rejection: nothing was actually dropped, so this is not narrowing.
        if requested == self.ops {
            return Err(CapError::NotNarrower);
        }
        // Structural guarantee: subset by intersection.
        Ok(Capability {
            resource: self.resource,
            ops: self.ops.intersect(requested),
        })
    }
}

// ---------------------------------------------------------------------------
// Per-guest capability table
// ---------------------------------------------------------------------------

/// One guest's capability table. A [`Handle`] is simply an index into `slots`.
///
/// Unforgeability: an out-of-range handle is rejected ([`CapError::NoSuchHandle`]);
/// an in-range handle can only ever name a capability the host already installed
/// for THIS guest, so guessing a handle cannot manufacture authority the guest
/// did not already hold.
#[derive(Default)]
pub struct CapTable {
    slots: Vec<Capability>,
}

impl CapTable {
    pub fn new() -> Self {
        CapTable { slots: Vec::new() }
    }

    /// Install a capability, returning its handle. Trusted host path
    /// (host minting and `cap_attenuate` both funnel through here).
    pub fn install(&mut self, cap: Capability) -> Handle {
        let h = self.slots.len() as Handle;
        self.slots.push(cap);
        h
    }

    /// THE ENFORCEMENT POINT. Resolve `handle`, then confirm it permits `op`.
    /// Every capability-gated host function calls this before doing anything.
    pub fn check(&self, handle: Handle, op: Ops) -> Result<Capability, CapError> {
        let cap = self
            .slots
            .get(handle as usize)
            .ok_or(CapError::NoSuchHandle)?;
        if !cap.ops.contains(op) {
            return Err(CapError::OpNotPermitted);
        }
        Ok(*cap)
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Resource backend (fake in-memory file table — no real I/O)
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct Resources {
    table: HashMap<ResourceId, Vec<u8>>,
}

impl Resources {
    pub fn new() -> Self {
        Resources {
            table: HashMap::new(),
        }
    }

    pub fn put(&mut self, id: ResourceId, data: Vec<u8>) {
        self.table.insert(id, data);
    }

    pub fn get(&self, id: ResourceId) -> Option<&Vec<u8>> {
        self.table.get(&id)
    }

    pub fn get_mut(&mut self, id: ResourceId) -> Option<&mut Vec<u8>> {
        self.table.get_mut(&id)
    }
}

// ---------------------------------------------------------------------------
// Host state (lives inside the wasmtime Store; one per guest instance)
// ---------------------------------------------------------------------------

pub struct HostState {
    pub resources: Resources,
    pub caps: CapTable,
    /// Bytes the guest emitted through `cap_print`, captured for assertions.
    pub stdout: Vec<u8>,
}

impl Default for HostState {
    fn default() -> Self {
        Self::new()
    }
}

impl HostState {
    pub fn new() -> Self {
        HostState {
            resources: Resources::new(),
            caps: CapTable::new(),
            stdout: Vec::new(),
        }
    }

    /// Host-trusted mint: create a fresh capability for `resource` with `ops`
    /// and install it, returning the handle to hand to the guest. This is the
    /// only public way to bring authority into existence.
    pub fn grant(&mut self, resource: ResourceId, ops: Ops) -> Handle {
        self.caps.install(Capability::grant(resource, ops))
    }
}

// ---------------------------------------------------------------------------
// Capability-gated host functions
// ---------------------------------------------------------------------------

/// Fetch the guest's exported linear memory, or report BadMemory.
fn memory_of(caller: &mut Caller<'_, HostState>) -> Result<Memory, CapError> {
    match caller.get_export("memory") {
        Some(Extern::Memory(m)) => Ok(m),
        _ => Err(CapError::BadMemory),
    }
}

/// Bounds-checked `[ptr, ptr+len)` against the linear-memory length.
fn range(ptr: u32, len: u32, mem_len: usize) -> Result<(usize, usize), CapError> {
    let start = ptr as usize;
    let end = start.checked_add(len as usize).ok_or(CapError::BadMemory)?;
    if end > mem_len {
        return Err(CapError::BadMemory);
    }
    Ok((start, end))
}

/// `cap_read(handle, out_ptr, out_cap) -> bytes_read | err`. Requires READ.
fn cap_read(mut caller: Caller<'_, HostState>, handle: u32, out_ptr: u32, out_cap: u32) -> i32 {
    let mem = match memory_of(&mut caller) {
        Ok(m) => m,
        Err(e) => return e.code(),
    };
    // 1. CAPABILITY CHECK — before any resource is touched.
    let cap = match caller.data().caps.check(handle, Ops::READ) {
        Ok(c) => c,
        Err(e) => return e.code(),
    };
    // 2. Fetch resource bytes (clone so the immutable borrow ends before we
    //    take a mutable borrow of guest memory).
    let bytes = match caller.data().resources.get(cap.resource()) {
        Some(b) => b.clone(),
        None => return CapError::NoResource.code(),
    };
    let n = core::cmp::min(bytes.len(), out_cap as usize);
    let data = mem.data_mut(&mut caller);
    let (start, end) = match range(out_ptr, n as u32, data.len()) {
        Ok(r) => r,
        Err(e) => return e.code(),
    };
    data[start..end].copy_from_slice(&bytes[..n]);
    n as i32
}

/// `cap_write(handle, src_ptr, src_len) -> bytes_written | err`. Requires WRITE.
fn cap_write(mut caller: Caller<'_, HostState>, handle: u32, src_ptr: u32, src_len: u32) -> i32 {
    let mem = match memory_of(&mut caller) {
        Ok(m) => m,
        Err(e) => return e.code(),
    };
    // 1. CAPABILITY CHECK.
    let cap = match caller.data().caps.check(handle, Ops::WRITE) {
        Ok(c) => c,
        Err(e) => return e.code(),
    };
    // 2. Copy the guest's bytes out (ends the immutable memory borrow).
    let data = mem.data(&caller);
    let (start, end) = match range(src_ptr, src_len, data.len()) {
        Ok(r) => r,
        Err(e) => return e.code(),
    };
    let buf = data[start..end].to_vec();
    // 3. Commit into the resource.
    match caller.data_mut().resources.get_mut(cap.resource()) {
        Some(r) => {
            *r = buf;
            src_len as i32
        }
        None => CapError::NoResource.code(),
    }
}

/// `cap_print(handle, src_ptr, src_len) -> bytes | err`. Requires PRINT.
fn cap_print(mut caller: Caller<'_, HostState>, handle: u32, src_ptr: u32, src_len: u32) -> i32 {
    let mem = match memory_of(&mut caller) {
        Ok(m) => m,
        Err(e) => return e.code(),
    };
    // 1. CAPABILITY CHECK.
    if let Err(e) = caller.data().caps.check(handle, Ops::PRINT) {
        return e.code();
    }
    // 2. Copy the guest's bytes out, then capture them.
    let data = mem.data(&caller);
    let (start, end) = match range(src_ptr, src_len, data.len()) {
        Ok(r) => r,
        Err(e) => return e.code(),
    };
    let buf = data[start..end].to_vec();
    caller.data_mut().stdout.extend_from_slice(&buf);
    src_len as i32
}

/// `cap_attenuate(handle, requested_ops) -> new_handle | err`.
///
/// Validates the handle, derives a strictly-narrower child via
/// [`Capability::derive`] (which cannot widen), installs it, and returns the new
/// handle. Returns a negative [`CapError`] code on any failure.
fn cap_attenuate(mut caller: Caller<'_, HostState>, handle: u32, requested_ops: u32) -> i64 {
    // Validate the handle (Ops::NONE is a subset of everything, so this is a
    // pure existence check) and copy out the parent capability.
    let parent = match caller.data().caps.check(handle, Ops::NONE) {
        Ok(c) => c,
        Err(e) => return e.code() as i64,
    };
    // Mask undefined bits off the guest-supplied request, then derive.
    let requested = Ops::from_bits_truncate(requested_ops);
    let child = match parent.derive(requested) {
        Ok(c) => c,
        Err(e) => return e.code() as i64,
    };
    caller.data_mut().caps.install(child) as i64
}

// ---------------------------------------------------------------------------
// Runtime wiring
// ---------------------------------------------------------------------------

/// Build a [`Linker`] exposing ONLY the four capability-gated host functions
/// under the import module `dezh`. No WASI, no other imports — anything else a
/// guest tries to import will fail to instantiate.
pub fn build_linker(engine: &Engine) -> wasmtime::Result<Linker<HostState>> {
    let mut linker = Linker::new(engine);
    linker.func_wrap("dezh", "cap_read", cap_read)?;
    linker.func_wrap("dezh", "cap_write", cap_write)?;
    linker.func_wrap("dezh", "cap_print", cap_print)?;
    linker.func_wrap("dezh", "cap_attenuate", cap_attenuate)?;
    Ok(linker)
}

/// Instantiate `wasm` with the given `state`, call its exported `run() -> i64`,
/// and return `(run_result, final_state)`.
pub fn run_guest(wasm: &[u8], state: HostState) -> wasmtime::Result<(i64, HostState)> {
    let engine = Engine::default();
    let module = Module::new(&engine, wasm)?;
    let linker = build_linker(&engine)?;
    let mut store = Store::new(&engine, state);
    let instance = linker.instantiate(&mut store, &module)?;
    let run = instance.get_typed_func::<(), i64>(&mut store, "run")?;
    let ret = run.call(&mut store, ())?;
    Ok((ret, store.into_data()))
}

// ---------------------------------------------------------------------------
// Embedded guest modules (compiled to wasm by build.rs, copied into OUT_DIR)
// ---------------------------------------------------------------------------

/// The example guests, embedded as wasm bytes so tests and the demo need no
/// external files at runtime.
pub mod guests_wasm {
    pub const G_GRANTED: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/g_granted.wasm"));
    pub const G_DENIED: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/g_denied.wasm"));
    pub const G_ATTENUATE: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/g_attenuate.wasm"));
}

// ---------------------------------------------------------------------------
// Unit tests for the pure capability core (crate-internal access)
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_rejects_widening() {
        // Parent has READ only; asking for WRITE must be rejected as widening.
        let parent = Capability::grant(0, Ops::READ);
        assert_eq!(parent.derive(Ops::WRITE), Err(CapError::Widening));
        assert_eq!(
            parent.derive(Ops::READ.union(Ops::WRITE)),
            Err(CapError::Widening)
        );
    }

    #[test]
    fn derive_rejects_noop() {
        let parent = Capability::grant(0, Ops::READ.union(Ops::WRITE));
        assert_eq!(
            parent.derive(Ops::READ.union(Ops::WRITE)),
            Err(CapError::NotNarrower)
        );
    }

    #[test]
    fn derive_narrows_ok() {
        let parent = Capability::grant(7, Ops::READ.union(Ops::WRITE));
        let child = parent.derive(Ops::READ).expect("READ is a strict subset");
        assert_eq!(child.resource(), 7);
        assert!(child.ops().contains(Ops::READ));
        assert!(!child.ops().contains(Ops::WRITE));
    }

    #[test]
    fn check_out_of_range_is_no_such_handle() {
        let table = CapTable::new();
        assert_eq!(table.check(0, Ops::READ), Err(CapError::NoSuchHandle));
        assert_eq!(table.check(999, Ops::READ), Err(CapError::NoSuchHandle));
    }

    #[test]
    fn check_wrong_op_is_denied() {
        let mut table = CapTable::new();
        let h = table.install(Capability::grant(1, Ops::READ));
        assert!(table.check(h, Ops::READ).is_ok());
        assert_eq!(table.check(h, Ops::WRITE), Err(CapError::OpNotPermitted));
    }

    #[test]
    fn derive_never_widens_across_entire_op_space() {
        // Exhaustively prove the structural guarantee: for every parent op set
        // and every (truncated) request, any capability `derive` produces is a
        // subset of its parent. This is the runnable backing for "there is no
        // API path that widens a capability."
        for praw in 0u32..=0b111 {
            let parent = Capability::grant(0, Ops::from_bits_truncate(praw));
            for rraw in 0u32..=0xFF {
                let requested = Ops::from_bits_truncate(rraw);
                if let Ok(child) = parent.derive(requested) {
                    assert!(
                        parent.ops().contains(child.ops()),
                        "child {:?} wider than parent {:?}",
                        child.ops(),
                        parent.ops()
                    );
                }
            }
        }
    }
}
