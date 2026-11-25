# Design: the farm daemon under launchd (macOS)

Issues: #369 (a launchd LaunchAgent, so `quaqua up` can start the daemon on macOS) and #363 (three `daemon::service` tests fail on macOS). Base: c4ca28c.

Baseline on macOS: `cargo test --locked -p qq-core --lib daemon::service` gives 13 passed, 3 failed (`dry_run_prints_the_unit_and_writes_nothing`, `install_writes_the_unit_and_prints_the_enable_command`, `install_names_legacy_auditor_units_and_keeps_them`). All three fail on the `!cfg!(target_os = "linux")` refusal at `crates/qq-core/src/daemon/service.rs:188`.

## Decision summary

- `daemon/service.rs` gets a runtime `Platform` value (`Systemd` or `Launchd`). Both backends are plain code in the same module, compiled and unit-tested on every OS. There is exactly one `cfg!` (a runtime bool, not `#[cfg]`), in `Platform::current()`.
- Every `systemctl` and `launchctl` call goes through a `Runner` trait. Tests inject a recording fake. The real `ProcessRunner` is built only by `ServiceHost::new`, which only `up_cmd` calls.
- On macOS, `daemon install` writes `<launchd_dir>/<label>.plist` and prints the `launchctl bootstrap` command. Like the systemd path, it never loads or starts anything.
- `quaqua up` starts the daemon on either OS through `Service::start`. On launchd that runs `print` first, then `kickstart` if the label is loaded or `bootstrap` if it is not. The engine never calls `bootout`.
- The macOS branch of `up/daemon.rs` and `MACOS_ISSUE` are removed. A target with no backend still gets `Ready::Unsupported`. The refusal is tested on every OS by passing `None`, so no test needs `#[cfg(target_os)]`.

## Decided unknowns

1. **Does `bootstrap` on an already-loaded label error?**
   - `man launchctl` does not say. Its only exit-status rule is "0 if the subcommand succeeded", otherwise a code for `launchctl error`. I did not run `bootstrap` to find out (forbidden here), so no error code is claimed.
   - The design does not depend on the answer. `start` asks `launchctl print gui/<uid>/<label>` first:
     - Exit 0 means loaded, so it runs `kickstart` without `-k`. The man page says `-k` kills a running instance, and a live daemon must never be killed.
     - Any other exit means not loaded, so it runs `bootstrap gui/<uid> <plist>`.
   - If a race loads the label between the two calls, `bootstrap` fails and its stderr is returned verbatim.
   - There is no `bootout` in the engine: it would kill a running daemon, and `up` only starts one after ping says "unreachable".
   - Source: `man launchctl` (bootstrap, kickstart `-k`, EXIT STATUS).
2. **Status check.**
   - Only the exit status of `launchctl print` counts, never its text. The man page says print output "is NOT API in any sense at all".
   - Measured on this Mac, read-only: an unknown label exits 113 (`launchctl error 113`: "Could not find specified service"). A loaded label (`com.apple.Finder`) exits 0.
   - Liveness stays with `quaqua ping`.
   - Source: `man launchctl` (print); the local probe.
3. **Legacy auditor detection on launchd.**
   - The old installer wrote `$HOME/Library/LaunchAgents/com.quaqua.auditor.plist` and printed `launchctl bootstrap gui/<uid> <plist>`.
   - The launchd legacy rule: a file in the agents dir whose name starts with `com.quaqua.auditor` and ends with `.plist`. It is reported and never deleted, like the systemd rule for `quaqua-auditor*.{timer,service}`.
   - Source: `git show 9aaa291^:templates/workspace/scripts/auditor/install-auditor.sh`, lines 37-38 and 59.
4. **Does the plist need PATH?**
   - Yes. The systemd unit carries the installer's PATH (`render_unit`, `service.rs:59`), because "a systemd user unit gets a bare PATH" (`service.rs:226`).
   - launchd is the same. Its default is `_PATH_STDPATH` = `/usr/bin:/bin:/usr/sbin:/sbin`, which has no herdr, Homebrew git or claude. The old auditor plist set PATH too.
   - Source: `service.rs:59,226`; `man launchd.plist` (Program); `paths.h:67`; the old `com.quaqua.auditor.plist.in`.
