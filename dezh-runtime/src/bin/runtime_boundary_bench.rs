//! runtime-boundary-bench — measures the actual WASM guest -> host boundary
//! paths through the Step 4 runtime.
//!
//! This is deliberately separate from `cap-bench`. `cap-bench` measures only
//! the native capability-table authority decision. This benchmark measures a
//! full guest call into host functions, including wasmtime trampoline overhead,
//! guest memory access, Cairn lookup/write, and invocation recording.
//!
//! Run: `cargo run --release -p dezh-runtime --bin runtime-boundary-bench`

use std::fs;
use std::hint::black_box;
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use dezh_cairn::CairnStore;
use dezh_identity::{Authority, AuthorityGrant, Principal, PrincipalKind, Scope};
use dezh_runtime::{RuntimeInstance, RuntimeState};

const REF_NAME: &str = "refs/bench/doc";

fn main() -> anyhow_like::Result<()> {
    let read_iters = 200_000u64;
    let write_iters = 10_000u64;

    let read = bench_read(read_iters)?;
    let write = bench_write(write_iters)?;

    println!("runtime boundary benchmark");
    println!("  read iterations  : {read_iters}");
    println!("  read total       : {:?}", read.elapsed);
    println!(
        "  read per call    : {:.3} us",
        read.per_call_us(read_iters)
    );
    println!("  read checksum    : {}", read.ok);
    println!();
    println!("  write iterations : {write_iters}");
    println!("  write total      : {:?}", write.elapsed);
    println!(
        "  write per call   : {:.3} us",
        write.per_call_us(write_iters)
    );
    println!("  writes ok        : {}", write.ok);
    println!();
    println!("labels:");
    println!("  read  = run() -> cap_read -> capability check -> Cairn ref/object lookup -> guest memory copy");
    println!("  write = run() -> cap_write -> capability check -> guest memory copy -> Cairn object+commit -> Invocation record");
    Ok(())
}

struct BenchResult {
    elapsed: std::time::Duration,
    ok: u64,
}

impl BenchResult {
    fn per_call_us(&self, iters: u64) -> f64 {
        self.elapsed.as_nanos() as f64 / iters as f64 / 1_000.0
    }
}

fn bench_read(iters: u64) -> anyhow_like::Result<BenchResult> {
    let (root, state) = seeded_state(
        b"hello",
        Authority::READ_REF.union(Authority::READ_OBJECT),
        "bench-read",
    )?;
    let wasm = dezh_runtime::wat_to_wasm(read_guest()).map_err(|e| e.to_string())?;
    let mut instance = RuntimeInstance::new(&wasm, state).map_err(|e| e.to_string())?;

    for _ in 0..10_000 {
        black_box(instance.call_run().map_err(|e| e.to_string())?);
    }

    let start = Instant::now();
    let mut ok = 0u64;
    for _ in 0..iters {
        if black_box(instance.call_run().map_err(|e| e.to_string())?) == 532 {
            ok += 1;
        }
    }
    let elapsed = start.elapsed();
    black_box(instance.into_state());
    fs::remove_dir_all(root).ok();
    Ok(BenchResult { elapsed, ok })
}

fn bench_write(iters: u64) -> anyhow_like::Result<BenchResult> {
    let (root, state) = seeded_state(b"seed", Authority::UPDATE_REF, "bench-write")?;
    let wasm = dezh_runtime::wat_to_wasm(write_guest()).map_err(|e| e.to_string())?;
    let mut instance = RuntimeInstance::new(&wasm, state).map_err(|e| e.to_string())?;

    for _ in 0..100 {
        black_box(instance.call_run().map_err(|e| e.to_string())?);
    }

    let start = Instant::now();
    let mut ok = 0u64;
    for _ in 0..iters {
        if black_box(instance.call_run().map_err(|e| e.to_string())?) == 11 {
            ok += 1;
        }
    }
    let elapsed = start.elapsed();
    black_box(instance.into_state());
    fs::remove_dir_all(root).ok();
    Ok(BenchResult { elapsed, ok })
}

fn seeded_state(
    bytes: &[u8],
    authority: Authority,
    name: &str,
) -> anyhow_like::Result<(PathBuf, RuntimeState)> {
    let root = temp_store(name);
    let mut cairn = CairnStore::open(&root).map_err(|e| e.to_string())?;
    let object = cairn.put(bytes).map_err(|e| e.to_string())?;
    cairn
        .begin_tx()
        .tap(|tx| tx.set_ref(REF_NAME, object))
        .commit("human:bench", "seed")
        .map_err(|e| e.to_string())?;

    let human = Principal::new(PrincipalKind::Human, "bench").map_err(|e| e.to_string())?;
    let grant = AuthorityGrant::root(
        human,
        Scope::new(REF_NAME).map_err(|e| e.to_string())?,
        authority,
    )
    .map_err(|e| e.to_string())?;
    let mut state = RuntimeState::new(cairn);
    state
        .grant_ref(REF_NAME, grant)
        .map_err(|e| e.to_string())?;
    Ok((root, state))
}

fn temp_store(name: &str) -> PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "dezh-runtime-bench-{name}-{}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    path
}

fn read_guest() -> &'static str {
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
    "#
}

fn write_guest() -> &'static str {
    r#"
    (module
      (import "dezh" "cap_write" (func $cap_write (param i32 i32 i32) (result i32)))
      (memory (export "memory") 1)
      (data (i32.const 16) "agent-write")
      (func (export "run") (result i64)
        (i64.extend_i32_s
          (call $cap_write (i32.const 0) (i32.const 16) (i32.const 11)))))
    "#
}

trait Tap: Sized {
    fn tap(mut self, f: impl FnOnce(&mut Self)) -> Self {
        f(&mut self);
        self
    }
}

impl<T> Tap for T {}

mod anyhow_like {
    pub type Result<T> = std::result::Result<T, String>;
}
