# sude-harnessdyn (T41c): done, uncommitted in worktree quaqua/sude-harnessdyn

## Changes (crates/qq-core)
- config: HARNESSES const gone. `[harnesses.<name>]` (herdr_kind, args, efforts, stop_keys, error_shape) parsed, validated, stored in the existing `harness` row (herdr_agent_kind + JSON in args_template). Natives come from `driver::Registry::native()` (claude, pi; no agy driver exists). Harness gained `spec: Option<HarnessSpec>`.
- validate: known harness = native | declared in file | stored generic row (set/validate/import all see stored ones). Refused: omp (UnknownHarness), native/uppercase names, missing {model}, unknown placeholders, {rules_file}/{prompt} (Q43 deferred, NG13), ctrl+c in stop_keys, bad error_shape, kind effort outside declared efforts.
- driver: Generic renders args per element ({effort} element and its preceding flag drop when the kind has no effort). New HarnessDriver methods with defaults: stop_keys(), error_shape(), trusts_folder_dialog(). ErrorShape moved here (task::prompt re-exports it as Shape). Registry::resolve(&Harness) builds from the DB row, so pipeline paths holding Drivers::native() also resolve declared harnesses.
- task: cleanup::stop_keys fn, Shape::parse and the spawn `== "claude"` check removed; all go through the driver.
- team.toml + tests/fixtures/team.toml: [harnesses.opencode] (args `--model {model}` is my assumption, stop esc esc, shape opencode).
- No migration: the spec fits existing columns, so db/migrate.rs (outside write list) is untouched.

## Verify
- `cargo test --locked` (in crates/qq-core): all suites ok, 0 failed. New: spawn::harness_from_toml_alone, config::harness_declared_in_toml_imports_and_validates.
- `cargo clippy --locked --all-targets -- -D warnings`: clean.
- grep outside driver/config: 3 hits left (engine/activity.rs:582, herdr/liveness.rs:317, task/worker_flow.rs:757), all inside #[cfg(test)].
- `cargo fmt --check` shows diffs only in untouched files (e.g. authority/push.rs).

## Behaviour changes / risks
- pi stop: was refused as Unverified, now stops on `esc` (driver is the one source). tests/ops.rs updated; claude stays Unverified.
- agy: no driver, and the const row is gone, so a kind with harness=agy now fails validation unless declared in toml (error_shape/stop_keys support it). Old DBs keep their agy row but check_db rejects such kinds.
- Q43 not wired: `resume_args` and {prompt}/{rules_file} rejected at validate, not stored. engine/ not touched.
- Spec lives as JSON in `args_template`; proper columns need a migration plus a LADDER line in db/migrate.rs.
