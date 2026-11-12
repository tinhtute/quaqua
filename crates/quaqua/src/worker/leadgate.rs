//! Lead-required gate for `worker spawn` (AGENTS.md '## Roles (SLP)': a Lead is
//! required when 3 or more task workers run in parallel, or when the work spans
//! more than one day).
//!
//! The gate is scoped to a project the CoS owns. It reads `projects/INDEX.md`,
//! finds the row for the spawn's repo, and acts only when that row's owner is
//! `cos`: a project already running under `<slug>-lead`, or a repo with no row
//! at all, passes untouched. It never decides "needs a Lead" from one signal —
//! two or more signals block the spawn, exactly one only warns.
//!
//! Signals, each derived from state that exists at spawn time:
//! 1. worker count — this spawn would be the 3rd (or later) live build worker
//!    on the same repo; a running pipeline counts as 2 slots (`team.toml`
//!    `limits.weights.pipeline`).
//! 2. plan — the project's active plan declares `phases_left: N` with N > 1.
//! 3. memory — `memory/YYYY-MM-DD.md` records `- [<slug>]` lines on two or more
//!    consecutive days, i.e. the project's work already spans days.
//! 4. brief budget — `Time budget` > 240 min or `Cost budget` > $10. Only the
//!    brief carries these, so only `worker prompt --check-brief` evaluates them.

use crate::cap::is_task_worker_name;
use crate::config::{kind_of_worker, Config};
use crate::herdr::HerdrOps;
use crate::pipeline::state::PipelineState;
use crate::state::WorkerMetadata;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// `--solo` needs a real reason, not a word.
pub const SOLO_MIN_CHARS: usize = 10;

/// A brief above this time cap is a signal that the unit is too big for one
/// worker without a Lead.
pub const TIME_BUDGET_MAX_SEC: u64 = 240 * 60;

/// Same, for money.
pub const COST_BUDGET_MAX_USD: f64 = 10.0;

/// Signals at or above this count block the spawn; one only warns.
pub const BLOCK_AT: usize = 2;

/// One reason this spawn probably needs a Lead.
#[derive(Debug, Clone, PartialEq)]
pub enum Signal {
    /// Live build-worker slots on this repo *after* this spawn.
    LiveBuildWorkers(i64),
    /// `phases_left` in the project's active plan.
    PhasesLeft { left: u32, plan: PathBuf },
    /// Consecutive days of `- [<slug>]` lines in `memory/`.
    MemoryDays(u32),
    /// The brief's `Time budget`.
    TimeBudget(u64),
    /// The brief's `Cost budget`.
    CostBudget(f64),
}

impl Signal {
    /// One line for the block/warn message.
    pub fn describe(&self) -> String {
        match self {
            Signal::LiveBuildWorkers(n) => format!(
                "worker count: {n} live build worker slots after this spawn (>= 3; team.toml strategy.build_kinds, a pipeline counts as 2)"
            ),
            Signal::PhasesLeft { left, plan } => format!(
                "active plan: phases_left {left} (> 1) in {}",
                plan.display()
            ),
            Signal::MemoryDays(days) => format!(
                "memory/: project lines on {days} consecutive days (> 1) — the work already spans days"
            ),
            Signal::TimeBudget(sec) => format!(
                "brief: Time budget {} min (> {})",
                sec / 60,
                TIME_BUDGET_MAX_SEC / 60
            ),
            Signal::CostBudget(usd) => {
                format!("brief: Cost budget ${usd} (> ${})", COST_BUDGET_MAX_USD)
            }
        }
    }
}

/// What the gate decided.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Pass,
    Warn(Vec<Signal>),
    Block(Vec<Signal>),
}

/// Two signals block, one warns, none passes.
pub fn verdict(signals: Vec<Signal>) -> Verdict {
    if signals.len() >= BLOCK_AT {
        Verdict::Block(signals)
    } else if signals.is_empty() {
        Verdict::Pass
    } else {
        Verdict::Warn(signals)
    }
}

// ── projects/INDEX.md ───────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexRow {
    pub slug: String,
    pub repo: String,
    pub owner: String,
}

/// Rows of a `projects/INDEX.md` table, header and separator skipped.
pub fn index_rows(content: &str) -> Vec<IndexRow> {
    content
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if !trimmed.starts_with('|') {
                return None;
            }
            let parts: Vec<&str> = trimmed.split('|').map(str::trim).collect();
            if parts.len() < 4 {
                return None;
            }
            let (slug, repo, owner) = (parts[1], parts[2], parts[3]);
            if slug.is_empty() || slug == "slug" || repo == "repo" {
                return None;
            }
            // The `|---|---|` separator row.
            if slug.chars().all(|c| c == '-' || c == ':') {
                return None;
            }
            Some(IndexRow {
                slug: slug.to_string(),
                repo: repo.to_string(),
                owner: owner.to_string(),
            })
        })
        .collect()
}

