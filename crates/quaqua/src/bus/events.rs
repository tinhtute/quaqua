//! Event-driven monitoring, inbox digest with per-reader cursors, and herdr event hook.

use serde::{Deserialize, Serialize};
use std::path::Path;

use super::{FarmBus, FarmEvent, FarmEventRecord};
use crate::bus::mail::MailMessage;
use crate::herdr::HerdrOps;

/// Digest of unacked mail and incidents for a consumer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxDigest {
    pub unacked_mail: Vec<MailMessage>,
    pub incidents: Vec<FarmEventRecord>,
    pub prev_cursor: u64,
    pub new_cursor: u64,
    #[serde(default)]
    pub prev_mail_cursor: u64,
    #[serde(default)]
    pub new_mail_cursor: u64,
}

impl InboxDigest {
    /// Renders the inbox digest into a human- and agent-readable string.
    pub fn render(&self, consumer: &str) -> String {
        let mut out = String::new();
        if !self.unacked_mail.is_empty() {
            for msg in &self.unacked_mail {
                let tag = if msg.deliveries > 1 {
                    format!(" (redelivery #{})", msg.deliveries)
                } else {
                    String::new()
                };
                out.push_str(&format!(
                    "#{} {} {}{}\n",
                    msg.id, msg.from_name, msg.kind, tag
                ));
                if !msg.body.trim().is_empty() {
                    out.push_str(&format!("{}\n", msg.body.trim_end()));
                }
                out.push('\n');
            }
        }
        if !self.incidents.is_empty() {
            for ev in &self.incidents {
                let key = ev.incident_key.as_deref().unwrap_or("none");
                let actor = ev.actor.as_deref().unwrap_or("-");
                let subj = ev.subject.as_deref().unwrap_or("-");
                out.push_str(&format!(
                    "! #{} {} {} [{} -> {}]\n",
                    ev.seq, ev.event_type, key, actor, subj
                ));
            }
        }
        if out.is_empty() {
            format!("no unacked mail or new incidents for '{consumer}'\n")
        } else {
            out
        }
    }
}

