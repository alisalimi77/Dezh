# dezh-scheduler — Step 7: task placement spike

This crate validates Dezh's scheduler direction before any kernel scheduler is
built. It treats scheduling as **task placement**: choose the best resource for
a task based on policy, workload hints, queue pressure, NUMA distance, energy,
and data locality.

It proves:

- mobile tensor tasks prefer a low-energy NPU;
- server batch tasks move compute toward data;
- latency-sensitive tasks avoid busy queues;
- batch policy tolerates queue depth for throughput;
- placement decisions are benchmarked and explainable.

## Out of scope for v0

No real task execution, no CPU affinity APIs, no GPU/NPU drivers, no hard
realtime, no cluster scheduler, no PGO database, and no kernel integration.

## Run

```sh
cargo test -p dezh-scheduler
cargo run --release -p dezh-scheduler --bin scheduler-bench
```

Measured on the development machine (`--release`, Windows/MSVC):

```text
per placement decision ~= 491.454 ns
resources              = 16
```

This is the v0 scoring engine only. It is not task execution, kernel scheduling,
or accelerator dispatch.
