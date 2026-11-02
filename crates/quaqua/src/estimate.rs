//! Duration estimation for active workers based on past completed workflows.
//!
//! Provides a pure function [`estimate`] and database query [`query_completed_durations`]
//! to calculate remaining runtime estimates from historical journal data.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

/// Minimum number of completed past runs required in a (kind, playbook) group
/// before providing an estimate.
pub const MIN_SAMPLES: usize = 5;

/// An estimate of time left for an active worker based on historical runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Estimate {
    /// Number of completed runs in this (kind, playbook) group.
    pub sample_size: usize,
    /// Historical median duration for this group.
    pub median_duration: Duration,
    /// Estimated time left: `median_duration - elapsed`, floored at 0.
    pub time_left: Duration,
}

impl Estimate {
    /// Formats the estimate label, e.g. `~12m left, n=28` or `~0s left, n=5`.
    pub fn label(&self) -> String {
        let left_str = format_duration_metric(self.time_left.as_secs());
        format!("~{left_str} left, n={}", self.sample_size)
    }
}

/// Compact duration format: `1h20m`, `15m`, or `45s`.
pub fn format_duration_metric(sec: u64) -> String {
    if sec >= 3600 {
        let h = sec / 3600;
        let m = ((sec % 3600) as f64 / 60.0).round() as u64;
        if m > 0 {
            format!("{h}h{m}m")
        } else {
            format!("{h}h")
        }
    } else if sec >= 60 {
        let m = (sec as f64 / 60.0).round() as u64;
        format!("{m}m")
    } else {
        format!("{sec}s")
    }
}

/// Pure estimation function.
///
/// Computes the median duration of `samples` and subtracts `elapsed` (floored at 0).
/// Returns `None` if `samples.len() < MIN_SAMPLES` (N = 5).
pub fn estimate(samples: &[Duration], elapsed: Duration) -> Option<Estimate> {
    if samples.len() < MIN_SAMPLES {
        return None;
    }

    let mut sorted = samples.to_vec();
    sorted.sort();

    let median_duration = sorted[sorted.len() / 2];
    let time_left = median_duration.saturating_sub(elapsed);

    Some(Estimate {
        sample_size: samples.len(),
        median_duration,
        time_left,
    })
}

/// Map of `(kind, playbook)` -> list of past completed durations.
pub type DurationGroupMap = HashMap<(String, String), Vec<Duration>>;

/// Query completed workflow durations from `quaqua.db`, grouped by `(kind, playbook)`.
///
/// Only successful workflows (`exit == 0`) with a valid non-zero duration are included.
/// Failed, killed, or stalled workflows (`exit != 0`) are excluded to avoid distorting
/// the completion baseline with early watchdog terminations or timeouts.
pub fn query_completed_durations(db_path: &Path) -> Result<DurationGroupMap, String> {
    if !db_path.exists() {
        return Ok(HashMap::new());
    }

    let conn =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| format!("db error: opening {}: {e}", db_path.display()))?;

    // Join events and latest snapshots for completed workflows.
    let mut stmt = conn
        .prepare(
            "SELECT e.payload_json, s.state_json
             FROM events e
             JOIN snapshots s ON s.workflow_id = e.workflow_id
             WHERE e.kind = 'workflow_completed'
               AND s.seq = (SELECT MAX(seq) FROM snapshots WHERE workflow_id = e.workflow_id);",
        )
        .map_err(|e| format!("db error: preparing completed workflows query: {e}"))?;

    let rows = stmt
        .query_map([], |row| {
            let payload: String = row.get(0)?;
            let state: String = row.get(1)?;
            Ok((payload, state))
        })
        .map_err(|e| format!("db error: querying completed workflows: {e}"))?;

    let mut map: DurationGroupMap = HashMap::new();

    for row_res in rows {
        let (payload_str, state_str) = match row_res {
            Ok(pair) => pair,
            Err(_) => continue,
        };

        let Ok(payload) = serde_json::from_str::<serde_json::Value>(&payload_str) else {
            continue;
        };

        // Exclude failed/killed/stalled runs: exit must be 0
        if payload.get("exit").and_then(|v| v.as_i64()) != Some(0) {
            continue;
        }

        let Ok(state) = serde_json::from_str::<serde_json::Value>(&state_str) else {
            continue;
        };

        let Some(kind) = state
            .get("input")
            .and_then(|i| i.get("kind"))
            .and_then(|k| k.as_str())
            .filter(|k| !k.is_empty())
        else {
            continue;
        };

        // Recover playbook:
        // 1. config.playbook.name (canonical pinned name)
        // 2. Playbook: header in input.brief
        let playbook_name = state
            .get("config")
            .and_then(|c| c.get("playbook"))
            .and_then(|p| p.get("name"))
            .and_then(|n| n.as_str())
            .filter(|n| !n.is_empty())
            .map(|n| n.to_string())
            .or_else(|| {
                state
                    .get("input")
                    .and_then(|i| i.get("brief"))
                    .and_then(|b| b.as_str())
                    .and_then(crate::worker::prompt::brief::named_playbook)
            });

        let Some(playbook) = playbook_name else {
            continue;
        };

        // Extract duration: prev_elapsed_ms holds cumulative active runtime
        let prev_ms = state
            .get("prev_elapsed_ms")
            .and_then(|p| p.as_u64())
            .unwrap_or(0);

        if prev_ms == 0 {
            continue;
        }

        map.entry((kind.to_string(), playbook))
            .or_default()
            .push(Duration::from_millis(prev_ms));
    }

    Ok(map)
}

