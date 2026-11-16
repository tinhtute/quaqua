# Workers — routing

Pick by task shape. Name = kind passed to `quaqua worker spawn`. Update "Seen" after each run (from `runs/*.md` + project Log).

| name | harness · model · effort | Use for | Avoid | Seen |
|------|--------------------------|---------|-------|------|
| sude | claude · sonnet-5.5 · medium | **default**: every task unless a tier is named; hard multi-file impl, audits (on request) | – | bpdeps, bpdocs, bpga |
| ni | claude · opus-5.5 · low | small Claude edits, quick answers | design, long work | – |
| suhuynh | claude · opus-5.5 · medium | refactor, review of intent | long mechanical edits | r0r1, pbslim, dashstyle |
| truonglao | claude · opus-5.5 · high | design, architecture, hard trade-offs | anything a cheaper tier can do | gridplan, bpaudit, pbglobal |

## Naming
- Worker name = `<name>-<task>`, lowercase, no diacritics, ≤20 chars, e.g. `sude-fonts`, `suhuynh-fonts`.
- Task must not end in `-lead` — `*-lead` names are project Leads (`quaqua lead spawn <slug>` → `<slug>-lead`, own `<slug>` workspace, no worktree); herdr caps agent names at 32 chars, so slugs are ≤27.
- Spawn: `quaqua worker spawn [--base <ref>] [--purpose <p>] [--solo "<reason>"] <name> [<kind>] <repo>` → branch `quaqua/<name>` (from <ref>, default HEAD), report `runs/<name>.md`. With `--purpose <p>`, kind resolves from `[purposes]` in `team.toml`. `--solo` (reason ≥ 10 chars, recorded in `memory/<today>.md`) bypasses the Lead gate on a `cos`-owned project (`rules/_all/lead-gate.md`).
- Old names (`mdtree`, `mdqa`…) stay as-is.

## Rules
- No name given → `sude`.
- Implementer and reviewer must be different kinds (`builder_reviewer_differ = "kind"`; e.g. sude builds → suhuynh reviews).
- Read-only question → `sude`; design unclear → `truonglao` before any builder.
- Unsure between two → cheaper tier first (sude < ni < suhuynh < truonglao); escalate only on a failed run.

## Purpose routing and model allowlists
- `[purposes]`: maps task shapes to kinds in `team.toml` (e.g. `build = "sude"`, `review = "suhuynh"`). `quaqua worker spawn --purpose <p> <name> [<kind>] <repo>` resolves the kind from this table; if a positional kind is also given, it must match the resolved kind or spawn fails.
- `[opencode_models]`: lists allowed models and effort variants for harness opencode only. Custom tables merge over the built-in defaults (`deepseek-v4.1-flash` with `low`/`high`/`max`) rather than replacing them; specifying a built-in model overrides its variants.
