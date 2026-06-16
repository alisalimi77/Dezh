# dezh-cairn — Step 2: Cairn v0

Cairn validates Dezh's second irreversible architecture decision: durable state
is built from immutable, content-addressed objects plus small mutable refs.

This is not a full filesystem. It is a focused spike that proves:

- object IDs are BLAKE3 hashes of content;
- duplicate content naturally deduplicates to the same `ObjectId`;
- objects are immutable and remain readable after ref updates;
- refs move through atomic commit records;
- v0 accepts one ref movement per transaction;
- rollback is a new commit that points a ref back to an earlier object;
- replay rebuilds state from an append-only disk log;
- an incomplete trailing log record is ignored during recovery;
- each commit carries basic provenance metadata.

## Out of scope for v0

No schema system, encryption, compression, distributed sync, garbage collection,
semantic graph directories, or high-performance indexes.

## Run

```sh
cargo test -p dezh-cairn
```
