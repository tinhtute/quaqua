//! Event-driven monitoring, inbox digest with per-reader cursors, and herdr event hook.

use serde::{Deserialize, Serialize};
use std::path::Path;

use super::{FarmBus, FarmEvent, FarmEventRecord};
use crate::bus::mail::MailMessage;
use crate::herdr::HerdrOps;

/// A trigger record from the `triggers` table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerRecord {
    pub id: u64,
    pub ts_ms: u64,
    pub source: String,
    pub kind: String,
    pub actor: String,
    pub to_name: Option<String>,
    pub thread_id: String,
    pub summary: String,
    pub ref_val: Option<String>,
    pub payload_json: String,
    pub dedupe_key: Option<String>,
    pub farm: String,
}
/// Delivery mode for `deliver_page`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliverMode {
    Advance,
    Peek,
    All,
    AllPeek,
    PeekFrom(u64),
}

impl DeliverMode {
    pub fn new(peek: bool, all: bool) -> Self {
        match (peek, all) {
            (false, false) => DeliverMode::Advance,
            (true, false) => DeliverMode::Peek,
            (false, true) => DeliverMode::All,
            (true, true) => DeliverMode::AllPeek,
        }
    }

    pub fn should_advance(&self) -> bool {
        matches!(self, DeliverMode::Advance | DeliverMode::All)
    }

    pub fn is_all(&self) -> bool {
        matches!(self, DeliverMode::All | DeliverMode::AllPeek)
    }
}

/// Delivery output format for `deliver_page`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliverFormat {
    Plain,
    HookUps,
    HookStop,
}

impl DeliverFormat {
    pub fn parse(s: &str) -> Result<Self, String> {
        match s {
            "plain" => Ok(DeliverFormat::Plain),
            "hook-ups" => Ok(DeliverFormat::HookUps),
            "hook-stop" => Ok(DeliverFormat::HookStop),
            other => Err(format!(
                "unknown deliver format '{other}': expected plain, hook-ups, or hook-stop"
            )),
        }
    }
}

/// A checked stdout writer that does not swallow EBADF or sanitized closed stdout.
pub struct SafeStdout;

impl SafeStdout {
    pub fn check_ebadf() -> std::io::Result<()> {
        extern "C" {
            fn fcntl(fd: i32, cmd: i32, ...) -> i32;
        }
        let fl = unsafe { fcntl(1, 3) };
        if fl < 0 {
            return Err(std::io::Error::last_os_error());
        }
        #[cfg(target_os = "linux")]
        if (fl & 3) == 2 && (fl & 0o100000) != 0 {
            if let Ok(target) = std::fs::read_link("/proc/self/fd/1") {
                if target == std::path::Path::new("/dev/null") {
                    return Err(std::io::Error::from_raw_os_error(9));
                }
            }
        }
        Ok(())
    }
}

impl std::io::Write for SafeStdout {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Self::check_ebadf()?;
        if buf.is_empty() {
            return Ok(0);
        }
        extern "C" {
            fn write(fd: i32, buf: *const std::ffi::c_void, count: usize) -> isize;
        }
        let ret = unsafe { write(1, buf.as_ptr() as *const std::ffi::c_void, buf.len()) };
        if ret < 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(ret as usize)
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Self::check_ebadf()
    }
}

/// Outcome of a delivery from `deliver_page`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryOutcome {
    pub prev_cursor: u64,
    pub new_cursor: u64,
    pub rendered_ids: Vec<u64>,
    pub triggers: Vec<TriggerRecord>,
    pub incidents: Vec<FarmEventRecord>,
    pub lines_count: usize,
    pub rendered_text: String,
}

/// Digest of unacked mail and incidents for a consumer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InboxDigest {
    #[serde(default)]
    pub unacked_mail: Vec<MailMessage>,
    pub incidents: Vec<FarmEventRecord>,
    pub prev_cursor: u64,
    pub new_cursor: u64,
    #[serde(default)]
    pub prev_mail_cursor: u64,
    #[serde(default)]
    pub new_mail_cursor: u64,
    #[serde(default)]
    pub triggers: Vec<TriggerRecord>,
    #[serde(default)]
    pub rendered_text: String,
}

impl InboxDigest {
    /// Renders the inbox digest into a human- and agent-readable string (capped at <=20 lines per R4).
    pub fn render(&self, _consumer: &str) -> String {
        self.rendered_text.clone()
    }
}

