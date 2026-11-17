//! `worker spawn` and `lead spawn`: the CLI-facing spawn verbs.
//!
//! Both are pure functions over a [`Ctx`] (database, herdr, drivers, runs dir,
//! caller, anchor pane); the `*_cli` handlers only wire the live pieces. Every
//! step derives from existing state, so a repeat converges: an open agent keeps
//! its pane, a registered worktree and an existing branch are reused, nothing
//! is created twice.
//!
//! Legacy contract (`crates/quaqua`): `worker/spawn.rs` (grammar, name rule,
//! `--base`, `--review`), `cap.rs` (owner cap), `worker/leadgate.rs` (two
//! signals block, one warns, `--solo` records), `lead/spawn.rs`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::authority::{self, Decision};
use crate::cli::{CliError, Env};
use crate::config;
use crate::driver::Registry as Drivers;
use crate::herdr::HerdrClient;
use crate::task::spawn::{spawn, Role, SpawnSpec};

const NAME_MAX: usize = 20;
const LEAD_NAME_MAX: usize = 32;
const SOLO_MIN_CHARS: usize = 10;
/// Open workers of a Lead that raise topology signal 1 (this spawn is the next).
const GATE_LIVE_BUILDS: i64 = 2;
/// `phases_left` above this raises topology signal 2 (legacy: `left > 1`).
const GATE_PHASES_LEFT: u32 = 1;
/// Consecutive days of project memory above this raise topology signal 3.
const GATE_MEMORY_DAYS: u32 = 1;
/// Longest wait for a fresh Lead to be ready for input, ms (`QQ_LEAD_READY_MS`).
const LEAD_READY_MS: u64 = 300_000;
const DEFAULT_CAP: u64 = 8;
const DEFAULT_MAX_PEERS: u64 = 4;

pub const WORKER_USAGE: &str = "usage: worker spawn [--base <ref>] [--purpose <p>] [--review] [--solo <reason>] <name> [<kind>] <repo-path>";
pub const LEAD_USAGE: &str = "usage: lead spawn <slug> [--task <kickoff>]";

