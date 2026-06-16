//! # dezh-runtime — Step 4: capability + Cairn + identity integration
//!
//! Step 1 proved unforgeable WASM capability handles. Step 2 proved Cairn's
//! rollbackable object store. Step 3 proved identity/delegation/provenance.
//! This crate connects the three: a WASM guest can read or mutate a Cairn ref
//! only through a granted handle, and every mutation records an invocation.

use std::fmt;

use dezh_cairn::{CairnStore, CommitId, ObjectId};
use dezh_identity::{Artifact, ArtifactKind, Authority, AuthorityGrant, Invocation, Scope};
use wasmtime::{Caller, Engine, Extern, Linker, Memory, Module, Store, TypedFunc};

pub type Handle = u32;

#[derive(Debug)]
pub enum RuntimeError {
    NoSuchHandle,
    OpNotPermitted,
    NoRef,
    NoObject,
    BadMemory,
    Store(String),
    Identity(String),
    InvalidGrantScope,
    Contract(String),
}

impl RuntimeError {
    pub fn code(&self) -> i32 {
        match self {
            RuntimeError::NoSuchHandle => -1,
            RuntimeError::OpNotPermitted => -2,
            RuntimeError::NoRef => -3,
            RuntimeError::NoObject => -4,
            RuntimeError::BadMemory => -5,
            RuntimeError::Store(_) => -6,
            RuntimeError::Identity(_) => -7,
            RuntimeError::InvalidGrantScope => -8,
            RuntimeError::Contract(_) => -9,
        }
    }
}

impl fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuntimeError::NoSuchHandle => write!(f, "no such handle"),
            RuntimeError::OpNotPermitted => write!(f, "operation not permitted"),
            RuntimeError::NoRef => write!(f, "ref does not exist"),
            RuntimeError::NoObject => write!(f, "object does not exist"),
            RuntimeError::BadMemory => write!(f, "guest memory error"),
            RuntimeError::Store(e) => write!(f, "store error: {e}"),
            RuntimeError::Identity(e) => write!(f, "identity error: {e}"),
            RuntimeError::InvalidGrantScope => write!(f, "grant scope does not cover ref"),
            RuntimeError::Contract(e) => write!(f, "ir contract rejected module: {e}"),
        }
    }
}

impl std::error::Error for RuntimeError {}

pub type Result<T> = std::result::Result<T, RuntimeError>;

#[derive(Clone, Debug)]
pub struct RuntimeCapability {
    ref_name: String,
    grant: AuthorityGrant,
}

impl RuntimeCapability {
    pub fn new(ref_name: impl Into<String>, grant: AuthorityGrant) -> Result<Self> {
        let ref_name = ref_name.into();
        let ref_scope =
            Scope::new(ref_name.clone()).map_err(|_| RuntimeError::InvalidGrantScope)?;
        if !grant.scope().contains(&ref_scope) {
            return Err(RuntimeError::InvalidGrantScope);
        }
        Ok(RuntimeCapability { ref_name, grant })
    }

    pub fn ref_name(&self) -> &str {
        &self.ref_name
    }

    pub fn grant(&self) -> &AuthorityGrant {
        &self.grant
    }
}

#[derive(Default)]
pub struct RuntimeCapTable {
    slots: Vec<RuntimeCapability>,
}

impl RuntimeCapTable {
    pub fn new() -> Self {
        RuntimeCapTable { slots: Vec::new() }
    }

    pub fn install(&mut self, cap: RuntimeCapability) -> Handle {
        let h = self.slots.len() as Handle;
        self.slots.push(cap);
        h
    }

    pub fn check(&self, handle: Handle, required: Authority) -> Result<&RuntimeCapability> {
        let cap = self
            .slots
            .get(handle as usize)
            .ok_or(RuntimeError::NoSuchHandle)?;
        if !cap.grant.authority().contains(required) {
            return Err(RuntimeError::OpNotPermitted);
        }
        Ok(cap)
    }
}

pub struct RuntimeState {
    pub cairn: CairnStore,
    pub caps: RuntimeCapTable,
    pub invocations: Vec<Invocation>,
}

impl RuntimeState {
    pub fn new(cairn: CairnStore) -> Self {
        RuntimeState {
            cairn,
            caps: RuntimeCapTable::new(),
            invocations: Vec::new(),
        }
    }

    pub fn grant_ref(
        &mut self,
        ref_name: impl Into<String>,
        grant: AuthorityGrant,
    ) -> Result<Handle> {
        let cap = RuntimeCapability::new(ref_name, grant)?;
        Ok(self.caps.install(cap))
    }
}

