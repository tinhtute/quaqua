//! Inter-team and worker mail: storage, verbs, dual-write, and fallback.
//!
//! Stores messages in the `mail` table in `runs/quaqua.db`.
//! Verbs:
//! - `send`: inserts row + `mail_sent` farm event in one transaction; dual-writes to markdown for leads;
//!   falls back to markdown append on missing/corrupt database.
//! - `inbox`: reads unacked rows, bumps deliveries, marks redeliveries.
//! - `ack`: acknowledges unacked mail; caller must be recipient.
//! - `import`: idempotent import of lead markdown inboxes; msg_key = sha256(header_line).
//! - `ls`: lists mail, with `--md` rendering the markdown inbox view.

use super::FarmBus;
use crate::herdr::HerdrOps;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Maximum body length allowed by SQLite schema CHECK (length(body) <= 4096).
pub const MAX_BODY_LEN: usize = 4096;

/// Default mail ack timeout: 30 minutes in milliseconds.
pub const DEFAULT_MAIL_ACK_TIMEOUT_MS: u64 = 30 * 60 * 1000;

/// Pure Rust SHA-256 implementation (FIPS 180-4) without external dependencies.
#[allow(clippy::chunks_exact_to_as_chunks)]
pub fn sha256_hex(data: &[u8]) -> String {
    let mut h0: u32 = 0x6a09e667;
    let mut h1: u32 = 0xbb67ae85;
    let mut h2: u32 = 0x3c6ef372;
    let mut h3: u32 = 0xa54ff53a;
    let mut h4: u32 = 0x510e527f;
    let mut h5: u32 = 0x9b05688c;
    let mut h6: u32 = 0x1f83d9ab;
    let mut h7: u32 = 0x5be0cd19;

    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut msg = Vec::with_capacity(data.len() + 64);
    msg.extend_from_slice(data);
    msg.push(0x80);
    while (msg.len() % 64) != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }

        let mut a = h0;
        let mut b = h1;
        let mut c = h2;
        let mut d = h3;
        let mut e = h4;
        let mut f = h5;
        let mut g = h6;
        let mut h = h7;

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        h0 = h0.wrapping_add(a);
        h1 = h1.wrapping_add(b);
        h2 = h2.wrapping_add(c);
        h3 = h3.wrapping_add(d);
        h4 = h4.wrapping_add(e);
        h5 = h5.wrapping_add(f);
        h6 = h6.wrapping_add(g);
        h7 = h7.wrapping_add(h);
    }

    format!(
        "{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}{:08x}",
        h0, h1, h2, h3, h4, h5, h6, h7
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = y + if m <= 2 { 1 } else { 0 };
    (y, m, d)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = y - if m <= 2 { 1 } else { 0 };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = (y - era * 400) as u32;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + (doe as i64) - 719468
}

/// Formats millisecond epoch timestamp as ISO-8601 UTC string (`YYYY-MM-DDTHH:MM:SSZ`).
pub fn format_iso_timestamp(ts_ms: u64) -> String {
    let total_secs = ts_ms / 1000;
    let days = (total_secs / 86400) as i64;
    let rem_secs = (total_secs % 86400) as u32;
    let hours = rem_secs / 3600;
    let minutes = (rem_secs % 3600) / 60;
    let seconds = rem_secs % 60;

    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

/// Parses an ISO-8601 / RFC-3339 timestamp into millisecond epoch timestamp.
/// Handles `Z` or timezone offsets (`+HH:MM` / `-HH:MM`).
pub fn parse_iso_timestamp(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.len() < 19 {
        return None;
    }
    let (dt, tz_part) = if let Some(idx) = s.find('Z') {
        (&s[..idx], "+00:00")
    } else if let Some(idx) = s[19..].find('+').or_else(|| s[19..].find('-')) {
        let abs_idx = 19 + idx;
        (&s[..abs_idx], &s[abs_idx..])
    } else {
        (s, "+00:00")
    };

    let parts: Vec<&str> = dt.split('T').collect();
    if parts.len() != 2 {
        return None;
    }
    let date_parts: Vec<&str> = parts[0].split('-').collect();
    let time_parts: Vec<&str> = parts[1].split(':').collect();
    if date_parts.len() != 3 || time_parts.len() != 3 {
        return None;
    }

    let year: i64 = date_parts[0].parse().ok()?;
    let month: u32 = date_parts[1].parse().ok()?;
    let day: u32 = date_parts[2].parse().ok()?;
    let hour: u32 = time_parts[0].parse().ok()?;
    let min: u32 = time_parts[1].parse().ok()?;
    let sec: u32 = time_parts[2].parse().ok()?;

    let days = days_from_civil(year, month, day);
    let mut secs = days * 86400 + (hour as i64) * 3600 + (min as i64) * 60 + (sec as i64);

    // Apply timezone offset to get UTC
    if tz_part.len() >= 5 {
        let sign = if tz_part.starts_with('+') { 1 } else { -1 };
        let offset_str = &tz_part[1..];
        let offset_parts: Vec<&str> = offset_str.split(':').collect();
        if offset_parts.len() == 2 {
            let tz_h: i64 = offset_parts[0].parse().ok()?;
            let tz_m: i64 = offset_parts[1].parse().ok()?;
            let offset_secs = sign * (tz_h * 3600 + tz_m * 60);
            secs -= offset_secs;
        }
    }

    if secs < 0 {
        return None;
    }
    Some((secs as u64) * 1000)
}

/// A stored mail message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MailMessage {
    pub id: u64,
    pub msg_key: String,
    pub ts_ms: u64,
    pub from_name: String,
    pub from_role: String,
    pub to_name: String,
    pub kind: String,
    pub project: Option<String>,
    pub reply_to: Option<u64>,
    pub body: String,
    pub deliveries: u64,
    pub last_delivered_ms: Option<u64>,
    pub acked_ms: Option<u64>,
    pub acked_by: Option<String>,
}