5. **How do tests tell the launchd backend from the real install path?**
   - Tests never read process state. They build a `Service` with `Service::resolve(Some(Platform::Launchd), <scratch home>, None, &cfg, 501)`, so the plist lands under the scratch dir and the uid is fixed.
   - They call `install_at`, which runs no command, and `Service::start` / `Service::installed` with a `FakeRunner`.
   - Only `Service::for_process` reads HOME, `XDG_CONFIG_HOME`, `getuid()` and the store. Only `install()` and `ServiceHost::new` call it.
   - Review check: `rg -n 'ProcessRunner|for_process|service::install\(' crates/qq-core` has no hit inside a `mod tests` or under `tests/`.
6. **KeepAlive form.**
   - `KeepAlive = { SuccessfulExit = false }` mirrors `Restart=on-failure`.
   - A clean `quaqua stop` makes `serve()` return `Ok`, and the CLI then exits 0 (`cli/mod.rs:247-251`, `main.rs:2`), so launchd leaves the job stopped. A crash or an `Err` exits non-zero and is restarted.
   - `ThrottleInterval = 5` mirrors `RestartSec=5`; launchd's default is 10 s.
   - Source: `man launchd.plist` (KeepAlive, SuccessfulExit, ThrottleInterval).

## Seam: `crates/qq-core/src/daemon/service.rs`

These signatures are pinned: the other builders code against them.

```rust
/// Which service manager runs the daemon. A runtime value: both are compiled everywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform { Systemd, Launchd }

impl Platform {
    /// The only cfg in the module: Linux -> Systemd, macOS -> Launchd, else None.
    pub fn current() -> Option<Platform> {
        if cfg!(target_os = "linux") { Some(Platform::Systemd) }
        else if cfg!(target_os = "macos") { Some(Platform::Launchd) }
        else { None }
    }
}

/// The refusal on a target with no backend (replaces "supports systemd on Linux only").
pub const NO_BACKEND: &str = "daemon install supports systemd (Linux) and launchd (macOS) only";
/// The launchd label when daemon.launchd_label is unset or unusable.
pub const LAUNCHD_LABEL: &str = "com.quaqua.daemon";
/// The agents dir under home when daemon.launchd_dir is unset or unusable.
pub const LAUNCHD_DIR: &str = "Library/LaunchAgents";
/// The launchd job's stdout and stderr, in the runs dir (systemd uses the journal).
pub const DAEMON_LOG: &str = "daemon.log";
const LEGACY_SYSTEMD: &str = "quaqua-auditor";      // + .timer | .service
const LEGACY_LAUNCHD: &str = "com.quaqua.auditor";  // + .plist

/// The resolved service: everything except the unit text.
/// resolve() builds it in tests; for_process() builds it for real.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Service {
    pub platform: Platform,
    pub name: String,   // the systemd UNIT_NAME, or the launchd label
    pub file: PathBuf,  // the unit file, or the plist
    pub uid: u32,       // the launchd domain is gui/<uid>; systemd ignores it
}

/// What the unit or plist runs. The env values come from the installing process, as today.
pub struct Spec<'a> {
    pub exe: &'a Path,
    pub workspace: &'a Path,  // QUAQUA_HOME
    pub path: &'a str,        // PATH
    pub herdr: &'a HerdrEnv,  // HERDR_SESSION, HERDR_SOCKET_PATH
    pub log: &'a Path,        // <runs_dir>/DAEMON_LOG, launchd only
}

/// One finished command. code is None when a signal killed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ran { pub code: Option<i32>, pub stdout: String, pub stderr: String }
impl Ran { pub fn ok(&self) -> bool { self.code == Some(0) } }

/// Every systemctl and launchctl call goes through this. Tests implement it and record the calls.
pub trait Runner {
    fn run(&mut self, program: &str, args: &[String]) -> std::io::Result<Ran>;
}
/// std::process::Command. Only ServiceHost::new constructs it.
pub struct ProcessRunner;
impl Runner for ProcessRunner { /* Command::new(program).args(args).output() */ }

/// The strings up/daemon.rs shows: the unit or label, and the command a human runs to inspect it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Names { pub unit: String, pub status: String }

impl Service {
    /// None -> Err(NO_BACKEND).
    /// Systemd: name = UNIT_NAME, file = unit_path(home, xdg_config).
    /// Launchd: name = the validated label, file = <validated dir>/<label>.plist (rules under Config keys).
    pub fn resolve(platform: Option<Platform>, home: &Path, xdg_config: Option<&Path>,
                   cfg: &crate::config::LaunchdConfig, uid: u32) -> Result<Service, String>;
    /// Reads HOME (Err "HOME is not set"), an absolute XDG_CONFIG_HOME, getuid(),
    /// config::launchd_config_in(runs_dir) and Platform::current(), then calls resolve.
    pub fn for_process(runs_dir: &Path) -> Result<Service, String>;
    pub fn render(&self, spec: &Spec) -> String;  // render_unit, or render_plist(&self.name, spec)
    pub fn enable_hint(&self) -> String;          // see "Install output"
    pub fn names(&self) -> Names;
    /// Systemd: systemctl show LoadState not in {"", "not-found"}.
    /// Launchd: self.file exists, and no command runs.
    pub fn installed(&self, r: &mut dyn Runner) -> bool;
    /// An Err names the failing command in backticks, then its stderr
    /// (or "exited <code>" when stderr is empty, or the io error).
    pub fn start(&self, r: &mut dyn Runner) -> Result<(), String>;
    /// Legacy auditor files in self.file's directory, sorted.
    pub fn legacy_auditor(&self) -> Vec<PathBuf>;
}

pub fn render_unit(exe: &Path, home: &Path, path: &str, herdr: &HerdrEnv) -> String;  // unchanged
pub fn render_plist(label: &str, spec: &Spec) -> String;
pub fn install_at(svc: &Service, args: &[String], spec: &Spec) -> Result<String, String>;
pub fn install(runs_dir: &Path, args: &[String]) -> Result<String, String>;
```

