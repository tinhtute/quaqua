//! Migration from legacy `farm_events`, `mail`, and `cursors` schema
//! to superagent `triggers`, `threads`, `subscriptions`, and `cursors`.
//!
//! Note: This is the ONLY file allowed to read and drop legacy tables.

use crate::herdr::HerdrOps;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Summary of a migration run (or dry run).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationSummary {
    pub backup_path: Option<PathBuf>,
    pub farm_events_count: i64,
    pub mail_count: i64,
    pub legacy_cursors_count: i64,
    pub triggers_count: i64,
    pub threads_count: i64,
    pub cursors_count: i64,
    pub consumer_cursors: Vec<(String, u64, u64, u64)>, // (consumer, old_seq, old_mail_id, new_trigger_id)
    pub already_migrated: bool,
    pub dry_run: bool,
}

#[derive(Clone, Debug)]
enum LegacyRef {
    FarmEvent(u64),
    Mail(u64),
}

struct CandidateTrigger {
    legacy_ref: LegacyRef,
    ts_ms: u64,
    source: String,
    kind: String,
    actor: String,
    to_name: Option<String>,
    thread_id: String,
    summary: String,
    ref_val: Option<String>,
    payload_json: Option<String>,
    dedupe_key: Option<String>,
    farm: String,
}

struct LegacyFarmEvent {
    seq: u64,
    ts_ms: u64,
    event_type: String,
    actor: Option<String>,
    subject: Option<String>,
    owner: Option<String>,
    incident_key: Option<String>,
    payload_json: String,
    farm: Option<String>,
}

struct LegacyMail {
    id: u64,
    ts_ms: u64,
    from_name: String,
    to_name: String,
    kind: String,
    body: String,
    farm: Option<String>,
}

struct LegacyCursor {
    consumer: String,
    seq: u64,
    mail_id: u64,
}