fn memory_of(caller: &mut Caller<'_, RuntimeState>) -> Result<Memory> {
    match caller.get_export("memory") {
        Some(Extern::Memory(m)) => Ok(m),
        _ => Err(RuntimeError::BadMemory),
    }
}

fn range(ptr: u32, len: u32, mem_len: usize) -> Result<(usize, usize)> {
    let start = ptr as usize;
    let end = start
        .checked_add(len as usize)
        .ok_or(RuntimeError::BadMemory)?;
    if end > mem_len {
        return Err(RuntimeError::BadMemory);
    }
    Ok((start, end))
}

/// `cap_read(handle, out_ptr, out_cap) -> bytes_read | err`.
///
/// Requires both READ_REF (to resolve the ref) and READ_OBJECT (to read the
/// target object bytes).
fn cap_read(mut caller: Caller<'_, RuntimeState>, handle: u32, out_ptr: u32, out_cap: u32) -> i32 {
    match cap_read_inner(&mut caller, handle, out_ptr, out_cap) {
        Ok(n) => n as i32,
        Err(e) => e.code(),
    }
}

fn cap_read_inner(
    caller: &mut Caller<'_, RuntimeState>,
    handle: u32,
    out_ptr: u32,
    out_cap: u32,
) -> Result<usize> {
    let mem = memory_of(caller)?;
    let cap = caller
        .data()
        .caps
        .check(handle, Authority::READ_REF.union(Authority::READ_OBJECT))?;
    let object_id = caller
        .data()
        .cairn
        .get_ref(cap.ref_name())
        .ok_or(RuntimeError::NoRef)?;
    let bytes = caller
        .data()
        .cairn
        .get(object_id)
        .ok_or(RuntimeError::NoObject)?
        .to_vec();
    let n = bytes.len().min(out_cap as usize);
    let data = mem.data_mut(caller);
    let (start, end) = range(out_ptr, n as u32, data.len())?;
    data[start..end].copy_from_slice(&bytes[..n]);
    Ok(n)
}

/// `cap_write(handle, src_ptr, src_len) -> bytes_written | err`.
///
/// Requires UPDATE_REF. The write creates a new Cairn object, advances the ref
/// in a commit, then records an invocation containing the produced object and
/// commit artifacts.
fn cap_write(mut caller: Caller<'_, RuntimeState>, handle: u32, src_ptr: u32, src_len: u32) -> i32 {
    match cap_write_inner(&mut caller, handle, src_ptr, src_len) {
        Ok(n) => n as i32,
        Err(e) => e.code(),
    }
}

fn cap_write_inner(
    caller: &mut Caller<'_, RuntimeState>,
    handle: u32,
    src_ptr: u32,
    src_len: u32,
) -> Result<usize> {
    let mem = memory_of(caller)?;
    let cap = caller
        .data()
        .caps
        .check(handle, Authority::UPDATE_REF)?
        .clone();
    let data = mem.data(&mut *caller);
    let (start, end) = range(src_ptr, src_len, data.len())?;
    let bytes = data[start..end].to_vec();
    let state = caller.data_mut();
    let object = state
        .cairn
        .put(&bytes)
        .map_err(|e| RuntimeError::Store(e.to_string()))?;
    let commit = state
        .cairn
        .begin_tx()
        .tap(|tx| tx.set_ref(cap.ref_name(), object))
        .commit(cap.grant().holder().name(), "wasm cap_write")
        .map_err(|e| RuntimeError::Store(e.to_string()))?;
    let invocation = record_write_invocation(cap.grant(), object, commit)?;
    state.invocations.push(invocation);
    Ok(src_len as usize)
}

fn record_write_invocation(
    grant: &AuthorityGrant,
    object: ObjectId,
    commit: CommitId,
) -> Result<Invocation> {
    Invocation::record(
        grant,
        Authority::UPDATE_REF,
        "cairn.commit",
        "wasm cap_write",
        vec![
            Artifact::new(ArtifactKind::Object, format!("object:{object}"))
                .map_err(|e| RuntimeError::Identity(e.to_string()))?,
            Artifact::new(ArtifactKind::Commit, format!("commit:{commit}"))
                .map_err(|e| RuntimeError::Identity(e.to_string()))?,
        ],
    )
    .map_err(|e| RuntimeError::Identity(e.to_string()))
}

/// The exact `dezh` host surface this runtime offers. It is narrower than the
/// full IR surface on purpose: the Cairn runtime exposes only ref read/write,
/// not `cap_print` or `cap_attenuate`. Every guest is validated against this set
/// before it is instantiated, so a module importing anything else is rejected at
/// the contract gate rather than failing opaquely at link time.
pub const HOST_SURFACE: &[dezh_ir::HostImport] =
    &[dezh_ir::HostImport::CapRead, dezh_ir::HostImport::CapWrite];