/// `~/x` → `<home>/x`. Any other path is returned unchanged.
pub fn expand_home(path: &str, home: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        return Path::new(home).join(rest);
    }
    if path == "~" {
        return PathBuf::from(home);
    }
    PathBuf::from(path)
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(ca), Ok(cb)) => ca == cb,
        _ => a == b,
    }
}

/// The INDEX row the gate applies to: the first row for `repo` whose owner is
/// `cos`. A repo with no row, or whose rows are all owned by a Lead, has none.
/// When the caller owns a row for this repo, the gate skips (returns `None`).
pub fn gate_row(
    rows: &[IndexRow],
    repo: &Path,
    home: &str,
    caller: &crate::identity::Caller,
) -> Option<IndexRow> {
    let matching_rows: Vec<&IndexRow> = rows
        .iter()
        .filter(|r| same_path(&expand_home(&r.repo, home), repo))
        .collect();

    if caller.name != "cos" && matching_rows.iter().any(|r| r.owner == caller.name) {
        return None;
    }

    matching_rows
        .into_iter()
        .find(|r| r.owner == "cos")
        .cloned()
}

// ── plan frontmatter ────────────────────────────────────────────────────────

/// The `docs/plans/active/<name>.md` path named by a project card, if any.
pub fn plan_path_in(text: &str) -> Option<String> {
    for token in text.split(|c: char| {
        c.is_whitespace() || matches!(c, '`' | '(' | ')' | ',' | ';' | '"' | '\'' | ':' | '*')
    }) {
        let token = token.trim_end_matches(['.', ',', ')']);
        if token.starts_with("docs/plans/") && token.ends_with(".md") {
            return Some(token.to_string());
        }
    }
    None
}

/// `phases_left` from a plan's leading `---` frontmatter block.
pub fn phases_left_in(text: &str) -> Option<u32> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return None;
    }
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        if let Some(rest) = trimmed.strip_prefix("phases_left:") {
            let digits: String = rest
                .trim()
                .chars()
                .take_while(|c| c.is_ascii_digit())
                .collect();
            return digits.parse().ok();
        }
    }
    None
}

