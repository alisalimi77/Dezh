# dezh-linux - Step 9: Linux Personality Server Spike

This crate validates Dezh's first compatibility bridge: a user-space
Linux-like personality server that exposes a virtual filesystem view while
enforcing Dezh capabilities before touching Cairn.

It is intentionally not a complete Linux ABI. The v0 surface is:

- `open(path, flags) -> fd`
- `read(fd, out) -> bytes`
- `write(fd, bytes) -> WriteReceipt`
- `close(fd)`
- unsupported syscalls return `ENOSYS`

## Security Model

Legacy code sees ordinary Linux-style paths such as `/home/app/readme.txt`.
The server maps those paths through explicit mounts to Cairn refs such as
`refs/legacy/home/readme.txt`.

Every operation is checked against an `AuthorityGrant`:

- read requires `READ_REF | READ_OBJECT`
- write/create/truncate requires `UPDATE_REF`
- paths outside a mount are invisible
- `..` escape attempts are rejected before any Cairn lookup
- writes create Cairn objects/commits and record provenance invocations

## What This Validates

Compatibility can be a bridge rather than the destination: a legacy-facing API
can look familiar while Dezh still controls the real authority boundary.

Run the focused tests with:

```sh
cargo test -p dezh-linux
```

## Deferred

ELF loading, real syscall numbers, process address spaces, directories,
permissions bits, `stat`, `mmap`, pipes, sockets, signals, `fork`, async I/O,
and binary translation are later compatibility phases.