pub struct Ctx<'a> {
    pub conn: &'a Connection,
    pub herdr: &'a dyn HerdrClient,
    pub drivers: &'a Drivers,
    pub runs_dir: PathBuf,
    /// The calling agent's name; `None` is the CoS.
    pub caller: Option<&'a str>,
    /// A pane to split new panes from.
    pub anchor: Option<&'a str>,
    pub now_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkerSpawned {
    pub name: String,
    pub kind: String,
    pub pane: String,
    pub worktree: String,
    pub branch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeadSpawned {
    pub name: String,
    pub kind: String,
    pub pane: String,
    pub integ_branch: String,
    pub kickoff_sent: bool,
    /// The command to run next when no `--task` was given.
    pub next: Option<String>,
}

type R<T> = Result<T, CliError>;

fn fail(msg: impl Into<String>) -> CliError {
    CliError::new(1, msg)
}

fn db<E: std::fmt::Display>(e: E) -> CliError {
    fail(format!("database: {e}"))
}

fn cfg<E: std::fmt::Debug>(e: E) -> CliError {
    fail(format!("config: {e:?}"))
}

// ---- git ------------------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| format!("git could not be run: {e}"))?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        Err(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

fn git_ok(dir: &Path, args: &[&str]) -> bool {
    git(dir, args).is_ok()
}

/// A worktree of `branch` at `wt`: reused when registered, else added on the
/// existing branch, else added on a new branch cut from `base`.
fn ensure_worktree(repo: &Path, wt: &Path, branch: &str, base: &str) -> R<()> {
    if wt.exists() && git_ok(wt, &["rev-parse", "--git-dir"]) {
        return Ok(());
    }
    let _ = git(repo, &["worktree", "prune"]);
    let wt_s = wt.to_string_lossy();
    let exists = git_ok(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    );
    let r = if exists {
        git(repo, &["worktree", "add", &wt_s, branch])
    } else {
        git(repo, &["worktree", "add", "-b", branch, &wt_s, base])
    };
    r.map(|_| ()).map_err(fail)
}

/// The worktree can commit but not push: the origin pushurl is dead.
fn block_push(repo: &Path, wt: &Path) {
    if !git_ok(repo, &["remote", "get-url", "origin"]) {
        return;
    }
    let _ = git(repo, &["config", "extensions.worktreeConfig", "true"]);
    let _ = git(
        wt,
        &["config", "--worktree", "remote.origin.pushurl", "no_push"],
    );
}

// ---- small helpers ----------------------------------------------------------------

fn workspace_root(runs_dir: &Path) -> PathBuf {
    runs_dir.parent().unwrap_or(runs_dir).to_path_buf()
}

/// `<repo parent>/.wt/<name>`: where cleanup and the prompt watchdog look.
fn worktree_of(repo: &Path, name: &str) -> PathBuf {
    repo.parent().unwrap_or(repo).join(".wt").join(name)
}

fn base_name(p: &Path) -> String {
    p.file_name()
        .map_or_else(|| "repo".to_string(), |f| f.to_string_lossy().into_owned())
}

/// UTC `YYYY-MM-DD` of `ms` since the epoch (civil-from-days).
fn date_of(ms: u64) -> String {
    let z = (ms / 86_400_000) as i64 + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Today's local date, as `date +%F` prints it (legacy `today_local`); the UTC
/// date only when `date` cannot run.
fn today_local(now_ms: u64) -> String {
    Command::new("date")
        .arg("+%F")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|d| day_number(d).is_some())
        .unwrap_or_else(|| date_of(now_ms))
}

/// Days since the epoch of a `YYYY-MM-DD` string.
fn day_number(s: &str) -> Option<i64> {
    let mut it = s.split('-');
    let y: i64 = it.next()?.parse().ok()?;
    let m: i64 = it.next()?.parse().ok()?;
    let d: i64 = it.next()?.parse().ok()?;
    if it.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = y - i64::from(m <= 2);
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

/// The longest run of consecutive days whose `memory/<date>.md` has a
/// `- [<slug>]` line (legacy `memory_days_with_project`).
fn memory_days(root: &Path, slug: &str) -> u32 {
    let needle = format!("- [{slug}]");
    let Ok(entries) = std::fs::read_dir(root.join("memory")) else {
        return 0;
    };
    let mut days: Vec<i64> = entries
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let day = day_number(name.strip_suffix(".md")?)?;
            let text = std::fs::read_to_string(e.path()).ok()?;
            text.lines()
                .any(|l| l.trim_start().starts_with(&needle))
                .then_some(day)
        })
        .collect();
    days.sort_unstable();
    days.dedup();
    let (mut best, mut run, mut prev) = (0u32, 0u32, None::<i64>);
    for d in days {
        run = if prev == Some(d - 1) { run + 1 } else { 1 };
        best = best.max(run);
        prev = Some(d);
    }
    best
}

fn valid_name(name: &str, kind: &str) -> Result<(), String> {
    let task = name
        .strip_prefix(kind)
        .and_then(|r| r.strip_prefix('-'))
        .filter(|t| !t.is_empty())
        .ok_or_else(|| format!("name '{name}' must be <kind>-<task> (kind '{kind}')"))?;
    let _ = task;
    let ok = name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !ok || name.len() > NAME_MAX || name.ends_with("-lead") {
        return Err(format!(
            "name '{name}' must be ascii [a-z0-9-], at most {NAME_MAX} characters, not ending in -lead"
        ));
    }
    Ok(())
}

fn limit(conn: &Connection, key: &str, default: u64) -> u64 {
    config::setting(conn, key)
        .ok()
        .flatten()
        .and_then(|v| v.as_u64())
        .unwrap_or(default)
}

fn text_setting(conn: &Connection, key: &str) -> Option<String> {
    config::setting(conn, key)
        .ok()
        .flatten()
        .and_then(|v| v.as_str().map(String::from))
}

fn pane_of(reply_body: &str) -> Option<String> {
    serde_json::from_str::<Value>(reply_body)
        .ok()?
        .pointer("/result/pane/pane_id")?
        .as_str()
        .map(String::from)
}

/// The pane herdr already holds for agent `name`, if any.
pub fn existing_pane(herdr: &dyn HerdrClient, name: &str) -> Option<String> {
    let r = herdr.agent_get(name).ok().filter(|r| r.ok)?;
    serde_json::from_str::<Value>(&r.body)
        .ok()?
        .pointer("/result/agent/pane_id")?
        .as_str()
        .map(String::from)
}

