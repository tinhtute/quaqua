//! The scheduled jobs. [`run_outer2`] runs loop 2 in the order of the sensor
//! table (computational first); [`score_runs`] is the scoring job alone.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use rusqlite::{params, Connection};

use crate::auditor::{run_auditor, AuditorRunOptions};
use crate::backlog::{self, Change, NewProposal, Text};
use crate::supervisor::agent::{AgentRunner, Finding};
use crate::supervisor::rubric::{Rubric, SCALE};
use crate::supervisor::sensors::{Kind, LOOPS};

fn sql(e: rusqlite::Error) -> String {
    format!("sqlite: {e}")
}

/// What the scorer reads: one finished run.
#[derive(Clone, Debug)]
pub struct RunView {
    pub run_id: i64,
    pub project_id: Option<i64>,
    pub candidate_sha: Option<String>,
    pub outcome: Option<String>,
    /// (role, content) in order.
    pub transcript: Vec<(String, String)>,
    /// (tool, ok) in order.
    pub tool_calls: Vec<(String, Option<bool>)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Verdict {
    pub value: u8,
    pub quote: Option<String>,
}

pub trait Scorer {
    fn score(&self, rubric: &Rubric, run: &RunView) -> Result<Verdict, String>;
}

pub fn scoring_prompt(rubric: &Rubric, run: &RunView) -> String {
    let mut p = format!(
        "Score one run against one rubric.\nRubric '{}': {}.\nScale: {SCALE}.\n\
         Answer with JSON only: {{\"score\": 0|1|2, \"quote\": \"<evidence>\"}}.\n\n\
         Run {} (sha {}, outcome {}).\n",
        rubric.id,
        rubric.criterion,
        run.run_id,
        run.candidate_sha.as_deref().unwrap_or("none"),
        run.outcome.as_deref().unwrap_or("none"),
    );
    for (role, content) in &run.transcript {
        p.push_str(&format!("[{role}] {content}\n"));
    }
    for (tool, ok) in &run.tool_calls {
        p.push_str(&format!("[tool {tool}] ok={ok:?}\n"));
    }
    p
}

/// A scorer that runs a program: prompt on stdin, JSON on stdout.
pub struct ExecScorer {
    pub program: PathBuf,
}

/// Read a pipe to its end on its own thread.
fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> mpsc::Receiver<Vec<u8>> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut p) = pipe {
            let _ = p.read_to_end(&mut buf);
        }
        let _ = tx.send(buf);
    });
    rx
}

/// How long one scorer run may take before it is killed. A `claude -p` scorer
/// reading a long transcript takes a few minutes at most; 10 minutes leaves
/// room for a slow model without letting a hung scorer block the job for good.
pub const SCORER_LIMIT: Duration = Duration::from_secs(600);

impl Scorer for ExecScorer {
    fn score(&self, rubric: &Rubric, run: &RunView) -> Result<Verdict, String> {
        self.score_within(rubric, run, SCORER_LIMIT)
    }
}

impl ExecScorer {
    pub fn score_within(
        &self,
        rubric: &Rubric,
        run: &RunView,
        limit: Duration,
    ) -> Result<Verdict, String> {
        let name = self.program.display();
        let mut child = Command::new(&self.program)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot run {name}: {e}"))?;
        // Stdin and both output pipes each get a thread: a scorer that never
        // reads its prompt, or fills a pipe we are not draining, cannot block us.
        let prompt = scoring_prompt(rubric, run);
        if let Some(mut stdin) = child.stdin.take() {
            std::thread::spawn(move || {
                let _ = stdin.write_all(prompt.as_bytes());
            });
        }
        let stdout = drain(child.stdout.take());
        let stderr = drain(child.stderr.take());
        let deadline = Instant::now() + limit;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "{name} timed out after {}s and was killed",
                        limit.as_secs_f32()
                    ));
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(e) => return Err(e.to_string()),
            }
        };
        // A grandchild can keep a pipe open past the exit: wait briefly, not for good.
        let grace = Duration::from_secs(2);
        let out = stdout.recv_timeout(grace).unwrap_or_default();
        if !status.success() {
            let err = stderr.recv_timeout(grace).unwrap_or_default();
            let last = String::from_utf8_lossy(&err)
                .lines()
                .rev()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .map(|l| l.chars().take(300).collect::<String>());
            return Err(match last {
                Some(l) => format!("{name} exited {status}: {l}"),
                None => format!("{name} exited {status}"),
            });
        }
        let v: serde_json::Value =
            serde_json::from_slice(&out).map_err(|e| format!("scorer printed no JSON: {e}"))?;
        let value = v
            .get("score")
            .and_then(|s| s.as_u64())
            .ok_or("scorer JSON has no score")?;
        Ok(Verdict {
            value: u8::try_from(value).unwrap_or(u8::MAX),
            quote: v.get("quote").and_then(|q| q.as_str()).map(str::to_string),
        })
    }
}