/// Options for sending mail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendMailOpts {
    pub to: String,
    pub kind: String,
    pub body: String,
    pub project: Option<String>,
    pub reply_to: Option<u64>,
    pub client_id: Option<String>,
}

/// Result of sending mail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendMailResult {
    pub id: u64,
    pub already_sent: bool,
}

/// Result of importing inbox files.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ImportResult {
    pub total: usize,
    pub imported: usize,
    pub skipped: usize,
}

/// Returns true if `name` is a safe single-component path name that cannot escape `runs/`.
pub fn is_safe_inbox_name(name: &str) -> bool {
    let p = Path::new(name);
    if p.components().count() != 1 {
        return false;
    }
    match p.components().next() {
        Some(std::path::Component::Normal(s)) => {
            if let Some(s) = s.to_str() {
                !s.is_empty()
                    && !s.starts_with('.')
                    && !s.contains('/')
                    && !s.contains('\\')
                    && !s.contains("..")
                    && s.chars()
                        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            } else {
                false
            }
        }
        _ => false,
    }
}

/// Helper to find a registered Lead's slug for a given pane ID from herdr agent list and projects/INDEX.md.
fn try_find_lead_slug_for_pane<H: HerdrOps>(herdr: &H, pane_id: &str) -> Option<String> {
    let agents_val = crate::lead::tag::get_agent_list_json(herdr)?;
    let agents = agents_val
        .pointer("/result/agents")
        .and_then(|a| a.as_array())?;
    let mut candidate = None;
    for agent in agents {
        let p = agent.get("pane_id").and_then(|p| p.as_str()).unwrap_or("");
        if p == pane_id {
            if let Some(name) = agent.get("name").and_then(|n| n.as_str()) {
                if let Some(slug) = name.strip_suffix("-lead") {
                    candidate = Some((slug.to_string(), name.to_string()));
                    break;
                }
            }
        }
    }
    let (slug, lead_name) = candidate?;

    let ws_root = crate::workspace::workspace_root_checked().ok()?;
    let index_file = ws_root.join("projects/INDEX.md");
    let content = std::fs::read_to_string(&index_file).ok()?;
    let rows = crate::worker::leadgate::index_rows(&content);
    let is_registered = rows.iter().any(|r| r.slug == slug && r.owner == lead_name);
    if is_registered {
        Some(slug)
    } else {
        None
    }
}

/// Resolves the sender identity (name, role) strictly from harness (env only under QUAQUA_TEST=1).
/// If the caller is on an untagged registered Lead pane, automatically self-heals by verifying
/// consistency with projects/INDEX.md and re-stamping orchestrator tokens. On failure, names the pane and the repair command.
pub fn resolve_sender<H: HerdrOps>(herdr: &H, runs_dir: &Path) -> Result<(String, String), String> {
    let caller = crate::identity::resolve_caller(herdr, runs_dir);
    if caller.role != crate::identity::Role::Unknown {
        return Ok((caller.name, caller.role.to_string()));
    }

    if let Ok(pane_id) = std::env::var("HERDR_PANE_ID") {
        let pane_id = pane_id.trim();
        if !pane_id.is_empty() {
            if let Some(slug) = try_find_lead_slug_for_pane(herdr, pane_id) {
                let farm = crate::workspace::farm_of_runs_dir(runs_dir);
                if crate::lead::tag::tag_lead_pane(herdr, &slug, pane_id, &farm).is_ok() {
                    let healed = crate::identity::resolve_caller(herdr, runs_dir);
                    if healed.role != crate::identity::Role::Unknown {
                        return Ok((healed.name, healed.role.to_string()));
                    }
                }
                return Err(format!(
                    "cannot determine sender identity from harness (unresolved caller on pane {pane_id}); run 'quaqua lead repair {slug}' to re-stamp tokens"
                ));
            }
            return Err(format!(
                "cannot determine sender identity from harness (unresolved caller on pane {pane_id}); run 'quaqua lead repair <slug>' to re-stamp tokens"
            ));
        }
    }

    Err("cannot determine sender identity from harness (unresolved caller); run 'quaqua lead repair <slug>' to re-stamp tokens".to_string())
}

/// Refuses mail when the caller's pane belongs to another farm than `runs_dir`.
///
/// The farm answer comes from the same ladder ingest uses (`bus::events::attribute_pane`),
/// so a pane carrying `grid_farm`, a pane whose `cwd` sits in another workspace, and a
/// dead pane are all rejected — while a caller with no pane (`HERDR_PANE_ID` unset) and a
/// pane herdr cannot describe pass through, because the ladder fails open on an unreadable
/// answer and the notification/mail channel must not go dark when herdr hiccups.
fn refuse_foreign_caller_farm<H: HerdrOps>(herdr: &H, runs_dir: &Path) -> Result<(), String> {
    let pane_id = match std::env::var("HERDR_PANE_ID") {
        Ok(p) if !p.trim().is_empty() => p.trim().to_string(),
        _ => return Ok(()),
    };
    let db_farm = crate::workspace::farm_of_runs_dir(runs_dir);
    let attribution = match super::events::resolve_pane_value(herdr, &pane_id) {
        Some(val) => super::events::attribute_pane(&val, &db_farm, runs_dir),
        None => return Ok(()),
    };
    match attribution {
        super::events::PaneAttribution::Foreign => Err(format!(
            "error: mail send refused: caller pane {pane_id} is not in farm '{db_farm}' \
             (database: {}); set QUAQUA_HOME to the caller's workspace, or send from inside it",
            runs_dir.join("quaqua.db").display()
        )),
        super::events::PaneAttribution::Local { .. } => Ok(()),
    }
}

/// Escapes a single line of mail body so that lines starting with `## ` or `<!--`
/// cannot be mistaken for markdown entry headers or metadata comments.
pub fn escape_body_line(line: &str) -> String {
    let backslashes = line.chars().take_while(|&c| c == '\\').count();
    let after_slashes = &line[backslashes..];
    if after_slashes.starts_with("## ") || after_slashes.starts_with("<!--") {
        format!("\\{line}")
    } else {
        line.to_string()
    }
}

