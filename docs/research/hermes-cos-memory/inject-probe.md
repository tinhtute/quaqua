Outcome: COMPLETE
## Changes
No repo, settings or mod changes. Probed in a scratch dir (`<scratchpad>/probe`, `<scratchpad>/oc`), Claude Code 2.1.327, `claude -p --model haiku --max-turns 1` (13 runs of 25; interactive launch aborted at the trust dialog, no model call), plus 1 `opencode run` on a free model. Labels: [observed] seen in a transcript or output this session.

### P1 SessionStart additionalContext — CONFIRMED
Hook `hook.sh` reads stdin JSON, emits `{"hookSpecificOutput":{"hookEventName":"SessionStart","additionalContext":"PROBE-SS-<source>: the codeword is MANGO"}}`.
- `claude -p --model haiku --max-turns 1 "What is the codeword from PROBE-SS...?" </dev/null` → `👾 · MANGO`. Hook stdin: `"hook_event_name":"SessionStart","source":"startup"`. So `-p` runs SessionStart hooks and the model sees the text [observed].
- resume: `claude -p --resume <sid> ...` → hook log `"source":"resume"`, model answers MANGO [observed].
- compact: `claude -p --resume <sid> "/compact"` works non-interactively → hook log `"source":"compact"` [observed]. (Answers the "how to trigger compact/resume" unknown.)
- After compaction, a follow-up resume run listed: `PROBE-SS-compact: ...` and `PROBE-SS-resume: ...`; it said the `startup` marker was no longer visible (summarized away). Transcript attachments: startup (line 6), resume (46), compact (81), resume (94). So each source appends a fresh copy; pre-compaction copies are lost, the compact-source copy re-injects. Consecutive resumes with no compaction between them were not tested (copies likely stack).

### P2 Authority — hook text WINS over CLAUDE.md, 6/6
Conflict: CLAUDE.md says "always end every reply with the single word F", hook injects the same rule with word H. Prompt "Say hi in one short sentence."
- Order A (file=ALPHA, hook=BRAVO): 3/3 replies ended `BRAVO`.
- Order B (file=BRAVO, hook=ALPHA): 3/3 replies ended `ALPHA`.
Swapping the words removes a word-preference bias. Haiku only, `-p` only. Round 1 risk "hook text may carry less authority than AGENTS.md" is refuted for this build.

### P3 Placement — SYSTEM PROMPT, at its head
`~/.claude/projects/<cwd>/<sid>.jsonl` holds `api-request-blob` entries (the real API request):
- system message, block 0 (28,040 chars) starts: `"SessionStart hook additional context: PROBE-SS-startup: the codeword is MANGO\n\n# Environment\nYou have been invoked in the following environment: ..."` [observed].
- user message: 3 `<system-reminder>` blocks (CLAUDE.md/instructions, context, attribution) + the prompt text. CLAUDE.md/AGENTS.md text lives in the **user turn**, the hook text in the **system prompt**.
- The transcript attachment shows `hook_additional_context` with `renderedRole: "system"`; `--output-format stream-json --verbose --include-hook-events` shows `hook_started`/`hook_response` for SessionStart:startup but not the injected text as a model message.
- Consequence: the text sits before `# Environment`, in the cache prefix. A text that differs per session (e.g. INDEX "Now") changes the system prefix per session; keep the stable parts stable.

### P4 Mod API — PARTIAL: registered, hooks did NOT fire in -p; interactive not probed
- Types exist in `index.d.ts` (`prompt.section` :5034, `prompt.context` :5046, `prompt.compose` :5055; 2.1.322 file).
- Loaded a throwaway mod from scratch with `claude -p --plugin-dir <scratch>/mod` (no file under `~/.claude/mods`, no settings edit). Both runs printed: `probe-mod: its prompt.compose hook was skipped. "cc-plugin-sec-default" runs first and sent the event on past it.` and the same for `prompt.section`; `ninja-kit` gets the same notice for `prompt.compose`. Model answer: `NONE` (no KIWI/PEAR codeword). A second version with no `$.fs` calls behaved the same, so it is not my handler failing [observed].
- Reading: in `-p` with this user's config, a third-party mod's prompt.* hooks are skipped by the seated built-in `cc-plugin-sec-default`. Not tested interactively: the scratch dir is untrusted, and accepting the trust dialog writes to `~/.claude.json`, which I left alone.
- "New dir without restart": not probed (each `-p` run is a fresh process; needs interactive).
- The 10 KB additionalContext cap: not probed (only short texts).

### P5 opencode — CONFIRMED (1 run, weak model)
`opencode v2.3.2`; scratch AGENTS.md = `Rule: the AGENTS codeword is LEMON.` + `@extra.md`; extra.md = `The IMPORTED codeword is GRAPE.`
`opencode run -m opencode/deepseek-v4.1-flash-free "... Do not use tools."` → `AGENTS=LEMON IMPORTED=NONE`. It reads AGENTS.md; `@file` is not expanded. Side note: the reply began with `👾 ·`, so opencode also picked up the user-level `~/.claude/CLAUDE.md` (inferred).

### Design assumption → probe → result
| Design assumption | Probe | Result |
|---|---|---|
| `claude -p`/workers run SessionStart hooks | P1 | confirmed |
| Injected text reaches the model at startup | P1/P3 | confirmed |
| Hook fires on resume and compact; compact re-injects | P1 | confirmed (also the way to script both) |
| Emit on every source is needed (startup text is dropped by compaction) | P1 | confirmed |
| Injected text has less authority than AGENTS.md | P2 | refuted: wins 6/6, both orders |
| Lands as a message at conversation head | P3 | refuted: lands in the system prompt, before `# Environment` (AGENTS.md is in the user turn) |
| `prompt.section/compose/context` exist | P4 | types yes; hooks skipped in `-p` by sec-default; interactive not probed |
| Mod loader picks up a new dir without restart | P4 | not probed (interactive only) |
| opencode reads AGENTS.md | P5 | confirmed |
| opencode expands `@file` | P5 | refuted (1 run) |
| 6.2 KB fits the 10k cap | none | not probed |

Verdict on the design: option 1 (SessionStart standing block on every source) holds, and is stronger than round 1 assumed (system-prompt placement, wins over AGENTS.md). Option 2 (mod) is not safe to rely on for workers: its hooks were skipped under `-p`. Keep the approval-list floor in AGENTS.md for opencode, where only AGENTS.md (no `@` imports, no hook) works.

### Trashed (via `trash`)
- `<scratchpad>/probe` (hook.sh, hook.log, CLAUDE.md, .claude/settings.json, mod/, run2.jsonl, run2.err, p2.out)
- `<scratchpad>/oc` (AGENTS.md, extra.md)
- `~/.claude/projects/<probe-cwd-slug>` (10 transcript files from the probe runs)
Left: opencode's own session record from the one `opencode run` (under its data dir), not located or removed.
## Verify
Commands and outputs are quoted above. Evidence files were trashed after excerpting; rerun needs the 3 scripts (hook.sh, mod, AGENTS.md) recreated. Approx cost: 13 haiku runs, well under $1.
## Risks
- All Claude findings are `-p` and haiku only; interactive placement/authority and the mod hooks were not observed.
- The skipped mod hooks come from `cc-plugin-sec-default` in this user's config; another machine may differ.
- P2 tested a benign formatting rule, not a prohibition; models can resist a harmful instruction regardless of placement.
- opencode result is a single run on a free model.
## Lessons
`claude -p --resume <sid> "/compact"` triggers compact non-interactively; `api-request-blob` in the transcript shows where text really lands.