#[derive(Debug, Default)]
pub struct ScoreReport {
    pub scored: usize,
    pub low: usize,
    pub proposals: Vec<i64>,
    pub errors: Vec<String>,
}

fn load_run(conn: &Connection, run_id: i64) -> Result<RunView, String> {
    let (project_id, candidate_sha, outcome) = conn
        .query_row(
            "SELECT t.project_id, r.candidate_sha, r.outcome FROM run r \
             LEFT JOIN task t ON t.id = r.task_id WHERE r.id = ?1",
            [run_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(sql)?;
    let mut st = conn
        .prepare("SELECT role, content FROM run_transcript WHERE run_id = ?1 ORDER BY seq")
        .map_err(sql)?;
    let transcript = st
        .query_map([run_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(sql)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql)?;
    let mut st = conn
        .prepare("SELECT tool, ok FROM run_tool_call WHERE run_id = ?1 ORDER BY seq")
        .map_err(sql)?;
    let tool_calls = st
        .query_map([run_id], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?.map(|v| v == 1)))
        })
        .map_err(sql)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql)?;
    Ok(RunView {
        run_id,
        project_id,
        candidate_sha,
        outcome,
        transcript,
        tool_calls,
    })
}

fn scored(conn: &Connection, run_id: i64, rubric: &str) -> Result<bool, String> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM score WHERE run_id = ?1 AND rubric = ?2)",
        params![run_id, rubric],
        |r| r.get(0),
    )
    .map_err(sql)
}

/// Scores every finished run against every rubric it has no score for. A
/// score of 0 needs a quote; without one it counts as 1. A 0 asks the agent,
/// then files one proposal and records the score in one transaction, so a
/// rerun never files twice.
pub fn score_runs(
    conn: &mut Connection,
    rubrics: &[Rubric],
    scorer: &dyn Scorer,
    agent: &dyn AgentRunner,
    now_ms: i64,
) -> Result<ScoreReport, String> {
    score_ended_between(conn, rubrics, scorer, agent, now_ms, i64::MIN, i64::MAX)
}

/// The schedule row that carries the scoring cursor in its `cursor_ms`.
pub const SCORE_JOB: &str = "supervisor.score";

/// One scheduled scoring pass: only runs that ended after the cursor, up to
/// the newest end seen now. The cursor moves to that end whether or not the
/// scorer failed on a run, so a broken scorer never re-spends tokens on the
/// same runs; the errors come back in the report. A missing cursor starts at
/// the newest end, so the history is never scored by accident.
pub fn score_new_runs(
    conn: &mut Connection,
    rubrics: &[Rubric],
    scorer: &dyn Scorer,
    agent: &dyn AgentRunner,
    now_ms: i64,
) -> Result<ScoreReport, String> {
    let cursor: Option<i64> = conn
        .query_row("SELECT cursor_ms FROM schedule WHERE name = ?1", [SCORE_JOB], |r| r.get(0))
        .map_err(sql)?;
    let newest: Option<i64> = conn
        .query_row(
            "SELECT MAX(ended_ms) FROM run WHERE ended_ms IS NOT NULL AND ended_ms > ?1",
            [cursor.unwrap_or(i64::MAX)],
            |r| r.get(0),
        )
        .map_err(sql)?;
    let Some(cursor) = cursor else {
        let start: i64 = conn
            .query_row("SELECT COALESCE(MAX(ended_ms), 0) FROM run", [], |r| r.get(0))
            .map_err(sql)?;
        conn.execute("UPDATE schedule SET cursor_ms = ?1 WHERE name = ?2", params![start, SCORE_JOB])
            .map_err(sql)?;
        return Ok(ScoreReport::default());
    };
    let Some(newest) = newest else {
        return Ok(ScoreReport::default());
    };
    let report = score_ended_between(conn, rubrics, scorer, agent, now_ms, cursor, newest)?;
    conn.execute("UPDATE schedule SET cursor_ms = ?1 WHERE name = ?2", params![newest, SCORE_JOB])
        .map_err(sql)?;
    Ok(report)
}

