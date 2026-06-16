//! scheduler-bench — measures Step 7 placement-decision overhead.
//!
//! Run: `cargo run --release -p dezh-scheduler --bin scheduler-bench`

use std::hint::black_box;
use std::time::Instant;

use dezh_scheduler::{
    NodeId, ObjectKey, ObjectLocations, PlacementEngine, Policy, Resource, ResourceKind, Task,
    TaskClass, WorkloadHint,
};

fn main() -> Result<(), String> {
    let engine = engine()?;
    let task = Task::new(
        TaskClass::Batch,
        WorkloadHint::DataParallel,
        vec![ObjectKey(3), ObjectKey(7), ObjectKey(11)],
    );

    let iters = 1_000_000u64;
    for _ in 0..10_000 {
        black_box(engine.place(&task, Policy::Server));
    }

    let start = Instant::now();
    for _ in 0..iters {
        black_box(engine.place(black_box(&task), black_box(Policy::Server)));
    }
    let elapsed = start.elapsed();
    let per = elapsed.as_nanos() as f64 / iters as f64;

    println!("scheduler placement benchmark");
    println!("  decisions   : {iters}");
    println!("  resources   : {}", engine.resources().len());
    println!("  total time  : {:?}", elapsed);
    println!("  per decision: {per:.3} ns");
    Ok(())
}

fn engine() -> Result<PlacementEngine, String> {
    let mut resources = Vec::new();
    for i in 0..16u32 {
        resources.push(Resource::new(
            NodeId(i),
            if i % 4 == 0 {
                ResourceKind::Gpu
            } else if i % 5 == 0 {
                ResourceKind::Npu
            } else if i % 2 == 0 {
                ResourceKind::PerformanceCpu
            } else {
                ResourceKind::EfficiencyCpu
            },
            4.0 + f64::from(i % 8),
            1.0 + f64::from(i % 6),
            i % 7,
            i % 4,
        ));
    }
    let mut locations = ObjectLocations::new();
    locations.set(ObjectKey(3), NodeId(4));
    locations.set(ObjectKey(7), NodeId(4));
    locations.set(ObjectKey(11), NodeId(8));
    PlacementEngine::new(resources, locations).map_err(|e| e.to_string())
}
