//! Pipeline engine (`quaqua pipeline`).
//! Implements build -> review -> fix orchestration state machine.

use crate::cap::count_owner_slots;
use crate::config::load_team_config;
use crate::git::{Git, GitOps};
use crate::herdr::Herdr;
use crate::worker::cleanup::default_runs_dir;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

pub const USAGE: &str = "\
# Usage: pipeline.sh <task> <repo> --brief <file> [--builder sude] [--reviewer suhuynh]
#                    [--review auto|always|skip] [--rounds 2] [--base <ref>]
#        pipeline.sh --resume <task> <repo>
#   Build -> review -> fix loop on top of
#   spawn-worker.sh / prompt-worker.sh / cleanup-worker.sh; counts as 2 workers
#   toward the cap. <task> is a slug ([a-z0-9-]): builder <builder>-<task>, a
#   fresh reviewer <reviewer>-<task>r<N> each round (spawn-worker.sh --review).
#   Flow: builder writes a plan -> exit 5 'plan-ready' (the Lead approves by
#   resuming) -> build -> review (auto: diff vs the spawn base is >150 changed
#   lines or touches >1 top-level dir; always/skip override) -> findings go
#   straight back to the builder for a fix, then a fresh reviewer; at most
#   <rounds> review rounds. Ends clean | disputed | blocked. '## Challenge' in
#   any worker runs file (builder or reviewer) stops the pipeline as disputed
#   (Lead decides); a reviewer runs file without a 'Verdict: clean | <N>
#   findings' line stops it as blocked. A phase report that declares itself
#   unfinished ('Outcome: BLOCKED|PARTIAL|INCOMPLETE|FAILED' or a non-empty
#   '## Blockers' section — e.g. written after the watchdog nudge) never
#   advances: the pipeline stops blocked with the evidence copied in, and the
#   state stays resumable ('--resume' continues the same phase) — only a
#   non-blocked exit 0 marks the run finished. After
#   BUILD/FIX the branch tip must hold real commits in a clean worktree — the
#   size gate and the reviewer are bound to that exact SHA; a builder that
#   intentionally changed nothing says 'Outcome: no changes' (clean, review
#   skipped). Reviewers are cleaned right after their runs file is read; the
#   caller cleans the builder after re-running its test.
#   Exit codes reuse prompt-worker.sh's watchdog meanings: 0 finished (status
#   line on stdout + runs/pipeline-<task>.md) · 1 usage/state error · 2 cap ·
#   3 stall/gone (also: resumable snapshot/spawn failures) · 4 budget · 5
#   plan-ready pause, a mandatory grid-arrange pause, or a worker blocked on
#   a question. On 2-5 fix the cause (a stuck wait keeps waiting via
#   'prompt-worker.sh --resume <worker>'), then 'pipeline.sh --resume <task>
#   <repo>' continues from runs/pipeline-<task>.state. Every stop appends one
#   line to runs/telemetry.jsonl {ts,task,repo,builder,reviewer,
#   review:run|skip,rounds,review_changed_outcome,status,minutes} — the last
#   line per task is current. Exit 6 (malformed report: prompt-worker.sh
#   rejects a phase report missing its '## Lessons' / 'Verdict:' tail, R3)
#   propagates here as blocked like every other watchdog exit.
#   Env budgets (sec): PIPELINE_PLAN_BUDGET_SEC 900 · _BUILD_ 2700 · _REVIEW_
#   900 · _FIX_ 1200 · PIPELINE_MAX_SEC 7200 (work time; time between
#   invocations does not count) · PIPELINE_REVIEW_LINES 150.
#   Grid: after each spawn/cleanup the pipeline arranges its whole owner
#   population into tab GRID_TAB (default <repo-basename>-workers): idle panes
#   move, working ones are reported 'deferred: <pane> (working)'; GRID_SKIP=1
#   with QUAQUA_TEST=1 disables (tests).";

pub fn harness_of(kind: &str) -> Option<&'static str> {
    match kind {
        "tieude" => Some("opencode"),
        "detu" | "sude" => Some("omp"),
        "ni" | "suhuynh" | "truonglao" => Some("claude"),
        _ => None,
    }
}

pub fn harness_of_kind<'a>(kind: &str, cfg: &'a crate::config::Config) -> Option<&'a str> {
    if let Some(k) = cfg.kinds.get(kind) {
        return Some(&k.harness);
    }
    harness_of(kind)
}
pub fn select_default_reviewer(builder: &str, cfg: &crate::config::Config) -> Result<String, (i32, String)> {
    let bh = harness_of_kind(builder, cfg).ok_or_else(|| (1, format!("unknown builder kind: {builder}\n")))?;
    let unavailable: Vec<String> = std::env::var("QUAQUA_UNAVAILABLE_REVIEWERS")
        .map(|s| s.split(',').map(|k| k.trim().to_string()).collect())
        .unwrap_or_default();

    for cand in &cfg.strategy.default_reviewer {
        if unavailable.iter().any(|u| u == cand) {
            continue;
        }
        if let Some(rh) = harness_of_kind(cand, cfg) {
            if bh != rh {
                if let Some(kind_cfg) = cfg.kinds.get(cand) {
                    if kind_cfg.can_review {
                        return Ok(cand.clone());
                    }
                }
            }
        }
    }
    Err((1, "no available reviewer found in strategy.default_reviewer\n".to_string()))
}


pub fn now_sec() -> u64 {
    if let Ok(clock_file) = std::env::var("PROMPT_WORKER_CLOCK_FILE") {
        if let Ok(c) = std::fs::read_to_string(&clock_file) {
            if let Ok(v) = c.trim().parse::<u64>() {
                return v;
            }
        }
    }
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
pub fn resolve_project_slug_from_repo(repo: &Path) -> Option<String> {
    if let Ok(root) = crate::lead::spawn::find_quaqua_root() {
        let index = root.join("projects/INDEX.md");
        if index.is_file() {
            if let Ok(content) = std::fs::read_to_string(&index) {
                let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
                for line in content.lines() {
                    let trimmed = line.trim();
                    if trimmed.starts_with('|') {
                        let parts: Vec<&str> = trimmed.split('|').map(|s| s.trim()).collect();
                        if parts.len() >= 3 && parts[2] != "repo" {
                            let slug = parts[1];
                            let r = parts[2];
                            let p = if let Some(stripped) = r.strip_prefix("~/") {
                                format!("{}/{}", home, stripped)
                            } else if r == "~" {
                                home.clone()
                            } else {
                                r.to_string()
                            };
                            if let Ok(c_repo) = std::fs::canonicalize(Path::new(&p)) {
                                if c_repo == repo {
                                    return Some(slug.to_string());
                                }
                            }
                            if Path::new(&p) == repo {
                                return Some(slug.to_string());
                            }
                        }
                    }
                }
            }
        }
    }
    if let Ok(owner) = std::env::var("PIPELINE_LEAD").or_else(|_| std::env::var("QUAQUA_OWNER")) {
        if let Some(slug) = owner.strip_suffix("-lead") {
            return Some(slug.to_string());
        }
    }
    None
}


fn file_mtime(path: &Path) -> u64 {
    path.metadata()
        .and_then(|m| m.modified())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)))
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn iso_timestamp() -> String {
    // Generate ISO 8601 string
    let sec = now_sec();
    let dt = match Command::new("date").arg("-Iseconds").output() {
        Ok(out) if out.status.success() => {
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        }
        _ => {
            format!("{sec}")
        }
    };
    dt
}