/// Runs that ended in `(after_ms, upto_ms]`.
fn score_ended_between(
    conn: &mut Connection,
    rubrics: &[Rubric],
    scorer: &dyn Scorer,
    agent: &dyn AgentRunner,
    now_ms: i64,
    after_ms: i64,
    upto_ms: i64,
) -> Result<ScoreReport, String> {
    let mut report = ScoreReport::default();
    let ids: Vec<i64> = {
        let mut st = conn
            .prepare(
                "SELECT id FROM run WHERE ended_ms IS NOT NULL AND ended_ms > ?1 \
                 AND ended_ms <= ?2 ORDER BY id",
            )
            .map_err(sql)?;
        let rows = st
            .query_map([after_ms, upto_ms], |r| r.get(0))
            .map_err(sql)?
            .collect::<Result<Vec<i64>, _>>()
            .map_err(sql)?;
        rows
    };
    for run_id in ids {
        let mut view: Option<RunView> = None;
        for rubric in rubrics {
            if scored(conn, run_id, &rubric.id)? {
                continue;
            }
            if view.is_none() {
                view = Some(load_run(conn, run_id)?);
            }
            let Some(run) = view.as_ref() else { continue };
            let mut verdict = match scorer.score(rubric, run) {
                Ok(v) => v,
                Err(e) => {
                    report.errors.push(format!("run {run_id} '{}': {e}", rubric.id));
                    continue;
                }
            };
            if verdict.value > 2 {
                report.errors.push(format!(
                    "run {run_id} '{}': score {} is outside 0..2",
                    rubric.id, verdict.value
                ));
                continue;
            }
            let quote = verdict.quote.take().filter(|q| !q.trim().is_empty());
            if verdict.value == 0 && quote.is_none() {
                verdict.value = 1;
            }
            let mut answer = None;
            if verdict.value == 0 {
                let finding = Finding {
                    run_id,
                    project_id: run.project_id,
                    rubric: rubric.clone(),
                    quote: quote.clone().unwrap_or_default(),
                };
                match agent.investigate(&finding).and_then(|a| a.check().map(|_| a)) {
                    Ok(a) => answer = Some(a),
                    Err(e) => {
                        report.errors.push(format!("run {run_id} '{}': agent: {e}", rubric.id));
                        continue;
                    }
                }
            }
            let tx = conn.transaction().map_err(sql)?;
            if scored(&tx, run_id, &rubric.id)? {
                continue;
            }
            let mut detail = serde_json::json!({ "quote": quote, "criterion": rubric.criterion });
            if let Some(a) = answer {
                let id = backlog::file(
                    &tx,
                    &NewProposal {
                        loop_no: 2,
                        project_id: run.project_id,
                        change: a.change,
                        protocol_floor: None,
                        text: a.text,
                    },
                    now_ms,
                )
                .map_err(|e| e.to_string())?;
                detail["proposal_id"] = id.into();
                report.proposals.push(id);
                report.low += 1;
            }
            backlog::record_score(
                &tx,
                run_id,
                &rubric.id,
                f64::from(verdict.value),
                Some(&detail.to_string()),
                now_ms,
            )
            .map_err(|e| e.to_string())?;
            tx.commit().map_err(sql)?;
            report.scored += 1;
        }
    }
    Ok(report)
}