Rules:

- `install_at`:
  - It parses the args first; an unknown flag fails as it does today. Then it renders.
  - With `--dry-run` it returns the rendered text and writes nothing.
  - Otherwise it runs `create_dir_all` on the file's parent and writes the file. On launchd it then sets mode `0o644` through `std::os::unix::fs::PermissionsExt`. `man launchctl` (load) says configuration files "must disallow group and world writes", and a umask of 002 would break that.
  - It returns the enable hint, then one `duplicate:` line per legacy file.
  - It takes no `Runner` and runs no command.
- `install` builds the `Spec` as it does today: exe from `current_exe`, workspace from the parent of the runs dir, PATH from the process with the same fallback, and herdr from the process env. It adds log = `runs_dir.join(DAEMON_LOG)`. Then it returns `install_at(&Service::for_process(runs_dir)?, args, &spec)`.
- The uid comes from `extern "C" { fn getuid() -> u32; }` in `service.rs`, the same pattern as `db/path.rs:74`. Do not add a libc dependency.
- The inhibitor thread (`spawn`, `systemd-inhibit`) stays Linux-only. A macOS sleep guard (`caffeinate`) is a non-goal here.

### Install output

- Systemd output is unchanged: `wrote <path>`, then `enable it with: systemctl --user enable --now quaqua-daemon.service`.
- Launchd output:
  - `wrote <plist>`
  - `load it with: launchctl bootstrap gui/<uid> <plist>`
  - `(if it is already loaded, run launchctl bootout gui/<uid>/<label> first)`
- A legacy plist on launchd adds this line: `duplicate: <path> is a legacy auditor agent; the daemon now runs the auditor. Unload it with launchctl bootout gui/<uid>/<file stem> and remove it yourself (not deleted)`.

### Exact argv per call (what the fake runner must record)

| Call | Backend | argv, program first | Result |
|---|---|---|---|
| `installed` | systemd | `systemctl --user show --property=LoadState --value quaqua-daemon.service` | True when the call succeeds and the trimmed stdout is not `""` or `not-found`. False otherwise, including an io error. |
| `installed` | launchd | none | `self.file.exists()` |
| `start` | systemd | `systemctl --user start quaqua-daemon.service` | Ok when the call succeeds |
| `start` step 1 | launchd | `launchctl print gui/<uid>/<label>` | Exit 0 goes to step 2a. Any other exit goes to step 2b. An io error (no launchctl) returns Err. |
| `start` step 2a | launchd | `launchctl kickstart gui/<uid>/<label>` | Ok when the call succeeds |
| `start` step 2b | launchd | `launchctl bootstrap gui/<uid> <file>` | Ok when the call succeeds. RunAtLoad starts the job, so no `kickstart` follows. |

The domain is `gui/<uid>`, as in `man launchctl` (domain targets) and the #369 wording. See Risks.

The `Names` values:
- Systemd: `{ unit: "quaqua-daemon.service", status: "systemctl --user status quaqua-daemon.service" }`
- Launchd: `{ unit: <label>, status: "launchctl print gui/<uid>/<label>" }`

