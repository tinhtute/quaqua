# MCP tools that send, publish or spend (R7 PreToolUse matcher)

Source: `~/quaqua/runs/cos-tool-list-2026-11-19.txt` (CoS session `99ebabbf`, 112 expanded `mcp__*` names, loaded + deferred). Classification is from names plus the server instructions the CoS session saw. Nothing was executed.

## Confirmed outward tools (9)

| Tool (exact name) | Class | Reason |
|---|---|---|
| `mcp__claude_ai_Claude_Data__share_connector` | publish | Exposes the connector's live results to every viewer of an artifact. |
| `mcp__claude_ai_Claude_Data__share_live_queries` | publish | Publishes queries; every viewer sees results run with the sharer's access. |
| `mcp__plugin_tinyfish_tinyfish__run_deep_research` | spend | Server instructions: deep research is "slow, billed". |
| `mcp__plugin_tinyfish_tinyfish__run_deep_research_async` | spend | Same billing, background variant. |
| `mcp__plugin_tinyfish_tinyfish__run_web_automation` | spend | Metered against the TinyFish wallet; also acts on live websites (can submit forms). |
| `mcp__plugin_tinyfish_tinyfish__run_web_automation_async` | spend | Same, background variant. |
| `mcp__plugin_tinyfish_tinyfish__create_dataset` | spend | Datasets are "billed per row"; create starts a populated table. |
| `mcp__plugin_tinyfish_tinyfish__populate_dataset` | spend | Billed per row. |
| `mcp__plugin_tinyfish_tinyfish__top_up_wallet` | spend | Adds money to the wallet directly. |

No tool in the CoS list is a plain `send` today: every messaging connector (Gmail, Slack, Intercom, HubSpot, ...) is listed with only `authenticate` / `complete_authentication` (not connected). Their send tools appear after auth, so the list must be re-derived then (see predictive matcher).

## Matcher regexes

PreToolUse `matcher` is a regex on the tool name. Two cover everything above; checked against all 112 names: they hit exactly the 9 tools and no other name.

```
^mcp__plugin_tinyfish_tinyfish__(run_deep_research(_async)?|run_web_automation(_async)?|create_dataset|populate_dataset|top_up_wallet)$
^mcp__claude_ai_Claude_Data__share_(connector|live_queries)$
```

Predictive (third regex, fail-closed; matches **zero** names in today's list, so it costs nothing until a connector authenticates). It catches outward verbs on the not-yet-connected `claude_ai` connectors. Expect false positives such as a read tool with `share` in its name; review once the real tool names are visible.

```
^mcp__claude_ai_(Asana|Atlassian|Box|Canva|Gmail|Google_Calendar|Google_Docs|Google_Drive|Google_Sheets|Google_Slides|Higgsfield|HubSpot|Intercom|Linear|Notion|Slack|monday_com)__.*(send|post|reply|publish|share|invite|comment|message|upload|generate)
```

## Borderline, excluded

| Tool(s) | Why excluded |
|---|---|
| `claude-in-chrome__computer`, `form_input`, `javascript_tool`, `file_upload`, `upload_image`, `navigate` | Can submit or exfiltrate, but only by what they are driven to do; the name does not say. Gate the browser as a whole (separate rule), not per name. |
| `Claude_Docs__create`, `update`, `batch`, `export`, `delete` | Docs are private by default; sharing is not visible in these names (`batch`/`update` may carry sharing ops). Revisit if the docs guide shows share ops. `delete` is destructive, not outward. |
| `Claude_Data__certify_query`, `set_certified_query_source` | Org-internal state, not an outward send. `unshare_*` and `uncertify_query` reduce exposure. |
| `tinyfish__create_monitor`, `run_monitor`, `resume_monitor`, `update_monitor` | Recurring runs likely cost money, but billing is not stated in the instructions. Unverified: move to `spend` if confirmed. |
| `tinyfish__create_browser_session`, `start_profile_setup_session`, `create_profile` | Billing not stated. Same as monitors. |
| `tinyfish__plan_dataset` | Planning step; no per-row billing stated. |
| `tinyfish__search`, `fetch_content`, `get_*`, `list_*`, `cancel_*`, `batch_*`, `close_*`, `pause_monitor`, `delete_dataset`, `save_profile_setup_session` | Free per instructions, read-only, or only stop or remove things. |
| `*__authenticate`, `*__complete_authentication` | OAuth handshake, carries no content. |
| `Meetings__*` | Documented read-only. |
| `cos-memory__*` | Local memory writes; covered by the memory threat scan (R6). |
| `mcp__idea__*` (server failed in the CoS session) | Not in scope of the list. `execute_terminal_command` is shell-equivalent: covered by the Bash gate, not this matcher. |

## Open items

- Billing for monitors, browser sessions and profiles: confirm with TinyFish docs or `get_wallet` deltas before widening the spend regex.
- Re-run this inventory after Gmail, Slack and the other connectors authenticate, then replace the predictive regex with exact names.