/// Enforce the Dezh IR contract against this runtime's host surface. This is the
/// security gate: no guest reaches `Module::new`/instantiation until it has been
/// proven to import only the capability-mediated functions this host provides
/// (and to satisfy the memory/`run`/signature contract).
pub fn enforce_contract(wasm: &[u8]) -> Result<()> {
    dezh_ir::validate_module_with(wasm, HOST_SURFACE)
        .map(|_| ())
        .map_err(|violations| {
            let detail = violations
                .iter()
                .map(|v| v.to_string())
                .collect::<Vec<_>>()
                .join("; ");
            RuntimeError::Contract(detail)
        })
}

pub fn build_linker(engine: &Engine) -> wasmtime::Result<Linker<RuntimeState>> {
    let mut linker = Linker::new(engine);
    linker.func_wrap("dezh", "cap_read", cap_read)?;
    linker.func_wrap("dezh", "cap_write", cap_write)?;
    Ok(linker)
}

pub fn run_guest(wasm: &[u8], state: RuntimeState) -> wasmtime::Result<(i64, RuntimeState)> {
    enforce_contract(wasm)?;
    let engine = Engine::default();
    let module = Module::new(&engine, wasm)?;
    let linker = build_linker(&engine)?;
    let mut store = Store::new(&engine, state);
    let instance = linker.instantiate(&mut store, &module)?;
    let run = instance.get_typed_func::<(), i64>(&mut store, "run")?;
    let ret = run.call(&mut store, ())?;
    Ok((ret, store.into_data()))
}

pub struct RuntimeInstance {
    store: Store<RuntimeState>,
    run: TypedFunc<(), i64>,
}

impl RuntimeInstance {
    pub fn new(wasm: &[u8], state: RuntimeState) -> wasmtime::Result<Self> {
        enforce_contract(wasm)?;
        let engine = Engine::default();
        let module = Module::new(&engine, wasm)?;
        let linker = build_linker(&engine)?;
        let mut store = Store::new(&engine, state);
        let instance = linker.instantiate(&mut store, &module)?;
        let run = instance.get_typed_func::<(), i64>(&mut store, "run")?;
        Ok(RuntimeInstance { store, run })
    }

    pub fn call_run(&mut self) -> wasmtime::Result<i64> {
        self.run.call(&mut self.store, ())
    }

    pub fn into_state(self) -> RuntimeState {
        self.store.into_data()
    }
}

pub fn wat_to_wasm(wat: &str) -> std::result::Result<Vec<u8>, wat::Error> {
    wat::parse_str(wat)
}

trait Tap: Sized {
    fn tap(mut self, f: impl FnOnce(&mut Self)) -> Self {
        f(&mut self);
        self
    }
}

impl<T> Tap for T {}

#[cfg(test)]
mod tests {
    use super::*;
    use dezh_identity::{Principal, PrincipalKind};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_store(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "dezh-runtime-{name}-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        path
    }

    fn read_guest() -> Vec<u8> {
        wat::parse_str(
            r#"
            (module
              (import "dezh" "cap_read" (func $cap_read (param i32 i32 i32) (result i32)))
              (memory (export "memory") 1)
              (func (export "run") (result i64)
                (local $n i32)
                (local $i i32)
                (local $sum i64)
                (local.set $n (call $cap_read (i32.const 0) (i32.const 64) (i32.const 64)))
                (if (i32.lt_s (local.get $n) (i32.const 0))
                  (then (return (i64.extend_i32_s (local.get $n)))))
                (loop $loop
                  (if (i32.lt_u (local.get $i) (local.get $n))
                    (then
                      (local.set $sum
                        (i64.add
                          (local.get $sum)
                          (i64.extend_i32_u (i32.load8_u (i32.add (i32.const 64) (local.get $i))))))
                      (local.set $i (i32.add (local.get $i) (i32.const 1)))
                      (br $loop))))
                (local.get $sum)))
            "#,
        )
        .unwrap()
    }

    fn write_guest() -> Vec<u8> {
        wat::parse_str(
            r#"
            (module
              (import "dezh" "cap_write" (func $cap_write (param i32 i32 i32) (result i32)))
              (memory (export "memory") 1)
              (data (i32.const 16) "agent-write")
              (func (export "run") (result i64)
                (i64.extend_i32_s
                  (call $cap_write (i32.const 0) (i32.const 16) (i32.const 11)))))
            "#,
        )
        .unwrap()
    }