/// A pane whose shell starts in `cwd`, split from the anchor.
fn new_pane(ctx: &Ctx, cwd: &Path) -> R<String> {
    let anchor = ctx
        .anchor
        .ok_or_else(|| fail("no anchor pane to split: run inside herdr (HERDR_PANE_ID)"))?;
    let r = ctx
        .herdr
        .pane_split_in(anchor, "right", &cwd.to_string_lossy())
        .map_err(|e| fail(format!("herdr could not split a pane: {e}")))?;
    if !r.ok {
        return Err(fail(format!(
            "herdr refused the split: {}",
            r.error_code().unwrap_or(r.body)
        )));
    }
    pane_of(&r.body).ok_or_else(|| fail("herdr split reply has no pane id"))
}

fn report_tokens(ctx: &Ctx, pane: &str, tokens: &[(&str, &str)]) -> R<()> {
    let mut all: Vec<(&str, &str)> = tokens.to_vec();
    let farm = crate::grid::farm_id(&ctx.runs_dir);
    if let Some(f) = farm.as_deref() {
        all.push(("grid_farm", f));
    }
    let r = ctx
        .herdr
        .pane_report_metadata(pane, "quaqua", &all, None)
        .map_err(|e| fail(format!("herdr could not tag the pane: {e}")))?;
    if r.ok {
        Ok(())
    } else {
        Err(fail(format!(
            "herdr refused the pane tokens: {}",
            r.error_code().unwrap_or(r.body)
        )))
    }
}

fn open_agent(conn: &Connection, name: &str) -> R<Option<i64>> {
    conn.query_row(
        "SELECT id FROM agent WHERE name = ?1 AND ended_ms IS NULL",
        [name],
        |r| r.get(0),
    )
    .optional()
    .map_err(db)
}

fn check_authority(
    ctx: &Ctx,
    target: authority::Role,
    project: Option<i64>,
) -> R<Option<authority::Actor>> {
    let Some(caller) = ctx.caller else {
        return Ok(None);
    };
    match authority::check_spawn(ctx.conn, caller, target, project)
        .map_err(|e| fail(e.to_string()))?
    {
        Decision::Allowed => authority::resolve(ctx.conn, caller).map_err(|e| fail(e.to_string())),
        Decision::Refused(r) => Err(fail(format!("refused: {}", r.reason))),
    }
}

// ---- worker spawn -------------------------------------------------------------------

#[derive(Debug, Default)]
struct WorkerArgs {
    base: Option<String>,
    purpose: Option<String>,
    review: bool,
    solo: Option<String>,
    pos: Vec<String>,
}

fn parse_worker(args: &[String]) -> R<WorkerArgs> {
    let mut out = WorkerArgs::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        let mut value = |flag: &str| {
            it.next()
                .cloned()
                .ok_or_else(|| fail(format!("{flag} needs a value\n{WORKER_USAGE}")))
        };
        match a.as_str() {
            "--base" => out.base = Some(value("--base")?),
            "--purpose" => out.purpose = Some(value("--purpose")?),
            "--solo" => out.solo = Some(value("--solo")?),
            "--review" => out.review = true,
            f if f.starts_with("--") => {
                return Err(fail(format!("unknown flag {f}\n{WORKER_USAGE}")))
            }
            _ => out.pos.push(a.clone()),
        }
    }
    Ok(out)
}

