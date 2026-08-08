# dezh-identity — Step 3: identity, delegation, and provenance

This crate validates the next Dezh architecture decision after Cairn: capability
security also needs a first-class answer to **who acted, on whose behalf, with
which attenuated authority, and what was produced**.

It proves:

- principals can be humans, services, or agents;
- root authority is explicitly minted by trusted host code;
- delegation requires `DELEGATE` authority;
- delegated authority can only be a subset of the parent grant;
- delegated scope can only stay equal or become narrower;
- sub-agents inherit the full delegation chain;
- invocations record actor, scope, used authority, action, reason, outputs, and
  delegation chain;
- invocations fail if the grant lacks the requested authority.

## Out of scope for v0

No cryptographic signatures, key storage, revocation, persistence, Cairn commit
integration, or capability transfer over IPC yet. Those belong to later phases.

## Run

```sh
cargo test -p dezh-identity
```
