# PROJECT — identity

## What is this project?
- quaqua: a Rust CLI engine that runs a CoS-led team of AI coding agents (Leads, workers) on herdr panes.

## Who is it for?
- anh iu (owner) and the CoS/Lead/worker agents running in a quaqua workspace.

## Non-goals
- Workspace private state (`memory/`, `projects/`, `runs/`, `raw/`, `reports/`, `USER.md`) never lives in this repo.

## What are the gate commands?
- run from: repository root
- tests: `cargo test --locked`
- types: n/a (covered by `cargo build`)
- lint: `cargo clippy --locked -p qq-core` and `actionlint -color .github/workflows/*.yml`
- build: `cargo build --locked -p qq-core`
- format: `cargo fmt --check`

## Architecture in one breath
- runtime shape: single binary `quaqua` (`crates/qq-core`), verbs shell out to `herdr` and `git`.
- where state lives: consuming workspace `runs/` (metadata files, `runs/quaqua.db`); managed templates in `templates/workspace/`.
- entrypoints: `crates/qq-core/src/main.rs`; PreToolUse gate `crates/qq-core/src/authority/gate.rs`.

## What are we working on right now?
- plan: .harness/plans/active/superagent-bus.md (active, high-risk, next: work full phase hermetic)
- program: docs/research/superagent.md (index; T1 = this plan, T2–T6 draft)