/// Helper to query the earliest timestamp of the current active (incomplete) workflow session
/// for a worker, if one exists in the journal.
pub fn query_active_worker_start_ms(conn: &rusqlite::Connection, name: &str) -> Option<u64> {
    let pattern = format!("worker:{name}#%");
    let query = "
        SELECT min(ts_ms) FROM events 
        WHERE workflow_id = (
            SELECT workflow_id FROM events 
            WHERE workflow_id LIKE ?1 
            GROUP BY workflow_id
            HAVING sum(case when kind in ('workflow_completed', 'workflow_failed') then 1 else 0 end) = 0
            ORDER BY max(ts_ms) DESC 
            LIMIT 1
        );
    ";
    conn.query_row(query, [&pattern], |row| row.get(0))
        .ok()
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_fewer_than_min_samples_returns_none() {
        let samples = vec![
            Duration::from_secs(100),
            Duration::from_secs(200),
            Duration::from_secs(300),
            Duration::from_secs(400),
        ];
        // 4 samples < MIN_SAMPLES (5) -> None
        assert_eq!(estimate(&samples, Duration::from_secs(50)), None);
    }

    #[test]
    fn test_estimate_with_five_samples_calculates_median_and_time_left() {
        let samples = vec![
            Duration::from_secs(500),
            Duration::from_secs(100),
            Duration::from_secs(300),
            Duration::from_secs(200),
            Duration::from_secs(400),
        ];
        // Sorted: 100, 200, 300, 400, 500. Median is 300.
        // Elapsed = 100s -> time_left = 200s
        let res = estimate(&samples, Duration::from_secs(100)).expect("estimate present");
        assert_eq!(res.sample_size, 5);
        assert_eq!(res.median_duration, Duration::from_secs(300));
        assert_eq!(res.time_left, Duration::from_secs(200));
        assert_eq!(res.label(), "~3m left, n=5");
    }

    #[test]
    fn test_estimate_even_number_of_samples() {
        let samples = vec![
            Duration::from_secs(100),
            Duration::from_secs(200),
            Duration::from_secs(300),
            Duration::from_secs(400),
            Duration::from_secs(500),
            Duration::from_secs(600),
        ];
        // Sorted: 100, 200, 300, 400, 500, 600. Index 6/2 = 3 -> 400s
        let res = estimate(&samples, Duration::from_secs(150)).expect("estimate present");
        assert_eq!(res.sample_size, 6);
        assert_eq!(res.median_duration, Duration::from_secs(400));
        assert_eq!(res.time_left, Duration::from_secs(250));
        assert_eq!(res.label(), "~4m left, n=6");
    }

    #[test]
    fn test_estimate_elapsed_exceeds_median_floors_at_zero() {
        let samples = vec![
            Duration::from_secs(100),
            Duration::from_secs(200),
            Duration::from_secs(300),
            Duration::from_secs(400),
            Duration::from_secs(500),
        ];
        // Median = 300s. Elapsed = 450s -> time_left = 0s
        let res = estimate(&samples, Duration::from_secs(450)).expect("estimate present");
        assert_eq!(res.sample_size, 5);
        assert_eq!(res.time_left, Duration::ZERO);
        assert_eq!(res.label(), "~0s left, n=5");
    }

    #[test]
    fn test_format_duration_metric() {
        assert_eq!(format_duration_metric(45), "45s");
        assert_eq!(format_duration_metric(60), "1m");
        assert_eq!(format_duration_metric(119), "2m");
        assert_eq!(format_duration_metric(3600), "1h");
        assert_eq!(format_duration_metric(3660), "1h1m");
        assert_eq!(format_duration_metric(7200), "2h");
    }

    #[test]
    fn test_query_completed_durations_from_db() {
        let tmp = std::env::temp_dir().join(format!("quaqua-test-est-db-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let db_path = tmp.join("test.db");

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute_batch(
            r#"
            CREATE TABLE events (
                workflow_id TEXT NOT NULL,
                seq INTEGER NOT NULL,
                ts_ms INTEGER NOT NULL,
                kind TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                PRIMARY KEY (workflow_id, seq)
            );
            CREATE TABLE snapshots (
                workflow_id TEXT NOT NULL,
                seq INTEGER NOT NULL,
                state_json TEXT NOT NULL,
                PRIMARY KEY (workflow_id, seq)
            );
            "#,
        )
        .unwrap();

        // 1. Successful run with config.playbook.name
        conn.execute(
            "INSERT INTO events VALUES ('worker:w1#1', 10, 1000, 'workflow_completed', '{\"exit\":0}');",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO snapshots VALUES ('worker:w1#1', 10, '{\"input\":{\"kind\":\"sude\"},\"config\":{\"playbook\":{\"name\":\"feature\"}},\"prev_elapsed_ms\":60000}');",
            [],
        )
        .unwrap();

        // 2. Successful run with playbook in brief
        conn.execute(
            "INSERT INTO events VALUES ('worker:w2#1', 10, 2000, 'workflow_completed', '{\"exit\":0}');",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO snapshots VALUES ('worker:w2#1', 10, '{\"input\":{\"kind\":\"sude\",\"brief\":\"Playbook: feature\\nGoal: test\"},\"prev_elapsed_ms\":120000}');",
            [],
        )
        .unwrap();

        // 3. Failed run (exit = 3) -> MUST be excluded
        conn.execute(
            "INSERT INTO events VALUES ('worker:w3#1', 10, 3000, 'workflow_completed', '{\"exit\":3}');",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO snapshots VALUES ('worker:w3#1', 10, '{\"input\":{\"kind\":\"sude\"},\"config\":{\"playbook\":{\"name\":\"feature\"}},\"prev_elapsed_ms\":10000}');",
            [],
        )
        .unwrap();

        // 4. Stalled run (exit = 6) -> MUST be excluded
        conn.execute(
            "INSERT INTO events VALUES ('worker:w4#1', 10, 4000, 'workflow_completed', '{\"exit\":6}');",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO snapshots VALUES ('worker:w4#1', 10, '{\"input\":{\"kind\":\"sude\"},\"config\":{\"playbook\":{\"name\":\"feature\"}},\"prev_elapsed_ms\":15000}');",
            [],
        )
        .unwrap();

        // 5. Run with zero prev_elapsed_ms -> MUST be excluded
        conn.execute(
            "INSERT INTO events VALUES ('worker:w5#1', 10, 5000, 'workflow_completed', '{\"exit\":0}');",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO snapshots VALUES ('worker:w5#1', 10, '{\"input\":{\"kind\":\"sude\"},\"config\":{\"playbook\":{\"name\":\"feature\"}},\"prev_elapsed_ms\":0}');",
            [],
        )
        .unwrap();

        // 6. Another group (tieude, bug-fix)
        conn.execute(
            "INSERT INTO events VALUES ('worker:t1#1', 10, 6000, 'workflow_completed', '{\"exit\":0}');",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO snapshots VALUES ('worker:t1#1', 10, '{\"input\":{\"kind\":\"tieude\"},\"config\":{\"playbook\":{\"name\":\"bug-fix\"}},\"prev_elapsed_ms\":45000}');",
            [],
        )
        .unwrap();

        let durations = query_completed_durations(&db_path).expect("query completed durations");

        let sude_feat = durations
            .get(&("sude".to_string(), "feature".to_string()))
            .expect("sude feature exists");
        assert_eq!(sude_feat.len(), 2);
        assert_eq!(sude_feat[0], Duration::from_secs(60));
        assert_eq!(sude_feat[1], Duration::from_secs(120));

        let tieude_bug = durations
            .get(&("tieude".to_string(), "bug-fix".to_string()))
            .expect("tieude bug-fix exists");
        assert_eq!(tieude_bug.len(), 1);
        assert_eq!(tieude_bug[0], Duration::from_secs(45));

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