#[derive(Debug, Clone, Default)]
pub struct PipelineState {
    pub repo: String,
    pub brief: String,
    pub builder: String,
    pub reviewer: String,
    pub review: String,
    pub rounds: u32,
    pub base: String,
    pub wt: String,
    pub base_sha: String,
    pub head_sha: String,
    pub planfile: String,
    pub commit: String,
    pub phase: String,
    pub round: u32,
    pub prompted: u32,
    pub prompt_ts: u64,
    pub rounds_run: u32,
    pub review_run: String,
    pub review_changed: bool,
    pub last_findings: u32,
    pub finished: u32,
    pub start_elapsed: u64,
    pub builder_spawned: u32,
    pub reviewer_spawned: u32,
    pub lead: String,
    pub lead_pane: String,
    pub pipeline_pid: u32,
}

impl PipelineState {
    pub fn load(path: &Path) -> Result<Self, String> {
        let content = std::fs::read_to_string(path)
            .map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let mut s = PipelineState::default();

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some(idx) = line.find('=') {
                let key = &line[..idx];
                let mut val = &line[idx + 1..];
                // strip quotes
                if (val.starts_with('\'') && val.ends_with('\''))
                    || (val.starts_with('"') && val.ends_with('"'))
                {
                    val = &val[1..val.len() - 1];
                }

                match key {
                    "repo" => s.repo = val.to_string(),
                    "brief" => s.brief = val.to_string(),
                    "builder" => s.builder = val.to_string(),
                    "reviewer" => s.reviewer = val.to_string(),
                    "review" => s.review = val.to_string(),
                    "rounds" => s.rounds = val.parse().unwrap_or(2),
                    "base" => s.base = val.to_string(),
                    "wt" => s.wt = val.to_string(),
                    "base_sha" => s.base_sha = val.to_string(),
                    "head_sha" => s.head_sha = val.to_string(),
                    "planfile" => s.planfile = val.to_string(),
                    "commit" => s.commit = val.to_string(),
                    "phase" => s.phase = val.to_string(),
                    "round" => s.round = val.parse().unwrap_or(0),
                    "prompted" => s.prompted = val.parse().unwrap_or(0),
                    "prompt_ts" => s.prompt_ts = val.parse().unwrap_or(0),
                    "rounds_run" => s.rounds_run = val.parse().unwrap_or(0),
                    "review_run" => s.review_run = val.to_string(),
                    "review_changed" => s.review_changed = val == "true" || val == "1",
                    "last_findings" => s.last_findings = val.parse().unwrap_or(0),
                    "finished" => s.finished = val.parse().unwrap_or(0),
                    "start_elapsed" => s.start_elapsed = val.parse().unwrap_or(0),
                    "builder_spawned" => s.builder_spawned = val.parse().unwrap_or(0),
                    "reviewer_spawned" => s.reviewer_spawned = val.parse().unwrap_or(0),
                    "lead" => s.lead = val.to_string(),
                    "lead_pane" => s.lead_pane = val.to_string(),
                    "pipeline_pid" => s.pipeline_pid = val.parse().unwrap_or(0),
                    _ => {}
                }
            }
        }
        Ok(s)
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let mut out = String::new();
        out.push_str(&format!("repo={}\n", self.repo));
        out.push_str(&format!("brief={}\n", self.brief));
        out.push_str(&format!("builder={}\n", self.builder));
        out.push_str(&format!("reviewer={}\n", self.reviewer));
        out.push_str(&format!("review={}\n", self.review));
        out.push_str(&format!("rounds={}\n", self.rounds));
        out.push_str(&format!("base={}\n", self.base));
        out.push_str(&format!("wt={}\n", self.wt));
        out.push_str(&format!("base_sha={}\n", self.base_sha));
        out.push_str(&format!("head_sha={}\n", self.head_sha));
        out.push_str(&format!("planfile={}\n", self.planfile));
        out.push_str(&format!("commit={}\n", self.commit));
        out.push_str(&format!("phase={}\n", self.phase));
        out.push_str(&format!("round={}\n", self.round));
        out.push_str(&format!("prompted={}\n", self.prompted));
        out.push_str(&format!("prompt_ts={}\n", self.prompt_ts));
        out.push_str(&format!("rounds_run={}\n", self.rounds_run));
        out.push_str(&format!("review_run={}\n", self.review_run));
        out.push_str(&format!("review_changed={}\n", if self.review_changed { "true" } else { "false" }));
        out.push_str(&format!("last_findings={}\n", self.last_findings));
        out.push_str(&format!("finished={}\n", self.finished));
        out.push_str(&format!("start_elapsed={}\n", self.start_elapsed));
        out.push_str(&format!("builder_spawned={}\n", self.builder_spawned));
        out.push_str(&format!("reviewer_spawned={}\n", self.reviewer_spawned));
        out.push_str(&format!("lead={}\n", self.lead));
        out.push_str(&format!("lead_pane={}\n", self.lead_pane));
        out.push_str(&format!("pipeline_pid={}\n", self.pipeline_pid));
        std::fs::write(path, out)
    }
}

/// Whether a pipeline stop is final: the state is marked `finished` and
/// `pipeline.sh --resume` refuses it. A blocked stop is deliberately not
/// final — the cause is fixed from outside, then the phase is resumed.
pub fn pipeline_finished(final_st: &str, code: i32) -> bool {
    code == 0 && final_st != "blocked"
}

pub fn has_real_blockers(content: &str) -> bool {
    let mut in_blockers = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("## Blockers") {
            in_blockers = true;
            continue;
        }
        if in_blockers && trimmed.starts_with("## ") {
            break;
        }
        if in_blockers && !trimmed.is_empty() {
            let item = trimmed
                .trim_start_matches('-')
                .trim_start_matches('*')
                .trim();
            let core = item.trim_end_matches(|c: char| c == '.' || c == '…' || c.is_whitespace());
            if !core.is_empty() && core.eq_ignore_ascii_case("none") {
                continue;
            }
            return true;
        }
    }
    false
}

pub fn blocked_outcome(path: &Path) -> bool {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    for line in content.lines() {
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("Outcome:").or_else(|| t.strip_prefix("outcome:"))
            .or_else(|| t.strip_prefix("Status:")).or_else(|| t.strip_prefix("status:"))
            .or_else(|| t.strip_prefix("Result:")).or_else(|| t.strip_prefix("result:")) {
            let val = rest.trim().to_ascii_uppercase();
            if val.starts_with("BLOCKED") || val.starts_with("PARTIAL") || val.starts_with("INCOMPLETE") || val.starts_with("FAILED") {
                return true;
            }
        }
    }
    has_real_blockers(&content)
}