/// Resolves the consumer's own pane ID if known (e.g. from HERDR_PANE_ID or herdr query).
pub fn resolve_consumer_pane(consumer: &str) -> Option<String> {
    if let Ok(p) = std::env::var("HERDR_PANE_ID") {
        let trimmed = p.trim().to_string();
        if !trimmed.is_empty() {
            return Some(trimmed);
        }
    }
    if consumer == "cos" {
        if let Ok(out) = std::process::Command::new(crate::herdr::bin_path())
            .args(["api", "snapshot"])
            .output()
        {
            if out.status.success() {
                if let Ok(val) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                    if let Some(panes) = val
                        .pointer("/result/snapshot/panes")
                        .and_then(|p| p.as_array())
                    {
                        for p in panes {
                            if p.pointer("/tokens/grid_role").and_then(|r| r.as_str())
                                == Some("cos")
                            {
                                if let Some(pid) = p.get("pane_id").and_then(|id| id.as_str()) {
                                    return Some(pid.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

/// Returns true if an incident record is noise from the consumer's own pane being blocked on a question.
pub fn is_consumer_own_pane_blocked_noise(r: &FarmEventRecord, own_pane: Option<&str>) -> bool {
    let Some(own) = own_pane else {
        return false;
    };
    if r.event_type != "blocked"
        && !r
            .incident_key
            .as_deref()
            .unwrap_or("")
            .starts_with("blocked:")
    {
        return false;
    }
    if r.subject.as_deref() == Some(own) {
        return true;
    }
    if let Some(key) = &r.incident_key {
        if key.starts_with(&format!("blocked:{own}:")) || key == &format!("blocked:{own}") {
            return true;
        }
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&r.payload_json) {
        if let Some(pid) = v
            .get("pane_id")
            .or_else(|| v.get("data").and_then(|d| d.get("pane_id")))
            .and_then(|p| p.as_str())
        {
            if pid == own {
                return true;
            }
        }
    }
    false
}

/// Helper to extract registered leads and projects from projects/INDEX.md.
pub fn resolve_known_leads_and_projects(runs_dir: &Path) -> (Vec<String>, Vec<String>) {
    let ws_root = runs_dir.parent().unwrap_or(runs_dir);
    let index_file = ws_root.join("projects/INDEX.md");
    let mut leads = Vec::new();
    let mut projects = Vec::new();
    if let Ok(content) = std::fs::read_to_string(&index_file) {
        let rows = crate::worker::leadgate::index_rows(&content);
        for row in rows {
            if row.owner != "cos" && !row.owner.is_empty() && !leads.contains(&row.owner) {
                leads.push(row.owner);
            }
            if !row.slug.is_empty() && !projects.contains(&row.slug) {
                projects.push(row.slug);
            }
        }
    }
    (leads, projects)
}

/// Helper to query pane tokens from payload or herdr.
pub fn resolve_pane_tokens_from_record(
    r: &FarmEventRecord,
) -> Option<serde_json::Map<String, serde_json::Value>> {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(&r.payload_json) {
        if let Some(tokens) = v
            .get("tokens")
            .or_else(|| v.get("data").and_then(|d| d.get("tokens")))
            .and_then(|t| t.as_object())
        {
            return Some(tokens.clone());
        }
    }
    let pid = if let Some(subj) = &r.subject {
        subj.as_str()
    } else {
        ""
    };
    let pid_from_payload = if let Ok(v) = serde_json::from_str::<serde_json::Value>(&r.payload_json)
    {
        v.get("pane_id")
            .or_else(|| v.get("data").and_then(|d| d.get("pane_id")))
            .and_then(|p| p.as_str())
            .map(String::from)
    } else {
        None
    };
    let target_pid = if !pid.is_empty() && pid.contains(':') {
        Some(pid)
    } else {
        pid_from_payload.as_deref()
    };
    if let Some(p) = target_pid {
        if let Ok(out) = std::process::Command::new(crate::herdr::bin_path())
            .args(["pane", "get", p])
            .output()
        {
            if out.status.success() {
                if let Ok(v) = serde_json::from_slice::<serde_json::Value>(&out.stdout) {
                    if let Some(tokens) =
                        v.pointer("/result/pane/tokens").and_then(|t| t.as_object())
                    {
                        return Some(tokens.clone());
                    }
                }
            }
        }
    }
    None
}

/// True when `cwd` is `root` or below it, comparing canonical paths when both resolve.
fn path_is_within(cwd: &Path, root: &Path) -> bool {
    if let (Ok(can_cwd), Ok(can_root)) = (cwd.canonicalize(), root.canonicalize()) {
        can_cwd.starts_with(&can_root)
    } else {
        cwd.starts_with(root)
    }
}

/// Checks whether a pane belongs to the workspace corresponding to `runs_dir`.
pub fn is_pane_in_runs_workspace(
    pid: &str,
    pane_cwd: Option<&str>,
    runs_dir: &Path,
    tokens_val: Option<&serde_json::Value>,
) -> bool {
    // 1. Check if it matches this workspace's recorded CoS pane
    if let Ok(content) = std::fs::read_to_string(runs_dir.join(crate::cos::COS_IDENTITY_FILE)) {
        if let Ok(ident) = serde_json::from_str::<serde_json::Value>(&content) {
            if let Some(recorded_pane) = ident.get("pane_id").and_then(|p| p.as_str()) {
                if recorded_pane == pid {
                    return true;
                }
            }
        }
    }

    // 2. Check worker metadata
    if crate::state::WorkerMetadata::owner_path(runs_dir, pid).is_file() {
        return true;
    }

    let ws_root = runs_dir.parent().unwrap_or(runs_dir);

    // 3. Check pane_cwd if known
    if let Some(cwd_str) = pane_cwd {
        let cwd_path = Path::new(cwd_str);
        // A `.cos` install keeps its runs under `<workspace>/.cos/runs`, so the pane's own
        // workspace root is one level above `ws_root`.
        let factory_root = if ws_root.file_name().and_then(|n| n.to_str()) == Some(".cos") {
            ws_root.parent()
        } else {
            None
        };
        for root in std::iter::once(ws_root).chain(factory_root) {
            if path_is_within(cwd_path, root) {
                return true;
            }
        }

        // Check project repos from projects/INDEX.md
        for repo in crate::merge::resolve_project_repos_from_workspace(ws_root) {
            if path_is_within(cwd_path, &repo) {
                return true;
            }
        }

        // Check if cwd is inside a worker worktree of this workspace (<repo>/.wt/<name>)
        if let Some(name) = cwd_path.file_name().and_then(|n| n.to_str()) {
            if crate::state::WorkerMetadata::owner_path(runs_dir, name).is_file() {
                return true;
            }
        }

        // cwd is known and does not belong to ws_root or any of its repos/worktrees
        return false;
    }

    // 4. If cwd is unknown but grid_role is cos:
    // If cos.identity.json exists with a different pane_id, then this pane is definitely not the local CoS
    if tokens_val
        .and_then(|t| t.get("grid_role"))
        .and_then(|v| v.as_str())
        == Some("cos")
    {
        if let Ok(content) = std::fs::read_to_string(runs_dir.join(crate::cos::COS_IDENTITY_FILE)) {
            if let Ok(ident) = serde_json::from_str::<serde_json::Value>(&content) {
                if let Some(recorded_pane) = ident.get("pane_id").and_then(|p| p.as_str()) {
                    if recorded_pane != pid {
                        return false;
                    }
                }
            }
        }
    }

    true
}

/// Checks whether an incident record belongs to the consumer's own group (quaqua-owned panes).
pub fn is_incident_in_consumer_group(r: &FarmEventRecord, consumer: &str, runs_dir: &Path) -> bool {
    let (known_leads, known_projects) = resolve_known_leads_and_projects(runs_dir);

    if consumer == "cos" {
        // Direct match with cos owner
        if r.owner.as_deref() == Some("cos") {
            return true;
        }
        // Direct match with internal bus
        if r.actor.as_deref() == Some("quaqua-bus") {
            return true;
        }
        // Check if owner is a registered Lead or any *-lead
        if let Some(owner) = &r.owner {
            if known_leads.contains(owner) || owner.ends_with("-lead") {
                return true;
            }
            if known_projects.contains(owner) {
                return true;
            }
            return false;
        }
        // Project match
        if let Some(project) = &r.project {
            if known_projects.contains(project) {
                return true;
            }
        }

        // When owner is None: check worker metadata in runs_dir
        if let Some(subj) = &r.subject {
            if crate::state::WorkerMetadata::owner_path(runs_dir, subj).is_file() {
                let o = crate::state::WorkerMetadata::load_owner(runs_dir, subj);
                if !o.is_empty() && (o == "cos" || known_leads.contains(&o) || o.ends_with("-lead"))
                {
                    return true;
                }
            }
        }
        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&r.payload_json) {
            if let Some(agent) = v
                .get("agent")
                .or_else(|| v.get("data").and_then(|d| d.get("agent")))
                .and_then(|a| a.as_str())
            {
                if crate::state::WorkerMetadata::owner_path(runs_dir, agent).is_file() {
                    let o = crate::state::WorkerMetadata::load_owner(runs_dir, agent);
                    if !o.is_empty()
                        && (o == "cos" || known_leads.contains(&o) || o.ends_with("-lead"))
                    {
                        return true;
                    }
                }
            }
        }

        // When owner is None: check tokens
        if let Some(tokens) = resolve_pane_tokens_from_record(r) {
            let has_grid_tokens = tokens.contains_key("grid_role")
                || tokens.contains_key("grid_owner")
                || tokens.contains_key("grid_group");
            if !has_grid_tokens {
                return false;
            }
            if let Some(o) = tokens.get("grid_owner").and_then(|v| v.as_str()) {
                if o == "cos" || known_leads.iter().any(|l| l == o) || o.ends_with("-lead") {
                    return true;
                }
            }
            if let Some(role) = tokens.get("grid_role").and_then(|v| v.as_str()) {
                if role == "cos" || role == "orchestrator" || role == "worker" {
                    return true;
                }
            }
            if let Some(grp) = tokens.get("grid_group").and_then(|v| v.as_str()) {
                if known_projects.iter().any(|p| p == grp) {
                    return true;
                }
            }
            return false;
        }

        false
    } else {
        // Consumer is a Lead (e.g. "mu-lead")
        if r.owner.as_deref() == Some(consumer) {
            return true;
        }
        if let Some(project) = &r.project {
            if project == consumer || known_projects.contains(project) {
                return true;
            }
        }
        if let Some(owner) = &r.owner {
            if owner != consumer {
                return false;
            }
        }
        if let Some(subj) = &r.subject {
            if crate::state::WorkerMetadata::owner_path(runs_dir, subj).is_file() {
                let o = crate::state::WorkerMetadata::load_owner(runs_dir, subj);
                if o == consumer {
                    return true;
                }
            }
        }
        if let Some(tokens) = resolve_pane_tokens_from_record(r) {
            if let Some(o) = tokens.get("grid_owner").and_then(|v| v.as_str()) {
                return o == consumer;
            }
        }
        false
    }
}

impl FarmBus {
    /// Loads incidents (events where `incident_key IS NOT NULL`) ordered by `seq` ASC,
    /// strictly after `since_seq`.
    ///
    /// If `consumer` is not `"cos"`, only incidents where `owner` or `project` matches
    /// `consumer`, or where `owner IS NULL`, are returned.
    pub fn load_incidents(
        &self,
        since_seq: u64,
        consumer: &str,
    ) -> rusqlite::Result<Vec<FarmEventRecord>> {
        let conn = self.conn_for_test();
        crate::engine::sqlite::retry_while_busy(|| {
            let mut stmt = conn.prepare_cached(
                r#"
                SELECT t.id, t.ts_ms, t.kind, t.actor, t.to_name, t.thread_id, t.dedupe_key, t.payload_json, t.farm, th.owner
                FROM triggers t
                LEFT JOIN threads th ON t.thread_id = th.id
                WHERE t.source != 'mail' AND t.id > ?1 AND t.dedupe_key IS NOT NULL
                  AND (t.farm = ?2 OR (t.farm IS NULL AND t.id <= ?3))
                ORDER BY t.id ASC
                "#,
            )?;
            let rows = stmt.query_map(
                rusqlite::params![since_seq as i64, &self.farm, self.events_watermark as i64],
                |row| {
                    let thread_id: String = row.get(5)?;
                    let subject = thread_id
                        .strip_prefix("agent:")
                        .unwrap_or(&thread_id)
                        .to_string();
                    let payload_str: String = row
                        .get::<_, Option<String>>(7)?
                        .unwrap_or_else(|| "{}".to_string());
                    let thread_owner: Option<String> = row.get(9)?;

                    let (owner, project) =
                        if let Ok(v) = serde_json::from_str::<serde_json::Value>(&payload_str) {
                            let o = v
                                .get("owner")
                                .and_then(|x| x.as_str())
                                .map(String::from)
                                .or(thread_owner);
                            let p = v.get("project").and_then(|x| x.as_str()).map(String::from);
                            (o, p)
                        } else {
                            (thread_owner, None)
                        };

                    Ok(FarmEventRecord {
                        seq: row.get::<_, i64>(0)? as u64,
                        ts_ms: row.get::<_, i64>(1)? as u64,
                        event_type: row.get(2)?,
                        actor: Some(row.get(3)?),
                        subject: Some(subject),
                        owner,
                        project,
                        incident_key: row.get(6)?,
                        payload_json: payload_str,
                        farm: row.get(8)?,
                    })
                },
            )?;
            let mut res = Vec::new();
            for r in rows {
                let rec = r?;
                if consumer == "cos" {
                    res.push(rec);
                } else {
                    let matches_consumer = rec.owner.as_deref() == Some(consumer)
                        || rec.owner.is_none()
                        || rec.subject.as_deref() == Some(consumer);
                    if matches_consumer {
                        res.push(rec);
                    }
                }
            }
            Ok(res)
        })
    }

    /// Reads the inbox digest for `consumer`:
    /// - Checks lazy unacked mail incidents.
    /// - Reads unacked mail addressed to `consumer`.
    /// - Reads incidents since the consumer's cursor.
    /// - Advances the cursor to the latest event sequence.
    ///
    /// A repeated read with no new events is idempotent: the cursor remains unchanged.
    pub fn inbox_digest(&self, runs_dir: &Path, consumer: &str) -> Result<InboxDigest, String> {
        let prev_cursor = self
            .get_cursor(consumer)
            .map_err(|e| format!("cannot get cursor: {e}"))?
            .unwrap_or(0);

        let prev_mail_cursor = self
            .get_mail_cursor(consumer)
            .map_err(|e| format!("cannot get mail cursor: {e}"))?
            .unwrap_or(0);

        // Check for any unacked mail incidents lazily
        {
            let conn = self.conn_for_test();
            let timeout = crate::bus::mail::get_mail_ack_timeout_ms();
            let _ = crate::bus::mail::check_unacked_incidents(&conn, timeout, &self.farm);
        }

        let unacked_mail =
            crate::bus::mail::read_unacked_mail_since(runs_dir, consumer, prev_mail_cursor)?;
        let raw_incidents = self
            .load_incidents(prev_cursor, consumer)
            .map_err(|e| format!("cannot load incidents: {e}"))?;

        let max_evaluated_seq = raw_incidents
            .iter()
            .map(|e| e.seq)
            .max()
            .unwrap_or(prev_cursor);

        let own_pane = resolve_consumer_pane(consumer);
        let incidents: Vec<_> = raw_incidents
            .into_iter()
            .filter(|r| !is_consumer_own_pane_blocked_noise(r, own_pane.as_deref()))
            .filter(|r| is_incident_in_consumer_group(r, consumer, runs_dir))
            .collect();

        let new_cursor = if max_evaluated_seq > prev_cursor {
            self.set_cursor(consumer, max_evaluated_seq)
                .map_err(|e| format!("cannot set cursor: {e}"))?;
            max_evaluated_seq
        } else {
            prev_cursor
        };

        let new_mail_cursor = if let Some(max_mail_id) = unacked_mail.iter().map(|m| m.id).max() {
            if max_mail_id > prev_mail_cursor {
                self.set_mail_cursor(consumer, max_mail_id)
                    .map_err(|e| format!("cannot set mail cursor: {e}"))?;
                max_mail_id
            } else {
                prev_mail_cursor
            }
        } else {
            prev_mail_cursor
        };

        Ok(InboxDigest {
            unacked_mail,
            incidents,
            prev_cursor,
            new_cursor,
            prev_mail_cursor,
            new_mail_cursor,
        })
    }
    /// Queries the latest event type and incident key recorded for `subject`.
    pub fn latest_event_for_subject(&self, subject: &str) -> Option<(String, Option<String>)> {
        let conn = self.conn_for_test();
        let thread_id = format!("agent:{subject}");
        crate::engine::sqlite::retry_while_busy(|| {
            conn.query_row(
                "SELECT kind, dedupe_key FROM triggers WHERE thread_id = ?1 OR actor = ?2 ORDER BY id DESC LIMIT 1",
                rusqlite::params![&thread_id, subject],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
        })
        .ok()
    }

    /// Checks for new items for `consumer` during `quaqua inbox --follow`.
    ///
    /// Yields only unacked mail with id > `last_seen_mail_id` without bumping deliveries,
    /// and new incidents since cursor.
    pub fn follow_tick(
        &self,
        runs_dir: &Path,
        consumer: &str,
        last_seen_mail_id: &mut u64,
    ) -> Result<InboxDigest, String> {
        let prev_cursor = self
            .get_cursor(consumer)
            .map_err(|e| format!("cannot get cursor: {e}"))?
            .unwrap_or(0);

        {
            let conn = self.conn_for_test();
            let timeout = crate::bus::mail::get_mail_ack_timeout_ms();
            let _ = crate::bus::mail::check_unacked_incidents(&conn, timeout, &self.farm);
        }

        let new_mail = {
            let conn = self.conn_for_test();
            let mut stmt = conn
                .prepare_cached(
                    r#"
                    SELECT t.id, t.dedupe_key, t.ts_ms, t.actor, t.to_name, t.kind, t.payload_json, t.farm
                    FROM triggers t
                    LEFT JOIN cursors c ON c.consumer = t.to_name
                    WHERE t.source = 'mail' AND t.to_name = ?1 AND t.id > ?2
                      AND (c.trigger_id IS NULL OR t.id > c.trigger_id)
                      AND (t.farm = ?3 OR t.farm IS NULL)
                    ORDER BY t.id ASC
                    "#,
                )
                .map_err(|e| e.to_string())?;

            let rows = stmt
                .query_map(
                    rusqlite::params![consumer, *last_seen_mail_id as i64, &self.farm],
                    |row| {
                        let id = row.get::<_, i64>(0)? as u64;
                        let dedupe_key: Option<String> = row.get(1)?;
                        let msg_key = dedupe_key
                            .as_deref()
                            .and_then(|k| k.strip_prefix("mail:"))
                            .unwrap_or("")
                            .to_string();
                        let ts_ms = row.get::<_, i64>(2)? as u64;
                        let from_name: String = row.get(3)?;
                        let to_name: String = row.get::<_, Option<String>>(4)?.unwrap_or_default();
                        let kind: String = row.get(5)?;
                        let body: String = row.get::<_, Option<String>>(6)?.unwrap_or_default();

                        Ok(MailMessage {
                            id,
                            msg_key,
                            ts_ms,
                            from_name,
                            from_role: "lead".to_string(),
                            to_name,
                            kind,
                            project: None,
                            reply_to: None,
                            body,
                            deliveries: 1,
                            last_delivered_ms: Some(ts_ms),
                            acked_ms: None,
                            acked_by: None,
                        })
                    },
                )
                .map_err(|e| e.to_string())?;

            let mut msgs = Vec::new();
            for r in rows {
                msgs.push(r.map_err(|e| e.to_string())?);
            }
            msgs
        };

        if let Some(max_mail_id) = new_mail.iter().map(|m| m.id).max() {
            *last_seen_mail_id = max_mail_id;
        }

        let own_pane = resolve_consumer_pane(consumer);
        let raw_incidents = self
            .load_incidents(prev_cursor, consumer)
            .map_err(|e| format!("cannot load incidents: {e}"))?;

        let max_evaluated_seq = raw_incidents
            .iter()
            .map(|e| e.seq)
            .max()
            .unwrap_or(prev_cursor);
        let incidents: Vec<_> = raw_incidents
            .into_iter()
            .filter(|r| !is_consumer_own_pane_blocked_noise(r, own_pane.as_deref()))
            .filter(|r| is_incident_in_consumer_group(r, consumer, runs_dir))
            .collect();

        let new_cursor = if max_evaluated_seq > prev_cursor {
            self.set_cursor(consumer, max_evaluated_seq)
                .map_err(|e| format!("cannot set cursor: {e}"))?;
            max_evaluated_seq
        } else {
            prev_cursor
        };

        Ok(InboxDigest {
            unacked_mail: new_mail,
            incidents,
            prev_cursor,
            new_cursor,
            prev_mail_cursor: *last_seen_mail_id,
            new_mail_cursor: *last_seen_mail_id,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaneAttribution {
    Local {
        owner: Option<String>,
        project: Option<String>,
        farm: String,
    },
    Foreign,
}

/// Fetches a pane once for attribution. `None` means herdr failed or answered
/// unparseable JSON, and the caller fails open to local.
pub fn resolve_pane_value<H: HerdrOps>(herdr: &H, pane: &str) -> Option<serde_json::Value> {
    match herdr.pane_get(pane) {
        Ok((_ok, text)) => match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(v) => Some(v),
            Err(_) => {
                eprintln!("warning: [resolve_pane_value] unparseable JSON for pane {pane}");
                None
            }
        },
        Err(e) => {
            eprintln!("warning: [resolve_pane_value] cannot get pane {pane}: {e}");
            None
        }
    }
}

/// Evaluates the pane attribution ladder to determine if a pane is Local or Foreign.
/// Ladder order:
/// 1. token `grid_farm == local` -> Local
/// 2. token `grid_farm != local` -> Foreign
/// 3. `cwd` canonical-prefixed by local root -> Local
/// 4. `basename(cwd)` has `runs/<basename>.owner` -> Local
/// 5. no `result.pane` -> Local (fail-open)
pub(crate) fn attribute_pane(
    val: &serde_json::Value,
    local: &str,
    runs_dir: &Path,
) -> PaneAttribution {
    // F1: Foreign only on error.code == pane_not_found; any other error code or shape -> Local (fail-open)
    if let Some(err) = val.get("error") {
        if err.get("code").and_then(|c| c.as_str()) == Some("pane_not_found") {
            return PaneAttribution::Foreign;
        }
        return PaneAttribution::Local {
            owner: None,
            project: None,
            farm: local.to_string(),
        };
    }

    let pane_obj = match val.get("result").and_then(|r| r.get("pane")) {
        Some(p) => p,
        None => {
            return PaneAttribution::Local {
                owner: None,
                project: None,
                farm: local.to_string(),
            };
        }
    };

    let tokens = pane_obj.get("tokens");
    let cwd_str = pane_obj.get("cwd").and_then(|c| c.as_str()).unwrap_or("");
    let cwd_path = Path::new(cwd_str);
    let base_name = if !cwd_str.is_empty() {
        cwd_path.file_name().and_then(|n| n.to_str()).unwrap_or("")
    } else {
        ""
    };

    // F2: check ancestors of cwd for runs/<name>.owner
    let mut ancestor_worker_name: Option<String> = None;
    if !cwd_str.is_empty() {
        for ancestor in cwd_path.ancestors() {
            if let Some(comp) = ancestor.file_name().and_then(|c| c.to_str()) {
                if !comp.is_empty() && runs_dir.join(format!("{comp}.owner")).is_file() {
                    ancestor_worker_name = Some(comp.to_string());
                    break;
                }
            }
        }
    }

    // Ladder 1 & 2: token grid_farm
    let grid_farm_opt = tokens
        .and_then(|t| t.get("grid_farm"))
        .and_then(|f| f.as_str());
    let is_local = if let Some(grid_farm) = grid_farm_opt {
        if grid_farm == local {
            true
        } else {
            return PaneAttribution::Foreign;
        }
    } else {
        // Ladder 3: cwd canonical-prefixed by local root
        let local_root = runs_dir.parent().unwrap_or(runs_dir);
        let canonical_local = local_root
            .canonicalize()
            .unwrap_or_else(|_| local_root.to_path_buf());
        let in_local_root = if !cwd_str.is_empty() {
            let canonical_cwd = cwd_path
                .canonicalize()
                .unwrap_or_else(|_| cwd_path.to_path_buf());
            canonical_cwd.starts_with(&canonical_local)
        } else {
            false
        };

        if in_local_root {
            true
        } else if ancestor_worker_name.is_some() {
            // Ladder 4: ancestor of cwd has runs/<ancestor>.owner -> Local
            true
        } else {
            // None matched -> Foreign
            return PaneAttribution::Foreign;
        }
    };

    if is_local {
        let worker_name = ancestor_worker_name.as_deref().unwrap_or(base_name);
        let owner = tokens
            .and_then(|t| t.get("grid_owner"))
            .and_then(|o| o.as_str())
            .map(String::from)
            .or_else(|| {
                if !worker_name.is_empty() {
                    let owner_file = runs_dir.join(format!("{worker_name}.owner"));
                    std::fs::read_to_string(&owner_file)
                        .ok()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                } else {
                    None
                }
            });

        let project = tokens
            .and_then(|t| t.get("grid_group"))
            .and_then(|g| g.as_str())
            .map(String::from)
            .or_else(|| {
                if !worker_name.is_empty() {
                    let repo_file = runs_dir.join(format!("{worker_name}.repo"));
                    std::fs::read_to_string(&repo_file).ok().and_then(|s| {
                        let trimmed = s.trim();
                        Path::new(trimmed)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .map(String::from)
                    })
                } else {
                    None
                }
            });

        PaneAttribution::Local {
            owner,
            project,
            farm: local.to_string(),
        }
    } else {
        PaneAttribution::Foreign
    }
}

/// A CoS pane of another workspace must not reach this workspace's inbox. Auto-tagging
/// gives every workspace's CoS pane the same `grid_role=cos` tokens, so the farm ladder
/// cannot tell two CoS panes apart on tokens alone; this asks the workspace itself.
fn is_foreign_workspace_cos_pane(pid: &str, pane_val: &serde_json::Value, runs_dir: &Path) -> bool {
    let pane_obj = match pane_val.pointer("/result/pane") {
        Some(p) => p,
        None => return false,
    };
    let tokens = pane_obj.get("tokens");
    match tokens {
        Some(t) if crate::cos::is_cos_tokens(t) => {}
        _ => return false,
    }
    let cwd = pane_obj.get("cwd").and_then(|c| c.as_str());
    !is_pane_in_runs_workspace(pid, cwd, runs_dir, tokens)
}

/// Handles an incoming herdr plugin hook event:
/// - Extracts pane, agent, and status.
/// - On `pane.agent_status_changed` where status is `blocked`, records an incident with
///   incident_key `blocked:<pane>:<status_since>`.
/// - If newly inserted, sends a desktop notification via `herdr notification show`.
/// - Returns `Ok(true)` if an incident was newly recorded and notification sent,
///   `Ok(false)` if duplicate or non-incident event.
pub fn handle_event_hook<H: HerdrOps>(
    runs_dir: &Path,
    event_name: Option<&str>,
    event_json: Option<&str>,
    herdr: &H,
) -> Result<bool, String> {
    let ej = match event_json {
        Some(s) if !s.trim().is_empty() => s,
        _ => return Ok(false),
    };

    let val: serde_json::Value = match serde_json::from_str(ej) {
        Ok(v) => v,
        Err(_) => return Ok(false),
    };

    let ev_name = event_name
        .or_else(|| val.get("type").and_then(|v| v.as_str()))
        .or_else(|| val.get("event").and_then(|v| v.as_str()))
        .unwrap_or("");

    let pid = val
        .get("pane_id")
        .or_else(|| val.get("data").and_then(|d| d.get("pane_id")))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let agent_status = val
        .get("agent_status")
        .or_else(|| val.get("data").and_then(|d| d.get("agent_status")))
        .and_then(|v| v.as_str())
        .unwrap_or("");

    let agent = val
        .get("agent")
        .or_else(|| val.get("data").and_then(|d| d.get("agent")))
        .and_then(|v| v.as_str());

    let subject_str = if !pid.is_empty() {
        pid.to_string()
    } else if let Some(a) = agent {
        if !a.is_empty() {
            a.to_string()
        } else {
            format!("anon-{}", crate::bus::now_ms())
        }
    } else {
        format!("anon-{}", crate::bus::now_ms())
    };

    let db_path = runs_dir.join("quaqua.db");
    let bus = match FarmBus::open(&db_path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!(
                "quaqua events hook: cannot open bus at {}: {e}",
                db_path.display()
            );
            return Ok(false);
        }
    };

    let local_farm = bus.farm().to_string();
    let attribution = if ev_name == "pane.exited" || ev_name == "pane_exited" {
        let (owner, project) = if let Some(a) = agent.filter(|s| !s.is_empty()) {
            let owner = runs_dir
                .join(format!("{a}.owner"))
                .is_file()
                .then(|| {
                    std::fs::read_to_string(runs_dir.join(format!("{a}.owner")))
                        .ok()
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                })
                .flatten();
            let project = runs_dir
                .join(format!("{a}.repo"))
                .is_file()
                .then(|| {
                    std::fs::read_to_string(runs_dir.join(format!("{a}.repo")))
                        .ok()
                        .and_then(|s| {
                            let trimmed = s.trim();
                            Path::new(trimmed)
                                .file_name()
                                .and_then(|n| n.to_str())
                                .map(String::from)
                        })
                })
                .flatten();
            (owner, project)
        } else {
            (None, None)
        };
        PaneAttribution::Local {
            owner,
            project,
            farm: local_farm.clone(),
        }
    } else if !pid.is_empty() {
        match resolve_pane_value(herdr, pid) {
            Some(pane_val) => {
                if is_foreign_workspace_cos_pane(pid, &pane_val, runs_dir) {
                    PaneAttribution::Foreign
                } else {
                    attribute_pane(&pane_val, &local_farm, runs_dir)
                }
            }
            None => PaneAttribution::Local {
                owner: None,
                project: None,
                farm: local_farm.clone(),
            },
        }
    } else {
        PaneAttribution::Local {
            owner: None,
            project: None,
            farm: local_farm.clone(),
        }
    };

    let (owner, project, farm) = match attribution {
        PaneAttribution::Local {
            owner,
            project,
            farm,
        } => (owner, project, farm),
        PaneAttribution::Foreign => {
            return Ok(false);
        }
    };
    if ev_name == "pane.agent_status_changed" || ev_name == "pane_agent_status_changed" {
        if agent_status == "blocked" {
            let incident_key = if let Some(k) = val.get("incident_key").and_then(|v| v.as_str()) {
                k.to_string()
            } else if let Some(ts) = val
                .get("status_since")
                .or_else(|| val.get("data").and_then(|d| d.get("status_since")))
                .or_else(|| val.get("state_labels").and_then(|l| l.get("status_since")))
                .and_then(|v| {
                    v.as_str()
                        .map(String::from)
                        .or_else(|| v.as_i64().map(|n| n.to_string()))
                })
            {
                format!("blocked:{subject_str}:{ts}")
            } else {
                let latest = bus.latest_event_for_subject(&subject_str);
                match &latest {
                    Some((typ, Some(prev_key))) if typ == "blocked" => prev_key.clone(),
                    _ => {
                        let now = crate::bus::now_ms();
                        format!("blocked:{subject_str}:{now}")
                    }
                }
            };

            let mut ev = FarmEvent::new(
                "blocked",
                Some("herdr"),
                Some(&subject_str),
                owner.as_deref(),
                project.as_deref(),
            )
            .with_incident_key(incident_key)
            .with_farm(&farm);

            if !ej.is_empty() && ej != "{}" {
                ev = ev.with_payload_json(ej);
            }

            let inserted = bus
                .record(&ev)
                .map_err(|e| format!("cannot record event: {e}"))?;
            if inserted {
                let title = "quaqua: blocked";
                let body = format!("Agent '{subject_str}' is blocked");
                let _ = herdr.notification_show(title, &body, Some("request"));
                return Ok(true);
            }
            return Ok(false);
        } else if !agent_status.is_empty() {
            let prev = bus.latest_event_for_subject(&subject_str);
            if let Some((typ, _)) = &prev {
                if typ == "blocked" {
                    let ev = FarmEvent::new(
                        "agent_status",
                        Some("herdr"),
                        Some(&subject_str),
                        owner.as_deref(),
                        project.as_deref(),
                    )
                    .with_payload_json(ej)
                    .with_farm(&farm);
                    let _ = bus.record(&ev);
                }
            }
            return Ok(false);
        }
    } else if ev_name == "pane.exited" || ev_name == "pane_exited" {
        let incident_key = val
            .get("incident_key")
            .and_then(|v| v.as_str())
            .map(String::from);

        let mut ev = FarmEvent::new(
            "agent_exited",
            Some("herdr"),
            Some(&subject_str),
            owner.as_deref(),
            project.as_deref(),
        )
        .with_farm(&farm);
        if let Some(key) = incident_key {
            ev = ev.with_incident_key(key);
        }
        if !ej.is_empty() && ej != "{}" {
            ev = ev.with_payload_json(ej);
        }
        let _ = bus.record(&ev);
        return Ok(false);
    }

    Ok(false)
}

/// Entry point for `quaqua events hook` CLI command.
pub fn run_events_hook() -> Result<i32, String> {
    let timeout_ms: u64 = std::env::var("QUAQUA_HOOK_TIMEOUT_MS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1500);

    // Watchdog thread ensuring hard deadline: exits 0 if exceeded
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(timeout_ms));
        eprintln!("quaqua events hook: hard deadline exceeded; exiting 0 to avoid blocking herdr");
        std::process::exit(0);
    });

    let runs_dir = if let Ok(dir) = std::env::var("QUAQUA_RUNS_DIR") {
        std::path::PathBuf::from(dir)
    } else {
        crate::workspace::workspace_root().join("runs")
    };

    let event_name = std::env::var("HERDR_PLUGIN_EVENT").ok();
    let event_json = std::env::var("HERDR_PLUGIN_EVENT_JSON").ok();

    let herdr = crate::herdr::Herdr::new();
    if let Err(e) = handle_event_hook(
        &runs_dir,
        event_name.as_deref(),
        event_json.as_deref(),
        &herdr,
    ) {
        eprintln!("quaqua events hook error: {e}");
    }
    Ok(0)
}
