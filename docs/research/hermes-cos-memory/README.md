# Hermes-style CoS memory and context injection

Research trail behind the engine-rendered CoS standing block (PR #286) and the follow-up ctx-trim plan. Reports are kept as written, with local paths and nicknames scrubbed.

## Files

- [hermes-inject-round1.md](hermes-inject-round1.md): round 1, docs only. How Hermes injects always-on context, and a first mapping onto quaqua.
- [hermes-source.md](hermes-source.md): Hermes source read (prompt tiers, SOUL, AGENTS.md chain, memory caps) with six design variants A-F.
- [inject-probe.md](inject-probe.md): empirical probes on Claude Code 2.1.327 (hook text authority, where injected text lands, opencode behavior).
- [ctx-trim-plan.md](ctx-trim-plan.md): ctx-trim v4 plan, including P7 (situation tool and memory parity).
- [brainstorm-2026-11-18.md](brainstorm-2026-11-18.md): round-3 decisions after PR #286 landed.

## Reading order

1. `hermes-inject-round1.md`: the question and the first mapping.
2. `hermes-source.md`: what Hermes really does, and the variants.
3. `inject-probe.md`: what Claude Code really does with injected text.
4. `ctx-trim-plan.md`: the plan that came out of 1-3.
5. `brainstorm-2026-11-18.md`: the latest decisions; read last.
