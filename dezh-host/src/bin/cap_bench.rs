//! cap-bench — measures the per-call overhead of the capability check, i.e. the
//! cost the model adds in front of every host operation: one table lookup plus
//! one permitted-ops test (`CapTable::check`).
//!
//! Run: `cargo run --release --bin cap-bench`
//! Paste the reported ns/check figure into README.md.

use std::hint::black_box;
use std::time::Instant;

use dezh_host::{HostState, Ops};

fn main() {
    let mut st = HostState::new();
    st.resources.put(1, vec![0u8; 16]);
    let h = st.grant(1, Ops::READ); // handle 0

    // Warm up (let the branch predictor / caches settle).
    for _ in 0..1_000_000u64 {
        black_box(st.caps.check(black_box(h), black_box(Ops::READ))).ok();
    }

    let iters: u64 = 100_000_000;
    let start = Instant::now();
    let mut ok: u64 = 0;
    for _ in 0..iters {
        // black_box on the handle/op stops the optimizer from hoisting the
        // lookup out of the loop; counting ok results keeps the call live.
        if black_box(st.caps.check(black_box(h), black_box(Ops::READ))).is_ok() {
            ok += 1;
        }
    }
    let elapsed = start.elapsed();
    black_box(ok);

    let per = elapsed.as_nanos() as f64 / iters as f64;
    println!("capability check microbenchmark");
    println!("  iterations : {iters}");
    println!("  total time : {:?}", elapsed);
    println!("  successful : {ok}");
    println!("  per check  : {per:.3} ns");
}
