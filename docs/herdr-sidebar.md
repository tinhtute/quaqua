# herdr sidebar: highlight the CoS and Lead panes

quaqua tags its panes through `herdr pane report-metadata`, so the sidebar can
style a row from what the engine already reports. Two channels do the work:

| Channel | What quaqua sets | Read in herdr config as |
|---|---|---|
| pane token | `grid_role` = `cos` \| `lead` \| `worker` | `$grid_role` |
| display label | `--display-agent`: `ui.cos_label` for the CoS, `<slug>-lead` for a Lead | `agent` |

The label is the primary signal: a rule matching `CoS` or `-lead` reads natural
text. `$grid_role` is the fallback, for a pane whose label is missing or was
written by an older engine.

`ui.cos_label` is the CoS label, read from the workspace config (`qq.db`,
seeded from `team.toml`); it defaults to `🍑 CoS`. Set another one with:

    quaqua config set ui.cos_label "🌾 CoS quê"       # then restart the CoS pane

The key survives `quaqua config export`/`import` as `[ui] cos_label = "..."`.

## The snippet

Paste into `~/.config/herdr/config.toml` (quaqua never writes that file) and
reload with `herdr server reload-config`:

```toml
[ui.sidebar.agents]
# "agent" is the --display-agent label; "$grid_role" is the fallback token.
rows = [[
  "state_icon",
  { token = "agent", fg = "#888888", rules = [
    { contains = "CoS", fg = "#ff5fa2", bold = true },
    { contains = "-lead", fg = "#ff9f43" },
  ] },
  { token = "$grid_role", fg = "#666666", rules = [
    { contains = "cos", fg = "#ff5fa2", bold = true },
    { contains = "lead", fg = "#ff9f43" },
  ] },
], ["workspace", "tab"]]
```

Per-harness rows (`[ui.sidebar.agents.rows_by_agent]`, one entry per canonical
agent id such as `claude`) take the same cells; keep the two rule lists in each
row you write.

## Why `$grid_role` needs `rules`

herdr resolves a custom token from the pane metadata quaqua reports (the
shipped config template: "Custom values reported through pane metadata use a
`$name` token"), and a rule is allowed on any text-valued token — the validator
rejects only non-text ones. So `contains = "cos"` on `$grid_role` is the same
mechanism as `contains = "CoS"` on `agent`, one on the token, one on the label.

## Checking a local edit

```sh
herdr config check                     # "config: ok", or the offending line
mkdir -p /tmp/hp/herdr && cp ~/.config/herdr/config.toml /tmp/hp/herdr/
XDG_CONFIG_HOME=/tmp/hp herdr config check    # try an edit without touching yours
```