/// Helper returning line records for triggers: (line_string, trigger_ids_in_line)
pub fn render_triggers_lines(triggers: &[TriggerRecord]) -> Vec<(String, Vec<u64>)> {
    if triggers.is_empty() {
        return Vec::new();
    }

    // Group triggers by thread_id
    let mut thread_map: std::collections::BTreeMap<&str, Vec<&TriggerRecord>> =
        std::collections::BTreeMap::new();
    for t in triggers {
        thread_map.entry(&t.thread_id).or_default().push(t);
    }

    // Sort threads newest-trigger-first (by max trigger id in this batch)
    let mut threads: Vec<(&str, Vec<&TriggerRecord>)> = thread_map.into_iter().collect();
    threads.sort_by(|a, b| {
        let max_a = a.1.iter().map(|t| t.id).max().unwrap_or(0);
        let max_b = b.1.iter().map(|t| t.id).max().unwrap_or(0);
        max_b.cmp(&max_a)
    });

    let mut line_records: Vec<(String, Vec<u64>)> = Vec::new();

    for (_thread_id, mut thread_triggers) in threads {
        // Ensure triggers are in ascending order by id within the thread
        thread_triggers.sort_by_key(|t| t.id);

        // Group consecutive triggers of the same kind
        let mut groups: Vec<Vec<&TriggerRecord>> = Vec::new();
        for t in thread_triggers {
            if let Some(last_group) = groups.last_mut() {
                if last_group[0].kind == t.kind {
                    last_group.push(t);
                    continue;
                }
            }
            groups.push(vec![t]);
        }

        for group in groups {
            let first = group[0];
            let last = *group.last().unwrap();
            let summary_clean: String = last.summary.replace(['\n', '\r'], " ");
            let summary_clean = summary_clean.trim();
            let summary_80: String = summary_clean.chars().take(80).collect();

            let ref_suffix = match &last.ref_val {
                Some(r) if !r.trim().is_empty() => format!(" → {}", r.trim()),
                _ => String::new(),
            };

            let line = if group.len() == 1 {
                format!(
                    "#{} {} {} {} {}{}\n",
                    first.id, first.kind, first.actor, first.thread_id, summary_80, ref_suffix
                )
            } else {
                format!(
                    "#{}..#{} {} ×{} {} {} {}{}\n",
                    first.id,
                    last.id,
                    first.kind,
                    group.len(),
                    last.actor,
                    last.thread_id,
                    summary_80,
                    ref_suffix
                )
            };
            let group_ids: Vec<u64> = group.iter().map(|t| t.id).collect();
            line_records.push((line, group_ids));
        }
    }
    line_records
}

/// Internal helper returning both the rendered trigger digest and the list of trigger IDs actually rendered.
pub fn render_triggers_internal(triggers: &[TriggerRecord], all: bool) -> (String, Vec<u64>) {
    let mut line_records = render_triggers_lines(triggers);

    let overflow = if !all && line_records.len() > 20 {
        let total_lines = line_records.len();
        line_records.truncate(19);
        total_lines - 19
    } else {
        0
    };

    let rendered_ids: Vec<u64> = line_records
        .iter()
        .flat_map(|(_, ids)| ids.iter().copied())
        .collect();
    let mut lines: Vec<String> = line_records.into_iter().map(|(l, _)| l).collect();
    if overflow > 0 {
        lines.push(format!("… +{} more: quaqua mail inbox --all\n", overflow));
    }

    (lines.concat(), rendered_ids)
}

