## Summary

Describe the change and why it is needed.

## Security And Authority Impact

Select all that apply:

- [ ] No capability, grant, IPC, service, package, storage, or device boundary changed
- [ ] Capability model changed
- [ ] IPC contract changed
- [ ] Service lifecycle changed
- [ ] Package install/remove/recovery changed
- [ ] Disk layout changed
- [ ] User-space driver or device grant changed
- [ ] Kernel isolation boundary changed

## Validation

Commands run:

```text

```

## Review Focus

List the highest-risk areas for reviewers.

## Checklist

- [ ] No hidden kernel-side block I/O path was added
- [ ] No app received ambient filesystem, device, MMIO, DMA, IPC, or storage authority
- [ ] Package states other than `Active` remain non-runnable
- [ ] Recovery does not widen capabilities
- [ ] Failure modes return explicit status rather than hanging
- [ ] Public docs were updated when behavior changed