/// Escapes a mail body for markdown inbox storage.
pub fn escape_body(body: &str) -> String {
    body.lines()
        .map(escape_body_line)
        .collect::<Vec<_>>()
        .join("\n")
}

/// Unescapes a single line of mail body imported from markdown storage.
pub fn unescape_body_line(line: &str) -> &str {
    let backslashes = line.chars().take_while(|&c| c == '\\').count();
    let after_slashes = &line[backslashes..];
    if backslashes > 0 && (after_slashes.starts_with("## ") || after_slashes.starts_with("<!--")) {
        &line[1..]
    } else {
        line
    }
}

/// Checks whether an entry in a markdown inbox already contains the given `msg_key`
/// in its real marker position (the fixed metadata lines directly following a `## ` header).
pub fn has_msg_key_at_marker_position(content: &str, target_key: &str) -> bool {
    let mut after_header = false;
    let mut metadata_checked = 0;

    for line in content.lines() {
        if line.starts_with("## ") {
            after_header = true;
            metadata_checked = 0;
            continue;
        }
        if after_header {
            if metadata_checked < 2 {
                let trimmed = line.trim();
                if trimmed.starts_with("<!-- msg_key:") && trimmed.ends_with("-->") {
                    let key = trimmed["<!-- msg_key:".len()..trimmed.len() - "-->".len()].trim();
                    if key == target_key {
                        return true;
                    }
                    metadata_checked += 1;
                    continue;
                }
                if trimmed.starts_with("<!-- to:") && trimmed.ends_with("-->") {
                    metadata_checked += 1;
                    continue;
                }
            }
            after_header = false;
        }
    }
    false
}

/// Appends an entry to `<runs_dir>/<target_inbox>.inbox.md`, ensuring idempotence via msg_key.
pub fn append_to_markdown_inbox(
    runs_dir: &Path,
    target_inbox: &str,
    from_name: &str,
    to_name: &str,
    iso_time: &str,
    kind: &str,
    body: &str,
    msg_key: &str,
) -> Result<(), String> {
    if !is_safe_inbox_name(target_inbox) {
        return Err(format!(
            "invalid target inbox name '{target_inbox}': cannot escape runs/"
        ));
    }
    if !is_safe_inbox_name(from_name) {
        return Err(format!(
            "invalid from_name '{from_name}': cannot escape runs/"
        ));
    }
    if !is_safe_inbox_name(to_name) {
        return Err(format!("invalid to_name '{to_name}': cannot escape runs/"));
    }

    let inbox_path = runs_dir.join(format!("{target_inbox}.inbox.md"));
    let key_marker = format!("<!-- msg_key: {msg_key} -->");
    let to_marker = format!("<!-- to: {to_name} -->");

    if inbox_path.exists() {
        if let Ok(existing) = std::fs::read_to_string(&inbox_path) {
            if has_msg_key_at_marker_position(&existing, msg_key) {
                return Ok(());
            }
        }
    }

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&inbox_path)
        .map_err(|e| format!("cannot open {}: {e}", inbox_path.display()))?;

    let header = if to_name == "cos" {
        format!("## {iso_time} {from_name} {kind}")
    } else {
        format!("## {iso_time} {from_name} -> {to_name} {kind}")
    };

    use std::io::Write;
    let entry = if body.trim().is_empty() {
        format!("{header}\n{key_marker}\n{to_marker}\n\n")
    } else {
        let escaped = escape_body(body);
        format!(
            "{header}\n{key_marker}\n{to_marker}\n{}\n",
            escaped.trim_end()
        )
    };
    file.write_all(entry.as_bytes())
        .map_err(|e| format!("cannot write to {}: {e}", inbox_path.display()))?;
    Ok(())
}

/// Returns configured mail ack timeout in milliseconds, defaulting to 30 minutes.
pub fn get_mail_ack_timeout_ms() -> u64 {
    if let Ok(cfg) = crate::config::load_team_config() {
        if let Ok(secs) = crate::config::parse_duration(&cfg.timers.mail_ack) {
            return secs.saturating_mul(1000);
        }
    }
    DEFAULT_MAIL_ACK_TIMEOUT_MS
}

/// Lazily inserts `mail_unacked:<id>` incident into `farm_events` for rows unacked past `ack_timeout_ms`.
pub fn check_unacked_incidents(
    conn: &rusqlite::Connection,
    ack_timeout_ms: u64,
    farm: &str,
) -> rusqlite::Result<usize> {
    let now = crate::bus::now_ms();
    let cutoff = now.saturating_sub(ack_timeout_ms);
    let mut stmt = conn.prepare_cached(
        r#"
        SELECT t.id, t.ts_ms, t.actor, t.to_name, t.kind, t.farm
        FROM triggers t
        LEFT JOIN cursors c ON c.consumer = t.to_name
        WHERE t.source = 'mail'
          AND t.ts_ms <= ?1
          AND (c.trigger_id IS NULL OR c.trigger_id < t.id)
          AND (t.farm = ?2 OR t.farm IS NULL)
        "#,
    )?;
    let rows = stmt.query_map(rusqlite::params![cutoff as i64, farm], |row| {
        Ok((
            row.get::<_, i64>(0)? as u64,
            row.get::<_, i64>(1)? as u64,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?.unwrap_or_default(),
            row.get::<_, String>(4)?,
            row.get::<_, Option<String>>(5)?,
        ))
    })?;

    let mut incidents = Vec::new();
    for r in rows {
        incidents.push(r?);
    }
    drop(stmt);

    let mut inserted_count = 0;
    for (id, ts_ms, from_name, to_name, kind, row_farm) in incidents {
        let incident_key = format!("mail_unacked:{id}");
        let payload = serde_json::json!({
            "mail_id": id,
            "kind": kind,
            "unacked_ms": now.saturating_sub(ts_ms),
        })
        .to_string();

        let farm_to_insert = row_farm.as_deref().unwrap_or(farm);
        let thread_id = format!("agent:{to_name}");
        let summary = format!("mail_unacked #{id} to {to_name}");
        let summary_trunc: String = summary.chars().take(80).collect();

        let _ = conn.execute(
            "INSERT OR IGNORE INTO threads (id, owner, state, last_trigger_ms) VALUES (?1, ?2, 'open', ?3)",
            rusqlite::params![&thread_id, &to_name, now as i64],
        );

        let mut insert_stmt = conn.prepare_cached(
            r#"
            INSERT OR IGNORE INTO triggers (
                ts_ms, source, kind, actor, to_name, thread_id, summary, ref, payload_json, dedupe_key, farm
            ) VALUES (?1, 'watchdog', 'mail_unacked', ?2, ?3, ?4, ?5, NULL, ?6, ?7, ?8)
            "#,
        )?;
        let rows_affected = insert_stmt.execute(rusqlite::params![
            now as i64,
            &from_name,
            &to_name,
            &thread_id,
            &summary_trunc,
            &payload,
            &incident_key,
            farm_to_insert,
        ])?;
        if rows_affected > 0 {
            inserted_count += 1;
            let herdr = crate::herdr::Herdr::new();
            let title = "quaqua mail: unacked";
            let body =
                format!("Mail #{id} from {from_name} to {to_name} ({kind}) is unacknowledged");
            let _ = herdr.notification_show(title, &body, Some("request"));
        }
    }
    Ok(inserted_count)
}

