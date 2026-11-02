# herdr fanout grid — MANDATORY

Applies to every Claude Code session in ~/quaqua that spawns or prompts herdr agents (CoS, project orchestrators, EA).

Single source: `rules/_all/fanout-agent-grid.md` (orchestrator-only). In short: `quaqua worker spawn` / `quaqua worker cleanup` grid the panes themselves; run `quaqua grid reconcile` by hand only to repair a layout. Close a pane (`quaqua worker cleanup`) as soon as its run is verified.