/// The project's active plan: the `docs/plans/active/*.md` the card names,
/// else `docs/plans/active/<slug>.md`.
pub fn plan_file(root: &Path, slug: &str) -> Option<PathBuf> {
    let card = root.join("projects").join(format!("{slug}.md"));
    if let Ok(text) = std::fs::read_to_string(&card) {
        if let Some(rel) = plan_path_in(&text) {
            let candidate = root.join(rel);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    let candidate = root.join("docs/plans/active").join(format!("{slug}.md"));
    candidate.is_file().then_some(candidate)
}

// ── memory/ dates ───────────────────────────────────────────────────────────

/// Days since 1970-01-01 for a proleptic Gregorian date (Howard Hinnant's
/// `days_from_civil`), so consecutive-day runs need no calendar crate.
pub fn days_from_civil(y: i32, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y } as i64;
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m as i64 + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// `YYYY-MM-DD` → day number.
pub fn parse_date(s: &str) -> Option<i64> {
    let mut parts = s.split('-');
    let y: i32 = parts.next()?.parse().ok()?;
    let m: u32 = parts.next()?.parse().ok()?;
    let d: u32 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    Some(days_from_civil(y, m, d))
}

/// Longest run of consecutive days whose entry is `true`.
pub fn longest_consecutive_run(days: &[(i64, bool)]) -> u32 {
    let mut hits: Vec<i64> = days
        .iter()
        .filter(|(_, hit)| *hit)
        .map(|(d, _)| *d)
        .collect();
    hits.sort_unstable();
    hits.dedup();
    let mut best = 0u32;
    let mut run = 0u32;
    let mut prev: Option<i64> = None;
    for day in hits {
        run = match prev {
            Some(p) if day == p + 1 => run + 1,
            _ => 1,
        };
        best = best.max(run);
        prev = Some(day);
    }
    best
}

/// Consecutive days of `- [<slug>]` lines across `memory/*.md`.
pub fn memory_days_with_project(root: &Path, slug: &str) -> u32 {
    let needle = format!("- [{slug}]");
    let mut days: Vec<(i64, bool)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(root.join("memory")) else {
        return 0;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        let Some(stem) = name.strip_suffix(".md") else {
            continue;
        };
        let Some(day) = parse_date(stem) else {
            continue;
        };
        let Ok(content) = std::fs::read_to_string(entry.path()) else {
            continue;
        };
        let hit = content
            .lines()
            .any(|line| line.trim_start().starts_with(&needle));
        days.push((day, hit));
    }
    longest_consecutive_run(&days)
}

/// Append `- [<slug>] solo override: <reason>` to `memory/<date>.md`.
pub fn append_solo_override(
    root: &Path,
    date: &str,
    slug: &str,
    reason: &str,
) -> std::io::Result<()> {
    let dir = root.join("memory");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{date}.md"));
    let mut line = String::new();
    if let Ok(existing) = std::fs::read_to_string(&path) {
        if !existing.is_empty() && !existing.ends_with('\n') {
            line.push('\n');
        }
    }
    line.push_str(&format!("- [{slug}] solo override: {reason}\n"));
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    file.write_all(line.as_bytes())
}

/// Today's local date (`YYYY-MM-DD`); UTC from `now_sec` when `date` is absent.
pub fn today_local() -> String {
    if let Ok(out) = std::process::Command::new("date").arg("+%F").output() {
        if out.status.success() {
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if parse_date(&s).is_some() {
                return s;
            }
        }
    }
    let days = (crate::util::now_sec() / 86_400) as i64;
    civil_from_days(days)
}

/// Inverse of [`days_from_civil`].
pub fn civil_from_days(z: i64) -> String {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

// ── live workers per repo ───────────────────────────────────────────────────

/// One live task worker as the gate sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveWorker {
    pub name: String,
    pub kind: String,
    /// The repo the worker builds on, from `runs/<name>.repo` (written at
    /// spawn). `None` = unknown, e.g. a worker spawned before the gate existed:
    /// a worktree path is `<repo-parent>/.wt/<name>`, so it cannot name the
    /// repo when several repos share that parent.
    pub repo: Option<PathBuf>,
    /// Spawned with `--review`: read-only, holds no build slot.
    pub review: bool,
    /// The pipeline task this worker belongs to (`runs/<name>.pipeline`).
    pub pipeline: Option<String>,
}

/// A pipeline that still runs: its members hold `pipeline` slots, at least the
/// configured weight, together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LivePipeline {
    pub task: String,
    pub repo: PathBuf,
}

/// Build-worker slots on `repo` after the candidate spawn.
///
/// Members of a live pipeline on the repo count together as at least
/// `pipeline_weight`; every other live build worker counts 1. A kind holds a
/// build slot only when `team.toml` says so (`strategy.build_kinds`), so
/// review-only and advisory kinds hold none. A worker whose repo cannot be
/// attributed counts for nothing (it is not evidence about this repo).
///
/// `own_pipeline` is the pipeline the candidate spawn belongs to
/// (`PIPELINE_TASK`): re-spawning one of its members does not add a concurrent
/// slot, so that whole group is left out of the count.
pub fn count_build_slots(
    cfg: &Config,
    workers: &[LiveWorker],
    pipelines: &[LivePipeline],
    repo: &Path,
    pipeline_weight: i64,
    own_pipeline: Option<&str>,
) -> i64 {
    let build_kinds = cfg.build_kinds();
    let mut covered: HashSet<&str> = HashSet::new();
    let mut slots = 0i64;
    for pipeline in pipelines {
        if !same_path(&pipeline.repo, repo) {
            continue;
        }
        if Some(pipeline.task.as_str()) == own_pipeline {
            continue;
        }
        let members: Vec<&LiveWorker> = workers
            .iter()
            .filter(|w| w.pipeline.as_deref() == Some(pipeline.task.as_str()))
            .collect();
        for member in &members {
            covered.insert(member.name.as_str());
        }
        slots += (members.len() as i64).max(pipeline_weight);
    }
    for worker in workers {
        if covered.contains(worker.name.as_str()) || worker.review {
            continue;
        }
        if worker.pipeline.as_deref() == own_pipeline && own_pipeline.is_some() {
            continue;
        }
        if !build_kinds.contains(&worker.kind.as_str()) {
            continue;
        }
        match worker.repo.as_deref() {
            Some(r) if same_path(r, repo) => slots += 1,
            _ => {}
        }
    }
    slots
}

/// Read the live workers and live pipelines herdr reports.
fn gather_live<H: HerdrOps>(
    runs_dir: &Path,
    exclude: &str,
    cfg: &Config,
    herdr: &H,
) -> (Vec<LiveWorker>, Vec<LivePipeline>) {
    let mut workers = Vec::new();
    if let Ok(raw) = herdr.agent_list() {
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&raw) {
            if let Some(agents) = json
                .get("result")
                .and_then(|r| r.get("agents"))
                .and_then(|a| a.as_array())
            {
                for agent in agents {
                    let name = agent.get("name").and_then(|n| n.as_str()).unwrap_or("");
                    let status = agent
                        .get("agent_status")
                        .and_then(|s| s.as_str())
                        .unwrap_or("");
                    if name.is_empty() || status == "dead" || name == exclude {
                        continue;
                    }
                    if !is_task_worker_name(name, cfg) {
                        continue;
                    }
                    let Some(kind) = kind_of_worker(cfg, name) else {
                        continue;
                    };
                    let repo = WorkerMetadata::load_repo(runs_dir, name);
                    workers.push(LiveWorker {
                        name: name.to_string(),
                        kind: kind.to_string(),
                        repo,
                        review: WorkerMetadata::has_review(runs_dir, name),
                        pipeline: WorkerMetadata::load_pipeline(runs_dir, name),
                    });
                }
            }
        }
    }

    let mut pipelines = Vec::new();
    if let Ok(entries) = std::fs::read_dir(runs_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if !(file_name.starts_with("pipeline-") && file_name.ends_with(".state")) {
                continue;
            }
            let Ok(state) = PipelineState::load(&path) else {
                continue;
            };
            if state.finished != 0 || state.repo.is_empty() {
                continue;
            }
            let task = file_name
                .strip_prefix("pipeline-")
                .and_then(|n| n.strip_suffix(".state"))
                .unwrap_or("")
                .to_string();
            if task.is_empty() {
                continue;
            }
            let has_members = workers
                .iter()
                .any(|w| w.pipeline.as_deref() == Some(task.as_str()));
            let pid = state.pipeline_pid.to_string();
            let alive =
                has_members || (!pid.is_empty() && pid != "0" && crate::util::is_pid_alive(&pid));
            if !alive {
                continue;
            }
            pipelines.push(LivePipeline {
                task,
                repo: PathBuf::from(state.repo),
            });
        }
    }
    (workers, pipelines)
}

