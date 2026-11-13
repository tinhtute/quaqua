//! `quaqua doctor` — check that the `quaqua` a shell resolves is the binary
//! worth using: installed on PATH, not older than the checkout it was built
//! from (when one can be found), and not shadowed by a shell function/alias
//! (issue #55 item 3).

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant, SystemTime};

use crate::config::Config;

/// Interactive shells can be slow with a large rc; never let the probe (or a
/// check command) hang `quaqua doctor`.
const PROBE_TIMEOUT: Duration = Duration::from_millis(5000);

pub struct Report {
    pub problems: Vec<String>,
    pub warnings: Vec<String>,
    pub notes: Vec<String>,
    /// Whether a binary was compared against a checkout: `run()` may only claim
    /// the staleness check when this is true.
    pub checkout_checked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriftProblem {
    pub drift: String,
    pub fix: String,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct WorkspaceDriftReport {
    pub problems: Vec<DriftProblem>,
    pub warnings: Vec<String>,
    pub notes: Vec<String>,
}

/// `quaqua doctor`: print what was found, one `error:` per problem, and exit
/// non-zero if anything is wrong.
pub fn run() -> i32 {
    let report = diagnose();
    for line in &report.notes {
        println!("{line}");
    }
    for line in &report.warnings {
        eprintln!("warning: {line}");
    }
    if report.problems.is_empty() {
        let ok = if report.checkout_checked {
            "ok: quaqua is installed, not older than the checkout, and not shadowed by a shell function/alias"
        } else {
            "ok: quaqua is installed and not shadowed by a shell function/alias — the staleness check was skipped (no checkout found)"
        };
        if report.warnings.is_empty() {
            println!("{ok}");
        } else {
            println!("{ok} (warnings above)");
        }
        0
    } else {
        for problem in &report.problems {
            eprintln!("error: {problem}");
        }
        1
    }
}

/// `quaqua doctor --workspace`: check workspace rules and herdr plugin event hooks
/// for drift against engine templates and manifest.
pub fn run_workspace(
    workspace: Option<&Path>,
    checkout: Option<&Path>,
    plugins_json: Option<&Path>,
) -> i32 {
    let report = evaluate_workspace(workspace, checkout, plugins_json);
    for line in &report.notes {
        println!("{line}");
    }
    for line in &report.warnings {
        eprintln!("warning: {line}");
    }
    if report.problems.is_empty() {
        println!(
            "ok: workspace rules and herdr plugin event hooks are in sync with engine templates"
        );
        0
    } else {
        for problem in &report.problems {
            eprintln!("error: {}", problem.drift);
            eprintln!("  fix: {}", problem.fix);
        }
        1
    }
}

pub const AUDITOR_FIX_CMD: &str =
    "bash scripts/auditor/install-auditor.sh && systemctl --user enable --now quaqua-auditor.timer";
pub const AUDITOR_UNITS: [&str; 2] = ["quaqua-auditor.service", "quaqua-auditor.timer"];

const GITHUB_CI_POLL_FIX_CMD: &str = "bash scripts/github-ci-poll/install-github-ci-poll.sh --repo <owner/name> && systemctl --user enable --now quaqua-github-ci-poll.timer";
const GITHUB_CI_POLL_UNITS: [&str; 2] = [
    "quaqua-github-ci-poll.service",
    "quaqua-github-ci-poll.timer",
];

/// One user timer `quaqua doctor` checks: the unit files an install writes
/// under `$HOME/.config/systemd/user` and the timer `systemctl --user
/// is-active` must report active.
struct TimerUnits {
    /// Label used in every message ("auditor", "github-ci poll").
    label: &'static str,
    /// Unit files the install writes.
    units: &'static [&'static str],
    /// The timer unit `is-active` is asked about.
    timer: &'static str,
    /// `list-unit-files` glob covering [`TimerUnits::units`].
    pattern: &'static str,
    /// Activation command printed after a problem.
    fix: &'static str,
    /// Whether never installing the timer is a legal, silent state: the
    /// github-ci poll timer takes a per-workspace `--repo` at install time.
    optional: bool,
}

const AUDITOR_TIMER: TimerUnits = TimerUnits {
    label: "auditor",
    units: &AUDITOR_UNITS,
    timer: "quaqua-auditor.timer",
    pattern: "quaqua-auditor*",
    fix: AUDITOR_FIX_CMD,
    optional: false,
};

const GITHUB_CI_POLL_TIMER: TimerUnits = TimerUnits {
    label: "github-ci poll",
    units: &GITHUB_CI_POLL_UNITS,
    timer: "quaqua-github-ci-poll.timer",
    pattern: "quaqua-github-ci-poll*",
    fix: GITHUB_CI_POLL_FIX_CMD,
    optional: true,
};

pub trait SystemdRunner {
    fn list_unit_files(&self, pattern: &str) -> Result<String, String>;
    fn is_active(&self, unit: &str) -> Result<bool, String>;
}

pub struct RealSystemd;

impl SystemdRunner for RealSystemd {
    fn list_unit_files(&self, pattern: &str) -> Result<String, String> {
        let mut cmd = Command::new("systemctl");
        cmd.args(["--user", "list-unit-files", pattern]);
        cmd.stdin(Stdio::null());
        let out = run_with_timeout(cmd, PROBE_TIMEOUT)?;
        Ok(String::from_utf8_lossy(&out.stdout).to_string())
    }

    fn is_active(&self, unit: &str) -> Result<bool, String> {
        let mut cmd = Command::new("systemctl");
        cmd.args(["--user", "is-active", unit]);
        cmd.stdin(Stdio::null());
        let out = run_with_timeout(cmd, PROBE_TIMEOUT)?;
        let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
        Ok(out.status.success() && s == "active")
    }
}

/// Check one user timer's systemd units (`$HOME/.config/systemd/user/<unit>`).
///
/// On Linux:
/// - Fails if unit files are missing
/// - Fails if unit symlinks are dangling
/// - Fails if `systemctl --user list-unit-files` reports `bad`
/// - Fails if `systemctl --user is-active <timer>` is not active
/// - Says nothing when no unit is installed at all and
///   [`TimerUnits::optional`] is set: such a timer needs a per-workspace
///   value at install time, so its absence is legal. A partial install is
///   still reported.
///
/// Each failure records a problem with the spec's fix line.
///
/// On macOS:
/// - Reports manual activation required in notes rather than failing.
fn check_timer_units_with<S: SystemdRunner>(
    home: Option<&Path>,
    is_macos: bool,
    spec: &TimerUnits,
    runner: &S,
    report: &mut Report,
) {
    if is_macos {
        report
            .notes
            .push(format!("{}: macOS manual activation required", spec.label));
        return;
    }

    let Some(home_path) = home else {
        report.problems.push(format!(
            "cannot resolve HOME directory to check {} systemd units — fix: {}",
            spec.label, spec.fix
        ));
        return;
    };

    let user_dir = home_path.join(".config/systemd/user");
    let installed = spec
        .units
        .iter()
        .filter(|unit| std::fs::symlink_metadata(user_dir.join(unit)).is_ok())
        .count();
    if installed == 0 && spec.optional {
        return;
    }

    let mut files_ok = true;
    for unit in spec.units {
        let path = user_dir.join(unit);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_symlink() && !path.exists() => {
                files_ok = false;
                report.problems.push(format!(
                    "{} systemd unit symlink is dangling: {} — fix: {}",
                    spec.label,
                    path.display(),
                    spec.fix
                ));
            }
            Ok(_) => {}
            Err(_) => {
                files_ok = false;
                report.problems.push(format!(
                    "{} systemd unit is missing: {} — fix: {}",
                    spec.label,
                    path.display(),
                    spec.fix
                ));
            }
        }
    }

    if !files_ok {
        return;
    }

    match runner.list_unit_files(spec.pattern) {
        Ok(output) => {
            let mut bad_unit = false;
            for line in output.lines() {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    let unit_name = parts[0];
                    let state = parts[1];
                    if spec.units.contains(&unit_name) && state == "bad" {
                        bad_unit = true;
                        report.problems.push(format!(
                            "{} systemd unit file is bad: {unit_name} — fix: {}",
                            spec.label, spec.fix
                        ));
                    }
                }
            }
            if bad_unit {
                return;
            }
        }
        Err(e) => {
            report.problems.push(format!(
                "failed to query systemctl list-unit-files: {e} — fix: {}",
                spec.fix
            ));
            return;
        }
    }

    match runner.is_active(spec.timer) {
        Ok(true) => {}
        Ok(false) => {
            report.problems.push(format!(
                "{} systemd timer is inactive ({}) — fix: {}",
                spec.label, spec.timer, spec.fix
            ));
        }
        Err(e) => {
            report.problems.push(format!(
                "failed to query systemctl is-active: {e} — fix: {}",
                spec.fix
            ));
        }
    }
}