    fn seeded_state(ref_name: &str, bytes: &[u8], authority: Authority) -> (PathBuf, RuntimeState) {
        let root = temp_store("state");
        let mut cairn = CairnStore::open(&root).unwrap();
        let object = cairn.put(bytes).unwrap();
        cairn
            .begin_tx()
            .tap(|tx| tx.set_ref(ref_name, object))
            .commit("human:ali", "seed")
            .unwrap();
        let human = Principal::new(PrincipalKind::Human, "ali").unwrap();
        let grant = AuthorityGrant::root(human, Scope::new(ref_name).unwrap(), authority).unwrap();
        let mut state = RuntimeState::new(cairn);
        state.grant_ref(ref_name, grant).unwrap();
        (root, state)
    }

    #[test]
    fn guest_reads_cairn_ref_only_with_granted_capability() {
        let (root, state) = seeded_state(
            "refs/projects/dezh/doc",
            b"hello",
            Authority::READ_REF.union(Authority::READ_OBJECT),
        );

        let (ret, state) = run_guest(&read_guest(), state).unwrap();

        assert_eq!(ret, 532);
        assert!(state.invocations.is_empty());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn guest_write_creates_cairn_commit_and_invocation() {
        let (root, state) =
            seeded_state("refs/projects/dezh/doc", b"before", Authority::UPDATE_REF);

        let (ret, state) = run_guest(&write_guest(), state).unwrap();

        assert_eq!(ret, 11);
        let current = state.cairn.get_ref("refs/projects/dezh/doc").unwrap();
        assert_eq!(state.cairn.get(current), Some(&b"agent-write"[..]));
        assert_eq!(state.cairn.history("refs/projects/dezh/doc").len(), 2);
        assert_eq!(state.invocations.len(), 1);
        let invocation = &state.invocations[0];
        assert_eq!(invocation.action, "cairn.commit");
        assert_eq!(invocation.used_authority, Authority::UPDATE_REF);
        assert_eq!(invocation.outputs.len(), 2);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn read_only_guest_cannot_write_cairn() {
        let (root, state) = seeded_state(
            "refs/projects/dezh/doc",
            b"before",
            Authority::READ_REF.union(Authority::READ_OBJECT),
        );

        let (ret, state) = run_guest(&write_guest(), state).unwrap();

        assert_eq!(ret, RuntimeError::OpNotPermitted.code() as i64);
        let current = state.cairn.get_ref("refs/projects/dezh/doc").unwrap();
        assert_eq!(state.cairn.get(current), Some(&b"before"[..]));
        assert!(state.invocations.is_empty());
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn module_with_forbidden_import_is_rejected_at_contract_gate() {
        let root = temp_store("contract-wasi");
        let cairn = CairnStore::open(&root).unwrap();
        let state = RuntimeState::new(cairn);

        // A WASI import is ambient authority — the contract must reject it before
        // the module is ever instantiated.
        let wasm = wat::parse_str(
            r#"
            (module
              (import "wasi_snapshot_preview1" "fd_write"
                (func (param i32 i32 i32 i32) (result i32)))
              (memory (export "memory") 1)
              (func (export "run") (result i64) (i64.const 0)))
            "#,
        )
        .unwrap();

        let err = match run_guest(&wasm, state) {
            Err(e) => e,
            Ok(_) => panic!("expected contract rejection"),
        };
        assert!(err.to_string().contains("contract"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn module_importing_unoffered_host_fn_is_rejected_at_contract_gate() {
        let root = temp_store("contract-print");
        let cairn = CairnStore::open(&root).unwrap();
        let state = RuntimeState::new(cairn);

        // cap_print is a defined host function but this runtime does not offer it.
        let wasm = wat::parse_str(
            r#"
            (module
              (import "dezh" "cap_print" (func (param i32 i32 i32) (result i32)))
              (memory (export "memory") 1)
              (func (export "run") (result i64) (i64.const 0)))
            "#,
        )
        .unwrap();

        let err = match run_guest(&wasm, state) {
            Err(e) => e,
            Ok(_) => panic!("expected contract rejection"),
        };
        assert!(err.to_string().contains("cap_print"));
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn forged_handle_is_rejected_before_cairn_access() {
        let root = temp_store("forged");
        let cairn = CairnStore::open(&root).unwrap();
        let state = RuntimeState::new(cairn);

        let (ret, state) = run_guest(&read_guest(), state).unwrap();

        assert_eq!(ret, RuntimeError::NoSuchHandle.code() as i64);
        assert!(state.invocations.is_empty());
        fs::remove_dir_all(root).ok();
    }
}