## Plist (exact text)

`render_plist(label, spec)` returns the text below, indented with two spaces and ending in a newline. Every `{…}` value goes through `xml_escape`, the plist counterpart of `quote()` for the unit. It replaces `&` first, then `<`, `>`, `"` and `'`, with `&amp;`, `&lt;`, `&gt;`, `&quot;` and `&apos;`.

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>{label}</string>
  <key>ProgramArguments</key>
  <array>
    <string>{exe}</string>
    <string>daemon</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>QUAQUA_HOME</key>
    <string>{workspace}</string>
    <key>PATH</key>
    <string>{path}</string>
    <key>HERDR_SESSION</key>
    <string>{herdr.session}</string>
    <key>HERDR_SOCKET_PATH</key>
    <string>{herdr.socket}</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <dict>
    <key>SuccessfulExit</key>
    <false/>
  </dict>
  <key>ThrottleInterval</key>
  <integer>5</integer>
  <key>StandardOutPath</key>
  <string>{log}</string>
  <key>StandardErrorPath</key>
  <string>{log}</string>
</dict>
</plist>
```

- The plist sets the same four env vars as `render_unit`.
- `RunAtLoad` is set explicitly even though `SuccessfulExit` implies it, so the file says what it means. A plist in `~/Library/LaunchAgents` is loaded at login, which plays the role of `WantedBy=default.target`.
- There is no `WorkingDirectory`: the root does not depend on the cwd (README:197), and the systemd unit sets none.

## Config keys

A new flat section, `daemon`, is read the same way as `notify.daemon_start_sec` and `ui.cos_label`:

| Key | Default | Rule |
|---|---|---|
| `daemon.launchd_label` | `com.quaqua.daemon` (`LAUNCHD_LABEL`) | The value is trimmed. A blank value, or one containing `/` or whitespace, falls back to the default, because the label is both a file name and a service-target. |
| `daemon.launchd_dir` | `<home>/Library/LaunchAgents` | An absolute path is used as is. `~/…` is joined to the injected home. A blank or relative value falls back to the default. |

```rust
// crates/qq-core/src/config/mod.rs
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LaunchdConfig { pub label: Option<String>, pub dir: Option<String> }  // raw, unvalidated
pub fn launchd_config(conn: &Connection) -> LaunchdConfig;   // setting("daemon.launchd_label" / "daemon.launchd_dir")
pub fn launchd_config_in(runs_dir: &Path) -> LaunchdConfig;  // read-only open as in cos_label_in; no db -> Default
```

Validation and defaults live in `Service::resolve`, which has home, so the config module stays raw. Wiring:

- Add `"daemon"` to `SETTING_SECTIONS` (`config/mod.rs:66`). That covers the top-level key check (`:325`), seeding (`:412`) and export (`export.rs:140`).
- Add this section to `crates/qq-core/templates/workspace/team.toml`:

```toml
[daemon]
# The launchd agent (macOS) that `quaqua daemon install` writes. systemd ignores these.
launchd_label = "com.quaqua.daemon"
launchd_dir = "~/Library/LaunchAgents"
```

Scope: #369 says every value (label, paths, env) comes from config.
- The label and paths do.
- The env values (QUAQUA_HOME, PATH, HERDR_*) still come from the installing process, as the systemd path does today (`install()`, `HerdrEnv::from_process`). They describe the live herdr session, not a preference.
- The systemd unit name and dir stay as they are (`UNIT_NAME`, XDG). Making them config keys is out of scope.

## Caller: `crates/qq-core/src/up/daemon.rs`

- Delete `enum Platform`, `MACOS_ISSUE` and the macOS branch at `:113-118`.
- Replace `DaemonHost::platform()` with `fn service(&self) -> Result<Names, String>`. `Err(why)` means nothing can be started here, for example `NO_BACKEND` or "HOME is not set". `up::daemon` re-exports the type with `pub use crate::daemon::service::Names;`.
- `ensure_answers` still pings first. The backend check moves to after `Ping::Unreachable`, because a daemon that already answers is fine on any OS:

```rust
let names = match host.service() {
    Ok(n) => n,
    Err(why) => {
        host.line(&format!("quaqua up: cannot start the daemon here ({why}); start it yourself, then run up again"));
        return Ok(Ready::Unsupported);
    }
};
```

- Every `UNIT_NAME` in a message becomes `names.unit`:
  - The start error becomes `format!("starting {} failed: {e}", names.unit)`. `e` already names the exact command.
  - The timeout becomes `"{unit} did not answer within {n}s of the start; see `{status}`"`.
  - `ask_install` and `no_unit_error` use `names.unit`.
  - `no_unit_error` and `wait_until_up` take `&Names`.
- `SystemdHost` becomes `ServiceHost { runs_dir, svc: Result<Service, String>, runner: Box<dyn Runner> }`. `ServiceHost::new(runs_dir)` calls `Service::for_process` and boxes a `ProcessRunner`. Its methods map like this:
  - `service()` returns `svc.names()`.
  - `unit_installed` calls `svc.installed(runner)`.
  - `start_unit` calls `svc.start(runner)`.
  - `install_unit` is unchanged (`service::install`).
  - Every `systemctl` helper leaves this file.
- `up/mod.rs:231` becomes `daemon::ServiceHost::new(env.runs_dir.clone())`.
- The help text at `cli/mod.rs:241` becomes "run the farm daemon in the foreground, or 'install' its systemd unit or launchd agent".

Tests in `up/daemon.rs`:

- `Fake` replaces `platform` with `service: Result<Names, String>`, set to the systemd names by default. `macos()` becomes `no_backend()`.
- `up_daemon_macos_says_unsupported_and_starts_nothing` becomes `up_daemon_without_a_backend_says_why_and_starts_nothing`. With pings `[Unreachable]` it expects `Ready::Unsupported`, one line that contains the why, and no starts or installs.
- New: `up_daemon_without_a_backend_still_accepts_a_live_daemon`. A ping that answers gives `Ready::Answers` and prints no line.
- `up_daemon_timeout_names_the_unit` and `up_daemon_start_failure_names_the_unit_and_systemd` keep their assertions, through the systemd names.
- New: `up_daemon_launchd_timeout_names_the_label_and_print`. With the launchd names, the message contains `com.quaqua.daemon` and `launchctl print gui/501/com.quaqua.daemon`.

In `crates/qq-core/tests/up.rs`, `FakeDaemon` drops `platform` and implements `service()` with the systemd names. The import on line 14 swaps `Platform` for `Names`.

## Fake runner and what the tests prove

In `mod tests` of `service.rs`:

```rust
struct FakeRunner { calls: Vec<Vec<String>>, answers: Vec<std::io::Result<Ran>> }  // answers taken from the front
impl Runner for FakeRunner {
    fn run(&mut self, program: &str, args: &[String]) -> std::io::Result<Ran> {
        self.calls.push(std::iter::once(program.to_string()).chain(args.iter().cloned()).collect());
        self.answers.remove(0)
    }
}
fn launchd(home: &Path) -> Service {
    Service::resolve(Some(Platform::Launchd), home, None, &LaunchdConfig::default(), 501).unwrap()
}
```

- `launchd_start_kickstarts_a_loaded_label`: with answers `[ok, ok]`, the calls are exactly `[["launchctl","print","gui/501/com.quaqua.daemon"], ["launchctl","kickstart","gui/501/com.quaqua.daemon"]]`.
- `launchd_start_bootstraps_an_unloaded_plist`: with answers `[exit 113, ok]`, the calls are exactly the print call and `["launchctl","bootstrap","gui/501","<scratch>/Library/LaunchAgents/com.quaqua.daemon.plist"]`. There is no third call.
- `launchd_start_failure_names_the_command_and_stderr`: with answers `[exit 113, exit 5 with stderr "Bootstrap failed: 5: Input/output error"]`, the Err contains `launchctl bootstrap gui/501` and that stderr. The stderr text is fixture data only; the design does not rely on it.
- `launchd_installed_is_the_plist_on_disk`: false before the file is written, true after, and `calls` stays empty.
- `systemd_start_runs_systemctl_start` and `systemd_installed_reads_load_state` cover stdout `loaded`, `masked` and `not-found` plus an io error. This logic moves out of `up/daemon.rs:278-298` and is unit-tested for the first time.
- `platform_current_matches_the_target`: Linux gives `Some(Systemd)`, macOS gives `Some(Launchd)`, anything else gives `None`. The test asserts through `cfg!`.
- `no_backend_refuses`: `Service::resolve(None, …)` returns `Err(NO_BACKEND)`. It runs on every OS and replaces any Linux-only refusal test.
- `plist_runs_daemon_with_env_and_keepalive`, the launchd twin of `unit_runs_daemon_with_home`, checks:
  - Label and ProgramArguments
  - the four env pairs
  - `RunAtLoad` true, `KeepAlive` with `SuccessfulExit` false, and `ThrottleInterval` 5
  - both log paths
- `plist_escapes_xml`: an exe `opt/q&a <x>/quaqua` renders as `opt/q&amp;a &lt;x&gt;/quaqua`.
- `launchd_resolve_reads_label_and_dir` checks the config rules:
  - `~/agents` becomes `<home>/agents` and `/abs` is kept.
  - A relative or blank dir falls back to the default.
  - The label `a/b` falls back to the default, and `com.x.y` is kept.
- In config: `launchd_config_reads_the_daemon_section` uses an in-memory db, like `up_daemon_wait_budget_is_the_daemon_start_reader_and_the_report_poll`. `launchd_config_in_without_a_db_is_default` covers a missing db.

## The #363 tests, split per backend

All six run on every OS with no gating. Each passes its `Platform` explicitly through `resolve(Some(…))`.

| Old test | Systemd twin | Launchd twin |
|---|---|---|
| `dry_run_prints_the_unit_and_writes_nothing` | `systemd_dry_run_prints_the_unit_and_writes_nothing`: the output equals `render_unit` and no `.config` dir appears. | `launchd_dry_run_prints_the_plist_and_writes_nothing`: the output equals `render_plist` and no `Library` dir appears. |
| `install_writes_the_unit_and_prints_the_enable_command` | `systemd_install_writes_the_unit_and_prints_the_enable_command`: the current assertions. | `launchd_install_writes_the_plist_and_prints_the_bootstrap_command`: the file equals `render_plist`; the output has the path and `launchctl bootstrap gui/501 <path>`; the mode satisfies `mode & 0o022 == 0`. |
| `install_names_legacy_auditor_units_and_keeps_them` | `systemd_install_names_legacy_auditor_units_and_keeps_them`: the current fixtures. | `launchd_install_names_legacy_auditor_plists_and_keeps_them`: with `com.quaqua.auditor.plist` and `com.quaqua.backup.plist` present, only the first is named and both remain. |

`install_refuses_an_unknown_flag` keeps its body, using a systemd `Service`.

## File areas per builder

`service.rs` has one owner. The other builders code against the signatures above.

| Builder | Files | Depends on |
|---|---|---|
| B1 config | `crates/qq-core/src/config/mod.rs` (`SETTING_SECTIONS`, `LaunchdConfig`, the two readers, their tests) and `crates/qq-core/templates/workspace/team.toml` | nothing; lands first |
| B2 service | `crates/qq-core/src/daemon/service.rs` (the whole seam, `render_plist`, install, `Runner`, all the tests above including the six #363 twins) and the help line at `crates/qq-core/src/cli/mod.rs:241` | B1's `LaunchdConfig` |
| B3 up caller | `crates/qq-core/src/up/daemon.rs`, `crates/qq-core/src/up/mod.rs:231`, `crates/qq-core/tests/up.rs`, `docs/daemon-service.md` (macOS supported: plist, bootstrap, print) and `CHANGELOG.md` | B2's `Service`, `Names`, `Runner` and `ProcessRunner` |

Targeted tests:

| Builder | Command | Expected |
|---|---|---|
| B1 | `cargo test --locked -p qq-core --lib config` | passes |
| B2 | `cargo test --locked -p qq-core --lib daemon::service` | 0 failed on macOS and on Linux CI |
| B3 | `cargo test --locked -p qq-core --lib up::daemon` and `cargo test --locked -p qq-core --test up` | passes |

No builder runs a real `launchctl bootstrap`, `bootout`, `kickstart` or `enable`, writes under the real HOME, or starts or stops the real farm daemon. Acceptance on a real Mac (install, log out and in, `quaqua ping`) is the owner's step in #369.

## Risks

- **No GUI session:** the `gui/<uid>` domain exists only with a GUI login. Over SSH, `bootstrap` fails and its error is shown verbatim. `user/<uid>` is a possible fallback, left out until a real case needs it.
- **Stale plist:** a plist changed after it was loaded (new PATH or session) takes effect only after a manual bootout and bootstrap; the install output says so. Changing `daemon.launchd_label` leaves the old plist and job in place.
- **Log growth:** `daemon.log` in the runs dir is never rotated.
- **Sleep:** the inhibitor stays off on macOS, so a sleeping Mac pauses the farm. This is the follow-up `docs/daemon-service.md` already names.
