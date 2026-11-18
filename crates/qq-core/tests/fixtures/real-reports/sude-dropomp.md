# sude-dropomp — omp dropped from qq-core

Branch quaqua/sude-dropomp (worktree ~/Lab/.wt/sude-dropomp), uncommitted. Only crates/qq-core/** touched.

## Changes
- task/prompt.rs: removed Shape::Omp and its "omp" parse arm. opencode still shared the parser, so omp_line is now opencode_line (used by Shape::Opencode and the None fallback). Comments reworded.
- task/cleanup.rs: stop_keys no longer matches "omp" (agy keeps ["esc"]).
- engine/activity.rs: test fixture harness "omp" -> "pi".
- tests/fixtures/team.toml: kind sude harness omp -> pi.
- tests: dropped all 9 `.replace("harness = \"omp\"", ...)` shims (config.rs helper template_without_omp removed). The classifier tests use Shape::Opencode. ops.rs stop test inserts its own opencode kind `oc` (sude is pi now). config.rs keeps the test that omp is refused as UnknownHarness.
- Untouched: db/old.rs legacy reading, 0001_config.sql (still CHECK name <> 'omp'), docs.rs banned-word list, ~/quaqua.

## Verification
- git grep -n -i -w omp -- src templates tests/fixtures: only config/mod.rs:84 doc comment, db/old.rs:5 comment, 0001_config.sql:8.
- cargo clippy --locked --all-targets -- -D warnings: clean.
- cargo test --locked: 324 passed, 0 failed.

## Risks
- sude-based tests now exercise pi instead of opencode, and the stop test needs its own opencode kind row.
- Kind order in UnknownHarness is nondeterministic across kinds, so the test asserts only the harness name.
- cargo fmt --check was already dirty on main; not touched.
