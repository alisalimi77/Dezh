//! ipc-bench — measures the Step 6 user-space actor message path.
//!
//! Run: `cargo run --release -p dezh-ipc --bin ipc-bench`

use std::sync::mpsc;
use std::time::{Duration, Instant};

use dezh_identity::{Principal, PrincipalKind};
use dezh_ipc::{ActorExit, ActorSystem};

fn main() -> Result<(), String> {
    let iters = 200_000u64;
    let mut sys = ActorSystem::new();
    let (done_tx, done_rx) = mpsc::channel();

    let receiver = sys.spawn(principal("receiver")?, Vec::new(), move |mut ctx| {
        for _ in 0..iters {
            let msg = ctx.recv().expect("message");
            assert_eq!(msg.body.len(), 8);
        }
        done_tx.send(()).unwrap();
    });

    let sender = sys.spawn(principal("sender")?, Vec::new(), move |ctx| {
        let payload = 42u64.to_le_bytes().to_vec();
        for _ in 0..iters {
            ctx.send(receiver, payload.clone()).expect("send");
        }
    });

    let start = Instant::now();
    done_rx
        .recv_timeout(Duration::from_secs(10))
        .map_err(|e| e.to_string())?;
    let elapsed = start.elapsed();

    assert_eq!(
        sys.join(sender).map_err(|e| e.to_string())?,
        ActorExit::Completed
    );
    assert_eq!(
        sys.join(receiver).map_err(|e| e.to_string())?,
        ActorExit::Completed
    );

    let per = elapsed.as_nanos() as f64 / iters as f64 / 1_000.0;
    println!("actor ipc benchmark");
    println!("  messages      : {iters}");
    println!("  total time    : {:?}", elapsed);
    println!("  per message   : {per:.3} us");
    println!("  payload bytes : 8");
    Ok(())
}

fn principal(name: &str) -> Result<Principal, String> {
    Principal::new(PrincipalKind::Agent, name).map_err(|e| e.to_string())
}
