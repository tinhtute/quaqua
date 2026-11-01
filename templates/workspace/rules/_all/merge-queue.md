# Merge Queue Discipline

- Leads merge Peer branches only via `quaqua merge <slug> <branch>...`.
- One branch at a time, in order.
- Note the integration commit before merging; run the project test suite.
- If checks pass, record the new commit as accepted.
- If checks fail or merge conflicts, undo immediately: `quaqua merge` restores the noted commit via `merge --abort` or `reset --hard`.
- Return rework to the Peer with evidence; never patch failing Peer code yourself.
- If the integration branch fails without the branch too, hold the queue and escalate to CoS.
- Never rewrite integration history except to undo its own last failing merge.
- Never push or merge to main without anh iu's explicit approval.
- After every merge into a `*-integ` branch run `cargo check --workspace` before the next merge; a clean merge-tree is not a green build — 2026-11-01, issue #170