/// Check the auditor timer
/// (`$HOME/.config/systemd/user/quaqua-auditor.{timer,service}`).
pub fn check_auditor_units_with<S: SystemdRunner>(
    home: Option<&Path>,
    is_macos: bool,
    runner: &S,
    report: &mut Report,
) {
    check_timer_units_with(home, is_macos, &AUDITOR_TIMER, runner, report);
}

pub fn check_auditor_units(home: Option<&Path>, report: &mut Report) {
    let is_macos = cfg!(target_os = "macos");
    check_auditor_units_with(home, is_macos, &RealSystemd, report);
}

/// Check the github-ci poll timer
/// (`$HOME/.config/systemd/user/quaqua-github-ci-poll.{timer,service}`).
/// Never installing it is silent: the unit carries a per-workspace
/// `--repo`, so the install is a choice. An install that is present but
/// broken is still a problem.
pub fn check_github_ci_poll_units_with<S: SystemdRunner>(
    home: Option<&Path>,
    is_macos: bool,
    runner: &S,
    report: &mut Report,
) {
    check_timer_units_with(home, is_macos, &GITHUB_CI_POLL_TIMER, runner, report);
}

pub fn check_github_ci_poll_units(home: Option<&Path>, report: &mut Report) {
    let is_macos = cfg!(target_os = "macos");
    check_github_ci_poll_units_with(home, is_macos, &RealSystemd, report);
}

/// The `provider/model` ids a `pi --list-models` table lists.
///
/// The table is one header line (`provider  model  context  max-out  thinking
/// images`) then one row per model, whitespace-separated; pi prints extension
/// warnings on stderr, so stdout is the table alone. pi resolves a kind's
/// `model` against exactly `provider/model`.
pub fn pi_model_ids(list_output: &str) -> Vec<String> {
    list_output
        .lines()
        .filter_map(|line| {
            let mut cols = line.split_whitespace();
            let provider = cols.next()?;
            let model = cols.next()?;
            (provider != "provider").then(|| format!("{provider}/{model}"))
        })
        .collect()
}

/// Warn for every kind that runs on pi with a `model` the table does not list.
///
/// Such a kind fails only when a worker launches, so `quaqua doctor` names it
/// first. `list` is `None` when pi is not on PATH: the check is skipped with
/// one note rather than guessed at. A warning, never a problem: a model pi
/// cannot run is a workspace config mistake, not a broken install.
pub fn check_pi_models_with(cfg: &Config, list: Option<&str>, report: &mut Report) {
    let pi_kinds: Vec<_> = cfg
        .kinds
        .iter()
        .filter(|(_, k)| k.harness == "pi")
        .collect();
    if pi_kinds.is_empty() {
        return;
    }
    let Some(list) = list else {
        report
            .notes
            .push("pi models: pi is not on PATH — the pi model check was skipped".to_string());
        return;
    };
    let listed = pi_model_ids(list);
    for (name, kind) in pi_kinds {
        if listed.contains(&kind.model) {
            continue;
        }
        let provider = kind
            .model
            .split_once('/')
            .map(|(p, _)| p)
            .unwrap_or(kind.model.as_str());
        report.warnings.push(format!(
            "kind '{name}' runs on pi with model '{}', which `pi --list-models` does not list — install the pi provider package for '{provider}' (or fix kinds.{name}.model in team.toml)",
            kind.model
        ));
    }
}

/// The real pi model check: resolve team.toml, find `pi` on PATH, read its
/// model table, and warn about the kinds it cannot run.
fn check_pi_models(report: &mut Report) {
    let cfg = match crate::config::load_team_config() {
        Ok(cfg) => cfg,
        Err((_, e)) => {
            report
                .notes
                .push(format!("pi models: check skipped — {}", e.trim()));
            return;
        }
    };
    if !cfg.kinds.values().any(|k| k.harness == "pi") {
        return;
    }
    let path = std::env::var_os("PATH");
    let Some(pi) = find_on_path("pi", path.as_deref()) else {
        check_pi_models_with(&cfg, None, report);
        return;
    };
    let mut cmd = Command::new(&pi);
    cmd.arg("--list-models");
    match run_with_timeout(cmd, PROBE_TIMEOUT) {
        Ok(out) if out.status.success() => {
            check_pi_models_with(&cfg, Some(&String::from_utf8_lossy(&out.stdout)), report);
        }
        Ok(out) => report.notes.push(format!(
            "pi models: check skipped — `pi --list-models` exited {}",
            out.status
        )),
        Err(e) => report.notes.push(format!("pi models: check skipped — {e}")),
    }
}

/// The real checks: PATH lookup, checkout discovery, and `$SHELL`.
pub fn diagnose() -> Report {
    let path = std::env::var_os("PATH");
    let binary = find_on_path("quaqua", path.as_deref());
    let checkout = checkout_root();
    let workspace = crate::workspace::workspace_root();
    let shell = shell_path();
    let mut report = evaluate(
        binary.as_deref(),
        checkout.as_deref(),
        Some(&workspace),
        shell.as_deref(),
    );
    let home = std::env::var_os("HOME").map(PathBuf::from);
    check_auditor_units(home.as_deref(), &mut report);
    check_github_ci_poll_units(home.as_deref(), &mut report);
    check_pi_models(&mut report);
    report
}

/// The check core with explicit inputs, so tests can drive every branch
/// without changing PATH or the user's shell.
pub fn evaluate(
    binary: Option<&Path>,
    checkout: Option<&Path>,
    workspace: Option<&Path>,
    shell: Option<&Path>,
) -> Report {
    let mut report = Report {
        problems: Vec::new(),
        warnings: workspace_warnings(workspace),
        notes: Vec::new(),
        checkout_checked: false,
    };
    match binary {
        Some(bin) => {
            report.notes.push(format!("binary: {}", bin.display()));
            if let Some(root) = checkout {
                report.notes.push(format!("checkout: {}", root.display()));
                report.checkout_checked = true;
                if let Some(newest) = stale_against(bin, root) {
                    report.problems.push(format!(
                        "{} is older than the checkout ({}) — rebuild/install it: cargo install --path crates/quaqua --locked",
                        bin.display(),
                        newest.display()
                    ));
                }
            } else {
                report.notes.push(
                    "checkout: not found — the staleness check was skipped (set QUAQUA_CHECKOUT=<engine checkout> or run inside one)"
                        .to_string(),
                );
            }
        }
        None => report.problems.push(
            "quaqua is not on PATH — install it (cargo install --path crates/quaqua --locked) or add its bin directory to PATH"
                .to_string(),
        ),
    }
    if let Some(shell) = shell {
        report.notes.push(format!("shell: {}", shell.display()));
        match shadow_probe(shell) {
            Ok(output) => {
                if let Some(line) = is_shadowed(&output) {
                    report.problems.push(format!(
                        "the shell resolves `quaqua` to a function/alias, not the binary: {line} — remove it from your shell rc"
                    ));
                }
            }
            Err(e) => report.notes.push(format!("shell probe skipped: {e}")),
        }
    }
    report
}