/// A command whose non-zero exit is a finding (static analysis).
#[derive(Clone, Debug)]
pub struct CommandCheck {
    pub name: String,
    pub program: String,
    pub args: Vec<String>,
    pub cwd: PathBuf,
}

/// Clippy when the root is a Cargo workspace, else nothing.
pub fn default_static_checks(root: &Path) -> Vec<CommandCheck> {
    if !root.join("Cargo.toml").is_file() {
        return vec![];
    }
    vec![CommandCheck {
        name: "clippy".into(),
        program: "cargo".into(),
        args: ["clippy", "--locked", "--all-targets", "--", "-D", "warnings"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        cwd: root.to_path_buf(),
    }]
}

/// Source files under `dir` (relative to the root) must not contain any of the tokens.
#[derive(Clone, Debug)]
pub struct ArchRule {
    pub name: String,
    pub dir: PathBuf,
    pub forbidden: Vec<String>,
}

/// R24: the daemon listens on its local socket only, no inbound network.
pub fn default_arch_rules() -> Vec<ArchRule> {
    vec![ArchRule {
        name: "no-inbound-network".into(),
        dir: PathBuf::from("crates/qq-core/src"),
        forbidden: vec![
            concat!("Tcp", "Listener").into(),
            concat!("Udp", "Socket").into(),
            concat!("tiny", "_http").into(),
            concat!("ax", "um::").into(),
        ],
    }]
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            rust_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

/// A finding of a computational sensor: becomes one proposal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Observed {
    pub sensor: &'static str,
    pub observation: String,
    pub evidence: String,
}

pub struct Sensors {
    pub root: PathBuf,
    pub auditor: AuditorRunOptions,
    pub static_checks: Vec<CommandCheck>,
    pub arch_rules: Vec<ArchRule>,
}

fn auditor_findings(s: &Sensors) -> Vec<Observed> {
    let mut opts = s.auditor.clone();
    opts.repo.get_or_insert_with(|| s.root.clone());
    match run_auditor(opts) {
        Ok(path) => {
            let text = std::fs::read_to_string(&path).unwrap_or_default();
            match text.lines().find(|l| l.starts_with("Verdict:")) {
                Some("Verdict: clean") => vec![],
                Some(v) => vec![Observed {
                    sensor: "auditor",
                    observation: format!("auditor {v}"),
                    evidence: path.display().to_string(),
                }],
                None => vec![Observed {
                    sensor: "auditor",
                    observation: "auditor report has no verdict".into(),
                    evidence: path.display().to_string(),
                }],
            }
        }
        Err(e) => vec![Observed {
            sensor: "auditor",
            observation: "auditor failed".into(),
            evidence: e.message,
        }],
    }
}

fn static_findings(s: &Sensors) -> Vec<Observed> {
    let mut out = vec![];
    for c in &s.static_checks {
        let r = Command::new(&c.program).args(&c.args).current_dir(&c.cwd).output();
        let (ok, text) = match r {
            Ok(o) => (
                o.status.success(),
                format!(
                    "{}{}",
                    String::from_utf8_lossy(&o.stdout),
                    String::from_utf8_lossy(&o.stderr)
                ),
            ),
            Err(e) => (false, format!("cannot run {}: {e}", c.program)),
        };
        if !ok {
            let tail: Vec<&str> = text.lines().rev().take(20).collect();
            out.push(Observed {
                sensor: "static-analysis check",
                observation: format!("static check '{}' failed", c.name),
                evidence: tail.into_iter().rev().collect::<Vec<_>>().join("\n"),
            });
        }
    }
    out
}

fn architecture_findings(s: &Sensors) -> Vec<Observed> {
    let mut out = vec![];
    for rule in &s.arch_rules {
        let mut files = vec![];
        rust_files(&s.root.join(&rule.dir), &mut files);
        for f in files {
            let text = std::fs::read_to_string(&f).unwrap_or_default();
            for (n, line) in text.lines().enumerate() {
                if let Some(t) = rule.forbidden.iter().find(|t| line.contains(t.as_str())) {
                    let rel = f.strip_prefix(&s.root).unwrap_or(&f);
                    out.push(Observed {
                        sensor: "architecture check",
                        observation: format!("architecture rule '{}' broken", rule.name),
                        evidence: format!("{}:{} contains '{t}'", rel.display(), n + 1),
                    });
                }
            }
        }
    }
    out
}

/// A loop-2 observation that already sits in the backlog twice becomes a
/// check candidate: one proposal, once.
fn repeated_rule_findings(conn: &Connection) -> Result<Vec<Observed>, String> {
    let mut st = conn
        .prepare(
            "SELECT observation FROM proposal WHERE loop = 2 AND observation IS NOT NULL \
             AND observation NOT LIKE 'repeated:%' GROUP BY observation HAVING COUNT(*) >= 2",
        )
        .map_err(sql)?;
    let rows = st
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(sql)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql)?;
    Ok(rows
        .into_iter()
        .map(|o| Observed {
            sensor: "repeated-rule counter",
            observation: format!("repeated: {o}"),
            evidence: format!("'{o}' was filed at least twice; make it a check"),
        })
        .collect())
}