pub fn run_worker(ctx: &Ctx, args: &[String]) -> R<WorkerSpawned> {
    let a = parse_worker(args)?;
    if let Some(reason) = &a.solo {
        if reason.trim().chars().count() < SOLO_MIN_CHARS {
            return Err(fail(format!(
                "--solo needs a reason of at least {SOLO_MIN_CHARS} characters (got '{}')",
                reason.trim()
            )));
        }
    }
    let from_purpose = match &a.purpose {
        Some(p) => Some((
            p.clone(),
            config::purpose_kind(ctx.conn, p)
                .map_err(cfg)?
                .ok_or_else(|| fail(format!("unknown purpose '{p}'")))?,
        )),
        None => None,
    };
    let (name, kind, repo_arg) = match (a.pos.as_slice(), &from_purpose) {
        ([n, k, r], _) => (n, k.clone(), r),
        ([n, r], Some((_, k))) => (n, k.clone(), r),
        ([_], None) => {
            return Err(fail(format!(
                "missing <kind> and <repo-path>\n{WORKER_USAGE}"
            )))
        }
        ([_, _], None) => return Err(fail(format!("missing <repo-path>\n{WORKER_USAGE}"))),
        _ => return Err(fail(WORKER_USAGE)),
    };
    if let Some((p, k)) = &from_purpose {
        if *k != kind {
            return Err(fail(format!(
                "--purpose {p} resolves to kind '{k}', not '{kind}'"
            )));
        }
    }
    valid_name(name, &kind).map_err(fail)?;
    let k = config::kind(ctx.conn, &kind)
        .map_err(cfg)?
        .ok_or_else(|| fail(format!("unknown kind '{kind}'")))?;
    if k.spawn_approval {
        return Err(fail(format!(
            "kind '{kind}' needs approval (spawn = approve); --approve-kind is not supported here"
        )));
    }
    if a.review && !k.can_review {
        return Err(fail(format!(
            "--review needs a kind with can_review = true ('{kind}' has none)"
        )));
    }
    let repo = std::fs::canonicalize(repo_arg)
        .map_err(|e| fail(format!("repo path '{repo_arg}': {e}")))?;
    if !git_ok(&repo, &["rev-parse", "--git-dir"]) {
        return Err(fail(format!(
            "'{}' is not a git repository",
            repo.display()
        )));
    }
    if let Some(b) = &a.base {
        if !git_ok(
            &repo,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{b}^{{commit}}"),
            ],
        ) {
            return Err(fail(format!("--base '{b}' is not a known commit-ish")));
        }
    }

    let caller = check_authority(ctx, authority::Role::Peer, None)?;
    let lead_project = caller
        .as_ref()
        .filter(|c| c.role == authority::Role::Lead)
        .and_then(|c| c.project_id);
    let (project_id, slug) = project_for(ctx.conn, &repo, lead_project, ctx.now_ms)?;
    let owner = caller
        .as_ref()
        .map_or("cos", |c| c.name.as_str())
        .to_string();
    let reopening = open_agent(ctx.conn, name)?.is_some();

    if !reopening {
        check_cap(ctx.conn, caller.as_ref(), &owner, &kind)?;
        if caller.is_none() {
            lead_gate(ctx, project_id, &slug, a.solo.as_deref())?;
        }
    }

    let branch = format!("quaqua/{name}");
    spawn_placed(
        ctx,
        &Placed {
            name,
            kind: &kind,
            repo: &repo,
            branch: &branch,
            base: a.base.as_deref().unwrap_or("HEAD"),
            record_base: a.base.as_deref(),
            review: a.review,
            project_id,
            spawner_id: caller.as_ref().map(|c| c.id),
            owner: &owner,
            slug: &slug,
        },
    )
}

/// What [`spawn_placed`] needs: one agent, its own worktree and its own pane.
pub struct Placed<'a> {
    pub name: &'a str,
    pub kind: &'a str,
    pub repo: &'a Path,
    /// The branch the worktree is on (made from `base` when it does not exist).
    pub branch: &'a str,
    pub base: &'a str,
    /// The `--base` the caller gave, recorded beside the run files.
    pub record_base: Option<&'a str>,
    pub review: bool,
    pub project_id: Option<i64>,
    pub spawner_id: Option<i64>,
    pub owner: &'a str,
    pub slug: &'a str,
}

/// The tail every placed agent shares (`worker spawn`, the pipeline's builder
/// and reviewers): a worktree beside the repo, a pane split from the anchor
/// with the worktree as its cwd, the spawn, the pane tokens. An agent herdr
/// already knows keeps its pane, so a repeat places nothing new.
pub fn spawn_placed(ctx: &Ctx, p: &Placed) -> R<WorkerSpawned> {
    let (name, repo) = (p.name, p.repo);
    let wt = worktree_of(repo, name);
    ensure_worktree(repo, &wt, p.branch, p.base)?;
    block_push(repo, &wt);
    // The harness's fence hook (pi loads `-e .qq/pi-gate.ts` at launch and exits
    // when the file is missing), gating through this binary's `gate` verb.
    let target = crate::config::resolve_spawn(ctx.conn, p.kind).map_err(|e| fail(e.to_string()))?;
    if let Some(driver) = ctx.drivers.get(&target.harness.name) {
        let gate = std::env::current_exe().map_err(|e| fail(e.to_string()))?;
        driver
            .install_hook(&wt, &gate)
            .map_err(|e| fail(format!("cannot install the fence hook: {e}")))?;
    }
    std::fs::create_dir_all(&ctx.runs_dir).map_err(|e| fail(e.to_string()))?;
    let side = |ext: &str, body: String| {
        std::fs::write(ctx.runs_dir.join(format!("{name}.{ext}")), body)
            .map_err(|e| fail(format!("cannot record {name}.{ext}: {e}")))
    };
    side("repo", format!("{}\n", repo.display()))?;
    if let Some(b) = p.record_base {
        side("base", format!("{b}\n"))?;
    }
    if p.review {
        side("review", "1\n".into())?;
    }

    let pane = match existing_pane(ctx.herdr, name) {
        Some(x) => x,
        None => new_pane(ctx, &wt)?,
    };
    spawn(
        ctx.conn,
        ctx.herdr,
        ctx.drivers,
        &SpawnSpec {
            name,
            kind: p.kind,
            role: Role::Peer,
            pane: &pane,
            project_id: p.project_id,
            spawner_id: p.spawner_id,
        },
        ctx.now_ms,
    )
    .map_err(|e| fail(e.to_string()))?;
    let group = base_name(repo);
    let tab = format!("{}-workers", p.slug);
    report_tokens(
        ctx,
        &pane,
        &[
            ("grid_role", "worker"),
            ("grid_group", &group),
            ("grid_owner", p.owner),
            ("grid_tab", &tab),
            ("grid_ready", "1"),
        ],
    )?;
    Ok(WorkerSpawned {
        name: name.to_string(),
        kind: p.kind.to_string(),
        pane,
        worktree: wt.to_string_lossy().into_owned(),
        branch: p.branch.to_string(),
    })
}