// ── brief headers ───────────────────────────────────────────────────────────

/// What the brief's `Topology:` header says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Topology {
    Solo(String),
    Lead(String),
}

/// Parse `Topology: solo|lead — <reason>` from the brief's header block.
pub fn topology(text: &str) -> Result<Topology, String> {
    const SHAPE: &str = "`Topology: solo|lead — <reason>`";
    let Some(value) = crate::worker::prompt::brief::header_value(text, "Topology") else {
        return Err(format!(
            "error: brief has no {SHAPE} header — declare the topology the coordinator chose (rules/_all/brief.md)\n"
        ));
    };
    if value.trim_start().starts_with('<') {
        return Err(format!(
            "error: brief's Topology header is still the {SHAPE} placeholder — fill it in\n"
        ));
    }
    let lower = value.to_ascii_lowercase();
    let (topology, rest) = if lower.starts_with("solo") {
        ("solo", &value[4..])
    } else if lower.starts_with("lead") {
        ("lead", &value[4..])
    } else {
        return Err(format!(
            "error: brief's Topology header must start with `solo` or `lead` (got '{value}')\n"
        ));
    };
    let boundary = rest.chars().next();
    if let Some(c) = boundary {
        if c.is_alphanumeric() {
            return Err(format!(
                "error: brief's Topology header must start with `solo` or `lead` (got '{value}')\n"
            ));
        }
    }
    let reason = rest
        .trim_start_matches(|c: char| c.is_whitespace() || matches!(c, '-' | '—' | ':' | ','))
        .trim();
    if reason.is_empty() {
        return Err(format!(
            "error: brief's Topology header needs a reason after `{topology} —`\n"
        ));
    }
    Ok(if topology == "solo" {
        Topology::Solo(reason.to_string())
    } else {
        Topology::Lead(reason.to_string())
    })
}

/// The budget signals a brief carries, for the check that has no repo.
pub fn brief_signals(text: &str) -> Vec<Signal> {
    let mut signals = Vec::new();
    if let Some(sec) = crate::worker::prompt::brief_budget_sec(text) {
        if sec > TIME_BUDGET_MAX_SEC {
            signals.push(Signal::TimeBudget(sec));
        }
    }
    if let Some(usd) = crate::worker::prompt::cost::brief_cost_budget(text) {
        if usd > COST_BUDGET_MAX_USD {
            signals.push(Signal::CostBudget(usd));
        }
    }
    signals
}