fn log_challenge(source: &Path, pfile: &Path) {
    if let Ok(content) = std::fs::read_to_string(source) {
        let mut extract = String::new();
        let mut in_challenge = false;
        for line in content.lines() {
            if line.starts_with("## Challenge") {
                in_challenge = true;
                extract.push_str(line);
                extract.push('\n');
                continue;
            }
            if in_challenge && line.starts_with("## ") {
                break;
            }
            if in_challenge {
                extract.push_str(line);
                extract.push('\n');
            }
        }
        if !extract.is_empty() {
            let sname = source.file_name().and_then(|n| n.to_str()).unwrap_or("");
            let header = format!("\n## Challenge (from {sname})\n");
            let mut file_content = std::fs::read_to_string(pfile).unwrap_or_default();
            file_content.push_str(&header);
            file_content.push_str(&extract);
            let _ = std::fs::write(pfile, file_content);
        }
    }
}

fn log_blockers(source: &Path, pfile: &Path) {
    if let Ok(content) = std::fs::read_to_string(source) {
        let sname = source.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let mut extract = format!("\n## Worker stopped early (from {sname})\n");

        for line in content.lines() {
            let t = line.trim();
            if t.starts_with("Outcome:") || t.starts_with("Status:") || t.starts_with("Result:") {
                extract.push_str(line);
                extract.push('\n');
            }
        }

        let mut in_blockers = false;
        for line in content.lines() {
            if line.starts_with("## Blockers") {
                in_blockers = true;
                continue;
            }
            if in_blockers && line.starts_with("## ") {
                break;
            }
            if in_blockers {
                extract.push_str(line);
                extract.push('\n');
            }
        }

        let mut file_content = std::fs::read_to_string(pfile).unwrap_or_default();
        file_content.push_str(&extract);
        let _ = std::fs::write(pfile, file_content);
    }
}

fn append_telemetry(runs_dir: &Path, ts: &str, task: &str, repo: &str, builder: &str, reviewer: &str, review: &str, rounds: u32, changed: bool, status: &str, minutes: u64) {
    let tele_path = runs_dir.join("telemetry.jsonl");
    let line = format!(
        "{{\"ts\":\"{}\",\"task\":\"{}\",\"repo\":\"{}\",\"builder\":\"{}\",\"reviewer\":\"{}\",\"review\":\"{}\",\"rounds\":{},\"review_changed_outcome\":{},\"status\":\"{}\",\"minutes\":{}}}\n",
        ts, task, repo, builder, reviewer, review, rounds, changed, status, minutes
    );
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(tele_path) {
        use std::io::Write;
        let _ = f.write_all(line.as_bytes());
    }
}

