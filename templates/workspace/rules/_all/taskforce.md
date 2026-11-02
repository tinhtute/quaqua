<!-- audience: orchestrator -->
# Taskforce

- Ephemeral, read-only workers spawned via `quaqua taskforce {advisor,dual-lens}`.
- Call taskforce instead of a builder when evaluating options, validating designs, or resolving trade-offs before code.
- Counts toward orchestrator worker cap per weights (`dual-lens` weighs 2).

## Shapes
- **advisor** (`suhuynh`): returns recommendation + confidence, evidence, alternatives, strongest downside, and the smallest check that would prove it wrong (marked not run).
- **dual-lens** (`truonglao` + `suhuynh`): gives both members the same neutral brief. The caller chairs: points both agree on are settled; each conflict goes to the other lens once.

## Lifecycle
- Short-lived and read-only.
- Reports directly to the caller (`runs/<name>.md`).
- Caller cleans up the taskforce workers immediately after reading runs files.