/// Files one proposal per observation. An open proposal with the same
/// observation absorbs it; a repeated-rule observation is filed once ever.
fn file_observed(conn: &Connection, o: &Observed, now_ms: i64) -> Result<Option<i64>, String> {
    let any = o.observation.starts_with("repeated:");
    let dup: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM proposal WHERE loop = 2 AND observation = ?1 \
             AND (status = 'open' OR ?2))",
            params![o.observation, any],
            |r| r.get(0),
        )
        .map_err(sql)?;
    if dup {
        return Ok(None);
    }
    let id = backlog::file(
        conn,
        &NewProposal {
            loop_no: 2,
            project_id: None,
            change: Change {
                reversible: true,
                projects: 0,
                surfaces: vec![],
                external_effect: false,
            },
            protocol_floor: None,
            text: Text {
                observation: Some(o.observation.clone()),
                evidence: Some(o.evidence.clone()),
                suspected_mechanism: Some(o.sensor.to_string()),
                ..Text::default()
            },
        },
        now_ms,
    )
    .map_err(|e| e.to_string())?;
    Ok(Some(id))
}

#[derive(Debug, Default)]
pub struct Outer2Report {
    /// `kind:sensor` in the order the sensors ran.
    pub trace: Vec<String>,
    pub sensor_proposals: Vec<i64>,
    pub scoring: ScoreReport,
}

/// Loop 2 in the order of the sensor table: computational sensors first,
/// then the inferential ones.
pub fn run_outer2(
    conn: &mut Connection,
    sensors: &Sensors,
    rubrics: &[Rubric],
    scorer: &dyn Scorer,
    agent: &dyn AgentRunner,
    now_ms: i64,
) -> Result<Outer2Report, String> {
    outer2(conn, sensors, Some((rubrics, scorer, agent)), now_ms)
}

/// The sensors of loop 2 without the inferential ones: the scheduled scoring
/// job owns scoring (and its cursor and its off switch), so this run never
/// calls a scorer.
pub fn run_sensors(
    conn: &mut Connection,
    sensors: &Sensors,
    now_ms: i64,
) -> Result<Outer2Report, String> {
    outer2(conn, sensors, None, now_ms)
}

type Scoring<'a> = (&'a [Rubric], &'a dyn Scorer, &'a dyn AgentRunner);