/// Executes or simulates migration on `db_path`.
pub fn migrate_bus<H: HerdrOps>(
    db_path: &Path,
    backup_dir: Option<&Path>,
    dry_run: bool,
    herdr: &H,
) -> Result<MigrationSummary, String> {
    if crate::workspace::live_db_open_refused(db_path) {
        return Err(format!(
            "refused live workspace database under test: {}",
            db_path.display()
        ));
    }

    if !db_path.exists() {
        return Err(format!("database not found at {}", db_path.display()));
    }

    // 1. Live worker check
    if let Ok(raw) = herdr.agent_list() {
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(agents) = val
                .get("result")
                .and_then(|r| r.get("agents"))
                .and_then(|a| a.as_array())
            {
                let mut active_workers = Vec::new();
                for a in agents {
                    let name = a.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let status = a
                        .get("agent_status")
                        .and_then(|s| s.as_str())
                        .unwrap_or("");
                    if status != "dead" && !name.is_empty() && name != "cos" {
                        active_workers.push(format!("{name} ({status})"));
                    }
                }
                if !active_workers.is_empty() {
                    return Err(format!(
                        "cannot migrate: active workers running: {}",
                        active_workers.join(", ")
                    ));
                }
            }
        }
    }

    let runs_dir = db_path.parent().unwrap_or(db_path);
    let local_farm = crate::workspace::farm_of_runs_dir(runs_dir);

    let mut conn = if dry_run {
        rusqlite::Connection::open_with_flags(
            db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|e| format!("cannot open database read-only: {e}"))?
    } else {
        crate::engine::sqlite::open_conn(db_path)
            .map_err(|e| format!("cannot open database: {e}"))?
    };

    // 2. Check if already migrated
    let is_already_migrated: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='bus_migration'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|c| c > 0)
        .unwrap_or(false)
        && conn
            .query_row(
                "SELECT COUNT(*) FROM bus_migration WHERE name='superagent-v1'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|c| c > 0)
            .unwrap_or(false);

    if is_already_migrated {
        return Ok(MigrationSummary {
            backup_path: None,
            farm_events_count: 0,
            mail_count: 0,
            legacy_cursors_count: 0,
            triggers_count: 0,
            threads_count: 0,
            cursors_count: 0,
            consumer_cursors: Vec::new(),
            already_migrated: true,
            dry_run,
        });
    }

    // 3. Inspect legacy tables
    let has_farm_events: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='farm_events'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|c| c > 0)
        .unwrap_or(false);

    let has_mail: bool = conn
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='mail'",
            [],
            |r| r.get::<_, i64>(0),
        )
        .map(|c| c > 0)
        .unwrap_or(false);

    let has_legacy_cursors: bool = {
        let has_c: bool = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='cursors'",
                [],
                |r| r.get::<_, i64>(0),
            )
            .map(|c| c > 0)
            .unwrap_or(false);
        if has_c {
            let mut stmt = conn
                .prepare("PRAGMA table_info(cursors)")
                .map_err(|e| e.to_string())?;
            let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
            let mut found = false;
            while let Some(row) = rows.next().map_err(|e| e.to_string())? {
                let col: String = row.get(1).map_err(|e| e.to_string())?;
                if col == "seq" {
                    found = true;
                    break;
                }
            }
            found
        } else {
            false
        }
    };

    // Read legacy farm_events
    let farm_events: Vec<LegacyFarmEvent> = if has_farm_events {
        let mut stmt = conn
            .prepare("SELECT seq, ts_ms, type, actor, subject, owner, incident_key, payload_json, farm FROM farm_events ORDER BY ts_ms ASC, seq ASC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok(LegacyFarmEvent {
                    seq: row.get::<_, i64>(0)? as u64,
                    ts_ms: row.get::<_, i64>(1)? as u64,
                    event_type: row.get(2)?,
                    actor: row.get(3)?,
                    subject: row.get(4)?,
                    owner: row.get(5)?,
                    incident_key: row.get(6)?,
                    payload_json: row
                        .get::<_, Option<String>>(7)?
                        .unwrap_or_else(|| "{}".to_string()),
                    farm: row.get(8)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut res = Vec::new();
        for r in rows {
            res.push(r.map_err(|e| e.to_string())?);
        }
        res
    } else {
        Vec::new()
    };

    // Read legacy mail
    let mails: Vec<LegacyMail> = if has_mail {
        let mut stmt = conn
            .prepare("SELECT id, ts_ms, from_name, to_name, kind, body, farm FROM mail ORDER BY ts_ms ASC, id ASC")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok(LegacyMail {
                    id: row.get::<_, i64>(0)? as u64,
                    ts_ms: row.get::<_, i64>(1)? as u64,
                    from_name: row.get(2)?,
                    to_name: row.get(3)?,
                    kind: row.get(4)?,
                    body: row.get(5)?,
                    farm: row.get(6)?,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut res = Vec::new();
        for r in rows {
            res.push(r.map_err(|e| e.to_string())?);
        }
        res
    } else {
        Vec::new()
    };

    // Read legacy cursors
    let cursors: Vec<LegacyCursor> = if has_legacy_cursors {
        let mut stmt = conn
            .prepare("SELECT consumer, seq, mail_id FROM cursors")
            .map_err(|e| e.to_string())?;
        let rows = stmt
            .query_map([], |row| {
                Ok(LegacyCursor {
                    consumer: row.get(0)?,
                    seq: row.get::<_, i64>(1)? as u64,
                    mail_id: row.get::<_, i64>(2)? as u64,
                })
            })
            .map_err(|e| e.to_string())?;
        let mut res = Vec::new();
        for r in rows {
            res.push(r.map_err(|e| e.to_string())?);
        }
        res
    } else {
        Vec::new()
    };

    // Build threads and candidate triggers
    let mut threads_map: BTreeMap<String, (String, u64)> = BTreeMap::new();
    threads_map.insert("agent:unknown".to_string(), ("unknown".to_string(), 0));

    let mut candidates = Vec::with_capacity(farm_events.len() + mails.len());

    for fe in &farm_events {
        let thread_name = fe
            .subject
            .as_deref()
            .or(fe.owner.as_deref())
            .unwrap_or("unknown");
        let thread_id = format!("agent:{thread_name}");
        let entry = threads_map
            .entry(thread_id.clone())
            .or_insert_with(|| (thread_name.to_string(), 0));
        if fe.ts_ms > entry.1 {
            entry.1 = fe.ts_ms;
        }

        let actor_str = fe.actor.as_deref().unwrap_or("unknown");
        let source = if actor_str == "herdr" || fe.event_type.starts_with("agent_") {
            "herdr"
        } else if actor_str == "watchdog" || fe.event_type == "stalled" {
            "watchdog"
        } else if fe.event_type.contains("github") || actor_str == "github" {
            "github-ci"
        } else {
            "worker"
        };

        let summary = format!(
            "{} {}",
            fe.event_type,
            fe.subject.as_deref().unwrap_or_default()
        );
        let summary_trunc: String = summary.trim().chars().take(80).collect();
        let summary_final = if summary_trunc.is_empty() {
            fe.event_type.chars().take(80).collect()
        } else {
            summary_trunc
        };

        let farm_final = fe.farm.clone().unwrap_or_else(|| local_farm.clone());

        candidates.push(CandidateTrigger {
            legacy_ref: LegacyRef::FarmEvent(fe.seq),
            ts_ms: fe.ts_ms,
            source: source.to_string(),
            kind: fe.event_type.clone(),
            actor: actor_str.to_string(),
            to_name: None,
            thread_id,
            summary: summary_final,
            ref_val: None,
            payload_json: if fe.payload_json.is_empty() {
                None
            } else {
                Some(fe.payload_json.clone())
            },
            dedupe_key: fe.incident_key.clone(),
            farm: farm_final,
        });
    }

    for m in &mails {
        let thread_name = &m.to_name;
        let thread_id = format!("agent:{thread_name}");
        let entry = threads_map
            .entry(thread_id.clone())
            .or_insert_with(|| (thread_name.to_string(), 0));
        if m.ts_ms > entry.1 {
            entry.1 = m.ts_ms;
        }

        let first_line = m.body.lines().next().unwrap_or("").trim();
        let summary_trunc: String = first_line.chars().take(80).collect();
        let summary_final = if summary_trunc.is_empty() {
            m.kind.chars().take(80).collect()
        } else {
            summary_trunc
        };

        let farm_final = m.farm.clone().unwrap_or_else(|| local_farm.clone());

        candidates.push(CandidateTrigger {
            legacy_ref: LegacyRef::Mail(m.id),
            ts_ms: m.ts_ms,
            source: "mail".to_string(),
            kind: m.kind.clone(),
            actor: m.from_name.clone(),
            to_name: Some(m.to_name.clone()),
            thread_id,
            summary: summary_final,
            ref_val: None,
            payload_json: Some(m.body.clone()),
            dedupe_key: None,
            farm: farm_final,
        });
    }

    // Sort candidates by ts_ms ASC, source priority, legacy id ASC
    candidates.sort_by(|a, b| {
        a.ts_ms.cmp(&b.ts_ms).then_with(|| match (&a.legacy_ref, &b.legacy_ref) {
            (LegacyRef::FarmEvent(s1), LegacyRef::FarmEvent(s2)) => s1.cmp(s2),
            (LegacyRef::Mail(m1), LegacyRef::Mail(m2)) => m1.cmp(m2),
            (LegacyRef::FarmEvent(_), LegacyRef::Mail(_)) => std::cmp::Ordering::Less,
            (LegacyRef::Mail(_), LegacyRef::FarmEvent(_)) => std::cmp::Ordering::Greater,
        })
    });

    // Compute new cursors
    let mut consumer_cursors = Vec::new();
    for c in &cursors {
        let mut new_cursor = candidates.len() as u64; // default: all read
        for (idx, cand) in candidates.iter().enumerate() {
            let is_read = match cand.legacy_ref {
                LegacyRef::FarmEvent(seq) => seq <= c.seq,
                LegacyRef::Mail(mail_id) => mail_id <= c.mail_id,
            };
            if !is_read {
                new_cursor = idx as u64;
                break;
            }
        }
        consumer_cursors.push((c.consumer.clone(), c.seq, c.mail_id, new_cursor));
    }

    if dry_run {
        println!("quaqua bus migrate --dry-run: {}", db_path.display());
        println!("  Before (legacy):");
        println!("    farm_events: {}", farm_events.len());
        println!("    mail:        {}", mails.len());
        println!("    cursors:     {}", cursors.len());
        println!("  After (superagent):");
        println!("    triggers:    {}", candidates.len());
        println!("    threads:     {}", threads_map.len());
        println!("    cursors:     {}", consumer_cursors.len());
        for (consumer, old_seq, old_mail, new_c) in &consumer_cursors {
            println!(
                "      consumer '{consumer}': old=(seq={old_seq}, mail_id={old_mail}) -> new_trigger_id={new_c}"
            );
        }
        return Ok(MigrationSummary {
            backup_path: None,
            farm_events_count: farm_events.len() as i64,
            mail_count: mails.len() as i64,
            legacy_cursors_count: cursors.len() as i64,
            triggers_count: candidates.len() as i64,
            threads_count: threads_map.len() as i64,
            cursors_count: consumer_cursors.len() as i64,
            consumer_cursors,
            already_migrated: false,
            dry_run: true,
        });
    }

    // 4. Real migration: Backup via VACUUM INTO
    let backup_dir_path = backup_dir.map(PathBuf::from).unwrap_or_else(|| {
        crate::workspace::workspace_root().join("raw")
    });
    std::fs::create_dir_all(&backup_dir_path)
        .map_err(|e| format!("cannot create backup directory {}: {e}", backup_dir_path.display()))?;

    let date_str = crate::auditor::current_date_hour()
        .chars()
        .take(10)
        .collect::<String>();
    let mut backup_path = backup_dir_path.join(format!("{date_str}-quaqua-db-pre-superagent.db"));
    if backup_path.exists() {
        backup_path = backup_dir_path.join(format!(
            "{date_str}-quaqua-db-pre-superagent-{}.db",
            crate::bus::now_ms()
        ));
    }

    conn.execute(
        &format!(
            "VACUUM INTO '{}'",
            backup_path.to_string_lossy().replace('\'', "''")
        ),
        [],
    )
    .map_err(|e| format!("VACUUM INTO backup failed: {e}"))?;

    // 5. Transaction: Create schema, insert data, map cursors, drop legacy tables, record watermark
    crate::engine::sqlite::retry_while_busy(|| {
        let tx = conn.transaction().map_err(|e| {
            rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(e.to_string())))
        })?;

        tx.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS threads (
                id              TEXT PRIMARY KEY,
                owner           TEXT NOT NULL,
                state           TEXT NOT NULL CHECK (state IN ('open','closed')),
                last_trigger_ms INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS triggers (
                id           INTEGER PRIMARY KEY AUTOINCREMENT,
                ts_ms        INTEGER NOT NULL,
                source       TEXT NOT NULL CHECK (source IN ('herdr','mail','watchdog','worker','github-ci')),
                kind         TEXT NOT NULL,
                actor        TEXT NOT NULL,
                to_name      TEXT,
                thread_id    TEXT NOT NULL REFERENCES threads(id),
                summary      TEXT NOT NULL CHECK (length(summary) <= 80),
                ref          TEXT,
                payload_json TEXT,
                dedupe_key   TEXT UNIQUE,
                farm         TEXT NOT NULL
            );
            CREATE INDEX IF NOT EXISTS triggers_thread ON triggers(thread_id, id);

            CREATE TABLE IF NOT EXISTS subscriptions (
                thread_id TEXT NOT NULL REFERENCES threads(id),
                consumer  TEXT NOT NULL,
                until_ms  INTEGER NOT NULL,
                PRIMARY KEY (thread_id, consumer)
            );

            CREATE TABLE IF NOT EXISTS cursors_new (
                consumer   TEXT PRIMARY KEY,
                trigger_id INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS source_state (
                source TEXT PRIMARY KEY,
                value  TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS bus_migration (
                name TEXT PRIMARY KEY,
                watermark INTEGER NOT NULL
            );
            "#,
        )?;

        // Populate threads
        {
            let mut thread_stmt = tx.prepare_cached(
                "INSERT OR REPLACE INTO threads (id, owner, state, last_trigger_ms) VALUES (?1, ?2, 'open', ?3)",
            )?;
            for (tid, (owner, last_ts)) in &threads_map {
                thread_stmt.execute(rusqlite::params![tid, owner, *last_ts as i64])?;
            }
        }

        // Populate triggers
        {
            let mut trig_stmt = tx.prepare_cached(
                r#"
                INSERT INTO triggers (
                    ts_ms, source, kind, actor, to_name, thread_id, summary, ref, payload_json, dedupe_key, farm
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                "#,
            )?;
            for cand in &candidates {
                trig_stmt.execute(rusqlite::params![
                    cand.ts_ms as i64,
                    cand.source,
                    cand.kind,
                    cand.actor,
                    cand.to_name,
                    cand.thread_id,
                    cand.summary,
                    cand.ref_val,
                    cand.payload_json,
                    cand.dedupe_key,
                    cand.farm,
                ])?;
            }
        }

        // Populate cursors_new
        {
            let mut cur_stmt = tx.prepare_cached(
                "INSERT INTO cursors_new (consumer, trigger_id) VALUES (?1, ?2)",
            )?;
            for (consumer, _old_s, _old_m, new_c) in &consumer_cursors {
                cur_stmt.execute(rusqlite::params![consumer, *new_c as i64])?;
            }
        }

        // Drop legacy tables
        tx.execute_batch(
            r#"
            DROP TABLE IF EXISTS farm_events;
            DROP TABLE IF EXISTS mail;
            DROP TABLE IF EXISTS cursors;
            ALTER TABLE cursors_new RENAME TO cursors;
            INSERT OR REPLACE INTO bus_migration (name, watermark) VALUES ('superagent-v1', 1);
            "#,
        )?;

        tx.commit()?;
        Ok(())
    })
    .map_err(|e| format!("migration transaction failed: {e}"))?;

    println!("quaqua bus migrate complete: {}", db_path.display());
    println!("  Backup: {}", backup_path.display());
    println!(
        "  Migrated {} legacy rows ({} events + {} mails) -> {} triggers",
        candidates.len(),
        farm_events.len(),
        mails.len(),
        candidates.len()
    );
    println!("  Migrated {} cursors", consumer_cursors.len());

    Ok(MigrationSummary {
        backup_path: Some(backup_path),
        farm_events_count: farm_events.len() as i64,
        mail_count: mails.len() as i64,
        legacy_cursors_count: cursors.len() as i64,
        triggers_count: candidates.len() as i64,
        threads_count: threads_map.len() as i64,
        cursors_count: consumer_cursors.len() as i64,
        consumer_cursors,
        already_migrated: false,
        dry_run: false,
    })
}

/// CLI runner for `quaqua bus migrate [--dry-run]`.
pub fn run_migrate(dry_run: bool) -> Result<i32, String> {
    let runs_dir = if let Ok(dir) = std::env::var("QUAQUA_RUNS_DIR") {
        PathBuf::from(dir)
    } else {
        crate::workspace::workspace_root().join("runs")
    };
    let db_path = runs_dir.join("quaqua.db");
    let raw_dir = crate::workspace::workspace_root().join("raw");
    let herdr = crate::herdr::Herdr::new();

    let summary = migrate_bus(&db_path, Some(&raw_dir), dry_run, &herdr)?;
    if summary.already_migrated {
        println!("bus migration: already migrated to superagent-v1; no-op");
    }
    Ok(0)
}