// ── the gate ────────────────────────────────────────────────────────────────

/// Everything the gate reads for one spawn.
pub struct GateInput<'a> {
    /// Workspace root (`projects/INDEX.md`, `docs/`, `memory/`).
    pub root: &'a Path,
    /// `runs/` — worker and pipeline metadata.
    pub runs_dir: &'a Path,
    /// The spawn's canonical repo path.
    pub repo: &'a Path,
    /// The worker about to spawn (never counted against itself).
    pub name: &'a str,
    /// The brief, when the caller has it (`worker spawn` does not).
    pub brief: Option<&'a str>,
    /// `--solo "<reason>"`, when given.
    pub solo: Option<&'a str>,
    /// The pipeline this spawn belongs to (`PIPELINE_TASK`), if any.
    pub pipeline: Option<&'a str>,
    /// Resolved caller identity.
    pub caller: &'a crate::identity::Caller,
}

/// The signals a spawn carries, and the project it resolved to.
#[derive(Debug, Clone, PartialEq)]
pub struct Facts {
    pub slug: String,
    pub signals: Vec<Signal>,
}

/// Collect the spawn's signals for a `cos`-owned project. `None` when the repo
/// is not in `projects/INDEX.md` or its row is owned by a Lead.
pub fn gather_facts<H: HerdrOps>(input: &GateInput<'_>, cfg: &Config, herdr: &H) -> Option<Facts> {
    let index = std::fs::read_to_string(input.root.join("projects/INDEX.md")).ok()?;
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
    let row = gate_row(&index_rows(&index), input.repo, &home, input.caller)?;
    let mut signals = Vec::new();

    let (workers, pipelines) = gather_live(input.runs_dir, input.name, cfg, herdr);
    let weight = cfg.limits.weights.get("pipeline").copied().unwrap_or(2);
    let slots = count_build_slots(
        cfg,
        &workers,
        &pipelines,
        input.repo,
        weight,
        input.pipeline,
    ) + 1;
    if slots >= 3 {
        signals.push(Signal::LiveBuildWorkers(slots));
    }

    if let Some(plan) = plan_file(input.root, &row.slug) {
        if let Ok(text) = std::fs::read_to_string(&plan) {
            if let Some(left) = phases_left_in(&text) {
                if left > 1 {
                    signals.push(Signal::PhasesLeft { left, plan });
                }
            }
        }
    }

    let days = memory_days_with_project(input.root, &row.slug);
    if days > 1 {
        signals.push(Signal::MemoryDays(days));
    }

    if let Some(brief) = input.brief {
        signals.extend(brief_signals(brief));
    }

    Some(Facts {
        slug: row.slug,
        signals,
    })
}

fn block_message(slug: &str, repo: &Path, signals: &[Signal]) -> String {
    let mut msg = format!(
        "error: this spawn needs a Lead — {} topology signals for '{slug}' ({}):\n",
        signals.len(),
        repo.display()
    );
    for signal in signals {
        msg.push_str(&format!("  - {}\n", signal.describe()));
    }
    msg.push_str(&format!(
        "hint: start the project Lead with `quaqua lead spawn {slug}` (AGENTS.md '## Roles (SLP)'), then spawn this worker from it\n"
    ));
    msg.push_str(&format!(
        "hint: or pass `--solo \"<reason, at least {SOLO_MIN_CHARS} chars>\"` to spawn without a Lead and record why\n"
    ));
    msg
}

fn warn_message(slug: &str, signals: &[Signal]) -> String {
    let mut msg = format!(
        "warning: topology: 1 signal for '{slug}' — a Lead is likely needed (`quaqua lead spawn {slug}`):\n"
    );
    for signal in signals {
        msg.push_str(&format!("  - {}\n", signal.describe()));
    }
    msg
}

/// Run the gate for one spawn. `Err` blocks the spawn; `Ok` lets it through.
pub fn check_spawn<H: HerdrOps>(
    input: GateInput<'_>,
    cfg: &Config,
    herdr: &H,
) -> Result<(), (i32, String)> {
    let Some(facts) = gather_facts(&input, cfg, herdr) else {
        return Ok(());
    };
    match verdict(facts.signals.clone()) {
        Verdict::Pass => Ok(()),
        Verdict::Warn(signals) => {
            eprint!("{}", warn_message(&facts.slug, &signals));
            Ok(())
        }
        Verdict::Block(signals) => match input.solo {
            Some(reason) => {
                eprintln!(
                    "solo override ({slug}): {reason}",
                    slug = facts.slug,
                    reason = reason
                );
                let today = today_local();
                if let Err(e) = append_solo_override(input.root, &today, &facts.slug, reason) {
                    return Err((
                        1,
                        format!(
                            "error: cannot record the solo override in {}/memory/{today}.md: {e}\n",
                            input.root.display()
                        ),
                    ));
                }
                Ok(())
            }
            None => Err((1, block_message(&facts.slug, input.repo, &signals))),
        },
    }
}