pub fn handle_pipeline(args: &[String], git: &Git, herdr: &Herdr) -> Result<(), (i32, String)> {
    let inv_start = now_sec();
    let runs_dir = default_runs_dir();
    let _ = std::fs::create_dir_all(&runs_dir);

    let plan_budget = std::env::var("PIPELINE_PLAN_BUDGET_SEC").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(900);
    let build_budget = std::env::var("PIPELINE_BUILD_BUDGET_SEC").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(2700);
    let review_budget = std::env::var("PIPELINE_REVIEW_BUDGET_SEC").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(900);
    let fix_budget = std::env::var("PIPELINE_FIX_BUDGET_SEC").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(1200);
    let max_sec = std::env::var("PIPELINE_MAX_SEC").ok().and_then(|s| s.parse::<u64>().ok()).unwrap_or(7200);
    let review_lines = std::env::var("PIPELINE_REVIEW_LINES").ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(150);

    let mut state: PipelineState;
    let task: String;
    let repo_path: PathBuf;
    let state_file: PathBuf;
    let pfile: PathBuf;

    if !args.is_empty() && args[0] == "--resume" {
        if args.len() != 3 {
            return Err((1, format!("{}\n", USAGE)));
        }
        task = args[1].clone();
        if !task.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
            return Err((1, format!("{}\n", USAGE)));
        }
        let want_repo = match std::fs::canonicalize(&args[2]) {
            Ok(p) => p,
            Err(_) => PathBuf::from(&args[2]),
        };

        state_file = runs_dir.join(format!("pipeline-{task}.state"));
        pfile = runs_dir.join(format!("pipeline-{task}.md"));

        if !state_file.is_file() {
            return Err((1, format!("no pipeline state for '{task}' ({}) — nothing to resume\n", state_file.display())));
        }

        state = PipelineState::load(&state_file).map_err(|e| (1, format!("{e}\n")))?;
        let state_repo = match std::fs::canonicalize(&state.repo) {
            Ok(p) => p,
            Err(_) => PathBuf::from(&state.repo),
        };
        if state_repo != want_repo {
            return Err((1, format!("state for '{task}' is on repo {}, not {}\n", state.repo, want_repo.display())));
        }
        if state.finished == 1 {
            return Err((1, format!("pipeline '{task}' already finished — see {}\n", pfile.display())));
        }
        state.pipeline_pid = std::process::id();
        let _ = state.save(&state_file);

        let mut pcontent = std::fs::read_to_string(&pfile).unwrap_or_default();
        pcontent.push_str(&format!("- resumed: {} (phase={})\n", iso_timestamp(), state.phase));
        let _ = std::fs::write(&pfile, pcontent);
        repo_path = state_repo;
    } else {
        // Fresh start
        let mut builder = "sude".to_string();
        let mut reviewer_opt: Option<String> = None;
        let mut review = "auto".to_string();
        let mut rounds = 2u32;
        let mut brief_opt: Option<PathBuf> = None;
        let mut base = String::new();
        let mut pos = Vec::new();

        let mut i = 0;
        while i < args.len() {
            let arg = &args[i];
            if arg == "--brief" {
                if i + 1 >= args.len() { return Err((1, format!("{}\n", USAGE))); }
                brief_opt = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            } else if let Some(v) = arg.strip_prefix("--brief=") {
                brief_opt = Some(PathBuf::from(v));
                i += 1;
            } else if arg == "--builder" {
                if i + 1 >= args.len() { return Err((1, format!("{}\n", USAGE))); }
                builder = args[i + 1].clone();
                i += 2;
            } else if let Some(v) = arg.strip_prefix("--builder=") {
                builder = v.to_string();
                i += 1;
            } else if arg == "--reviewer" {
                if i + 1 >= args.len() { return Err((1, format!("{}\n", USAGE))); }
                reviewer_opt = Some(args[i + 1].clone());
                i += 2;
            } else if let Some(v) = arg.strip_prefix("--reviewer=") {
                reviewer_opt = Some(v.to_string());
                i += 1;
            } else if arg == "--review" {
                if i + 1 >= args.len() { return Err((1, format!("{}\n", USAGE))); }
                review = args[i + 1].clone();
                i += 2;
            } else if let Some(v) = arg.strip_prefix("--review=") {
                review = v.to_string();
                i += 1;
            } else if arg == "--rounds" {
                if i + 1 >= args.len() { return Err((1, format!("{}\n", USAGE))); }
                rounds = args[i + 1].parse().map_err(|_| (1, "--rounds must be an integer >= 1\n".to_string()))?;
                i += 2;
            } else if let Some(v) = arg.strip_prefix("--rounds=") {
                rounds = v.parse().map_err(|_| (1, "--rounds must be an integer >= 1\n".to_string()))?;
                i += 1;
            } else if arg == "--base" {
                if i + 1 >= args.len() { return Err((1, format!("{}\n", USAGE))); }
                base = args[i + 1].clone();
                i += 2;
            } else if let Some(v) = arg.strip_prefix("--base=") {
                base = v.to_string();
                i += 1;
            } else if arg.starts_with('-') {
                return Err((1, format!("{}\n", USAGE)));
            } else {
                pos.push(arg.clone());
                i += 1;
            }
        }

        if pos.len() != 2 {
            return Err((1, format!("{}\n", USAGE)));
        }
        task = pos[0].clone();
        if !task.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-') {
            return Err((1, format!("{}\n", USAGE)));
        }

        let raw_repo = PathBuf::from(&pos[1]);
        repo_path = match std::fs::canonicalize(&raw_repo) {
            Ok(p) => p,
            Err(_) => raw_repo,
        };

        let brief = match brief_opt {
            Some(p) if p.is_file() => match std::fs::canonicalize(&p) {
                Ok(cp) => cp,
                Err(_) => p,
            },
            _ => return Err((1, format!("--brief <file> required and must exist (got: '{}')\n", brief_opt.map(|p| p.to_string_lossy().to_string()).unwrap_or_else(|| "none".to_string())))),
        };

        let brief_str = std::fs::read_to_string(&brief).map_err(|e| (1, format!("cannot read {}: {e}\n", brief.display())))?;
        if !crate::worker::prompt::brief_headers_ok(&brief_str) {
            return Err((1, "brief must carry the Goal:/Hard constraints:/Current design headers (rules/_all/brief.md)\n".to_string()));
        }

        let base_cfg = load_team_config()?;
        let proj_slug = resolve_project_slug_from_repo(&repo_path);
        let cfg = if let Some(slug) = proj_slug.as_deref() {
            crate::config::resolve(base_cfg.clone(), Some(slug)).unwrap_or(base_cfg)
        } else {
            base_cfg
        };
        let bh = harness_of_kind(&builder, &cfg).ok_or_else(|| (1, format!("unknown builder kind: {builder}\n")))?;

        // Reviewer selection
        let reviewer = match reviewer_opt {
            Some(r) => {
                let rh = harness_of_kind(&r, &cfg).ok_or_else(|| (1, format!("unknown reviewer kind: {r}\n")))?;
                // Any kind with can_review = true may review — harness-independent
                // (Claude out: suhuynh runs on opencode).
                if !cfg.kinds.get(&r).map(|k| k.can_review).unwrap_or(false) {
                    return Err((1, format!("reviewer kind '{r}' has can_review = false in team.toml\n")));
                }
                if bh == rh {
                    return Err((1, format!("builder and reviewer must be different harnesses (models.md): {builder}/{r} are both {bh}\n")));
                }
                r
            }
            None => select_default_reviewer(&builder, &cfg)?,
        };

        let rh = harness_of_kind(&reviewer, &cfg).unwrap();
        if !["auto", "always", "skip"].contains(&review.as_str()) {
            return Err((1, "--review must be auto|always|skip\n".to_string()));
        }
        if rounds < 1 {
            return Err((1, "--rounds must be an integer >= 1\n".to_string()));
        }

        if builder.len() + 1 + task.len() > 20 {
            return Err((1, format!("builder name {builder}-{task} exceeds 20 chars (models.md Naming)\n")));
        }
        let round_str = rounds.to_string();
        if reviewer.len() + 1 + task.len() + 1 + round_str.len() > 20 {
            return Err((1, format!("reviewer name {reviewer}-{task}r{rounds} exceeds 20 chars\n")));
        }

        let owner = std::env::var("PIPELINE_LEAD")
            .or_else(|_| std::env::var("QUAQUA_OWNER"))
            .unwrap_or_else(|_| "cos".to_string());
        let max_workers = cfg.limits.max_workers_per_orchestrator;
        let pipe_weight = cfg.limits.weights.get("pipeline").copied().unwrap_or(2);

        let slots_used = count_owner_slots(&runs_dir, &owner, None, herdr);
        if slots_used + pipe_weight > max_workers {
            eprintln!("error: worker cap exceeded (owner '{owner}' has {slots_used} slots in use; pipeline reserves {pipe_weight}, cap is {max_workers})");
            return Err((2, String::new()));
        }

        state_file = runs_dir.join(format!("pipeline-{task}.state"));
        pfile = runs_dir.join(format!("pipeline-{task}.md"));

        if state_file.is_file() {
            return Err((1, format!("pipeline '{task}' already has state — use --resume or a new task slug\n")));
        }

        let parent_wt = repo_path.parent().unwrap_or(Path::new("."));
        let wt = parent_wt.join(".wt").join(format!("{builder}-{task}"));

        state = PipelineState {
            repo: repo_path.to_string_lossy().to_string(),
            brief: brief.to_string_lossy().to_string(),
            builder: builder.clone(),
            reviewer: reviewer.clone(),
            review: review.clone(),
            rounds,
            base,
            wt: wt.to_string_lossy().to_string(),
            base_sha: String::new(),
            head_sha: String::new(),
            planfile: String::new(),
            commit: String::new(),
            phase: "spawn".to_string(),
            round: 0,
            prompted: 0,
            prompt_ts: 0,
            rounds_run: 0,
            review_run: "skip".to_string(),
            review_changed: false,
            last_findings: 0,
            finished: 0,
            start_elapsed: 0,
            builder_spawned: 0,
            reviewer_spawned: 0,
            lead: std::env::var("PIPELINE_LEAD").or_else(|_| std::env::var("QUAQUA_OWNER")).unwrap_or_default(),
            lead_pane: std::env::var("HERDR_PANE_ID").unwrap_or_default(),
            pipeline_pid: std::process::id(),
        };

        let _ = state.save(&state_file);

        let initial_md = format!(
            "# pipeline {}\n- task: {} · repo: {}\n- builder: {}-{} ({}) · reviewer: {} ({}) · review: {} · max rounds: {}\n- started: {}\n",
            task, task, state.repo, builder, task, bh, reviewer, rh, review, rounds, iso_timestamp()
        );
        let _ = std::fs::write(&pfile, initial_md);
    }

    let brief_text = std::fs::read_to_string(&state.brief)
        .map_err(|e| (1, format!("brief file unreadable: {}: {e}\n", state.brief)))?;

    let grid_tab = std::env::var("GRID_TAB").unwrap_or_else(|_| {
        let bname = repo_path.file_name().and_then(|n| n.to_str()).unwrap_or("repo");
        format!("{bname}-workers")
    });
    let grid_owner = if state.lead.is_empty() { "cos".to_string() } else { state.lead.clone() };
    let run_grid_hook = || -> bool {
        if std::env::var("GRID_SKIP").as_deref() == Ok("1") && std::env::var("QUAQUA_TEST").as_deref() == Ok("1") {
            return true;
        }
        match crate::grid::run_grid_repair(&grid_tab, Some(&grid_owner)) {
            Ok(()) => true,
            Err(e) => {
                eprintln!("warning: grid check failed ({e}); arrange panes manually");
                false
            }
        }
    };

    let finish = |state: &mut PipelineState, st: &str, code: i32, blocker: Option<&str>| -> Result<(), (i32, String)> {
        let el = state.start_elapsed + (now_sec() - inv_start);
        state.start_elapsed = el;

        let mut final_st = st.to_string();
        let mut final_blocker = blocker.map(|s| s.to_string());

        if final_st == "clean" && code == 0 && state.start_elapsed > max_sec {
            final_st = "blocked".to_string();
            if final_blocker.is_none() {
                final_blocker = Some(format!("total budget {max_sec}s of work time exceeded — outcome arrived over budget"));
            }
        }

        let mins = state.start_elapsed / 60;
        if pipeline_finished(&final_st, code) {
            state.finished = 1;
        }
        let _ = state.save(&state_file);

        let mut pcontent = std::fs::read_to_string(&pfile).unwrap_or_default();
        let commit_str = if state.commit.is_empty() { "none" } else { &state.commit };
        pcontent.push_str(&format!("- status: {final_st} · commit {commit_str} · elapsed {mins}m\n"));

        if let Some(b) = &final_blocker {
            pcontent.push_str(&format!("\n## Blockers\n- {b}\n"));
        }
        let _ = std::fs::write(&pfile, pcontent);

        append_telemetry(
            &runs_dir,
            &iso_timestamp(),
            &task,
            &state.repo,
            &state.builder,
            &state.reviewer,
            &state.review_run,
            state.rounds_run,
            state.review_changed,
            &final_st,
            mins,
        );

        println!(
            "pipeline-{task}: status={final_st} rounds={} commit={commit_str} run={}",
            state.rounds_run,
            pfile.display()
        );

        if code == 0 {
            Ok(())
        } else {
            Err((code, String::new()))
        }
    };

    let check_clock = |state: &mut PipelineState| -> Option<Result<(), (i32, String)>> {
        let el = state.start_elapsed + (now_sec() - inv_start);
        if el > max_sec {
            let b = format!("total budget {max_sec}s of work time exceeded — remaining work is in the log above");
            Some(finish(state, "blocked", 4, Some(&b)))
        } else {
            None
        }
    };

    let archive_stale = |f: &Path, pfile: &Path| {
        let bname = f.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let stale_path = format!("{}.stale.{}", f.display(), now_sec());
        if std::fs::rename(f, &stale_path).is_ok() {
            let mut pcontent = std::fs::read_to_string(pfile).unwrap_or_default();
            pcontent.push_str(&format!("- archived stale report {bname} (predates this phase's prompt)\n"));
            let _ = std::fs::write(pfile, pcontent);
        }
    };

    let wait_file = |state: &mut PipelineState, worker: &str, prompt: &str, budget_sec: u64| -> Result<(), (i32, String)> {
        let el = state.start_elapsed + (now_sec() - inv_start);
        let rem = if max_sec > el { max_sec - el } else { 0 };
        let mut budget = if rem < budget_sec { rem } else { budget_sec };
        if budget < 1 { budget = 1; }

        let report_file = runs_dir.join(format!("{worker}.md"));
        let rc: i32;

        if state.prompted == 0 {
            if report_file.exists() {
                archive_stale(&report_file, &pfile);
            }
            state.prompted = 1;
            state.prompt_ts = now_sec();
            let _ = state.save(&state_file);

            // Execute prompt worker
            let prompt_sh = crate::worker::cleanup::find_engine_script("prompt-worker.sh");

            if let Some(psh) = prompt_sh {
                let res = Command::new("bash")
                    .arg(psh)
                    .arg(worker)
                    .arg(prompt)
                    .env("PROMPT_WORKER_BUDGET_SEC", budget.to_string())
                    .status();
                rc = res.map(|s| s.code().unwrap_or(1)).unwrap_or(1);
            } else {
                let _prev_b = std::env::var("PROMPT_WORKER_BUDGET_SEC").ok();
                std::env::set_var("PROMPT_WORKER_BUDGET_SEC", budget.to_string());
                rc = match crate::worker::prompt::prompt_worker(worker, Some(prompt), false, None, git, herdr) {
                    Ok(()) => 0,
                    Err((c, _)) => c,
                };
                if let Some(pb) = _prev_b {
                    std::env::set_var("PROMPT_WORKER_BUDGET_SEC", pb);
                } else {
                    std::env::remove_var("PROMPT_WORKER_BUDGET_SEC");
                }
            }
        } else {
            // Resume
            if report_file.is_file() && file_mtime(&report_file) < state.prompt_ts {
                archive_stale(&report_file, &pfile);
            }
            let prompt_sh = crate::worker::cleanup::find_engine_script("prompt-worker.sh");

            if let Some(psh) = prompt_sh {
                let res = Command::new("bash")
                    .arg(psh)
                    .arg("--resume")
                    .arg(worker)
                    .env("PROMPT_WORKER_BUDGET_SEC", budget.to_string())
                    .status();
                rc = res.map(|s| s.code().unwrap_or(1)).unwrap_or(1);
            } else {
                let _prev_b = std::env::var("PROMPT_WORKER_BUDGET_SEC").ok();
                std::env::set_var("PROMPT_WORKER_BUDGET_SEC", budget.to_string());
                rc = match crate::worker::prompt::prompt_worker(worker, None, true, None, git, herdr) {
                    Ok(()) => 0,
                    Err((c, _)) => c,
                };
                if let Some(pb) = _prev_b {
                    std::env::set_var("PROMPT_WORKER_BUDGET_SEC", pb);
                } else {
                    std::env::remove_var("PROMPT_WORKER_BUDGET_SEC");
                }
            }
        }

        if rc != 0 {
            let b = format!(
                "{} wait for {worker}: watchdog exit {rc} — fix the cause ('prompt-worker.sh --resume {worker}' keeps a live wait going), then 'pipeline.sh --resume {task} {}'",
                state.phase, state.repo
            );
            return finish(state, "blocked", rc, Some(&b));
        }
        Ok(())
    };

    let check_impl = |state: &mut PipelineState, report_path: &Path, prev: Option<&str>| -> Result<(), (i32, String)> {
        let wt_path = Path::new(&state.wt);
        let rev_out = git.run_in(wt_path, &["rev-parse", "HEAD"]);
        let head = match rev_out {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
            _ => "?".to_string(),
        };

        let st_out = git.run_in(wt_path, &["status", "--porcelain"]);
        let dirty = match st_out {
            Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
            _ => "?".to_string(),
        };

        if head == "?" || dirty == "?" {
            let b = format!("cannot inspect {} (git rev-parse/status failed) — fix the worktree, then 'pipeline.sh --resume {task} {}'", state.wt, state.repo);
            return finish(state, "blocked", 3, Some(&b));
        }

        state.head_sha = head.clone();
        state.commit = if head.len() >= 7 { head[..7].to_string() } else { head.clone() };

        if let Some(p) = prev {
            if head == p {
                let prev7 = if p.len() >= 7 { &p[..7] } else { p };
                let b = format!(
                    "fix produced no commits past {prev7} on quaqua/{}-{task} — a no-op answer to findings belongs in '## Challenge'. Commit the fixes, then 'pipeline.sh --resume {task} {}'",
                    state.builder, state.repo
                );
                return finish(state, "blocked", 3, Some(&b));
            }
        } else if head == state.base_sha {
            if dirty.is_empty() {
                if let Ok(rep_content) = std::fs::read_to_string(report_path) {
                    for line in rep_content.lines() {
                        let t = line.trim();
                        if let Some(rest) = t.strip_prefix("Outcome:").or_else(|| t.strip_prefix("outcome:")) {
                            let rest_upper = rest.trim().to_ascii_uppercase();
                            if rest_upper.starts_with("NO CHANGES") || rest_upper.starts_with("NO-OP") || rest_upper.starts_with("NO OP") {
                                let mut pcontent = std::fs::read_to_string(&pfile).unwrap_or_default();
                                pcontent.push_str("- build: no commits — builder declared 'Outcome: no changes'; nothing to review\n");
                                let _ = std::fs::write(&pfile, pcontent);
                                return finish(state, "clean", 0, None);
                            }
                        }
                    }
                }
            }
            let base7 = if state.base_sha.len() >= 7 { &state.base_sha[..7] } else { &state.base_sha };
            let b = format!(
                "builder produced no commits on quaqua/{}-{task} (HEAD == base {base7}) and did not declare 'Outcome: no changes' — commit the work (or fix the report), then 'pipeline.sh --resume {task} {}'",
                state.builder, state.repo
            );
            return finish(state, "blocked", 3, Some(&b));
        }

        if !dirty.is_empty() {
            let mut pcontent = std::fs::read_to_string(&pfile).unwrap_or_default();
            pcontent.push_str(&format!("- uncommitted/untracked in {}:\n{}\n", state.wt, dirty));
            let _ = std::fs::write(&pfile, pcontent);
            let b = format!(
                "worktree dirty — the files above are missing from quaqua/{}-{task}, so the gate and the reviewer would see a different snapshot. Commit or clean them, then 'pipeline.sh --resume {task} {}'",
                state.builder, state.repo
            );
            return finish(state, "blocked", 3, Some(&b));
        }

        Ok(())
    };

    // Phase machine loop
    loop {
        if let Some(res) = check_clock(&mut state) {
            return res;
        }

        match state.phase.as_str() {
            "spawn" => {
                let bname = format!("{}-{task}", state.builder);
                if state.builder_spawned == 0 {
                    let mut sargs = Vec::new();
                    if !state.base.is_empty() {
                        sargs.push("--base".to_string());
                        sargs.push(state.base.clone());
                    }
                    sargs.push(bname.clone());
                    sargs.push(state.builder.clone());
                    sargs.push(state.repo.clone());

                    let spawn_sh = crate::worker::cleanup::find_engine_script("spawn-worker.sh");

                    let spawn_ok = if let Some(ssh) = spawn_sh {
                        let mut cmd = Command::new("bash");
                        cmd.arg(ssh);
                        cmd.args(&sargs);
                        cmd.env("PIPELINE_TASK", &task);
                        cmd.env("QUAQUA_OWNER", if state.lead.is_empty() { "cos" } else { &state.lead });
                        cmd.env("GRID_SKIP", "1");
                        cmd.status().map(|s| s.success()).unwrap_or(false)
                    } else {
                        std::env::set_var("PIPELINE_TASK", &task);
                        std::env::set_var("QUAQUA_OWNER", if state.lead.is_empty() { "cos" } else { &state.lead });
                        let spawn_res = (|| -> Result<(), (i32, String)> {
                            let opts = crate::worker::spawn::parse_spawn_args(&sargs)?;
                            let cfg = load_team_config()?;
                            crate::worker::spawn::spawn_worker(opts, &cfg, git, herdr)
                        })();
                        std::env::remove_var("PIPELINE_TASK");
                        spawn_res.is_ok()
                    };

                    if !spawn_ok {
                        let b = format!("spawn-worker.sh failed for builder {bname} — fix the cause, then 'pipeline.sh --resume {task} {}'", state.repo);
                        return finish(&mut state, "blocked", 3, Some(&b));
                    }
                    state.builder_spawned = 1;
                    let _ = state.save(&state_file);
                }

                if !run_grid_hook() {
                    let b = format!("grid check failed for {bname} — arrange the pane per rules/_all/fanout-agent-grid.md, then 'pipeline.sh --resume {task} {}'", state.repo);
                    let _ = state.save(&state_file);
                    return finish(&mut state, "blocked", 5, Some(&b));
                }

                let wt_path = Path::new(&state.wt);
                let rev_out = git.run_in(wt_path, &["rev-parse", "HEAD"]);
                state.base_sha = rev_out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_else(|_| "unknown".to_string());

                let mut pcontent = std::fs::read_to_string(&pfile).unwrap_or_default();
                pcontent.push_str(&format!("- builder {bname} spawned (base {})\n", state.base_sha));
                let _ = std::fs::write(&pfile, pcontent);

                state.phase = "plan".to_string();
                state.prompted = 0;
                let _ = state.save(&state_file);
            }

            "plan" => {
                let bname = format!("{}-{task}", state.builder);
                let plan_prompt = format!(
                    "{brief_text}\n\nPipeline phase: PLAN — write only the implementation plan (steps, files to touch, risks) as your runs file; do not change code yet. The plan is approved before you build. If the premise is wrong, write '## Challenge' with evidence instead of planning around it."
                );

                wait_file(&mut state, &bname, &plan_prompt, plan_budget)?;

                let planfile = runs_dir.join(format!("{bname}.plan.md"));
                let cur_md = runs_dir.join(format!("{bname}.md"));
                let _ = std::fs::rename(&cur_md, &planfile);

                state.planfile = planfile.to_string_lossy().to_string();
                let mut pcontent = std::fs::read_to_string(&pfile).unwrap_or_default();
                pcontent.push_str(&format!("- plan: {}\n", planfile.file_name().and_then(|n| n.to_str()).unwrap_or("")));
                let _ = std::fs::write(&pfile, pcontent);

                if let Ok(content) = std::fs::read_to_string(&planfile) {
                    if content.lines().any(|l| l.trim().starts_with("## Challenge")) {
                        log_challenge(&planfile, &pfile);
                        let b = "builder challenged the premise at plan stage — Lead resolves the Challenge above";
                        return finish(&mut state, "disputed", 0, Some(b));
                    }
                }

                if blocked_outcome(&planfile) {
                    log_blockers(&planfile, &pfile);
                    let b = "builder's plan reports unfinished work — Lead decides how to proceed";
                    return finish(&mut state, "blocked", 0, Some(b));
                }

                state.phase = "build".to_string();
                state.prompted = 0;
                let _ = state.save(&state_file);

                eprintln!(
                    "PIPELINE plan-ready: approve {} then resume: pipeline.sh --resume {task} {}",
                    state.planfile, state.repo
                );
                return finish(&mut state, "plan-ready", 5, None);
            }

            "build" => {
                let bname = format!("{}-{task}", state.builder);
                let build_prompt = format!(
                    "{brief_text}\n\nPipeline phase: BUILD — your plan is approved ({}). Implement it in your worktree, commit on quaqua/{bname}, run the tests. If the premise or the plan proves wrong, stop and write '## Challenge' with evidence instead of forcing it. If the work turns out to need no changes at all, say 'Outcome: no changes' in your runs file.",
                    state.planfile
                );

                wait_file(&mut state, &bname, &build_prompt, build_budget)?;

                let build_md = runs_dir.join(format!("{bname}.build.md"));
                let cur_md = runs_dir.join(format!("{bname}.md"));
                let _ = std::fs::rename(&cur_md, &build_md);

                if let Ok(content) = std::fs::read_to_string(&build_md) {
                    if content.lines().any(|l| l.trim().starts_with("## Challenge")) {
                        log_challenge(&build_md, &pfile);
                        let b = "builder challenged the premise during build — Lead resolves the Challenge above";
                        return finish(&mut state, "disputed", 0, Some(b));
                    }
                }

                if blocked_outcome(&build_md) {
                    log_blockers(&build_md, &pfile);
                    let b = "builder's build report is unfinished (see above) — Lead decides how to proceed";
                    return finish(&mut state, "blocked", 0, Some(b));
                }

                check_impl(&mut state, &build_md, None)?;

                let mut pcontent = std::fs::read_to_string(&pfile).unwrap_or_default();
                pcontent.push_str(&format!("- build: commit {} ({bname}.build.md)\n", state.commit));
                let _ = std::fs::write(&pfile, pcontent);

                let wt_path = Path::new(&state.wt);
                match state.review.as_str() {
                    "always" => state.review_run = "run".to_string(),
                    "skip" => state.review_run = "skip".to_string(),
                    _ => {
                        // auto
                        let numstat_out = git.run_in(wt_path, &["diff", "--numstat", &state.base_sha, &state.head_sha]);
                        let mut total_lines = 0usize;
                        if let Ok(o) = numstat_out {
                            for line in String::from_utf8_lossy(&o.stdout).lines() {
                                let parts: Vec<&str> = line.split_whitespace().collect();
                                if parts.len() >= 2 {
                                    let add: usize = parts[0].parse().unwrap_or(0);
                                    let del: usize = parts[1].parse().unwrap_or(0);
                                    total_lines += add + del;
                                }
                            }
                        }

                        let names_out = git.run_in(wt_path, &["diff", "--name-only", &state.base_sha, &state.head_sha]);
                        let mut dirs: BTreeSet<String> = BTreeSet::new();
                        if let Ok(o) = names_out {
                            for line in String::from_utf8_lossy(&o.stdout).lines() {
                                let path = line.trim();
                                if let Some(idx) = path.find('/') {
                                    dirs.insert(path[..idx].to_string());
                                } else if !path.is_empty() {
                                    dirs.insert(".".to_string());
                                }
                            }
                        }

                        let mut pcontent = std::fs::read_to_string(&pfile).unwrap_or_default();
                        pcontent.push_str(&format!("- review gate: {total_lines} changed lines, {} top-level dirs (run if >{review_lines} or >1)\n", dirs.len()));
                        let _ = std::fs::write(&pfile, pcontent);

                        if total_lines > review_lines || dirs.len() > 1 {
                            state.review_run = "run".to_string();
                        } else {
                            state.review_run = "skip".to_string();
                        }
                    }
                }

                if state.review_run == "skip" {
                    let mut pcontent = std::fs::read_to_string(&pfile).unwrap_or_default();
                    pcontent.push_str(&format!("- review: skipped (gate: {})\n", state.review));
                    let _ = std::fs::write(&pfile, pcontent);
                    return finish(&mut state, "clean", 0, None);
                }

                state.phase = "review".to_string();
                state.round = 1;
                state.prompted = 0;
                state.reviewer_spawned = 0;
                let _ = state.save(&state_file);
            }

            "review" => {
                let bname = format!("{}-{task}", state.builder);
                let revname = format!("{}-{task}r{}", state.reviewer, state.round);

                if state.reviewer_spawned == 0 {
                    let rev_report = runs_dir.join(format!("{revname}.md"));
                    if rev_report.is_file() {
                        archive_stale(&rev_report, &pfile);
                    }

                    let spawn_sh = crate::worker::cleanup::find_engine_script("spawn-worker.sh");

                    let sargs = vec![
                        "--review".to_string(),
                        "--base".to_string(),
                        state.head_sha.clone(),
                        revname.clone(),
                        state.reviewer.clone(),
                        state.repo.clone(),
                    ];

                    let spawn_ok = if let Some(ssh) = spawn_sh {
                        let mut cmd = Command::new("bash");
                        cmd.arg(ssh);
                        cmd.args(&sargs);
                        cmd.env("PIPELINE_TASK", &task);
                        cmd.env("QUAQUA_OWNER", if state.lead.is_empty() { "cos" } else { &state.lead });
                        cmd.env("GRID_SKIP", "1");
                        cmd.status().map(|s| s.success()).unwrap_or(false)
                    } else {
                        std::env::set_var("PIPELINE_TASK", &task);
                        std::env::set_var("QUAQUA_OWNER", if state.lead.is_empty() { "cos" } else { &state.lead });
                        let spawn_res = (|| -> Result<(), (i32, String)> {
                            let opts = crate::worker::spawn::parse_spawn_args(&sargs)?;
                            let cfg = load_team_config()?;
                            crate::worker::spawn::spawn_worker(opts, &cfg, git, herdr)
                        })();
                        std::env::remove_var("PIPELINE_TASK");
                        spawn_res.is_ok()
                    };

                    if !spawn_ok {
                        let b = format!("spawn-worker.sh failed for reviewer {revname} — fix the cause, then 'pipeline.sh --resume {task} {}'", state.repo);
                        return finish(&mut state, "blocked", 3, Some(&b));
                    }
                    state.reviewer_spawned = 1;
                    let _ = state.save(&state_file);

                    let mut pcontent = std::fs::read_to_string(&pfile).unwrap_or_default();
                    pcontent.push_str(&format!("- review round {}: {revname} spawned on {} (quaqua/{bname} tip)\n", state.round, state.head_sha));
                    let _ = std::fs::write(&pfile, pcontent);
                }

                // Grid hook for builder + reviewer (2 workers -> GRID_TAB workers tab)
                if !run_grid_hook() {
                    let b = format!("grid check failed for {revname} — arrange the panes per rules/_all/fanout-agent-grid.md, then 'pipeline.sh --resume {task} {}'", state.repo);
                    let _ = state.save(&state_file);
                    return finish(&mut state, "blocked", 5, Some(&b));
                }

                let review_prompt = format!(
                    "Goal: review the diff for task '{task}' and report findings with file:line evidence\nHard constraints: read-only — do not edit or create files outside ~/quaqua/runs; every finding must cite file:line in the diff.\nCurrent design (open to challenge): builder {bname} implemented the task on branch quaqua/{bname} — your worktree is on its tip; run 'git diff {}' for the full change. Original brief: {} (readable).\nUnknowns: whether the change meets the brief — verify correctness, missing tests, and scope creep against the diff, not the report.\nEvidence: a findings list, then the runs file MUST end with exactly one line: 'Verdict: clean' or 'Verdict: <N> findings'. Write to a temp file and rename it to ~/quaqua/runs/{revname}.md — publish atomically, never a partial file.\nPipeline phase: REVIEW (round {} of {})",
                    state.base_sha, state.brief, state.round, state.rounds
                );

                wait_file(&mut state, &revname, &review_prompt, review_budget)?;
                state.rounds_run = state.round;

                let rev_report = runs_dir.join(format!("{revname}.md"));
                let rev_content = std::fs::read_to_string(&rev_report).unwrap_or_default();

                let cleanup_reviewer = || {
                    let clean_sh = crate::worker::cleanup::find_engine_script("cleanup-worker.sh");
                    if let Some(csh) = clean_sh {
                        let mut cmd = Command::new("bash");
                        cmd.arg(csh);
                        cmd.arg(&revname);
                        cmd.arg(&state.repo);
                        cmd.env("PIPELINE_TASK", &task);
                        cmd.env("GRID_SKIP", "1");
                        let _ = cmd.status();
                    } else {
                        let _ = crate::worker::cleanup::cleanup_worker(&revname, Path::new(&state.repo), git, herdr);
                    }
                    let _ = run_grid_hook();
                };

                if rev_content.lines().any(|l| l.trim().starts_with("## Challenge")) {
                    log_challenge(&rev_report, &pfile);
                    let mut pcontent = std::fs::read_to_string(&pfile).unwrap_or_default();
                    pcontent.push_str(&format!("- review round {}: {revname} challenged the premise\n", state.round));
                    let _ = std::fs::write(&pfile, pcontent);
                    cleanup_reviewer();
                    let b = format!("reviewer {revname} challenged the premise — Lead resolves the Challenge above");
                    return finish(&mut state, "disputed", 0, Some(&b));
                }

                let mut verdict_line: Option<String> = None;
                for line in rev_content.lines() {
                    let t = line.trim();
                    if t.starts_with("Verdict:") || t.starts_with("verdict:") {
                        verdict_line = Some(t.to_string());
                    }
                }

                let mut pcontent = std::fs::read_to_string(&pfile).unwrap_or_default();
                pcontent.push_str(&format!(
                    "- review round {}: {} ({revname}.md)\n",
                    state.round,
                    verdict_line.as_deref().unwrap_or("NO VERDICT LINE")
                ));
                let _ = std::fs::write(&pfile, pcontent);

                cleanup_reviewer();

                let v = match verdict_line {
                    Some(vl) => vl,
                    None => {
                        let b = format!("reviewer {revname} runs file has no 'Verdict: clean | <N> findings' line (lock §1) — Lead decides");
                        return finish(&mut state, "blocked", 0, Some(&b));
                    }
                };

                let v_lower = v.to_ascii_lowercase();
                if v_lower.contains("clean") {
                    state.last_findings = 0;
                } else {
                    let mut found_count = false;
                    for part in v.split_whitespace() {
                        if let Ok(n) = part.parse::<u32>() {
                            state.last_findings = n;
                            found_count = true;
                            break;
                        }
                    }
                    if !found_count {
                        let b = format!("reviewer {revname} runs file has no 'Verdict: clean | <N> findings' line (lock §1) — Lead decides");
                        return finish(&mut state, "blocked", 0, Some(&b));
                    }
                }

                if state.last_findings == 0 {
                    return finish(&mut state, "clean", 0, None);
                }

                state.review_changed = true;
                if state.round >= state.rounds {
                    let b = format!("review rounds exhausted: {} findings still open after {} round(s) — Lead decides", state.last_findings, state.rounds);
                    return finish(&mut state, "blocked", 0, Some(&b));
                }

                state.phase = "fix".to_string();
                state.prompted = 0;
                let _ = state.save(&state_file);
            }

            "fix" => {
                let bname = format!("{}-{task}", state.builder);
                let revname = format!("{}-{task}r{}", state.reviewer, state.round);
                let fix_prompt = format!(
                    "Goal: resolve the {} review findings for task '{task}'\nHard constraints: stay in your worktree on quaqua/{bname}; commit each fix; keep tests green. A finding that is wrong is NOT fixed — dispute it under '## Challenge' in your runs file with evidence.\nCurrent design (open to challenge): reviewer {revname} reported {} findings — full list in {}/{revname}.md (readable); your worktree and branch are unchanged.\nUnknowns: which findings are real — verify each against the code before changing anything.\nEvidence: committed fixes + test output, or '## Challenge' per wrong finding.\nPipeline phase: FIX (after review round {})",
                    state.last_findings, state.last_findings, runs_dir.display(), state.round
                );

                wait_file(&mut state, &bname, &fix_prompt, fix_budget)?;

                let fix_md = runs_dir.join(format!("{bname}.fix{}.md", state.round));
                let cur_md = runs_dir.join(format!("{bname}.md"));
                let _ = std::fs::rename(&cur_md, &fix_md);

                if let Ok(content) = std::fs::read_to_string(&fix_md) {
                    if content.lines().any(|l| l.trim().starts_with("## Challenge")) {
                        log_challenge(&fix_md, &pfile);
                        let b = "builder disputed review findings — Lead resolves the Challenge above";
                        return finish(&mut state, "disputed", 0, Some(b));
                    }
                }

                if blocked_outcome(&fix_md) {
                    log_blockers(&fix_md, &pfile);
                    let b = "builder's fix report is unfinished (see above) — Lead decides how to proceed";
                    return finish(&mut state, "blocked", 0, Some(b));
                }

                let prev_head = state.head_sha.clone();
                check_impl(&mut state, &fix_md, Some(&prev_head))?;

                let mut pcontent = std::fs::read_to_string(&pfile).unwrap_or_default();
                pcontent.push_str(&format!("- fix {}: commit {} ({bname}.fix{}.md)\n", state.round, state.commit, state.round));
                let _ = std::fs::write(&pfile, pcontent);

                state.phase = "review".to_string();
                state.round += 1;
                state.prompted = 0;
                state.reviewer_spawned = 0;
                let _ = state.save(&state_file);
            }

            _ => {
                return Err((1, format!("corrupt state: phase={} ({})\n", state.phase, state_file.display())));
            }
        }
    }
}