/// The project of `repo` (created on first use), or the Lead's own project.
fn project_for(
    conn: &Connection,
    repo: &Path,
    lead_project: Option<i64>,
    now_ms: u64,
) -> R<(Option<i64>, String)> {
    let by_id = |id: i64| -> R<Option<(i64, String)>> {
        conn.query_row("SELECT id, slug FROM project WHERE id = ?1", [id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()
        .map_err(db)
    };
    if let Some((id, slug)) = lead_project.map(by_id).transpose()?.flatten() {
        return Ok((Some(id), slug));
    }
    let path = repo.to_string_lossy().into_owned();
    let found: Option<(i64, String)> = conn
        .query_row(
            "SELECT id, slug FROM project WHERE repo_path = ?1",
            [&path],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(db)?;
    if let Some((id, slug)) = found {
        return Ok((Some(id), slug));
    }
    let base: String = base_name(repo)
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    // The slug is UNIQUE: a second repo with the same basename gets `-2`, `-3`...
    for n in 1..=1000 {
        let slug = if n == 1 {
            base.clone()
        } else {
            format!("{base}-{n}")
        };
        let taken: Option<Option<String>> = conn
            .query_row("SELECT repo_path FROM project WHERE slug = ?1", [&slug], |r| {
                r.get(0)
            })
            .optional()
            .map_err(db)?;
        match taken {
            Some(p) if p.as_deref() == Some(path.as_str()) => {}
            Some(_) => continue,
            None => {
                conn.execute(
                    "INSERT INTO project (slug, repo_path, created_ms) VALUES (?1, ?2, ?3)",
                    params![slug, path, now_ms as i64],
                )
                .map_err(db)?;
            }
        }
        let id: i64 = conn
            .query_row("SELECT id FROM project WHERE slug = ?1", [&slug], |r| {
                r.get(0)
            })
            .map_err(db)?;
        return Ok((Some(id), slug));
    }
    Err(fail(format!("no free project slug for '{base}'")))
}

/// Slots one open worker of `kind` holds: `limits.weights.<kind>`, else
/// `limits.weights.default`, else 1.
fn weight(conn: &Connection, kind: &str) -> u64 {
    let w = config::setting(conn, "limits.weights").ok().flatten();
    let of = |k: &str| w.as_ref().and_then(|w| w.get(k)).and_then(Value::as_u64);
    of(kind).or_else(|| of("default")).unwrap_or(1)
}

/// Refuses a spawn of `kind` that needs more weighted slots than `owner` has
/// left. A Lead is held to `min(roles.lead.max_peers, global)`, anyone else to
/// `limits.max_workers_per_orchestrator`.
fn check_cap(
    conn: &Connection,
    caller: Option<&authority::Actor>,
    owner: &str,
    kind: &str,
) -> R<()> {
    let global = limit(conn, "limits.max_workers_per_orchestrator", DEFAULT_CAP);
    let (max, key) = match caller {
        Some(c) if c.role == authority::Role::Lead => {
            let peers = limit(conn, "roles.lead.max_peers", DEFAULT_MAX_PEERS);
            if peers <= global {
                (peers, "roles.lead.max_peers")
            } else {
                (global, "limits.max_workers_per_orchestrator")
            }
        }
        _ => (global, "limits.max_workers_per_orchestrator"),
    };
    let mut stmt = conn
        .prepare(
            "SELECT kind FROM agent WHERE role = 'peer' AND ended_ms IS NULL \
             AND spawner_id IS ?1",
        )
        .map_err(db)?;
    let kinds = stmt
        .query_map([caller.map(|c| c.id)], |r| r.get::<_, Option<String>>(0))
        .map_err(db)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db)?;
    let used: u64 = kinds
        .iter()
        .map(|k| weight(conn, k.as_deref().unwrap_or("")))
        .sum();
    let need = weight(conn, kind);
    if used + need > max {
        return Err(fail(format!(
            "worker cap exceeded: {owner} holds {used} worker slots, this spawn needs {need} (limit {max}, {key})"
        )));
    }
    Ok(())
}

/// `phases_left: N` of the active plan of `slug`, if any.
fn phases_left(root: &Path, slug: &str) -> Option<u32> {
    let text =
        std::fs::read_to_string(root.join("docs/plans/active").join(format!("{slug}.md"))).ok()?;
    text.lines()
        .find_map(|l| l.trim().strip_prefix("phases_left:"))
        .and_then(|v| v.trim().parse().ok())
}

/// The Lead gate for a CoS-owned spawn into a project with no active Lead:
/// two topology signals block (unless `--solo`), one warns.
fn lead_gate(ctx: &Ctx, project: Option<i64>, slug: &str, solo: Option<&str>) -> R<()> {
    let Some(pid) = project else { return Ok(()) };
    let has_lead: bool = ctx
        .conn
        .query_row(
            "SELECT 1 FROM agent WHERE project_id = ?1 AND role = 'lead' AND ended_ms IS NULL",
            [pid],
            |_| Ok(true),
        )
        .optional()
        .map_err(db)?
        .unwrap_or(false);
    if has_lead {
        return Ok(());
    }
    let build_kinds: Vec<String> = config::setting(ctx.conn, "strategy.build_kinds")
        .ok()
        .flatten()
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let mut live = 0i64;
    for k in &build_kinds {
        live += ctx
            .conn
            .query_row(
                "SELECT COUNT(*) FROM agent WHERE project_id = ?1 AND role = 'peer' \
                 AND ended_ms IS NULL AND kind = ?2",
                params![pid, k],
                |r| r.get::<_, i64>(0),
            )
            .map_err(db)?;
    }
    let root = workspace_root(&ctx.runs_dir);
    let mut signals = Vec::new();
    if live >= GATE_LIVE_BUILDS {
        signals.push(format!("{} live build workers (this is the next)", live));
    }
    if let Some(n) = phases_left(&root, slug).filter(|n| *n > GATE_PHASES_LEFT) {
        signals.push(format!("{n} phases left in the active plan"));
    }
    let days = memory_days(&root, slug);
    if days > GATE_MEMORY_DAYS {
        signals.push(format!("{days} consecutive days of project memory"));
    }
    match (signals.len(), solo) {
        (0, _) => Ok(()),
        (1, _) => {
            eprintln!("warning: project '{slug}' is growing: {}", signals[0]);
            Ok(())
        }
        (_, Some(reason)) => {
            eprintln!("solo override ({slug}): {reason}");
            let dir = root.join("memory");
            std::fs::create_dir_all(&dir)
                .and_then(|()| {
                    use std::io::Write;
                    let mut f = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(dir.join(format!("{}.md", today_local(ctx.now_ms))))?;
                    writeln!(f, "- [{slug}] solo override: {reason}")
                })
                .map_err(|e| fail(format!("cannot record the solo override in memory/: {e}")))
        }
        _ => Err(fail(format!(
            "project '{slug}' needs a Lead:\n  - {}\nrun `quaqua lead spawn {slug}`, or pass --solo \"<reason>\" to go on without one",
            signals.join("\n  - ")
        ))),
    }
}

// ---- lead spawn ----------------------------------------------------------------------

fn valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && !slug.starts_with('-')
        && slug
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn brief(slug: &str, repo: &str, integ: &str) -> String {
    format!(
        "You are the Lead of project {slug} (repo {repo}). Integrate on branch {integ}. \
         Plan the work, spawn peers with `quaqua worker spawn`, review and accept their \
         reports, and report to the CoS with `quaqua mail send --to cos`."
    )
}

pub fn run_lead(ctx: &Ctx, args: &[String]) -> R<LeadSpawned> {
    let mut slug: Option<&String> = None;
    let mut task: Option<&String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        match a.as_str() {
            "--task" => {
                task = Some(
                    it.next()
                        .ok_or_else(|| fail(format!("--task needs a value\n{LEAD_USAGE}")))?,
                )
            }
            f if f.starts_with("--") => {
                return Err(fail(format!("unknown flag {f}\n{LEAD_USAGE}")))
            }
            _ if slug.is_none() => slug = Some(a),
            _ => return Err(fail(LEAD_USAGE)),
        }
    }
    let slug = slug.ok_or_else(|| fail(LEAD_USAGE))?;
    if !valid_slug(slug) {
        return Err(fail(format!("slug '{slug}' must be ascii [a-z0-9-]")));
    }
    let name = format!("{slug}-lead");
    if name.len() > LEAD_NAME_MAX {
        return Err(fail(format!(
            "lead name '{name}' is longer than {LEAD_NAME_MAX} characters"
        )));
    }
    let (pid, repo): (i64, Option<String>) = ctx
        .conn
        .query_row(
            "SELECT id, repo_path FROM project WHERE slug = ?1",
            [slug],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(db)?
        .ok_or_else(|| fail(format!("unknown project '{slug}'")))?;
    if open_agent(ctx.conn, &name)?.is_some() {
        return Err(fail(format!("lead '{name}' already exists")));
    }
    let repo = repo.ok_or_else(|| fail(format!("project '{slug}' has no repo path")))?;
    let repo =
        std::fs::canonicalize(&repo).map_err(|e| fail(format!("repo path '{repo}': {e}")))?;
    let kind = text_setting(ctx.conn, "strategy.lead_kind")
        .ok_or_else(|| fail("strategy.lead_kind is not set in team.toml"))?;
    let caller = check_authority(ctx, authority::Role::Lead, Some(pid))?;

    let integ = format!("quaqua/{slug}-integ");
    let wt = worktree_of(&repo, &name);
    ensure_worktree(&repo, &wt, &integ, "HEAD")?;
    ctx.conn
        .execute(
            "UPDATE project SET integ_branch = ?1 WHERE id = ?2",
            params![integ, pid],
        )
        .map_err(db)?;
    let pane = match existing_pane(ctx.herdr, &name) {
        Some(p) => p,
        None => new_pane(ctx, &wt)?,
    };
    spawn(
        ctx.conn,
        ctx.herdr,
        ctx.drivers,
        &SpawnSpec {
            name: &name,
            kind: &kind,
            role: Role::Lead,
            pane: &pane,
            project_id: Some(pid),
            spawner_id: caller.as_ref().map(|c| c.id),
        },
        ctx.now_ms,
    )
    .map_err(|e| fail(e.to_string()))?;
    report_tokens(
        ctx,
        &pane,
        &[
            ("grid_role", "lead"),
            ("grid_group", slug),
            ("grid_owner", "cos"),
            ("grid_tab", &format!("{slug}-lead")),
            ("grid_ready", "1"),
        ],
    )?;
    let prompt = |text: &str| -> R<()> {
        match crate::herdr::prompt_landed(ctx.herdr, &name, text).0 {
            crate::herdr::Landing::Landed => Ok(()),
            crate::herdr::Landing::Blocked => Err(fail(format!(
                "herdr refused the prompt to {name}: agent_blocked"
            ))),
            crate::herdr::Landing::NotLanded(why) => Err(fail(why)),
        }
    };
    let ms = std::env::var("QQ_LEAD_READY_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|v| *v > 0)
        .unwrap_or(LEAD_READY_MS);
    let ready = ctx
        .herdr
        .agent_wait(&name, &["idle", "done"], ms)
        .map(|r| r.ok)
        .unwrap_or(false);
    if !ready {
        return Err(fail(format!(
            "lead '{name}' was spawned but not ready within {ms} ms; send its brief yourself: `herdr agent prompt {name} \"<brief>\"`"
        )));
    }
    prompt(&brief(slug, &repo.to_string_lossy(), &integ))?;
    let (kickoff_sent, next) = match task {
        Some(t) => {
            prompt(t)?;
            (true, None)
        }
        None => (
            false,
            Some(format!("NEXT: herdr agent prompt {name} \"<kickoff>\"")),
        ),
    };
    Ok(LeadSpawned {
        name,
        kind,
        pane,
        integ_branch: integ,
        kickoff_sent,
        next,
    })
}

// ---- CLI handlers ------------------------------------------------------------------------

/// The live pieces: the workspace database, the herdr binary, the drivers of
/// the roster's harnesses; the caller is the open agent living in this pane.
fn live<T>(env: &Env, f: impl FnOnce(&Ctx) -> R<T>) -> R<T> {
    let pane = std::env::var("HERDR_PANE_ID")
        .ok()
        .filter(|p| !p.is_empty());
    live_with(
        env,
        Arc::new(crate::herdr::CliHerdr::new()),
        std::process::id() as i32,
        pane,
        crate::cli::caller_claim().as_deref(),
        f,
    )
}

fn live_with<T>(
    env: &Env,
    herdr: Arc<dyn HerdrClient>,
    pid: i32,
    pane: Option<String>,
    claim: Option<&str>,
    f: impl FnOnce(&Ctx) -> R<T>,
) -> R<T> {
    let conn = crate::db::open(&env.runs_dir.join(crate::db::path::DB_FILE)).map_err(db)?;
    let harnesses: Vec<_> = config::roster(&conn)
        .map_err(cfg)?
        .iter()
        .filter_map(|k| config::harness(&conn, &k.harness).ok().flatten())
        .collect();
    let drivers = Drivers::from_config(&harnesses);
    let caller = caller_of(&herdr, pid, claim)?;
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64);
    f(&Ctx {
        conn: &conn,
        herdr: herdr.as_ref(),
        drivers: &drivers,
        runs_dir: env.runs_dir.clone(),
        caller: caller.as_deref(),
        anchor: pane.as_deref(),
        now_ms,
    })
}

/// Who this process is, named by herdr's pane of its pid (the same resolver the
/// daemon uses for a socket peer): `None` is the CoS. A process in no pane, or
/// in a pane with no agent, is refused: never taken for the CoS. A `QQ_AGENT`
/// claim must match that pane's agent (`cos` for the CoS); it never names a
/// process the pane does not, so a mismatch is refused.
fn caller_of(
    herdr: &Arc<dyn HerdrClient>,
    pid: i32,
    claim: Option<&str>,
) -> R<Option<String>> {
    use crate::identity::peer::{HerdrPeers, PeerCred, PeerResolver};
    // The resolver keys on the pid; the uid is not part of the decision.
    let who = HerdrPeers::new(Arc::clone(herdr))
        .resolve(PeerCred { pid, uid: 0 })
        .map_err(|e| fail(e.message))?;
    if let Some(c) = claim.filter(|c| *c != who.as_str()) {
        return Err(fail(format!(
            "QQ_AGENT claims '{c}' but this pane runs '{}'",
            who.as_str()
        )));
    }
    Ok((who.as_str() != "cos").then_some(who.0))
}

/// `worker spawn` with the herdr client, calling pid and anchor pane given.
pub fn worker_spawn_with(
    env: &Env,
    herdr: Arc<dyn HerdrClient>,
    pid: i32,
    anchor: Option<String>,
    args: &[String],
) -> R<WorkerSpawned> {
    worker_spawn_as(env, herdr, pid, anchor, None, args)
}

/// [`worker_spawn_with`] plus the caller's `QQ_AGENT` claim, already read by
/// the CLI boundary: a claim the pane does not back is refused.
pub fn worker_spawn_as(
    env: &Env,
    herdr: Arc<dyn HerdrClient>,
    pid: i32,
    anchor: Option<String>,
    claim: Option<&str>,
    args: &[String],
) -> R<WorkerSpawned> {
    live_with(env, herdr, pid, anchor, claim, |ctx| run_worker(ctx, args))
}

/// `worker spawn ...` (the args after `spawn`): prints the result as JSON.
pub fn worker_spawn_cli(env: &Env, args: &[String]) -> R<String> {
    live(env, |ctx| {
        let s = run_worker(ctx, args)?;
        Ok(json!({
            "name": s.name, "kind": s.kind, "pane": s.pane,
            "worktree": s.worktree, "branch": s.branch,
        })
        .to_string())
    })
}

/// `lead spawn ...` (the args after `spawn`): prints the result as JSON, then
/// the `NEXT:` line when no kickoff was sent.
pub fn lead_spawn_cli(env: &Env, args: &[String]) -> R<String> {
    live(env, |ctx| {
        let s = run_lead(ctx, args)?;
        let body = json!({
            "name": s.name, "kind": s.kind, "pane": s.pane,
            "integ_branch": s.integ_branch, "kickoff_sent": s.kickoff_sent,
        })
        .to_string();
        Ok(match s.next {
            Some(n) => format!("{body}\n{n}"),
            None => body,
        })
    })
}