/// Sends mail: inserts mail row + `mail_sent` event in one transaction; dual-writes to markdown for leads;
/// falls back to markdown append on missing/corrupt database with warning.
pub fn send_mail<H: HerdrOps>(
    runs_dir: &Path,
    opts: &SendMailOpts,
    herdr: &H,
) -> Result<SendMailResult, String> {
    if opts.body.chars().count() > MAX_BODY_LEN {
        return Err(format!(
            "error: mail body exceeds {} characters (length: {})",
            MAX_BODY_LEN,
            opts.body.chars().count()
        ));
    }

    // A pane from another farm must not write into this database, not even through the
    // markdown fallback: `mail send` typed in a foreign workspace's pane resolves the DB
    // from *this* workspace (`default_runs_dir` is QUAQUA_HOME-aware, never pane-aware).
    refuse_foreign_caller_farm(herdr, runs_dir)?;

    let (from_name, from_role) = resolve_sender(herdr, runs_dir)?;
    let now = crate::bus::now_ms();
    let iso_time = format_iso_timestamp(now);

    let client_id = opts
        .client_id
        .clone()
        .unwrap_or_else(|| sha256_hex(opts.body.as_bytes()));
    let msg_key = sha256_hex(format!("{from_name}|{client_id}").as_bytes());

    let _project = opts.project.clone().or_else(|| {
        if from_name.ends_with("-lead") {
            from_name.strip_suffix("-lead").map(|s| s.to_string())
        } else {
            None
        }
    });

    let db_path = runs_dir.join("quaqua.db");
    let fallback_target =
        if opts.to == "cos" && (from_role == "lead" || from_name.ends_with("-lead")) {
            from_name.clone()
        } else {
            opts.to.clone()
        };

    // Try opening database; fallback to markdown append on failure (unless locked)
    let bus = match FarmBus::open(&db_path) {
        Ok(b) => b,
        Err(e) => {
            if crate::bus::is_legacy_db_error(&e) {
                return Err(format!(
                    "{}: {}",
                    db_path.display(),
                    crate::bus::LEGACY_DB_ERROR_MSG
                ));
            }
            if crate::engine::sqlite::is_lock_error(&e) {
                return Err(format!("database locked at {}: {e}", db_path.display()));
            }
            eprintln!(
                "warning: database unavailable at {}: {}; falling back to markdown append",
                db_path.display(),
                e
            );
            append_to_markdown_inbox(
                runs_dir,
                &fallback_target,
                &from_name,
                &opts.to,
                &iso_time,
                &opts.kind,
                &opts.body,
                &msg_key,
            )?;
            return Ok(SendMailResult {
                id: 0,
                already_sent: false,
            });
        }
    };

    let mut conn = bus.conn.lock().unwrap();

    // Check unacked incidents on every mail verb (Finding 8)
    let timeout_ms = get_mail_ack_timeout_ms();
    let _ = check_unacked_incidents(&conn, timeout_ms, bus.farm());

    let tx_res: rusqlite::Result<SendMailResult> = crate::engine::sqlite::retry_while_busy(|| {
        let tx = conn.transaction()?;
        let thread_id = format!("agent:{}", opts.to);
        tx.execute(
            "INSERT OR IGNORE INTO threads (id, owner, state, last_trigger_ms) VALUES (?1, ?2, 'open', ?3)",
            rusqlite::params![&thread_id, &opts.to, now as i64],
        )?;
        tx.execute(
            "UPDATE threads SET last_trigger_ms = MAX(last_trigger_ms, ?1) WHERE id = ?2",
            rusqlite::params![now as i64, &thread_id],
        )?;

        let first_line = opts.body.lines().next().unwrap_or("").trim();
        let summary_trunc: String = first_line.chars().take(80).collect();
        let summary_final = if summary_trunc.is_empty() {
            opts.kind.chars().take(80).collect()
        } else {
            summary_trunc
        };

        let dedupe_key = format!("mail:{msg_key}");
        let rows = tx.execute(
            r#"
            INSERT OR IGNORE INTO triggers (
                ts_ms, source, kind, actor, to_name, thread_id, summary, ref, payload_json, dedupe_key, farm
            ) VALUES (?1, 'mail', ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8, ?9)
            "#,
            rusqlite::params![
                now as i64,
                opts.kind,
                from_name,
                opts.to,
                thread_id,
                summary_final,
                opts.body,
                dedupe_key,
                bus.farm(),
            ],
        )?;

        if rows > 0 {
            let id = tx.last_insert_rowid() as u64;
            tx.commit()?;
            Ok(SendMailResult {
                id,
                already_sent: false,
            })
        } else {
            let existing_id: i64 = {
                let mut select =
                    tx.prepare_cached("SELECT id FROM triggers WHERE dedupe_key = ?1")?;
                select.query_row(rusqlite::params![dedupe_key], |row| row.get(0))?
            };
            tx.commit()?;
            Ok(SendMailResult {
                id: existing_id as u64,
                already_sent: true,
            })
        }
    });

    let result = match tx_res {
        Ok(r) => r,
        Err(e) => {
            if crate::engine::sqlite::is_lock_error(&e) {
                return Err(format!("database locked at {}: {e}", db_path.display()));
            }
            eprintln!(
                "warning: database transaction failed at {}: {}; falling back to markdown append",
                db_path.display(),
                e
            );
            append_to_markdown_inbox(
                runs_dir,
                &fallback_target,
                &from_name,
                &opts.to,
                &iso_time,
                &opts.kind,
                &opts.body,
                &msg_key,
            )?;
            return Ok(SendMailResult {
                id: 0,
                already_sent: false,
            });
        }
    };

    // Dual-write to markdown for lead senders
    if from_role == "lead" || from_name.ends_with("-lead") {
        let dual_write_target = if opts.to == "cos" {
            &from_name
        } else {
            &opts.to
        };
        if let Err(e) = append_to_markdown_inbox(
            runs_dir,
            dual_write_target,
            &from_name,
            &opts.to,
            &iso_time,
            &opts.kind,
            &opts.body,
            &msg_key,
        ) {
            eprintln!("warning: markdown dual-write failed: {e}");
        }
    }
    if !result.already_sent && (opts.kind == "escalate" || opts.kind == "blocked") {
        let title = format!("quaqua mail: {}", opts.kind);
        let body = format!("{} -> {}: {}", from_name, opts.to, opts.kind);
        let _ = herdr.notification_show(&title, &body, Some("request"));
    }

    Ok(result)
}