/// The first executable `name` on `path` (a `None` PATH finds nothing).
pub fn find_on_path(name: &str, path: Option<&OsStr>) -> Option<PathBuf> {
    for dir in std::env::split_paths(path?) {
        let candidate = dir.join(name);
        if is_executable(&candidate) {
            return Some(candidate);
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    let Ok(meta) = path.metadata() else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// `QUAQUA_CHECKOUT`, else the current directory or an ancestor containing
/// `crates/quaqua/src`, else nothing. There is no `$HOME` guess: the workspace
/// root is not the engine checkout (`workspace_warnings` reports the overlap
/// instead).
pub fn checkout_root() -> Option<PathBuf> {
    let explicit = std::env::var_os("QUAQUA_CHECKOUT")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    let cwd = std::env::current_dir().ok();
    let workspace = crate::workspace::workspace_root();
    checkout_root_from(explicit.as_deref(), cwd.as_deref(), Some(&workspace))
}

/// Layout warnings for the workspace `quaqua doctor` runs against: a workspace
/// must not also be an engine checkout, and its root `AGENTS.md` must import
/// the persona. Both are warnings — the run still exits 0.
pub fn workspace_warnings(workspace: Option<&Path>) -> Vec<String> {
    let Some(root) = workspace else {
        return Vec::new();
    };
    if !root.is_dir() {
        return Vec::new();
    }
    let mut warnings = Vec::new();
    let mut engine = Vec::new();
    if root.join("Cargo.toml").is_file() {
        engine.push("Cargo.toml");
    }
    if root.join("crates").is_dir() {
        engine.push("crates/");
    }
    if !engine.is_empty() {
        warnings.push(format!(
            "{} is also an engine checkout ({}) — keep the workspace and the engine checkout apart, or the engine AGENTS.md replaces the workspace one",
            root.display(),
            engine.join(", ")
        ));
    }
    let agents = root.join("AGENTS.md");
    if let Ok(text) = std::fs::read_to_string(&agents) {
        if !text.lines().any(|line| line.trim() == "@identity.md") {
            warnings.push(format!(
                "{} has no `@identity.md` import — the workspace persona will not load",
                agents.display()
            ));
        }
    }
    warnings
}

/// [`checkout_root`] with its process inputs passed in, so tests can prove a
/// workspace root is never taken for the engine checkout.
fn checkout_root_from(
    explicit: Option<&Path>,
    cwd: Option<&Path>,
    workspace: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(explicit) = explicit {
        return Some(explicit.to_path_buf());
    }
    let mut dir = cwd?.to_path_buf();
    loop {
        if is_checkout(&dir) {
            // A workspace holding the engine sources is a broken layout, not a
            // checkout to compare a binary against.
            return (Some(dir.as_path()) != workspace).then_some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn is_checkout(path: &Path) -> bool {
    path.join("crates/quaqua/src").is_dir()
}

fn shell_path() -> Option<PathBuf> {
    std::env::var_os("QUAQUA_SHELL")
        .filter(|v| !v.is_empty())
        .or_else(|| std::env::var_os("SHELL").filter(|v| !v.is_empty()))
        .map(PathBuf::from)
}

/// Ask the user's interactive shell how it resolves `quaqua`. `-i` loads the
/// rc files where functions and aliases live; `type` is a builtin in bash,
/// zsh, sh and fish.
pub fn shadow_probe(shell: &Path) -> Result<String, String> {
    let mut cmd = Command::new(shell);
    cmd.args(["-ic", "type quaqua 2>&1 || true"]);
    cmd.stdin(Stdio::null());
    let out = run_with_timeout(cmd, PROBE_TIMEOUT)?;
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// The `type` output names a shell function or alias. A binary path does not
/// (and a function's body printout starts with `quaqua ()`).
pub fn is_shadowed(output: &str) -> Option<String> {
    output
        .lines()
        .map(str::trim)
        .find(|line| {
            let l = line.to_lowercase();
            (l.contains("quaqua")
                && (l.contains("function") || l.contains("alias") || l.contains("aliased")))
                || l.starts_with("quaqua ()")
        })
        .map(str::to_string)
}

/// The checkout file newer than the binary, if any: an installed binary built
/// before the source it claims to run is stale.
pub fn stale_against(binary: &Path, checkout: &Path) -> Option<PathBuf> {
    let newest = newest_source(&checkout.join("crates/quaqua/src"))?;
    let bin_time = std::fs::metadata(binary).ok()?.modified().ok()?;
    let src_time = std::fs::metadata(&newest).ok()?.modified().ok()?;
    (bin_time < src_time).then_some(newest)
}

fn newest_source(dir: &Path) -> Option<PathBuf> {
    newest_file(dir).map(|(_, path)| path)
}

fn newest_file(dir: &Path) -> Option<(SystemTime, PathBuf)> {
    let mut best: Option<(SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(dir).ok()?.flatten() {
        let path = entry.path();
        let Ok(meta) = entry.metadata() else {
            continue;
        };
        let found = if meta.is_dir() {
            newest_file(&path)
        } else {
            meta.modified().ok().map(|t| (t, path))
        };
        if let Some((time, candidate)) = found {
            if best.as_ref().map(|(t, _)| time > *t).unwrap_or(true) {
                best = Some((time, candidate));
            }
        }
    }
    best
}

fn run_with_timeout(mut cmd: Command, timeout: Duration) -> Result<Output, String> {
    let mut child = cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot run {}: {e}", cmd.get_program().to_string_lossy()))?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!(
                        "{} timed out after {}ms",
                        cmd.get_program().to_string_lossy(),
                        timeout.as_millis()
                    ));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(e) => return Err(format!("waiting for the shell failed: {e}")),
        }
    }
    child
        .wait_with_output()
        .map_err(|e| format!("reading shell output failed: {e}"))
}

/// Compare workspace rule files under `workspace_rules_dir` against template files
/// under `template_dir`. Returns a `DriftProblem` for each missing or differing rule.
pub fn diff_rules(template_dir: &Path, workspace_rules_dir: &Path) -> Vec<DriftProblem> {
    let mut problems = Vec::new();
    let Ok(entries) = std::fs::read_dir(template_dir) else {
        return problems;
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("md"))
        .collect();
    files.sort();

    for t_path in files {
        let Some(file_name) = t_path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let ws_path = workspace_rules_dir.join(file_name);
        let t_content = match std::fs::read_to_string(&t_path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        if !ws_path.is_file() {
            problems.push(DriftProblem {
                drift: format!(
                    "rule drift: rules/_all/{} is missing (template exists under {})",
                    file_name,
                    t_path.display()
                ),
                fix: format!("cp {} {}", t_path.display(), ws_path.display()),
            });
        } else {
            match std::fs::read_to_string(&ws_path) {
                Ok(ws_content) => {
                    if ws_content != t_content {
                        problems.push(DriftProblem {
                            drift: format!(
                                "rule drift: rules/_all/{} differs from template {}",
                                file_name,
                                t_path.display()
                            ),
                            fix: format!("cp {} {}", t_path.display(), ws_path.display()),
                        });
                    }
                }
                Err(e) => {
                    problems.push(DriftProblem {
                        drift: format!("rule drift: rules/_all/{} cannot be read: {e}", file_name),
                        fix: format!("cp {} {}", t_path.display(), ws_path.display()),
                    });
                }
            }
        }
    }
    problems
}

/// Fallback when no checkout is found: compare against the embedded template table.
pub fn diff_embedded_rules(workspace_rules_dir: &Path) -> Vec<DriftProblem> {
    let mut problems = Vec::new();
    for entry in crate::init::ENTRIES {
        if let Some(rel) = entry.path.strip_prefix("rules/_all/") {
            let ws_file = workspace_rules_dir.join(rel);
            let t_path_disp = format!("templates/workspace/rules/_all/{rel}");
            if !ws_file.is_file() {
                problems.push(DriftProblem {
                    drift: format!("rule drift: rules/_all/{rel} is missing (template exists under {t_path_disp})"),
                    fix: format!("cp {t_path_disp} {}", ws_file.display()),
                });
            } else {
                match std::fs::read(&ws_file) {
                    Ok(bytes) => {
                        if bytes != entry.bytes {
                            problems.push(DriftProblem {
                                drift: format!("rule drift: rules/_all/{rel} differs from template {t_path_disp}"),
                                fix: format!("cp {t_path_disp} {}", ws_file.display()),
                            });
                        }
                    }
                    Err(e) => {
                        problems.push(DriftProblem {
                            drift: format!("rule drift: rules/_all/{rel} cannot be read: {e}"),
                            fix: format!("cp {t_path_disp} {}", ws_file.display()),
                        });
                    }
                }
            }
        }
    }
    problems
}

/// Pure diff function: compare event hooks declared in `manifest_toml` against
/// registrations in `plugins_json`.
pub fn diff_plugin_hooks(
    manifest_toml: &str,
    manifest_path: &Path,
    plugins_json: &str,
) -> Result<Vec<DriftProblem>, String> {
    let mut problems = Vec::new();
    let manifest_val: toml::Value =
        toml::from_str(manifest_toml).map_err(|e| format!("cannot parse manifest TOML: {e}"))?;

    let plugin_id = manifest_val
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("herdr-quaqua");

    let mut declared_events: Vec<(String, Vec<String>)> = Vec::new();
    if let Some(events_arr) = manifest_val.get("events").and_then(|v| v.as_array()) {
        for ev in events_arr {
            let on = ev.get("on").and_then(|v| v.as_str()).unwrap_or("");
            let cmd: Vec<String> = ev
                .get("command")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|s| s.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            if !on.is_empty() && !cmd.is_empty() {
                declared_events.push((on.to_string(), cmd));
            }
        }
    }

    let plugin_dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let fix_cmd = format!("herdr plugin link {}", plugin_dir.display());

    let parsed_json: serde_json::Value = match serde_json::from_str(plugins_json) {
        Ok(v) => v,
        Err(e) => {
            problems.push(DriftProblem {
                drift: format!("plugin registration drift: plugins.json cannot be parsed ({e})"),
                fix: fix_cmd,
            });
            return Ok(problems);
        }
    };

    let entries = match parsed_json.as_array() {
        Some(arr) => arr,
        None => {
            problems.push(DriftProblem {
                drift: "plugin registration drift: plugins.json is not an array".to_string(),
                fix: fix_cmd,
            });
            return Ok(problems);
        }
    };

    let matching_plugin = entries.iter().find(|entry| {
        entry.get("plugin_id").and_then(|v| v.as_str()) == Some(plugin_id)
            || entry.get("name").and_then(|v| v.as_str()) == Some(plugin_id)
    });

    let Some(plugin) = matching_plugin else {
        problems.push(DriftProblem {
            drift: format!(
                "plugin hook drift: herdr plugin '{plugin_id}' is not registered in plugins.json"
            ),
            fix: fix_cmd,
        });
        return Ok(problems);
    };

    let registered_events = plugin
        .get("events")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    let mut missing_hooks = Vec::new();
    for (on, cmd) in &declared_events {
        let found = registered_events.iter().any(|ev| {
            let ev_on = ev.get("on").and_then(|v| v.as_str()).unwrap_or("");
            let ev_cmd: Vec<String> = ev
                .get("command")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|s| s.as_str().map(String::from))
                        .collect()
                })
                .unwrap_or_default();
            ev_on == on && ev_cmd == *cmd
        });
        if !found {
            missing_hooks.push(format!("{on} -> {cmd:?}"));
        }
    }

    if !missing_hooks.is_empty() {
        problems.push(DriftProblem {
            drift: format!(
                "plugin hook drift: herdr plugin '{plugin_id}' lacks event hook(s) declared in {}: {}",
                manifest_path.display(),
                missing_hooks.join(", ")
            ),
            fix: fix_cmd,
        });
    }

    Ok(problems)
}

/// Check pane identity tokens against the live herdr registry.
///
/// Problems: a registered Lead in `projects/INDEX.md` whose pane tokens are
/// missing `grid_role=orchestrator` or `grid_owner=<slug>-lead`.
/// Warnings: herdr is live but no pane carries `grid_role=cos` (no CoS session
/// running is a legal state), and a named pane carrying no `grid_*` token at
/// all. Both warnings are the shape a herdr server restart leaves behind —
/// herdr does not persist pane tokens, so nothing re-applies `grid_role`.
pub fn diff_lead_tokens_with<H: crate::herdr::HerdrOps>(
    workspace: &Path,
    herdr: &H,
) -> WorkspaceDriftReport {
    let mut report = WorkspaceDriftReport::default();

    let Some(agents_val) = crate::lead::tag::get_agent_list_json(herdr) else {
        return report;
    };
    let Some(agents) = agents_val
        .pointer("/result/agents")
        .and_then(|a| a.as_array())
    else {
        return report;
    };

    // One pane tagged `cos` is the CoS identity for this herdr session.
    let has_cos_pane = agents.iter().any(|agent| {
        agent
            .get("tokens")
            .and_then(|t| t.get("grid_role"))
            .and_then(|v| v.as_str())
            == Some("cos")
    });
    if !has_cos_pane {
        report.warnings.push(
            "identity drift: herdr is live but no pane carries grid_role=cos — if a CoS session \
             is running, run `quaqua cos tag` in its pane"
                .to_string(),
        );
    }

    let mut flagged_leads: Vec<String> = Vec::new();
    let index_file = workspace.join("projects/INDEX.md");
    if let Ok(content) = std::fs::read_to_string(&index_file) {
        let lead_rows: Vec<_> = crate::worker::leadgate::index_rows(&content)
            .into_iter()
            .filter(|r| r.owner == format!("{}-lead", r.slug))
            .collect();

        for row in lead_rows {
            for agent in agents {
                let name = agent.get("name").and_then(|n| n.as_str()).unwrap_or("");
                if name == row.owner {
                    let pane_id = agent.get("pane_id").and_then(|p| p.as_str()).unwrap_or("");
                    let tokens = agent.get("tokens").and_then(|t| t.as_object());
                    let role = tokens
                        .and_then(|t| t.get("grid_role"))
                        .and_then(|v| v.as_str());
                    let owner = tokens
                        .and_then(|t| t.get("grid_owner"))
                        .and_then(|v| v.as_str());
                    if role != Some("orchestrator") || owner != Some(&row.owner) {
                        flagged_leads.push(name.to_string());
                        report.problems.push(DriftProblem {
                            drift: format!(
                                "lead identity drift: registered lead '{}' (pane '{}') is missing orchestrator tokens",
                                row.slug, pane_id
                            ),
                            fix: format!("quaqua lead repair {}", row.slug),
                        });
                    }
                }
            }
        }
    }

    // A named pane with no `grid_*` token at all lost the whole quaqua block;
    // a registered Lead already has a problem above, so report each pane once.
    for agent in agents {
        let name = agent.get("name").and_then(|n| n.as_str()).unwrap_or("");
        if name.is_empty() || flagged_leads.iter().any(|n| n == name) {
            continue;
        }
        let tokens = agent.get("tokens").and_then(|t| t.as_object());
        let carries_grid = tokens
            .map(|t| t.keys().any(|k| k.starts_with("grid_")))
            .unwrap_or(false);
        if carries_grid {
            continue;
        }
        let pane_id = agent.get("pane_id").and_then(|p| p.as_str()).unwrap_or("");
        // `quaqua lead repair` fails closed for a slug with no `projects/INDEX.md`
        // row, which is exactly the state left here (a registered Lead missing its
        // tokens is a problem above, and skipped): state the precondition too.
        let repair = match name.strip_suffix("-lead") {
            Some(slug) => format!(
                " — if this is a registered Lead, run `quaqua lead repair {slug}` (it needs a projects/INDEX.md row)"
            ),
            None => String::new(),
        };
        report.warnings.push(format!(
            "identity drift: named pane '{name}' (pane '{pane_id}') carries no grid_* token{repair}"
        ));
    }

    report
}

/// Default `diff_lead_tokens` using the system Herdr driver.
/// Skips live daemon lookup during unit tests unless HERDR_BIN_PATH is provided.
pub fn diff_lead_tokens(workspace: &Path) -> WorkspaceDriftReport {
    if std::env::var("QUAQUA_TEST").as_deref() == Ok("1")
        && std::env::var("HERDR_BIN_PATH").is_err()
    {
        return WorkspaceDriftReport::default();
    }
    let herdr = crate::herdr::Herdr::new();
    diff_lead_tokens_with(workspace, &herdr)
}
pub fn resolve_engine_from_workspace(ws: &Path) -> Option<PathBuf> {
    let index = ws.join("projects/INDEX.md");
    if let Ok(content) = std::fs::read_to_string(&index) {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
        for line in content.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with('|') {
                let parts: Vec<&str> = trimmed.split('|').map(|s| s.trim()).collect();
                if parts.len() >= 3 && parts[1] != "slug" && !parts[1].starts_with('-') {
                    let r = parts[2];
                    let repo_str = if let Some(stripped) = r.strip_prefix("~/") {
                        format!("{}/{}", home, stripped)
                    } else if r == "~" {
                        home.clone()
                    } else {
                        r.to_string()
                    };
                    let p = PathBuf::from(repo_str);
                    if p.join("crates/quaqua/src").is_dir() {
                        return Some(p);
                    }
                }
            }
        }
    }
    if let Ok(home) = std::env::var("HOME") {
        let default_engine = PathBuf::from(home).join("Lab/quaqua");
        if default_engine.join("crates/quaqua/src").is_dir() {
            return Some(default_engine);
        }
    }
    None
}

/// Resolve the herdr config directory according to herdr conventions:
/// 1. `HERDR_CONFIG_DIR` explicit override
/// 2. `HERDR_CONFIG_PATH` explicit override (parent directory of config file)
/// 3. `XDG_CONFIG_HOME/herdr`
/// 4. `$HOME/.config/herdr`
pub fn resolve_herdr_config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("HERDR_CONFIG_DIR").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(dir));
    }
    if let Some(path) = std::env::var_os("HERDR_CONFIG_PATH").filter(|v| !v.is_empty()) {
        if let Some(parent) = Path::new(&path).parent() {
            return Some(parent.to_path_buf());
        }
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(xdg).join("herdr"));
    }
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(home).join(".config/herdr"));
    }
    None
}