fn outer2(
    conn: &mut Connection,
    sensors: &Sensors,
    scoring: Option<Scoring<'_>>,
    now_ms: i64,
) -> Result<Outer2Report, String> {
    let row = &LOOPS[2];
    let mut rep = Outer2Report::default();
    for (kind, name) in crate::supervisor::sensors::order(row) {
        if scoring.is_none() && kind == Kind::Inferential {
            continue;
        }
        let tag = match kind {
            Kind::Computational => "computational",
            Kind::Inferential => "inferential",
        };
        let found = match (kind, name) {
            (Kind::Computational, "auditor") => auditor_findings(sensors),
            (Kind::Computational, "static-analysis check") => static_findings(sensors),
            (Kind::Computational, "architecture check") => architecture_findings(sensors),
            (Kind::Computational, "repeated-rule counter") => repeated_rule_findings(conn)?,
            (Kind::Inferential, "conversation scoring") => {
                if let Some((rubrics, scorer, agent)) = scoring {
                    rep.scoring = score_runs(conn, rubrics, scorer, agent, now_ms)?;
                }
                vec![]
            }
            // The agent's question runs inside the scoring step, once per low score.
            (Kind::Inferential, _) => vec![],
            (Kind::Computational, other) => return Err(format!("no sensor named '{other}'")),
        };
        rep.trace.push(format!("{tag}:{name}"));
        for o in &found {
            if let Some(id) = file_observed(conn, o, now_ms)? {
                rep.sensor_proposals.push(id);
            }
        }
    }
    Ok(rep)
}

#[cfg(test)]
mod scorer_tests {
    use super::*;
    use crate::backlog::Surface;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::mpsc;
    use std::time::Instant;

    struct Dir(PathBuf);
    impl Dir {
        fn new(tag: &str) -> Dir {
            let p = std::env::temp_dir().join(format!("qq-scorer-{}-{tag}", std::process::id()));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            Dir(p)
        }
    }
    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn script(dir: &Dir, body: &str) -> ExecScorer {
        let p = dir.0.join("scorer.sh");
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        ExecScorer { program: p }
    }

    fn rubric() -> Rubric {
        Rubric {
            id: "r".into(),
            criterion: "c".into(),
            surface: Surface::Rule,
        }
    }

    fn view(pad: usize) -> RunView {
        RunView {
            run_id: 1,
            project_id: None,
            candidate_sha: None,
            outcome: None,
            transcript: vec![("user".into(), "x".repeat(pad))],
            tool_calls: vec![],
        }
    }

    /// Run the scorer on a thread so a hang fails the test instead of freezing it.
    fn score(s: ExecScorer, pad: usize, limit: Duration) -> (Result<Verdict, String>, Duration) {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let t = Instant::now();
            let r = s.score_within(&rubric(), &view(pad), limit);
            let _ = tx.send((r, t.elapsed()));
        });
        rx.recv_timeout(Duration::from_secs(15))
            .expect("scorer did not return within 15 s")
    }

    #[test]
    fn supervisor_scorer_error_carries_last_stderr_line() {
        let d = Dir::new("t1");
        let s = script(&d, "echo first >&2; echo 'no API key set' >&2; exit 1");
        let (r, _) = score(s, 10, Duration::from_secs(5));
        let e = r.unwrap_err();
        assert!(e.contains("no API key set"), "{e}");
        assert!(!e.contains("first"), "{e}");
    }

    #[test]
    fn supervisor_scorer_that_sleeps_is_killed_at_the_limit() {
        let d = Dir::new("t2");
        let s = script(&d, "exec sleep 30");
        let (r, took) = score(s, 10, Duration::from_millis(300));
        assert!(r.unwrap_err().contains("timed out"));
        assert!(took < Duration::from_secs(5), "{took:?}");
    }

    #[test]
    fn supervisor_scorer_that_never_reads_a_large_prompt_does_not_deadlock() {
        let d = Dir::new("t3");
        let s = script(&d, "exec sleep 30");
        // 4 MiB is far above the 64 KiB pipe buffer.
        let (r, took) = score(s, 4 << 20, Duration::from_millis(300));
        assert!(r.unwrap_err().contains("timed out"));
        assert!(took < Duration::from_secs(5), "{took:?}");
    }

    #[test]
    fn supervisor_scorer_that_answers_without_reading_still_scores() {
        let d = Dir::new("t4");
        let s = script(&d, r#"echo '{"score": 2, "quote": "ok"}'"#);
        let (r, _) = score(s, 4 << 20, Duration::from_secs(5));
        assert_eq!(r.unwrap().value, 2);
    }
}