/// Reads unacked mail addressed to `to_name` with id > `since_id`, bumps delivery counts, and updates last_delivered_ms.
pub fn read_unacked_mail_since(
    runs_dir: &Path,
    to_name: &str,
    since_id: u64,
) -> Result<Vec<MailMessage>, String> {
    let db_path = runs_dir.join("quaqua.db");
    let bus = FarmBus::open(&db_path).map_err(|e| format!("cannot open database: {e}"))?;
    let conn = bus.conn.lock().unwrap();

    let timeout_ms = get_mail_ack_timeout_ms();
    let _ = check_unacked_incidents(&conn, timeout_ms, bus.farm());
    let now = crate::bus::now_ms();
    let min_id = if since_id > 0 {
        since_id
    } else {
        let mut c_stmt = conn
            .prepare_cached("SELECT trigger_id FROM cursors WHERE consumer = ?1")
            .map_err(|e| e.to_string())?;
        let mut c_rows = c_stmt
            .query(rusqlite::params![to_name])
            .map_err(|e| e.to_string())?;
        if let Some(c_row) = c_rows.next().map_err(|e| e.to_string())? {
            c_row.get::<_, i64>(0).map_err(|e| e.to_string())? as u64
        } else {
            0
        }
    };

    let mut stmt = conn
        .prepare_cached(
            r#"
            SELECT id, dedupe_key, ts_ms, actor, to_name, kind, payload_json, farm
            FROM triggers
            WHERE source = 'mail' AND to_name = ?1 AND id > ?2 AND (farm = ?3 OR farm IS NULL)
            ORDER BY id ASC
            "#,
        )
        .map_err(|e| e.to_string())?;

    let rows = stmt
        .query_map(
            rusqlite::params![to_name, min_id as i64, bus.farm()],
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
                let _farm: Option<String> = row.get(7)?;

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

    let mut messages = Vec::new();
    for r in rows {
        messages.push(r.map_err(|e| e.to_string())?);
    }
    drop(stmt);

    for msg in &mut messages {
        let key = format!("mail_delivery:{}", msg.id);
        let delivery_count: u64 = conn
            .query_row(
                "SELECT value FROM source_state WHERE source = ?1",
                rusqlite::params![&key],
                |r| r.get::<_, String>(0),
            )
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0)
            + 1;
        let _ = conn.execute(
            "INSERT INTO source_state (source, value) VALUES (?1, ?2) ON CONFLICT(source) DO UPDATE SET value = excluded.value",
            rusqlite::params![&key, delivery_count.to_string()],
        );
        msg.deliveries = delivery_count;
        msg.last_delivered_ms = Some(now);
    }
    Ok(messages)
}

/// Reads unacked mail addressed to `to_name`, bumps delivery counts, and updates last_delivered_ms.
pub fn read_inbox(runs_dir: &Path, to_name: &str) -> Result<Vec<MailMessage>, String> {
    read_unacked_mail_since(runs_dir, to_name, 0)
}