pub fn resolve_herdr_plugins_json() -> Option<PathBuf> {
    resolve_herdr_config_dir().map(|dir| dir.join("plugins.json"))
}

pub fn evaluate_workspace(
    workspace: Option<&Path>,
    checkout: Option<&Path>,
    plugins_json_path: Option<&Path>,
) -> WorkspaceDriftReport {
    let mut report = WorkspaceDriftReport::default();
    let ws = workspace
        .map(PathBuf::from)
        .unwrap_or_else(crate::workspace::workspace_root);

    report.notes.push(format!("workspace: {}", ws.display()));

    let checkout_dir = checkout
        .map(PathBuf::from)
        .or_else(checkout_root)
        .or_else(|| resolve_engine_from_workspace(&ws));

    // 1. Rules drift
    if let Some(co) = &checkout_dir {
        report.notes.push(format!("checkout: {}", co.display()));
        let t_rules = co.join("templates/workspace/rules/_all");
        let ws_rules = ws.join("rules/_all");
        let rule_problems = diff_rules(&t_rules, &ws_rules);
        report.problems.extend(rule_problems);
    } else {
        let ws_rules = ws.join("rules/_all");
        let rule_problems = diff_embedded_rules(&ws_rules);
        report.problems.extend(rule_problems);
    }

    // 2. Herdr plugin hook drift
    let manifest_path = checkout_dir
        .as_ref()
        .map(|c| c.join("plugins/herdr-quaqua/herdr-plugin.toml"))
        .unwrap_or_else(|| PathBuf::from("plugins/herdr-quaqua/herdr-plugin.toml"));

    let manifest_toml = if manifest_path.is_file() {
        std::fs::read_to_string(&manifest_path).ok()
    } else {
        Some(include_str!("../../../plugins/herdr-quaqua/herdr-plugin.toml").to_string())
    };

    let pjson_path = plugins_json_path
        .map(PathBuf::from)
        .or_else(resolve_herdr_plugins_json);

    match (manifest_toml, pjson_path) {
        (Some(toml_str), Some(pj_path)) => {
            if pj_path.is_file() {
                match std::fs::read_to_string(&pj_path) {
                    Ok(json_str) => match diff_plugin_hooks(&toml_str, &manifest_path, &json_str) {
                        Ok(probs) => report.problems.extend(probs),
                        Err(e) => report
                            .warnings
                            .push(format!("plugin hook check error: {e}")),
                    },
                    Err(e) => {
                        let plugin_dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));
                        report.problems.push(DriftProblem {
                            drift: format!(
                                "plugin hook drift: herdr plugins.json at {} cannot be read: {e}",
                                pj_path.display()
                            ),
                            fix: format!("herdr plugin link {}", plugin_dir.display()),
                        });
                    }
                }
            } else {
                let plugin_dir = manifest_path.parent().unwrap_or_else(|| Path::new("."));
                report.problems.push(DriftProblem {
                    drift: format!(
                        "plugin hook drift: herdr plugins.json not found at {}",
                        pj_path.display()
                    ),
                    fix: format!("herdr plugin link {}", plugin_dir.display()),
                });
            }
        }
        (None, _) => {
            report
                .warnings
                .push("cannot find herdr-plugin.toml manifest".to_string());
        }
        (_, None) => {
            report
                .warnings
                .push("cannot resolve herdr plugins.json path".to_string());
        }
    }

    // 3. Pane identity token drift (Lead tokens are problems; a missing CoS
    //    pane or an untagged named pane is a warning)
    let identity = diff_lead_tokens(&ws);
    report.problems.extend(identity.problems);
    report.warnings.extend(identity.warnings);

    // 4. Workspace warnings
    report.warnings.extend(workspace_warnings(Some(&ws)));

    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("quaqua-doctor-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Deterministic mtime (filesystem timestamp granularity differs).
    fn set_mtime(path: &Path, secs: u64) {
        let file = fs::OpenOptions::new().write(true).open(path).unwrap();
        file.set_modified(SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
            .unwrap();
    }

    fn stub_shell(dir: &Path, line: &str) -> PathBuf {
        let path = dir.join("shell");
        fs::write(&path, format!("#!/bin/sh\nprintf '%s\\n' '{line}'\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    #[test]
    fn missing_binary_is_a_problem() {
        let report = evaluate(None, None, None, None);
        assert_eq!(report.problems.len(), 1);
        assert!(
            report.problems[0].contains("not on PATH"),
            "{:?}",
            report.problems
        );
    }

    #[test]
    fn a_shell_function_or_alias_shadowing_quaqua_is_a_problem() {
        assert!(is_shadowed("quaqua is a function\nquaqua () { ... }").is_some());
        assert!(is_shadowed("quaqua is aliased to `~/bin/quaqua-old'").is_some());
        assert!(is_shadowed("quaqua: function").is_some());
        assert!(is_shadowed("quaqua is /usr/local/bin/quaqua").is_none());
        assert!(is_shadowed("").is_none());

        let dir = temp_dir("shadow");
        let shell = stub_shell(&dir, "quaqua is a shell function");
        let bin = dir.join("quaqua");
        fs::write(&bin, "").unwrap();
        let report = evaluate(Some(&bin), None, None, Some(&shell));
        assert!(
            report.problems.iter().any(|p| p.contains("function/alias")),
            "{:?}",
            report.problems
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_binary_older_than_the_checkout_is_stale() {
        let dir = temp_dir("stale");
        let checkout = dir.join("checkout");
        let src = checkout.join("crates/quaqua/src");
        fs::create_dir_all(&src).unwrap();
        let bin = dir.join("quaqua");
        fs::write(&bin, "").unwrap();
        // The source is newer than the binary: the binary is stale.
        fs::write(src.join("main.rs"), "fn main() {}").unwrap();
        set_mtime(&bin, 1_000);
        set_mtime(&src.join("main.rs"), 2_000);
        let report = evaluate(Some(&bin), Some(&checkout), None, None);
        assert!(
            report
                .problems
                .iter()
                .any(|p| p.contains("older than the checkout")),
            "{:?}",
            report.problems
        );

        // A fresh binary (newer than the source) is accepted.
        let fresh = dir.join("quaqua-fresh");
        fs::write(&fresh, "").unwrap();
        set_mtime(&fresh, 3_000);
        let report = evaluate(Some(&fresh), Some(&checkout), None, None);
        assert!(report.problems.is_empty(), "{:?}", report.problems);
        let _ = fs::remove_dir_all(&dir);
    }

    /// A workspace dir with a project index and a persona-importing AGENTS.md:
    /// the layout `quaqua init` is supposed to produce.
    fn workspace_dir(dir: &Path) -> PathBuf {
        let ws = dir.join("workspace");
        fs::create_dir_all(ws.join("projects")).unwrap();
        fs::write(ws.join("projects/INDEX.md"), "").unwrap();
        fs::write(
            ws.join("AGENTS.md"),
            "# CoS workspace\n\n@identity.md\n@SOUL.md\n",
        )
        .unwrap();
        ws
    }

    #[test]
    fn a_workspace_that_also_holds_the_engine_checkout_is_a_warning_not_a_problem() {
        let dir = temp_dir("overlap");
        let ws = workspace_dir(&dir);

        // A clean workspace is quiet (only the missing binary is a problem).
        let report = evaluate(None, None, Some(&ws), None);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert_eq!(report.problems.len(), 1, "{:?}", report.problems);

        // `Cargo.toml` at the root: the workspace is also an engine checkout.
        fs::write(ws.join("Cargo.toml"), "[workspace]\n").unwrap();
        let report = evaluate(None, None, Some(&ws), None);
        assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
        assert!(
            report.warnings[0].contains("Cargo.toml"),
            "{:?}",
            report.warnings
        );
        assert_eq!(report.problems.len(), 1, "{:?}", report.problems);

        // `crates/` alone is enough of a signal.
        let other = dir.join("other");
        fs::create_dir_all(other.join("crates/quaqua/src")).unwrap();
        let report = evaluate(None, None, Some(&other), None);
        assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
        assert!(
            report.warnings[0].contains("crates/"),
            "{:?}",
            report.warnings
        );

        // Neither a missing workspace dir nor no workspace at all warns.
        let report = evaluate(None, None, Some(&dir.join("absent")), None);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        let report = evaluate(None, None, None, None);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_workspace_agents_md_without_the_persona_import_is_a_warning() {
        let dir = temp_dir("agents");
        let ws = workspace_dir(&dir);
        let agents = ws.join("AGENTS.md");

        // Importing the persona (the `quaqua init` layout) is quiet.
        let report = evaluate(None, None, Some(&ws), None);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);

        // No AGENTS.md at all: nothing claims to import anything.
        fs::remove_file(&agents).unwrap();
        let report = evaluate(None, None, Some(&ws), None);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);

        // The engine AGENTS.md the workspace root used to carry: no persona.
        fs::write(
            &agents,
            "# quaqua engine\n\nEngine-only: crates/, tests/.\n",
        )
        .unwrap();
        let report = evaluate(None, None, Some(&ws), None);
        assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
        assert!(
            report.warnings[0].contains("@identity.md"),
            "{:?}",
            report.warnings
        );
        // A warning never fails the run.
        assert_eq!(report.problems.len(), 1, "{:?}", report.problems);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_checkout_is_the_engine_sources_never_the_workspace_root() {
        let dir = temp_dir("checkout");
        let engine = dir.join("Lab/quaqua");
        fs::create_dir_all(engine.join("crates/quaqua/src")).unwrap();
        let ws = dir.join("quaqua");
        fs::create_dir_all(ws.join("projects")).unwrap();

        // Running inside the engine checkout discovers it from the cwd.
        assert_eq!(
            checkout_root_from(None, Some(&engine.join("crates/quaqua")), Some(&ws)),
            Some(engine.clone())
        );
        // An explicit QUAQUA_CHECKOUT wins.
        assert_eq!(
            checkout_root_from(Some(&engine), None, Some(&ws)),
            Some(engine.clone())
        );
        // The workspace root is not the checkout, even when engine sources sit
        // there (`~/quaqua` before the split): that layout is a warning instead.
        let overlapping = dir.join("overlap");
        fs::create_dir_all(overlapping.join("crates/quaqua/src")).unwrap();
        assert_eq!(
            checkout_root_from(None, Some(&overlapping), Some(&overlapping)),
            None
        );
        // Outside any checkout nothing is assumed.
        assert_eq!(checkout_root_from(None, Some(&ws), Some(&ws)), None);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_on_path_skips_missing_entries() {
        let dir = temp_dir("path");
        let bin = dir.join("quaqua");
        fs::write(&bin, "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&bin, fs::Permissions::from_mode(0o755)).unwrap();
        }
        let missing = format!("/nope/{}", dir.display());
        let path = OsStr::new(&missing);
        assert!(find_on_path("quaqua", Some(path)).is_none());
        let path = dir.as_os_str();
        assert_eq!(find_on_path("quaqua", Some(path)), Some(bin));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn diff_rules_detects_content_difference_and_missing_rules() {
        let dir = temp_dir("diff-rules");
        let template_dir = dir.join("templates/rules");
        let ws_rules_dir = dir.join("ws/rules");
        fs::create_dir_all(&template_dir).unwrap();
        fs::create_dir_all(&ws_rules_dir).unwrap();

        fs::write(
            template_dir.join("sample-rule.md"),
            "template sample rule content",
        )
        .unwrap();
        fs::write(template_dir.join("brief.md"), "common brief content").unwrap();
        fs::write(template_dir.join("missing.md"), "template only content").unwrap();

        // ws has differing lead-inbox, identical brief, and missing.md is absent
        fs::write(
            ws_rules_dir.join("sample-rule.md"),
            "stale workspace sample rule",
        )
        .unwrap();
        fs::write(ws_rules_dir.join("brief.md"), "common brief content").unwrap();

        let problems = diff_rules(&template_dir, &ws_rules_dir);
        assert_eq!(problems.len(), 2, "{problems:?}");

        let diff_lead = problems
            .iter()
            .find(|p| p.drift.contains("sample-rule.md"))
            .unwrap();
        assert!(diff_lead.drift.contains("differs from template"));
        assert!(diff_lead.fix.starts_with("cp "));

        let missing = problems
            .iter()
            .find(|p| p.drift.contains("missing.md"))
            .unwrap();
        assert!(missing.drift.contains("is missing"));
        assert!(missing.fix.starts_with("cp "));

        // When workspace is synced: 0 problems
        fs::write(
            ws_rules_dir.join("sample-rule.md"),
            "template sample rule content",
        )
        .unwrap();
        fs::write(ws_rules_dir.join("missing.md"), "template only content").unwrap();
        let clean = diff_rules(&template_dir, &ws_rules_dir);
        assert!(clean.is_empty(), "{clean:?}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn diff_plugin_hooks_detects_missing_event_hooks() {
        let dir = temp_dir("diff-hooks");
        let manifest_path = dir.join("herdr-quaqua/herdr-plugin.toml");
        fs::create_dir_all(manifest_path.parent().unwrap()).unwrap();

        let manifest_toml = r#"
id = "herdr-quaqua"
name = "herdr-quaqua"

[[events]]
on = "pane.created"
command = ["quaqua", "grid", "hook"]

[[events]]
on = "pane.agent_status_changed"
command = ["quaqua", "events", "hook"]

[[events]]
on = "pane.exited"
command = ["quaqua", "events", "hook"]
"#;

        // plugins.json has only pane.created
        let partial_json = r#"[
  {
    "plugin_id": "herdr-quaqua",
    "name": "herdr-quaqua",
    "events": [
      {
        "on": "pane.created",
        "command": ["quaqua", "grid", "hook"]
      }
    ]
  }
]"#;

        let problems = diff_plugin_hooks(manifest_toml, &manifest_path, partial_json).unwrap();
        assert_eq!(problems.len(), 1, "{problems:?}");
        assert!(problems[0].drift.contains("lacks event hook(s)"));
        assert!(problems[0].drift.contains("pane.agent_status_changed"));
        assert!(problems[0].drift.contains("pane.exited"));
        assert!(problems[0].fix.contains("herdr plugin link"));

        // When plugins.json has all hooks: 0 problems
        let full_json = r#"[
  {
    "plugin_id": "herdr-quaqua",
    "name": "herdr-quaqua",
    "events": [
      {
        "on": "pane.created",
        "command": ["quaqua", "grid", "hook"]
      },
      {
        "on": "pane.agent_status_changed",
        "command": ["quaqua", "events", "hook"]
      },
      {
        "on": "pane.exited",
        "command": ["quaqua", "events", "hook"]
      }
    ]
  }
]"#;
        let clean = diff_plugin_hooks(manifest_toml, &manifest_path, full_json).unwrap();
        assert!(clean.is_empty(), "{clean:?}");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_resolve_herdr_config_dir_resolution_order() {
        let _env_lock = crate::test_env::lock_env();
        let _g_dir = crate::test_env::TestEnvVarGuard::remove("HERDR_CONFIG_DIR");
        let _g_path = crate::test_env::TestEnvVarGuard::remove("HERDR_CONFIG_PATH");
        let _g_xdg = crate::test_env::TestEnvVarGuard::remove("XDG_CONFIG_HOME");
        let _g_home = crate::test_env::TestEnvVarGuard::remove("HOME");

        let tmp = temp_dir("herdr-cfg-order");

        // 1. None when all unset
        assert_eq!(resolve_herdr_config_dir(), None);
        assert_eq!(resolve_herdr_plugins_json(), None);

        // 2. Fallback to HOME/.config/herdr
        std::env::set_var("HOME", tmp.join("fake-home"));
        assert_eq!(
            resolve_herdr_config_dir(),
            Some(tmp.join("fake-home/.config/herdr"))
        );
        assert_eq!(
            resolve_herdr_plugins_json(),
            Some(tmp.join("fake-home/.config/herdr/plugins.json"))
        );

        // 3. XDG_CONFIG_HOME takes precedence over HOME
        std::env::set_var("XDG_CONFIG_HOME", tmp.join("fake-xdg"));
        assert_eq!(resolve_herdr_config_dir(), Some(tmp.join("fake-xdg/herdr")));
        assert_eq!(
            resolve_herdr_plugins_json(),
            Some(tmp.join("fake-xdg/herdr/plugins.json"))
        );

        // 4. HERDR_CONFIG_PATH takes precedence over XDG
        std::env::set_var("HERDR_CONFIG_PATH", tmp.join("fake-path/config.toml"));
        assert_eq!(resolve_herdr_config_dir(), Some(tmp.join("fake-path")));
        assert_eq!(
            resolve_herdr_plugins_json(),
            Some(tmp.join("fake-path/plugins.json"))
        );

        // 5. HERDR_CONFIG_DIR takes top precedence
        std::env::set_var("HERDR_CONFIG_DIR", tmp.join("fake-dir"));
        assert_eq!(resolve_herdr_config_dir(), Some(tmp.join("fake-dir")));
        assert_eq!(
            resolve_herdr_plugins_json(),
            Some(tmp.join("fake-dir/plugins.json"))
        );

        let _ = fs::remove_dir_all(&tmp);
    }
    struct MockDoctorHerdr(String);
    impl crate::herdr::HerdrOps for MockDoctorHerdr {
        fn run(&self, _args: &[&str]) -> std::io::Result<std::process::Output> {
            unimplemented!()
        }
        fn agent_list(&self) -> std::io::Result<String> {
            Ok(self.0.clone())
        }
        fn agent_get(&self, _n: &str) -> std::io::Result<(bool, String)> {
            unimplemented!()
        }
        fn agent_read(&self, _n: &str, _s: &str, _l: u32) -> std::io::Result<String> {
            Ok(String::new())
        }
        fn agent_start(
            &self,
            _n: &str,
            _k: &str,
            _p: &str,
            _t: u32,
            _a: &[&str],
        ) -> std::io::Result<std::process::Output> {
            unimplemented!()
        }
        fn pane_split(
            &self,
            _c: &str,
            _o: &str,
            _e: &[(&str, &str)],
        ) -> std::io::Result<std::process::Output> {
            unimplemented!()
        }
        fn pane_close(&self, _p: &str) -> std::io::Result<std::process::Output> {
            unimplemented!()
        }
        fn pane_report_metadata(
            &self,
            _p: &str,
            _t: &[&str],
        ) -> std::io::Result<std::process::Output> {
            unimplemented!()
        }
        fn pane_wait_output(&self, _p: &str, _m: &str, _t: u32) -> bool {
            true
        }
        fn pane_read(&self, _p: &str, _s: &str, _l: u32) -> std::io::Result<String> {
            Ok(String::new())
        }
        fn pane_send_keys(&self, _p: &str, _k: &str) -> std::io::Result<std::process::Output> {
            unimplemented!()
        }
        fn agent_send_keys(&self, _n: &str, _key: &str) -> std::io::Result<std::process::Output> {
            unimplemented!()
        }
        fn agent_wait_idle(&self, _n: &str, _t: u32) -> std::io::Result<std::process::Output> {
            unimplemented!()
        }
        fn agent_prompt_nowait(&self, _n: &str, _p: &str) -> std::io::Result<std::process::Output> {
            unimplemented!()
        }
        fn agent_prompt_wait_background(
            &self,
            _n: &str,
            _p: &str,
            _t: u32,
            _f: std::fs::File,
        ) -> std::io::Result<std::process::Child> {
            unimplemented!()
        }
    }

    #[test]
    fn diff_lead_tokens_detects_untagged_lead_and_clean_when_tagged() {
        let dir = temp_dir("diff-lead-tokens");
        let ws = workspace_dir(&dir);
        let projects_dir = ws.join("projects");
        fs::create_dir_all(&projects_dir).unwrap();
        fs::write(
            projects_dir.join("INDEX.md"),
            "| slug | repo | owner | status | goal |\n|---|---|---|---|---|\n| mu | ~/Lab/mu | mu-lead | active | Mole |\n",
        ).unwrap();

        // Case 1: Untagged lead pane beside a tagged CoS pane -> detected as drift
        let untagged_json = r#"{"result":{"agents":[
            {"pane_id":"w1:p1X","tokens":{"grid_role":"cos","grid_owner":"cos"}},
            {"name":"mu-lead","pane_id":"wB:p1","tokens":{"logo":"\ue1a3"}}
        ]}}"#;
        let report = diff_lead_tokens_with(&ws, &MockDoctorHerdr(untagged_json.to_string()));
        assert_eq!(report.problems.len(), 1);
        assert!(report.problems[0]
            .drift
            .contains("missing orchestrator tokens"));
        assert!(report.problems[0].drift.contains("wB:p1"));
        assert_eq!(report.problems[0].fix, "quaqua lead repair mu");
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);

        // Case 2: Correctly tagged lead beside a tagged CoS pane -> silent
        let tagged_json = r#"{"result":{"agents":[
            {"pane_id":"w1:p1X","tokens":{"grid_role":"cos","grid_owner":"cos"}},
            {"name":"mu-lead","pane_id":"wB:p1","tokens":{"grid_role":"orchestrator","grid_owner":"mu-lead","logo":"\ue1a3"}}
        ]}}"#;
        let clean = diff_lead_tokens_with(&ws, &MockDoctorHerdr(tagged_json.to_string()));
        assert!(clean.problems.is_empty(), "{:?}", clean.problems);
        assert!(clean.warnings.is_empty(), "{:?}", clean.warnings);

        // Case 3: herdr is live but no pane carries the CoS role (a herdr
        // restart dropped the CoS token) -> one warning with the repair, no error
        let no_cos_json = r#"{"result":{"agents":[
            {"name":"mu-lead","pane_id":"wB:p1","tokens":{"grid_role":"orchestrator","grid_owner":"mu-lead"}},
            {"name":"sude-task","pane_id":"wC:p2","tokens":{"grid_role":"worker","grid_owner":"mu-lead","grid_name":"sude-task"}}
        ]}}"#;
        let no_cos = diff_lead_tokens_with(&ws, &MockDoctorHerdr(no_cos_json.to_string()));
        assert!(no_cos.problems.is_empty(), "{:?}", no_cos.problems);
        assert_eq!(no_cos.warnings.len(), 1, "{:?}", no_cos.warnings);
        assert!(
            no_cos.warnings[0].contains("quaqua cos tag"),
            "{:?}",
            no_cos.warnings
        );

        // Case 4: named panes whose tokens carry no grid_* at all -> one warning
        // each, naming the pane and (for a Lead) the repair; the tagged CoS pane
        // and the tagged registered lead stay silent
        let untagged_workers_json = r#"{"result":{"agents":[
            {"pane_id":"w1:p1X","tokens":{"grid_role":"cos","grid_owner":"cos"}},
            {"name":"mu-lead","pane_id":"wB:p1","tokens":{"grid_role":"orchestrator","grid_owner":"mu-lead"}},
            {"name":"sude-task","pane_id":"wC:p2","tokens":{"logo":"\ue1a3"}},
            {"name":"llmhub-lead","pane_id":"wA:p1","tokens":{"logo":"\ue1a3"}}
        ]}}"#;
        let workers =
            diff_lead_tokens_with(&ws, &MockDoctorHerdr(untagged_workers_json.to_string()));
        assert!(workers.problems.is_empty(), "{:?}", workers.problems);
        assert_eq!(workers.warnings.len(), 2, "{:?}", workers.warnings);
        assert!(
            workers.warnings[0].contains("'sude-task' (pane 'wC:p2')"),
            "{:?}",
            workers.warnings
        );
        assert!(
            workers.warnings[1].contains("'llmhub-lead' (pane 'wA:p1')")
                && workers.warnings[1].contains("quaqua lead repair llmhub"),
            "{:?}",
            workers.warnings
        );

        let _ = fs::remove_dir_all(&dir);
    }
    #[test]
    fn doctor_auditor_unit() {
        let dir = temp_dir("auditor-unit");
        let valid_home = dir.join("valid-home");
        let dangling_home = dir.join("dangling-home");
        let missing_home = dir.join("missing-home");

        for h in [&valid_home, &dangling_home, &missing_home] {
            fs::create_dir_all(h).unwrap();
        }

        // Target files for symlinks
        let target_dir = dir.join("targets");
        fs::create_dir_all(&target_dir).unwrap();
        let svc_target = target_dir.join("quaqua-auditor.service");
        let tmr_target = target_dir.join("quaqua-auditor.timer");
        fs::write(&svc_target, "[Unit]\nDescription=Auditor\n").unwrap();
        fs::write(&tmr_target, "[Unit]\nDescription=Auditor Timer\n").unwrap();

        // (a) Valid symlink/unit
        let valid_systemd = valid_home.join(".config/systemd/user");
        fs::create_dir_all(&valid_systemd).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&svc_target, valid_systemd.join("quaqua-auditor.service"))
                .unwrap();
            std::os::unix::fs::symlink(&tmr_target, valid_systemd.join("quaqua-auditor.timer"))
                .unwrap();
        }

        // (b) Dangling symlink
        let dangling_systemd = dangling_home.join(".config/systemd/user");
        fs::create_dir_all(&dangling_systemd).unwrap();
        let nonexistent = dir.join("nonexistent");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(
                nonexistent.join("quaqua-auditor.service"),
                dangling_systemd.join("quaqua-auditor.service"),
            )
            .unwrap();
            std::os::unix::fs::symlink(
                nonexistent.join("quaqua-auditor.timer"),
                dangling_systemd.join("quaqua-auditor.timer"),
            )
            .unwrap();
        }

        // (c) Missing unit: missing_home has no .config/systemd/user units

        // Mock systemd runner that returns clean output and active timer
        struct MockSystemdSuccess;
        impl SystemdRunner for MockSystemdSuccess {
            fn list_unit_files(&self, _pattern: &str) -> Result<String, String> {
                Ok("quaqua-auditor.service enabled enabled\nquaqua-auditor.timer enabled enabled\n"
                    .to_string())
            }
            fn is_active(&self, _unit: &str) -> Result<bool, String> {
                Ok(true)
            }
        }

        // (a) Valid symlink/unit: passes
        let mut report_valid = Report {
            problems: Vec::new(),
            warnings: Vec::new(),
            notes: Vec::new(),
            checkout_checked: false,
        };
        check_auditor_units_with(
            Some(&valid_home),
            false,
            &MockSystemdSuccess,
            &mut report_valid,
        );
        assert!(
            report_valid.problems.is_empty(),
            "valid symlinks should pass, got: {:?}",
            report_valid.problems
        );

        // (b) Dangling symlink: fail-with-fix
        let mut report_dangling = Report {
            problems: Vec::new(),
            warnings: Vec::new(),
            notes: Vec::new(),
            checkout_checked: false,
        };
        check_auditor_units_with(
            Some(&dangling_home),
            false,
            &MockSystemdSuccess,
            &mut report_dangling,
        );
        assert!(
            !report_dangling.problems.is_empty(),
            "dangling symlinks must fail"
        );
        for p in &report_dangling.problems {
            assert!(
                p.contains(AUDITOR_FIX_CMD),
                "problem must contain fix command: {p}"
            );
            assert!(
                p.contains("dangling"),
                "problem must report dangling symlink: {p}"
            );
        }

        // (c) Missing unit: fail-with-fix
        let mut report_missing = Report {
            problems: Vec::new(),
            warnings: Vec::new(),
            notes: Vec::new(),
            checkout_checked: false,
        };
        check_auditor_units_with(
            Some(&missing_home),
            false,
            &MockSystemdSuccess,
            &mut report_missing,
        );
        assert!(
            !report_missing.problems.is_empty(),
            "missing units must fail"
        );
        for p in &report_missing.problems {
            assert!(
                p.contains(AUDITOR_FIX_CMD),
                "problem must contain fix command: {p}"
            );
            assert!(
                p.contains("missing"),
                "problem must report missing unit: {p}"
            );
        }

        // Bad unit file in systemctl: fail-with-fix
        struct MockSystemdBad;
        impl SystemdRunner for MockSystemdBad {
            fn list_unit_files(&self, _pattern: &str) -> Result<String, String> {
                Ok(
                    "quaqua-auditor.service bad enabled\nquaqua-auditor.timer bad enabled\n"
                        .to_string(),
                )
            }
            fn is_active(&self, _unit: &str) -> Result<bool, String> {
                Ok(true)
            }
        }
        let mut report_bad = Report {
            problems: Vec::new(),
            warnings: Vec::new(),
            notes: Vec::new(),
            checkout_checked: false,
        };
        check_auditor_units_with(Some(&valid_home), false, &MockSystemdBad, &mut report_bad);
        assert!(
            !report_bad.problems.is_empty(),
            "bad units in list-unit-files must fail"
        );
        for p in &report_bad.problems {
            assert!(
                p.contains(AUDITOR_FIX_CMD),
                "problem must contain fix command: {p}"
            );
            assert!(p.contains("bad"), "problem must report bad unit: {p}");
        }

        // Inactive timer in systemctl: fail-with-fix
        struct MockSystemdInactive;
        impl SystemdRunner for MockSystemdInactive {
            fn list_unit_files(&self, _pattern: &str) -> Result<String, String> {
                Ok(
                    "quaqua-auditor.service enabled enabled\nquaqua-auditor.timer enabled enabled\n"
                        .to_string(),
                )
            }
            fn is_active(&self, _unit: &str) -> Result<bool, String> {
                Ok(false)
            }
        }
        let mut report_inactive = Report {
            problems: Vec::new(),
            warnings: Vec::new(),
            notes: Vec::new(),
            checkout_checked: false,
        };
        check_auditor_units_with(
            Some(&valid_home),
            false,
            &MockSystemdInactive,
            &mut report_inactive,
        );
        assert!(
            !report_inactive.problems.is_empty(),
            "inactive timer must fail"
        );
        for p in &report_inactive.problems {
            assert!(
                p.contains(AUDITOR_FIX_CMD),
                "problem must contain fix command: {p}"
            );
            assert!(p.contains("inactive"), "problem must report inactive: {p}");
        }

        // macOS branch reports manual activation required rather than failing
        let mut report_macos = Report {
            problems: Vec::new(),
            warnings: Vec::new(),
            notes: Vec::new(),
            checkout_checked: false,
        };
        check_auditor_units_with(
            Some(&missing_home),
            true,
            &MockSystemdSuccess,
            &mut report_macos,
        );
        assert!(
            report_macos.problems.is_empty(),
            "macOS branch must not record problem"
        );
        assert!(
            report_macos
                .notes
                .iter()
                .any(|n| n.contains("manual activation required")),
            "macOS branch must note manual activation required: {:?}",
            report_macos.notes
        );

        let _ = fs::remove_dir_all(&dir);
    }

    fn empty_report() -> Report {
        Report {
            problems: Vec::new(),
            warnings: Vec::new(),
            notes: Vec::new(),
            checkout_checked: false,
        }
    }

    /// A real `pi --list-models` table (stdout only; extension
    /// warnings go to stderr): the header names the columns, every row is
    /// `provider  model  context  max-out  thinking  images`.
    const PI_TABLE: &str = "\
provider  model                                 context  max-out  thinking  images
mistral   deepseek-v4-1-flash-1017              1.0M     128K     yes       yes
mistral   codestral-2508                        256K     8.2K     no        no
hive      deepseek-ai/deepseek-v4.1-flash-1017  1M       32.8K    yes       yes
";

    fn team_toml(dir: &Path, kinds: &str) -> PathBuf {
        let path = dir.join("team.toml");
        fs::write(&path, format!("schema_version = 3\n\n{kinds}")).unwrap();
        path
    }

    /// A `pi` stub on PATH that prints `table` for `--list-models`.
    fn pi_stub(dir: &Path, table: &str) -> PathBuf {
        let table_file = dir.join("table");
        fs::write(&table_file, table).unwrap();
        let path = dir.join("pi");
        fs::write(
            &path,
            format!("#!/bin/sh\ncat '{}'\n", table_file.display()),
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    #[test]
    fn a_pi_kind_is_warned_about_only_when_pi_does_not_list_its_model() {
        let dir = temp_dir("pimodels");
        let cfg = crate::config::load(&team_toml(
            &dir,
            "[kinds.tieude]\nharness = \"pi\"\nmodel = \"mistral/deepseek-v4-1-flash-1017\"\n\n\
             [kinds.sude]\nharness = \"omp\"\nmodel = \"antigravity/not-a-model\"\n",
        ))
        .unwrap();

        // Listed: quiet. The omp kind is never pi's business.
        let mut report = empty_report();
        check_pi_models_with(&cfg, Some(PI_TABLE), &mut report);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);

        // Not listed: one warning naming the kind, the model and the fix.
        let cfg = crate::config::load(&team_toml(
            &dir,
            "[kinds.tieude]\nharness = \"pi\"\nmodel = \"mistral/not-a-model\"\n",
        ))
        .unwrap();
        let mut report = empty_report();
        check_pi_models_with(&cfg, Some(PI_TABLE), &mut report);
        assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
        assert!(
            report.warnings[0].contains("kind 'tieude'")
                && report.warnings[0].contains("mistral/not-a-model")
                && report.warnings[0].contains("kinds.tieude.model"),
            "{:?}",
            report.warnings
        );

        // pi absent: one note, no warning, and never a problem.
        let mut report = empty_report();
        check_pi_models_with(&cfg, None, &mut report);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);
        assert!(report.problems.is_empty(), "{:?}", report.problems);
        assert!(
            report.notes.iter().any(|n| n.contains("pi is not on PATH")),
            "{:?}",
            report.notes
        );

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_pi_kind_is_checked_against_the_pi_on_path() {
        let _lock = crate::test_env::lock_env();
        let dir = temp_dir("pistub");
        let bin = dir.join("bin");
        fs::create_dir_all(&bin).unwrap();
        let toml = team_toml(
            &dir,
            "[kinds.tieude]\nharness = \"pi\"\nmodel = \"mistral/deepseek-v4-1-flash-1017\"\n",
        );
        let _toml = crate::test_env::TestEnvVarGuard::set("QUAQUA_TEAM_TOML", &toml);
        // Prepended, not replaced: other tests keep the tools they spawn.
        let prev_path = std::env::var("PATH").unwrap_or_default();
        let _path =
            crate::test_env::TestEnvVarGuard::set("PATH", format!("{}:{prev_path}", bin.display()));

        // The stub pi lists the kind's model: the check is quiet.
        pi_stub(&bin, PI_TABLE);
        let mut report = empty_report();
        check_pi_models(&mut report);
        assert!(report.warnings.is_empty(), "{:?}", report.warnings);

        // The same pi without that model: one warning.
        pi_stub(
            &bin,
            "provider  model   context\nmistral   codestral-2508  256K\n",
        );
        let mut report = empty_report();
        check_pi_models(&mut report);
        assert_eq!(report.warnings.len(), 1, "{:?}", report.warnings);
        assert!(
            report.warnings[0].contains("mistral/deepseek-v4-1-flash-1017"),
            "{:?}",
            report.warnings
        );
        assert!(report.problems.is_empty(), "{:?}", report.problems);
        let _ = fs::remove_dir_all(&dir);
    }
}