/// `--solo` reason validation: a real reason, not a word.
pub fn validate_solo(reason: &str) -> Result<(), (i32, String)> {
    let trimmed = reason.trim();
    if trimmed.chars().count() < SOLO_MIN_CHARS {
        return Err((
            1,
            format!(
                "error: --solo needs a reason of at least {SOLO_MIN_CHARS} characters (got '{}')\n",
                trimmed
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(slug: &str, repo: &str, owner: &str) -> String {
        format!("| {slug} | {repo} | {owner} | active | goal |\n")
    }

    #[test]
    fn index_rows_skip_header_and_short_lines() {
        let content = format!(
            "# Projects\n\n| slug | repo | owner | status | goal |\n|---|---|---|---|---|\n{}| mu | ~/Lab/mu | mu-lead | active | x |\n| short |\n",
            row("q", "~/Lab/q", "cos")
        );
        let rows = index_rows(&content);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].slug, "q");
        assert_eq!(rows[0].owner, "cos");
        assert_eq!(rows[1].owner, "mu-lead");
    }

    #[test]
    fn gate_row_needs_a_cos_owner_for_that_repo() {
        let tmp = std::env::temp_dir().join(format!("quaqua-leadgate-row-{}", std::process::id()));
        let repo = tmp.join("repo");
        let other = tmp.join("other");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let content = format!(
            "| slug | repo | owner | status | goal |\n|---|---|---|---|---|\n{}{}{}",
            row("alpha", repo.to_str().unwrap(), "alpha-lead"),
            row("beta", repo.to_str().unwrap(), "cos"),
            row("gamma", "~/Lab/gamma", "cos"),
        );
        let rows = index_rows(&content);
        let cos = crate::identity::Caller {
            name: "cos".to_string(),
            role: crate::identity::Role::Cos,
            depth: crate::identity::CallerDepth::Resolved(0),
            source: crate::identity::IdentitySource::PaneTokens,
        };
        assert_eq!(
            gate_row(&rows, &repo, "/tmp/home", &cos).map(|r| r.slug),
            Some("beta".to_string())
        );
        assert_eq!(gate_row(&rows, &other, "/tmp/home", &cos), None);
        let _ = std::fs::remove_dir_all(&tmp);
    }
    #[test]
    fn gate_row_skips_when_caller_is_lead_or_owns_repo_row() {
        let tmp = std::env::temp_dir().join(format!("quaqua-leadgate-lead-{}", std::process::id()));
        let repo = tmp.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let content = format!(
            "| slug | repo | owner | status | goal |\n|---|---|---|---|---|\n{}{}{}",
            row("quaqua-rs", repo.to_str().unwrap(), "cos"),
            row("gate", repo.to_str().unwrap(), "gate-lead"),
            row("bus", repo.to_str().unwrap(), "bus-lead"),
        );
        let rows = index_rows(&content);

        // A CoS caller finds the cos-owned row
        let cos_caller = crate::identity::Caller {
            name: "cos".to_string(),
            role: crate::identity::Role::Cos,
            depth: crate::identity::CallerDepth::Resolved(0),
            source: crate::identity::IdentitySource::PaneTokens,
        };
        assert_eq!(
            gate_row(&rows, &repo, "/tmp/home", &cos_caller).map(|r| r.slug),
            Some("quaqua-rs".to_string())
        );

        // A running project Lead on that repo (gate-lead) skips the gate
        let lead_caller = crate::identity::Caller {
            name: "gate-lead".to_string(),
            role: crate::identity::Role::Lead,
            depth: crate::identity::CallerDepth::Resolved(1),
            source: crate::identity::IdentitySource::PaneTokens,
        };
        assert_eq!(gate_row(&rows, &repo, "/tmp/home", &lead_caller), None);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn gate_row_gates_lead_on_foreign_cos_repo() {
        let tmp =
            std::env::temp_dir().join(format!("quaqua-leadgate-foreign-{}", std::process::id()));
        let repo = tmp.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let content = format!(
            "| slug | repo | owner | status | goal |\n|---|---|---|---|---|\n{}{}",
            row("foreign-cos", repo.to_str().unwrap(), "cos"),
            row("my-proj", "~/Lab/my-proj", "my-lead"),
        );
        let rows = index_rows(&content);

        // my-lead does NOT own a row on `repo`; it must be gated on foreign-cos
        let foreign_lead = crate::identity::Caller {
            name: "my-lead".to_string(),
            role: crate::identity::Role::Lead,
            depth: crate::identity::CallerDepth::Resolved(1),
            source: crate::identity::IdentitySource::PaneTokens,
        };
        assert_eq!(
            gate_row(&rows, &repo, "/tmp/home", &foreign_lead).map(|r| r.slug),
            Some("foreign-cos".to_string())
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn gate_row_skips_when_non_lead_owns_repo_row() {
        let tmp =
            std::env::temp_dir().join(format!("quaqua-leadgate-nonlead-{}", std::process::id()));
        let repo = tmp.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let content = format!(
            "| slug | repo | owner | status | goal |\n|---|---|---|---|---|\n{}{}",
            row("custom-proj", repo.to_str().unwrap(), "custom-agent"),
            row("cos-proj", repo.to_str().unwrap(), "cos"),
        );
        let rows = index_rows(&content);

        // Caller with Role::Unknown who owns custom-proj skips the gate
        let non_lead = crate::identity::Caller {
            name: "custom-agent".to_string(),
            role: crate::identity::Role::Unknown,
            depth: crate::identity::CallerDepth::Unknown,
            source: crate::identity::IdentitySource::Unknown,
        };
        assert_eq!(gate_row(&rows, &repo, "/tmp/home", &non_lead), None);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn phases_left_reads_frontmatter_only() {
        assert_eq!(
            phases_left_in("---\nstatus: active\nphases_left: 3\n---\n# plan\nphases_left: 9\n"),
            Some(3)
        );
        assert_eq!(phases_left_in("# plan\nphases_left: 3\n"), None);
        assert_eq!(phases_left_in("---\nstatus: active\n---\n"), None);
    }

    #[test]
    fn plan_path_from_card_and_slug() {
        assert_eq!(
            plan_path_in("per docs/plans/active/mole-parity.md (Phases 1–6).").as_deref(),
            Some("docs/plans/active/mole-parity.md")
        );
        assert_eq!(plan_path_in("no plan here"), None);
    }

    #[test]
    fn consecutive_days_need_neighbours() {
        let d = |s: &str| parse_date(s).unwrap();
        let days = vec![
            (d("2026-10-19"), true),
            (d("2026-10-20"), true),
            (d("2026-10-21"), false),
            (d("2026-10-22"), true),
            (d("2026-10-23"), true),
        ];
        assert_eq!(longest_consecutive_run(&days), 2);
        let sparse = vec![(d("2026-10-19"), true), (d("2026-10-21"), true)];
        assert_eq!(longest_consecutive_run(&sparse), 1);
        assert_eq!(longest_consecutive_run(&[]), 0);
    }

    #[test]
    fn build_slots_count_reviewers_for_nothing_and_pipelines_as_weight() {
        // The template's own set: `tieude`, `sude` and `ni` hold a slot,
        // `suhuynh` and `truonglao` do not.
        let cfg = crate::config::embedded_team_config().expect("embedded team.toml");
        let repo = PathBuf::from("/r");
        let worker = |name: &str, kind: &str, review: bool, pipeline: Option<&str>| LiveWorker {
            name: name.to_string(),
            kind: kind.to_string(),
            repo: Some(repo.clone()),
            review,
            pipeline: pipeline.map(str::to_string),
        };
        let workers = vec![
            worker("tieude-a", "tieude", false, None),
            worker("sude-b", "sude", false, None),
            worker("suhuynh-c", "suhuynh", true, None),
            worker("sude-pipe", "sude", false, Some("p1")),
            worker("suhuynh-p1r", "suhuynh", true, Some("p1")),
        ];
        // No pipeline state on disk: every live build worker counts alone, so
        // the pipeline builder is not swallowed by a pipeline we cannot see.
        assert_eq!(count_build_slots(&cfg, &workers, &[], &repo, 2, None), 3);
        let pipelines = vec![LivePipeline {
            task: "p1".to_string(),
            repo: repo.clone(),
        }];
        assert_eq!(
            count_build_slots(&cfg, &workers, &pipelines, &repo, 2, None),
            4
        );
        // Re-spawning a member of that pipeline adds no concurrent slot.
        assert_eq!(
            count_build_slots(&cfg, &workers, &pipelines, &repo, 2, Some("p1")),
            2
        );
        // A pipeline on another repo does not count here.
        let elsewhere = vec![LivePipeline {
            task: "p1".to_string(),
            repo: PathBuf::from("/elsewhere"),
        }];
        assert_eq!(
            count_build_slots(&cfg, &workers, &elsewhere, &repo, 2, None),
            3
        );
    }

    /// A fixture team.toml whose kinds are `sude2` and `suhuynh`; `strategy`
    /// is the lines under `[strategy]` beyond `default_kind`.
    fn cfg_with_strategy(strategy: &str) -> Config {
        let doc = format!(
            r#"
schema_version = 2
[limits]
max_workers_per_orchestrator = 4
[strategy]
default_kind = "sude2"
{strategy}
[kinds.sude2]
harness = "omp"
model = "m"
[kinds.suhuynh]
harness = "claude"
model = "m"
"#
        );
        toml::from_str(&doc).expect("fixture team.toml")
    }

    #[test]
    fn build_slot_kinds_come_from_team_toml() {
        let repo = PathBuf::from("/r");
        let worker = |name: &str, kind: &str| LiveWorker {
            name: name.to_string(),
            kind: kind.to_string(),
            repo: Some(repo.clone()),
            review: false,
            pipeline: None,
        };
        let workers = vec![worker("sude2-a", "sude2"), worker("suhuynh-b", "suhuynh")];

        // A workspace that renames its builder kind: the renamed kind holds a
        // slot, the reviewer kind does not.
        let renamed = cfg_with_strategy(r#"build_kinds = ["sude2"]"#);
        assert_eq!(
            count_build_slots(&renamed, &workers, &[], &repo, 2, None),
            1
        );

        // No `strategy.build_kinds`: every configured kind holds a slot.
        let bare = cfg_with_strategy("");
        assert_eq!(count_build_slots(&bare, &workers, &[], &repo, 2, None), 2);
    }

    #[test]
    fn verdict_needs_two_signals() {
        assert_eq!(verdict(vec![]), Verdict::Pass);
        assert!(matches!(
            verdict(vec![Signal::MemoryDays(2)]),
            Verdict::Warn(_)
        ));
        assert!(matches!(
            verdict(vec![Signal::MemoryDays(2), Signal::TimeBudget(15000)]),
            Verdict::Block(_)
        ));
    }

    #[test]
    fn topology_header_shapes() {
        let brief = |t: &str| {
            format!("Playbook: none, tiny\nContext: none, tiny\nTopology: {t}\nGoal: x\n")
        };
        assert_eq!(
            topology(&brief("solo — one engine task")),
            Ok(Topology::Solo("one engine task".to_string()))
        );
        assert_eq!(
            topology(&brief("lead - 3 workers in parallel")),
            Ok(Topology::Lead("3 workers in parallel".to_string()))
        );
        assert_eq!(
            topology(&brief("Lead: the CoS runs P7–P11")),
            Ok(Topology::Lead("the CoS runs P7–P11".to_string()))
        );
        assert!(topology("Goal: x\nHard constraints: y\n").is_err());
        assert!(topology(&brief("solo")).is_err());
        assert!(topology(&brief("solo — ")).is_err());
        assert!(topology(&brief("<solo|lead — reason>")).is_err());
        assert!(topology(&brief("soloist — nope")).is_err());
    }

    #[test]
    fn solo_reason_needs_ten_characters() {
        assert!(validate_solo("short").is_err());
        assert!(validate_solo("  ").is_err());
        assert!(validate_solo("one engine task").is_ok());
    }

    #[test]
    fn brief_signals_fire_above_the_caps() {
        let over = "Time budget: 300 min\nCost budget: $12\n";
        assert_eq!(
            brief_signals(over),
            vec![Signal::TimeBudget(18000), Signal::CostBudget(12.0)]
        );
        assert!(brief_signals("Time budget: 240 min\nCost budget: $10\n").is_empty());
    }

    #[test]
    fn solo_override_appends_one_line_to_todays_memory() {
        let tmp = std::env::temp_dir().join(format!("quaqua-leadgate-solo-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("memory")).unwrap();
        std::fs::write(tmp.join("memory/2026-10-23.md"), "- [q] earlier line").unwrap();
        append_solo_override(&tmp, "2026-10-23", "q", "one engine task").unwrap();
        let text = std::fs::read_to_string(tmp.join("memory/2026-10-23.md")).unwrap();
        assert_eq!(
            text,
            "- [q] earlier line\n- [q] solo override: one engine task\n"
        );
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn today_local_is_a_date() {
        assert!(parse_date(&today_local()).is_some());
        assert_eq!(civil_from_days(0), "1970-01-01");
        assert_eq!(days_from_civil(1970, 1, 1), 0);
    }
}