/// Renders unacked inbox messages for `to_name`.
pub fn render_inbox(runs_dir: &Path, to_name: &str) -> Result<String, String> {
    let msgs = read_inbox(runs_dir, to_name)?;
    if msgs.is_empty() {
        return Ok(format!("no unacked mail for '{to_name}'\n"));
    }
    let mut out = String::new();
    for msg in msgs {
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
    Ok(out)
}

/// Acknowledges unacked mail ids. Only the recipient may ack. Refuses non-recipient.
pub fn ack_mail(runs_dir: &Path, caller_name: &str, mail_ids: &[u64]) -> Result<Vec<u64>, String> {
    let db_path = runs_dir.join("quaqua.db");
    let bus = FarmBus::open(&db_path).map_err(|e| format!("cannot open database: {e}"))?;
    let conn = bus.conn.lock().unwrap();

    let timeout_ms = get_mail_ack_timeout_ms();
    let _ = check_unacked_incidents(&conn, timeout_ms, bus.farm());

    let _now = crate::bus::now_ms();
    let mut acked_ids = Vec::new();

    for &id in mail_ids {
        let mut query = conn
            .prepare_cached(
                "SELECT to_name FROM triggers WHERE id = ?1 AND (farm = ?2 OR farm IS NULL)",
            )
            .map_err(|e| e.to_string())?;

        let mut rows = query
            .query(rusqlite::params![id as i64, bus.farm()])
            .map_err(|e| e.to_string())?;
        if let Some(row) = rows.next().map_err(|e| e.to_string())? {
            let to_name: String = row
                .get::<_, Option<String>>(0)
                .map_err(|e| e.to_string())?
                .unwrap_or_default();

            if caller_name != to_name {
                return Err(format!(
                    "refused: caller '{caller_name}' is not the recipient of mail #{id} (addressed to '{to_name}')"
                ));
            }

            drop(rows);
            drop(query);

            let current_cursor: Option<u64> = {
                let mut c_stmt = conn
                    .prepare_cached("SELECT trigger_id FROM cursors WHERE consumer = ?1")
                    .map_err(|e| e.to_string())?;
                let mut c_rows = c_stmt
                    .query(rusqlite::params![caller_name])
                    .map_err(|e| e.to_string())?;
                if let Some(c_row) = c_rows.next().map_err(|e| e.to_string())? {
                    Some(c_row.get::<_, i64>(0).map_err(|e| e.to_string())? as u64)
                } else {
                    None
                }
            };

            let new_cursor = current_cursor.map(|c| c.max(id)).unwrap_or(id);
            let mut set_stmt = conn
                .prepare_cached(
                    r#"
                    INSERT INTO cursors (consumer, trigger_id) VALUES (?1, ?2)
                    ON CONFLICT(consumer) DO UPDATE SET trigger_id = MAX(trigger_id, excluded.trigger_id)
                    "#,
                )
                .map_err(|e| e.to_string())?;
            set_stmt
                .execute(rusqlite::params![caller_name, new_cursor as i64])
                .map_err(|e| e.to_string())?;

            acked_ids.push(id);
        } else {
            return Err(format!("mail #{id} not found"));
        }
    }

    Ok(acked_ids)
}

/// Imports markdown inbox file. Idempotent: msg_key = sha256(header_line) or metadata msg_key.
pub fn import_inbox_file(runs_dir: &Path, file_path: &Path) -> Result<ImportResult, String> {
    let content = std::fs::read_to_string(file_path)
        .map_err(|e| format!("cannot read {}: {e}", file_path.display()))?;

    let db_path = runs_dir.join("quaqua.db");
    let bus = FarmBus::open(&db_path).map_err(|e| format!("cannot open database: {e}"))?;
    let conn = bus.conn.lock().unwrap();

    let timeout_ms = get_mail_ack_timeout_ms();
    let _ = check_unacked_incidents(&conn, timeout_ms, bus.farm());

    let mut result = ImportResult::default();

    let mut current_header: Option<String> = None;
    let mut current_body_lines: Vec<String> = Vec::new();
    let mut entries = Vec::new();

    for line in content.lines() {
        if line.starts_with("## ") {
            if let Some(h) = current_header.take() {
                entries.push((h, current_body_lines.join("\n")));
                current_body_lines.clear();
            }
            current_header = Some(line.to_string());
        } else if current_header.is_some() {
            current_body_lines.push(line.to_string());
        }
    }
    if let Some(h) = current_header {
        entries.push((h, current_body_lines.join("\n")));
    }

    result.total = entries.len();

    let file_stem = file_path
        .file_name()
        .and_then(|n| n.to_str())
        .and_then(|s| s.strip_suffix(".inbox.md"))
        .unwrap_or("");

    for (header, raw_body) in entries {
        // Parse metadata comments ONLY from fixed lines directly after header
        let mut msg_key_opt: Option<String> = None;
        let mut to_name_opt: Option<String> = None;
        let mut body_lines = Vec::new();

        let mut checking_metadata = true;
        let mut metadata_count = 0;

        for line in raw_body.lines() {
            if checking_metadata && metadata_count < 2 {
                let trimmed = line.trim();
                if msg_key_opt.is_none()
                    && trimmed.starts_with("<!-- msg_key:")
                    && trimmed.ends_with("-->")
                {
                    let key = trimmed["<!-- msg_key:".len()..trimmed.len() - "-->".len()].trim();
                    msg_key_opt = Some(key.to_string());
                    metadata_count += 1;
                    continue;
                }
                if to_name_opt.is_none()
                    && trimmed.starts_with("<!-- to:")
                    && trimmed.ends_with("-->")
                {
                    let target = trimmed["<!-- to:".len()..trimmed.len() - "-->".len()].trim();
                    to_name_opt = Some(target.to_string());
                    metadata_count += 1;
                    continue;
                }
            }
            checking_metadata = false;
            body_lines.push(line);
        }
        let clean_body = body_lines
            .into_iter()
            .map(unescape_body_line)
            .collect::<Vec<_>>()
            .join("\n");

        let msg_key = msg_key_opt.unwrap_or_else(|| sha256_hex(header.trim().as_bytes()));

        let parts: Vec<&str> = header["## ".len()..].split_whitespace().collect();
        if parts.len() < 3 {
            continue;
        }
        let time_str = parts[0];
        let from_name = parts[1];

        let (to_name, kind) = if parts.len() >= 5 && parts[2] == "->" {
            (parts[3].to_string(), parts[4])
        } else {
            let default_to = if file_stem.ends_with("-lead") {
                "cos".to_string()
            } else if !file_stem.is_empty() {
                file_stem.to_string()
            } else {
                "cos".to_string()
            };
            (to_name_opt.unwrap_or(default_to), parts[2])
        };

        let _from_role = if from_name == "cos" {
            "cos"
        } else if from_name.ends_with("-lead") {
            "lead"
        } else {
            "peer"
        };

        let ts_ms = parse_iso_timestamp(time_str).unwrap_or_else(crate::bus::now_ms);
        let _project = from_name
            .strip_suffix("-lead")
            .unwrap_or(from_name)
            .to_string();

        let body_str = if clean_body.chars().count() > MAX_BODY_LEN {
            match clean_body.char_indices().nth(MAX_BODY_LEN) {
                Some((idx, _)) => &clean_body[..idx],
                None => &clean_body,
            }
        } else {
            &clean_body
        };

        let first_line = body_str.lines().next().unwrap_or("").trim();
        let summary_trunc: String = first_line.chars().take(80).collect();
        let summary_final = if summary_trunc.is_empty() {
            kind.chars().take(80).collect()
        } else {
            summary_trunc
        };
        let dedupe_key = format!("mail:{msg_key}");
        let thread_id = format!("agent:{to_name}");

        let _ = conn.execute(
            "INSERT OR IGNORE INTO threads (id, owner, state, last_trigger_ms) VALUES (?1, ?2, 'open', ?3)",
            rusqlite::params![&thread_id, to_name, ts_ms as i64],
        );
        let _ = conn.execute(
            "UPDATE threads SET last_trigger_ms = MAX(last_trigger_ms, ?1) WHERE id = ?2",
            rusqlite::params![ts_ms as i64, &thread_id],
        );

        let mut stmt = conn
            .prepare_cached(
                r#"
                INSERT INTO triggers (
                    ts_ms, source, kind, actor, to_name, thread_id, summary, ref, payload_json, dedupe_key, farm
                ) VALUES (?1, 'mail', ?2, ?3, ?4, ?5, ?6, NULL, ?7, ?8, ?9)
                ON CONFLICT(dedupe_key) DO NOTHING
                "#,
            )
            .map_err(|e| e.to_string())?;

        let rows = stmt
            .execute(rusqlite::params![
                ts_ms as i64,
                kind,
                from_name,
                to_name,
                thread_id,
                summary_final,
                body_str,
                dedupe_key,
                bus.farm(),
            ])
            .map_err(|e| e.to_string())?;

        if rows > 0 {
            result.imported += 1;
        } else {
            result.skipped += 1;
        }
    }

    Ok(result)
}

/// Lists messages from `mail` table, optionally formatted as markdown inbox view (`--md`).
pub fn list_mail(
    runs_dir: &Path,
    md_format: bool,
    filter_to: Option<&str>,
    filter_from: Option<&str>,
    filter_kind: Option<&str>,
) -> Result<String, String> {
    let db_path = runs_dir.join("quaqua.db");
    let bus = FarmBus::open(&db_path).map_err(|e| format!("cannot open database: {e}"))?;
    let conn = bus.conn.lock().unwrap();

    let timeout_ms = get_mail_ack_timeout_ms();
    let _ = check_unacked_incidents(&conn, timeout_ms, bus.farm());

    let mut query = String::from(
        r#"
        SELECT t.id, t.dedupe_key, t.ts_ms, t.actor, t.to_name, t.kind, t.payload_json, t.farm, c.trigger_id
        FROM triggers t
        LEFT JOIN cursors c ON c.consumer = t.to_name
        WHERE t.source = 'mail' AND (t.farm = ? OR t.farm IS NULL)
        "#,
    );
    let mut params: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(bus.farm().to_string())];
    if let Some(to) = filter_to {
        query.push_str(" AND t.to_name = ?");
        params.push(Box::new(to.to_string()));
    }
    if let Some(from) = filter_from {
        query.push_str(" AND t.actor = ?");
        params.push(Box::new(from.to_string()));
    }
    if let Some(k) = filter_kind {
        query.push_str(" AND t.kind = ?");
        params.push(Box::new(k.to_string()));
    }
    query.push_str(" ORDER BY t.id ASC");

    let mut stmt = conn.prepare(&query).map_err(|e| e.to_string())?;
    let params_slice: Vec<&dyn rusqlite::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let rows = stmt
        .query_map(&params_slice[..], |row| {
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
            let _farm: Option<String> = row.get(7)?;
            let cursor_id: Option<i64> = row.get(8)?;
            let acked_ms = if cursor_id.map(|cid| cid >= id as i64).unwrap_or(false) {
                Some(ts_ms)
            } else {
                None
            };

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
                acked_ms,
                acked_by: None,
            })
        })
        .map_err(|e| e.to_string())?;

    let mut out = String::new();
    for r in rows {
        let msg = r.map_err(|e| e.to_string())?;
        if md_format {
            let iso_time = format_iso_timestamp(msg.ts_ms);
            out.push_str(&format!("## {iso_time} {} {}\n", msg.from_name, msg.kind));
            if !msg.body.trim().is_empty() {
                out.push_str(&format!("{}\n", msg.body.trim_end()));
            }
            out.push('\n');
        } else {
            let ack_status = if msg.acked_ms.is_some() {
                "acked"
            } else {
                "unacked"
            };
            out.push_str(&format!(
                "#{:<4} {:<12} -> {:<12} {:<10} [{}]\n",
                msg.id, msg.from_name, msg.to_name, msg.kind, ack_status
            ));
        }
    }
    Ok(out)
}