/// Renders triggers into the superagent digest format per spec §5-§6.
pub fn render_triggers(triggers: &[TriggerRecord], all: bool) -> String {
    render_triggers_internal(triggers, all).0
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

fn load_incidents_conn(
    conn: &rusqlite::Connection,
    farm: &str,
    events_watermark: u64,
    since_seq: u64,
    consumer: &str,
) -> rusqlite::Result<Vec<FarmEventRecord>> {
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
        rusqlite::params![since_seq as i64, farm, events_watermark as i64],
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
}

fn load_unread_triggers_conn(
    conn: &rusqlite::Connection,
    farm: &str,
    consumer: &str,
    since_cursor: u64,
) -> rusqlite::Result<Vec<TriggerRecord>> {
    let now = crate::bus::now_ms() as i64;
    let mut stmt = conn.prepare_cached(
        r#"
        SELECT t.id, t.ts_ms, t.source, t.kind, t.actor, t.to_name, t.thread_id, t.summary, t.ref, t.payload_json, t.dedupe_key, t.farm
        FROM triggers t
        LEFT JOIN subscriptions s ON s.thread_id = t.thread_id AND s.consumer = ?1 AND s.until_ms >= ?2
        WHERE t.id > ?3
          AND (t.farm = ?4 OR t.farm IS NULL)
          AND (t.to_name = ?1 OR s.consumer IS NOT NULL)
        ORDER BY t.id ASC
        "#,
    )?;
    let rows = stmt.query_map(
        rusqlite::params![consumer, now, since_cursor as i64, farm],
        |row| {
            Ok(TriggerRecord {
                id: row.get::<_, i64>(0)? as u64,
                ts_ms: row.get::<_, i64>(1)? as u64,
                source: row.get(2)?,
                kind: row.get(3)?,
                actor: row.get(4)?,
                to_name: row.get(5)?,
                thread_id: row.get(6)?,
                summary: row.get(7)?,
                ref_val: row.get(8)?,
                payload_json: row
                    .get::<_, Option<String>>(9)?
                    .unwrap_or_else(|| "{}".to_string()),
                dedupe_key: row.get(10)?,
                farm: row.get::<_, Option<String>>(11)?.unwrap_or_default(),
            })
        },
    )?;
    let mut res = Vec::new();
    for r in rows {
        res.push(r?);
    }
    Ok(res)
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
            load_incidents_conn(
                &conn,
                &self.farm,
                self.events_watermark,
                since_seq,
                consumer,
            )
        })
    }

    /// Loads unread triggers for `consumer` with id > `since_cursor`:
    /// Where `to_name = consumer` OR `consumer` is subscribed to `thread_id` with `until_ms >= now`.
    pub fn load_unread_triggers(
        &self,
        consumer: &str,
        since_cursor: u64,
    ) -> rusqlite::Result<Vec<TriggerRecord>> {
        let conn = self.conn_for_test();
        crate::engine::sqlite::retry_while_busy(|| {
            load_unread_triggers_conn(&conn, &self.farm, consumer, since_cursor)
        })
    }

    /// Delivers an inbox page for `consumer`.
    ///
    /// Selects the longest id-ascending prefix of the consumer's pending set whose
    /// grouped rendering fits in 20 lines (19 + overflow line when items remain).
    /// If `mode` is `All` or `AllPeek`, delivers all pending items without line cap.
    ///
    /// Writes the final bytes to `sink`, flushes, and commits the cursor only if
    /// the write succeeded and `mode` advances.
    pub fn deliver_page<W: std::io::Write>(
        &self,
        consumer: &str,
        mode: DeliverMode,
        format: DeliverFormat,
        sink: &mut W,
    ) -> Result<DeliveryOutcome, String> {
        let prev_cursor = match mode {
            DeliverMode::PeekFrom(c) => c,
            _ => self
                .get_cursor(consumer)
                .map_err(|e| format!("cannot get cursor: {e}"))?
                .unwrap_or(0),
        };

        let runs_dir = self.path.parent().unwrap_or(Path::new("."));
        let (raw_triggers, raw_incidents) = {
            let mut conn = self.conn_for_test();
            crate::engine::sqlite::retry_while_busy(|| {
                let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Deferred)?;
                let trs = load_unread_triggers_conn(&tx, &self.farm, consumer, prev_cursor)?;
                let incs = load_incidents_conn(
                    &tx,
                    &self.farm,
                    self.events_watermark,
                    prev_cursor,
                    consumer,
                )?;
                tx.finish()?;
                Ok((trs, incs))
            })
        }
        .map_err(|e| format!("cannot load pending items: {e}"))?;

        let own_pane = resolve_consumer_pane(consumer);
        let incidents: Vec<FarmEventRecord> = raw_incidents
            .into_iter()
            .filter(|r| !is_consumer_own_pane_blocked_noise(r, own_pane.as_deref()))
            .filter(|r| is_incident_in_consumer_group(r, consumer, runs_dir))
            .collect();

        let trigger_ids: std::collections::HashSet<u64> =
            raw_triggers.iter().map(|t| t.id).collect();

        let max_evaluated = raw_triggers
            .iter()
            .map(|t| t.id)
            .max()
            .max(incidents.iter().map(|r| r.seq).max())
            .unwrap_or(prev_cursor);

        #[derive(Clone)]
        enum PendingItem {
            Trigger(TriggerRecord),
            Incident(FarmEventRecord),
        }
        impl PendingItem {
            fn id(&self) -> u64 {
                match self {
                    PendingItem::Trigger(t) => t.id,
                    PendingItem::Incident(i) => i.seq,
                }
            }
        }

        let mut pending: Vec<PendingItem> =
            Vec::with_capacity(raw_triggers.len() + incidents.len());
        for t in &raw_triggers {
            pending.push(PendingItem::Trigger(t.clone()));
        }
        for inc in &incidents {
            if !trigger_ids.contains(&inc.seq) {
                pending.push(PendingItem::Incident(inc.clone()));
            }
        }
        pending.sort_by_key(|item| item.id());

        let n = pending.len();
        if n == 0 {
            return Ok(DeliveryOutcome {
                prev_cursor,
                new_cursor: prev_cursor,
                rendered_ids: Vec::new(),
                triggers: Vec::new(),
                incidents: Vec::new(),
                lines_count: 0,
                rendered_text: String::new(),
            });
        }

        fn render_slice(
            items: &[PendingItem],
        ) -> (
            Vec<(String, Vec<u64>)>,
            Vec<TriggerRecord>,
            Vec<FarmEventRecord>,
        ) {
            let mut trs = Vec::new();
            let mut incs = Vec::new();
            for item in items {
                match item {
                    PendingItem::Trigger(t) => trs.push(t.clone()),
                    PendingItem::Incident(i) => incs.push(i.clone()),
                }
            }
            let mut line_records = render_triggers_lines(&trs);
            for ev in &incs {
                let key = ev.incident_key.as_deref().unwrap_or("none");
                let actor = ev.actor.as_deref().unwrap_or("-");
                let subj = ev.subject.as_deref().unwrap_or("-");
                let line = format!(
                    "! #{} {} {} [{} -> {}]\n",
                    ev.seq, ev.event_type, key, actor, subj
                );
                line_records.push((line, vec![ev.seq]));
            }
            (line_records, trs, incs)
        }

        let (k, overflow) = if mode.is_all() {
            (n, 0)
        } else {
            let mut line_counts = Vec::with_capacity(n);
            let mut last_kind_by_thread: std::collections::HashMap<&str, &str> =
                std::collections::HashMap::new();
            let mut cur_lines = 0;
            for item in &pending {
                match item {
                    PendingItem::Incident(_) => {
                        cur_lines += 1;
                    }
                    PendingItem::Trigger(t) => {
                        if last_kind_by_thread.get(t.thread_id.as_str()) != Some(&t.kind.as_str()) {
                            cur_lines += 1;
                            last_kind_by_thread.insert(t.thread_id.as_str(), t.kind.as_str());
                        }
                    }
                }
                line_counts.push(cur_lines);
            }

            let mut best_k = 1;
            for candidate in 1..=n {
                let count = line_counts[candidate - 1];
                if candidate == n {
                    if count <= 20 {
                        best_k = n;
                    }
                } else if count <= 19 {
                    best_k = candidate;
                } else {
                    break;
                }
            }
            let overflow = if best_k < n { n - best_k } else { 0 };
            (best_k, overflow)
        };

        let (line_records, selected_triggers, _selected_incidents) = render_slice(&pending[..k]);
        let rendered_ids: Vec<u64> = line_records
            .iter()
            .flat_map(|(_, ids)| ids.iter().copied())
            .collect();
        let mut lines: Vec<String> = line_records.into_iter().map(|(l, _)| l).collect();
        if overflow > 0 {
            lines.push(format!("… +{} more: quaqua mail inbox --all\n", overflow));
        }

        let new_cursor = if k == n {
            max_evaluated
        } else {
            pending[..k]
                .iter()
                .map(|item| item.id())
                .max()
                .unwrap_or(prev_cursor)
        };

        let rendered_text = lines.concat();
        let lines_count = lines.len();

        let final_bytes = match format {
            DeliverFormat::Plain => rendered_text.as_bytes().to_vec(),
            DeliverFormat::HookUps => {
                let val = serde_json::json!({
                    "hookSpecificOutput": {
                        "hookEventName": "UserPromptSubmit",
                        "additionalContext": rendered_text,
                    }
                });
                let mut s = serde_json::to_string(&val).map_err(|e| e.to_string())?;
                s.push('\n');
                s.into_bytes()
            }
            DeliverFormat::HookStop => {
                let val = serde_json::json!({
                    "decision": "block",
                    "reason": "New lead mail received",
                    "hookSpecificOutput": {
                        "hookEventName": "Stop",
                        "additionalContext": rendered_text,
                    }
                });
                let mut s = serde_json::to_string(&val).map_err(|e| e.to_string())?;
                s.push('\n');
                s.into_bytes()
            }
        };

        sink.write_all(&final_bytes)
            .map_err(|e| format!("cannot write to sink: {e}"))?;
        sink.flush()
            .map_err(|e| format!("cannot flush sink: {e}"))?;

        if mode.should_advance() && new_cursor > prev_cursor {
            self.set_cursor(consumer, new_cursor)
                .map_err(|e| format!("cannot set cursor: {e}"))?;
        }

        let delivered_incidents: Vec<FarmEventRecord> = incidents
            .into_iter()
            .filter(|i| i.seq <= new_cursor)
            .collect();

        Ok(DeliveryOutcome {
            prev_cursor,
            new_cursor,
            rendered_ids,
            triggers: selected_triggers,
            incidents: delivered_incidents,
            lines_count,
            rendered_text,
        })
    }

    /// Reads the inbox digest for `consumer`:
    /// - Delegates to `deliver_page` in Peek mode (never advances cursor into memory).
    pub fn inbox_digest(&self, runs_dir: &Path, consumer: &str) -> Result<InboxDigest, String> {
        self.inbox_digest_peek(runs_dir, consumer, true)
    }

    /// Reads the inbox digest for `consumer`:
    /// - Delegates to `deliver_page` in Peek mode (never advances cursor into memory).
    pub fn inbox_digest_peek(
        &self,
        _runs_dir: &Path,
        consumer: &str,
        _peek: bool,
    ) -> Result<InboxDigest, String> {
        let mode = DeliverMode::Peek;
        let mut buf = Vec::new();
        let outcome = self.deliver_page(consumer, mode, DeliverFormat::Plain, &mut buf)?;
        Ok(InboxDigest {
            unacked_mail: Vec::new(),
            incidents: outcome.incidents,
            prev_cursor: outcome.prev_cursor,
            new_cursor: outcome.new_cursor,
            prev_mail_cursor: outcome.prev_cursor,
            new_mail_cursor: outcome.new_cursor,
            triggers: outcome.triggers,
            rendered_text: outcome.rendered_text,
        })
    }
    /// Queries the latest event type and incident key recorded for `subject`.
    pub fn latest_event_for_subject(&self, subject: &str) -> Option<(String, Option<String>)> {
        let conn = self.conn_for_test();
        let thread_id = format!("agent:{subject}");
        crate::engine::sqlite::retry_while_busy(|| {
            conn.query_row(
                "SELECT kind, dedupe_key FROM triggers WHERE source != 'mail' AND thread_id = ?1 ORDER BY id DESC LIMIT 1",
                rusqlite::params![&thread_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
        })
        .ok()
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

/// `source_state` key holding the highest trigger id already nudged for a consumer.
fn wake_state_key(consumer: &str) -> String {
    format!("wake:{consumer}")
}

/// Highest trigger id already nudged for `consumer`; 0 when it was never nudged.
fn wake_watermark(bus: &FarmBus, consumer: &str) -> u64 {
    let key = wake_state_key(consumer);
    let conn = bus.conn_for_test();
    conn.query_row(
        "SELECT value FROM source_state WHERE source = ?1",
        rusqlite::params![key],
        |r| r.get::<_, String>(0),
    )
    .ok()
    .and_then(|s| s.trim().parse().ok())
    .unwrap_or(0)
}

fn set_wake_watermark(bus: &FarmBus, consumer: &str, trigger_id: u64) -> rusqlite::Result<()> {
    let key = wake_state_key(consumer);
    let value = trigger_id.to_string();
    let conn = bus.conn_for_test();
    crate::engine::sqlite::retry_while_busy(|| {
        conn.execute(
            "INSERT INTO source_state (source, value) VALUES (?1, ?2)
             ON CONFLICT(source) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, value],
        )?;
        Ok(())
    })
}

/// The single line typed into an idle agent's pane. Always one line: newlines in
/// the actor or kind are folded to spaces.
fn wake_nudge_line(count: usize, latest: &TriggerRecord) -> String {
    let actor = latest.actor.replace(['\n', '\r'], " ");
    let kind = latest.kind.replace(['\n', '\r'], " ");
    let plural = if count == 1 { "" } else { "s" };
    format!(
        "quaqua: {count} new trigger{plural}, latest #{} {kind} from {actor}. Run: quaqua mail inbox",
        latest.id
    )
}

/// Agent name that owns `pane_id`, from one `herdr agent list`.
///
/// A `pane.agent_status_changed` payload names the harness kind in `agent`
/// (`"omp"`, `"claude"`), never the agent, so the name has to come from the
/// pane the event carries.
fn agent_name_for_pane<H: HerdrOps>(herdr: &H, pane_id: &str) -> Option<String> {
    if pane_id.is_empty() {
        return None;
    }
    let val = crate::lead::tag::get_agent_list_json(herdr)?;
    val.pointer("/result/agents")?
        .as_array()?
        .iter()
        .find(|a| a.get("pane_id").and_then(|p| p.as_str()) == Some(pane_id))
        .and_then(|a| a.get("name").and_then(|n| n.as_str()))
        .filter(|n| !n.is_empty())
        .map(str::to_string)
}

/// Nudges an idle consumer about its unread triggers: one `pane send-text` line plus
/// Enter into the agent's own pane, at most once per batch.
///
/// A batch is the consumer's unread set (`to_name = consumer`, or a live subscription)
/// above its read cursor. The `wake:<consumer>` watermark in `source_state` records the
/// newest trigger id already announced, so a second hook with no newer trigger sends
/// nothing even though the read cursor has not moved. The watermark is claimed before
/// the send: a crash between the two costs one missed nudge (the next trigger nudges
/// again), never a second turn in the same batch.
///
/// Never nudges `cos` (that pane is where anh iu types) and never nudges an agent that
/// is not idle/done — `working` has its own turn, `blocked` is sitting on a prompt.
pub fn wake_idle_consumer<H: HerdrOps>(
    bus: &FarmBus,
    consumer: &str,
    herdr: &H,
) -> Result<bool, String> {
    let consumer = consumer.trim();
    if consumer.is_empty() || consumer == "cos" {
        return Ok(false);
    }

    let cursor = bus
        .get_cursor(consumer)
        .map_err(|e| format!("cannot get cursor for {consumer}: {e}"))?
        .unwrap_or(0);
    let unread = bus
        .load_unread_triggers(consumer, cursor)
        .map_err(|e| format!("cannot load unread triggers for {consumer}: {e}"))?;
    let latest = match unread.iter().max_by_key(|t| t.id) {
        Some(t) => t,
        None => return Ok(false),
    };
    let prev_watermark = wake_watermark(bus, consumer);
    if latest.id <= prev_watermark {
        return Ok(false);
    }

    let agent_json = match herdr.agent_get(consumer) {
        Ok((true, body)) => body,
        _ => return Ok(false),
    };
    let agent: serde_json::Value = match serde_json::from_str(&agent_json) {
        Ok(v) => v,
        Err(_) => return Ok(false),
    };
    let status = agent
        .pointer("/result/agent/agent_status")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if status != "idle" && status != "done" {
        return Ok(false);
    }
    let pane = agent
        .pointer("/result/agent/pane_id")
        .and_then(|v| v.as_str())
        .unwrap_or("");
    if pane.is_empty() {
        return Ok(false);
    }

    set_wake_watermark(bus, consumer, latest.id)
        .map_err(|e| format!("cannot claim wake watermark for {consumer}: {e}"))?;

    let line = wake_nudge_line(unread.len(), latest);
    let sent = herdr
        .run(&["pane", "send-text", pane, &line])
        .and_then(|out| {
            if out.status.success() {
                herdr.pane_send_keys(pane, "Enter")
            } else {
                Ok(out)
            }
        });
    match sent {
        Ok(out) if out.status.success() => Ok(true),
        _ => {
            let _ = set_wake_watermark(bus, consumer, prev_watermark);
            Ok(false)
        }
    }
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
            // R3: an agent that just went idle picks up whatever arrived while it worked.
            if agent_status == "idle" || agent_status == "done" {
                if let Some(name) = agent_name_for_pane(herdr, pid) {
                    let _ = wake_idle_consumer(&bus, &name, herdr);
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