/// Dispatches CLI args for `quaqua mail`.
pub fn handle_mail<H: HerdrOps>(args: &[String], herdr: &H) -> Result<i32, String> {
    if args.is_empty() {
        eprintln!("Usage: quaqua mail <send|inbox|ack|import|ls> [options]");
        return Ok(1);
    }

    let runs_dir = if let Ok(dir) = std::env::var("QUAQUA_RUNS_DIR") {
        PathBuf::from(dir)
    } else {
        crate::workspace::workspace_root().join("runs")
    };

    let verb = args[0].as_str();
    let subargs = &args[1..];

    match verb {
        "send" => {
            let mut to = None;
            let mut kind = None;
            let mut project = None;
            let mut reply_to = None;
            let mut client_id = None;
            let mut body_file = None;
            let mut body_direct = None;

            let mut i = 0;
            while i < subargs.len() {
                match subargs[i].as_str() {
                    "--to" => {
                        i += 1;
                        if i < subargs.len() {
                            to = Some(subargs[i].clone());
                        }
                    }
                    "--kind" => {
                        i += 1;
                        if i < subargs.len() {
                            kind = Some(subargs[i].clone());
                        }
                    }
                    "--from" => {
                        return Err("error: --from override is not permitted; sender identity is inferred from harness".to_string());
                    }
                    "--project" => {
                        i += 1;
                        if i < subargs.len() {
                            project = Some(subargs[i].clone());
                        }
                    }
                    "--reply-to" => {
                        i += 1;
                        if i < subargs.len() {
                            reply_to = subargs[i].parse::<u64>().ok();
                        }
                    }
                    "--client-id" => {
                        i += 1;
                        if i < subargs.len() {
                            client_id = Some(subargs[i].clone());
                        }
                    }
                    "--body-file" => {
                        i += 1;
                        if i < subargs.len() {
                            body_file = Some(subargs[i].clone());
                        }
                    }
                    "--body" => {
                        i += 1;
                        if i < subargs.len() {
                            body_direct = Some(subargs[i].clone());
                        }
                    }
                    other => {
                        return Err(format!("unknown option for mail send: {other}"));
                    }
                }
                i += 1;
            }

            let to_name = to.ok_or_else(|| "missing required flag: --to <name>".to_string())?;
            let kind_val =
                kind.ok_or_else(|| "missing required flag: --kind <kind>".to_string())?;

            let body_content = if let Some(b) = body_direct {
                b
            } else if let Some(file_ref) = body_file {
                if file_ref == "-" {
                    use std::io::Read;
                    let mut s = String::new();
                    std::io::stdin()
                        .read_to_string(&mut s)
                        .map_err(|e| format!("failed to read stdin: {e}"))?;
                    s
                } else {
                    std::fs::read_to_string(&file_ref)
                        .map_err(|e| format!("failed to read body file '{file_ref}': {e}"))?
                }
            } else {
                // Default to reading stdin if available
                use std::io::Read;
                let mut s = String::new();
                let _ = std::io::stdin().read_to_string(&mut s);
                s
            };

            let opts = SendMailOpts {
                to: to_name,
                kind: kind_val,
                body: body_content,
                project,
                reply_to,
                client_id,
            };

            let res = send_mail(&runs_dir, &opts, herdr)?;
            println!("mail id={}", res.id);
            Ok(0)
        }
        "inbox" => {
            let mut for_user = None;
            let mut i = 0;
            while i < subargs.len() {
                if subargs[i] == "--for" {
                    i += 1;
                    if i < subargs.len() {
                        for_user = Some(subargs[i].clone());
                    }
                }
                i += 1;
            }

            let recipient = if let Some(u) = for_user {
                u
            } else {
                let caller = crate::identity::resolve_caller(herdr, &runs_dir);
                if caller.role != crate::identity::Role::Unknown {
                    caller.name
                } else {
                    return Err(
                        "cannot determine recipient identity from harness (unresolved caller)"
                            .to_string(),
                    );
                }
            };

            let output = render_inbox(&runs_dir, &recipient)?;
            print!("{output}");
            Ok(0)
        }
        "ack" => {
            let mut ids = Vec::new();

            let mut i = 0;
            while i < subargs.len() {
                if subargs[i] == "--as" {
                    return Err("error: --as override is not permitted; caller identity is inferred from harness".to_string());
                } else if let Ok(id) = subargs[i].parse::<u64>() {
                    ids.push(id);
                } else {
                    return Err(format!("invalid mail id for ack: {}", subargs[i]));
                }
                i += 1;
            }

            if ids.is_empty() {
                return Err("missing mail id(s) to ack".to_string());
            }

            let caller = {
                let c = crate::identity::resolve_caller(herdr, &runs_dir);
                if c.role != crate::identity::Role::Unknown {
                    c.name
                } else {
                    return Err(
                        "cannot determine caller identity from harness (unresolved caller)"
                            .to_string(),
                    );
                }
            };

            let acked = ack_mail(&runs_dir, &caller, &ids)?;
            for id in acked {
                println!("acked mail {id}");
            }
            Ok(0)
        }
        "import" => {
            if subargs.is_empty() {
                return Err("missing file path for mail import".to_string());
            }
            let file_path = Path::new(&subargs[0]);
            let res = import_inbox_file(&runs_dir, file_path)?;
            println!(
                "imported {}/{} mail from {} ({} existing skipped)",
                res.imported,
                res.total,
                file_path.display(),
                res.skipped
            );
            Ok(0)
        }
        "ls" => {
            let mut md_format = false;
            let mut filter_to = None;
            let mut filter_from = None;
            let mut filter_kind = None;

            let mut i = 0;
            while i < subargs.len() {
                match subargs[i].as_str() {
                    "--md" => md_format = true,
                    "--to" => {
                        i += 1;
                        if i < subargs.len() {
                            filter_to = Some(subargs[i].as_str());
                        }
                    }
                    "--from" => {
                        i += 1;
                        if i < subargs.len() {
                            filter_from = Some(subargs[i].as_str());
                        }
                    }
                    "--kind" => {
                        i += 1;
                        if i < subargs.len() {
                            filter_kind = Some(subargs[i].as_str());
                        }
                    }
                    other => {
                        return Err(format!("unknown option for mail ls: {other}"));
                    }
                }
                i += 1;
            }

            let output = list_mail(&runs_dir, md_format, filter_to, filter_from, filter_kind)?;
            print!("{output}");
            Ok(0)
        }
        other => Err(format!("unknown mail verb: {other}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sha256_known_vectors() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn test_iso_timestamp_roundtrip() {
        let ts = 1792800000000u64; // arbitrary ms timestamp
        let formatted = format_iso_timestamp(ts);
        let parsed = parse_iso_timestamp(&formatted).expect("parse iso");
        assert_eq!(ts, parsed);
    }
}
