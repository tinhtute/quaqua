//! `quaqua doctor` end-to-end: missing on PATH, a shell function/alias
//! shadowing it, a stale binary, and a healthy setup (issue #55 item 3).

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

struct Fixture {
    root: PathBuf,
    bin_dir: PathBuf,
    home: PathBuf,
    cwd: PathBuf,
    checkout: PathBuf,
    shell: PathBuf,
}

impl Fixture {
    fn new(tag: &str, shell_line: &str) -> Self {
        static CTR: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = CTR.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "quaqua-doctor-cli-{tag}-{}-{n}-{nanos}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        let f = Fixture {
            bin_dir: root.join("bin"),
            home: root.join("home"),
            cwd: root.join("cwd"),
            checkout: root.join("checkout"),
            shell: root.join("shell"),
            root,
        };
        for d in [&f.root, &f.bin_dir, &f.home, &f.cwd] {
            fs::create_dir_all(d).unwrap();
        }
        fs::create_dir_all(f.checkout.join("crates/quaqua/src")).unwrap();
        fs::write(
            &f.shell,
            format!("#!/bin/sh\nprintf '%s\\n' '{shell_line}'\n"),
        )
        .unwrap();
        fs::set_permissions(&f.shell, fs::Permissions::from_mode(0o755)).unwrap();
        f.install_systemd_session();
        f
    }

    /// Model the systemd user session a healthy Linux workstation has: the
    /// auditor units installed under `$HOME/.config/systemd/user` and a
    /// `systemctl` stub on the fixture PATH that reports them `enabled` and
    /// the timer `active`. `doctor` checks both (issue #198), and the check is
    /// hermetic here because the fixture owns `$HOME` and the whole `PATH`.
    fn install_systemd_session(&self) {
        let user_dir = self.home.join(".config/systemd/user");
        fs::create_dir_all(&user_dir).unwrap();
        for unit in ["quaqua-auditor.service", "quaqua-auditor.timer"] {
            fs::write(user_dir.join(unit), "[Unit]\nDescription=fixture auditor\n").unwrap();
        }
        self.install_systemctl("active");
    }

    /// `systemctl --user …` stub: `list-unit-files` reports both auditor units
    /// enabled, `is-active` reports `timer_state` (and only `active` exits 0,
    /// like the real one).
    fn install_systemctl(&self, timer_state: &str) {
        let path = self.bin_dir.join("systemctl");
        fs::write(
            &path,
            format!(
                "#!/bin/sh\n\
                 case \"$*\" in\n\
                 *is-active*) printf '%s\\n' '{timer_state}'; [ '{timer_state}' = active ] ;;\n\
                 *list-unit-files*) printf 'quaqua-auditor.service enabled enabled\\nquaqua-auditor.timer enabled enabled\\n' ;;\n\
                 *) exit 1 ;;\n\
                 esac\n"
            ),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// Link the built binary into the fixture's PATH: doctor only reads the
    /// installed path's metadata (`find_on_path`, `stale_against`), while a
    /// private copy costs the binary's full size per test — six of them
    /// exhausted the CI sandbox's scratch quota (EDQUOT from `fs::copy`).
    /// `tests/cli.rs` links the binary for the same cases.
    fn install_binary(&self) {
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_quaqua"), self.bin_dir.join("quaqua"))
            .expect("link quaqua into the fixture bin dir");
    }

    fn run(&self, path: &Path) -> Output {
        self.run_env(path, &[])
    }

    /// [`Fixture::run`] with extra environment the case is about.
    fn run_env(&self, path: &Path, env: &[(&str, &str)]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_quaqua"));
        cmd.arg("doctor")
            .env("PATH", path)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env_remove("HERDR_CONFIG_DIR")
            .env_remove("HERDR_CONFIG_PATH")
            .env("QUAQUA_CHECKOUT", &self.checkout)
            .env("QUAQUA_SHELL", &self.shell)
            // `workspace_root()` prefers these over `$HOME`; a caller that has
            // them set must not redirect the fixture's workspace.
            .env_remove("QUAQUA_HOME")
            .env_remove("QUAQUA_ROOT")
            .current_dir(&self.cwd);
        for (key, value) in env {
            cmd.env(key, value);
        }
        cmd.output().expect("run quaqua doctor")
    }

    /// [`Fixture::run`] with no `QUAQUA_CHECKOUT` and an empty `cwd`: nothing
    /// in the environment can be mistaken for the engine checkout.
    fn run_without_checkout(&self, path: &Path) -> Output {
        Command::new(env!("CARGO_BIN_EXE_quaqua"))
            .arg("doctor")
            .env("PATH", path)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env_remove("HERDR_CONFIG_DIR")
            .env_remove("HERDR_CONFIG_PATH")
            .env("QUAQUA_SHELL", &self.shell)
            .env_remove("QUAQUA_CHECKOUT")
            .env_remove("QUAQUA_HOME")
            .env_remove("QUAQUA_ROOT")
            .current_dir(&self.cwd)
            .output()
            .expect("run quaqua doctor")
    }

    /// `doctor --workspace` asks `herdr agent list` for pane identity, and
    /// `herdr::bin_path()` falls back to `herdr` on PATH, so an inherited
    /// PATH/`HERDR_SOCKET_PATH`/`HERDR_BIN_PATH` would read the operator's live
    /// daemon and make the asserted stderr depend on live pane tokens. Pin PATH
    /// to the fixture bin dir (no `herdr` there) and drop both herdr overrides.
    fn run_workspace_flag(&self) -> Output {
        Command::new(env!("CARGO_BIN_EXE_quaqua"))
            .args(["doctor", "--workspace"])
            .env("PATH", &self.bin_dir)
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env_remove("HERDR_CONFIG_DIR")
            .env_remove("HERDR_CONFIG_PATH")
            .env_remove("HERDR_BIN_PATH")
            .env_remove("HERDR_SOCKET_PATH")
            .env("QUAQUA_CHECKOUT", &self.checkout)
            .env_remove("QUAQUA_HOME")
            .env_remove("QUAQUA_ROOT")
            .current_dir(&self.cwd)
            .output()
            .expect("run quaqua doctor --workspace")
    }

    fn run_workspace_flag_no_xdg(&self) -> Output {
        Command::new(env!("CARGO_BIN_EXE_quaqua"))
            .args(["doctor", "--workspace"])
            .env("PATH", &self.bin_dir)
            .env("HOME", &self.home)
            .env_remove("XDG_CONFIG_HOME")
            .env_remove("HERDR_CONFIG_DIR")
            .env_remove("HERDR_CONFIG_PATH")
            .env_remove("HERDR_BIN_PATH")
            .env_remove("HERDR_SOCKET_PATH")
            .env("QUAQUA_CHECKOUT", &self.checkout)
            .env_remove("QUAQUA_HOME")
            .env_remove("QUAQUA_ROOT")
            .current_dir(&self.cwd)
            .output()
            .expect("run quaqua doctor --workspace (no xdg)")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

#[test]
fn missing_binary_exits_nonzero() {
    let f = Fixture::new("missing", "quaqua not found");
    let empty = f.root.join("empty-path");
    fs::create_dir_all(&empty).unwrap();
    let out = f.run(&empty);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not on PATH"), "{stderr}");
}

#[test]
fn shadowed_by_a_shell_function_exits_nonzero() {
    let f = Fixture::new("shadow", "quaqua is a shell function");
    f.install_binary();
    let out = f.run(&f.bin_dir);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("function/alias"), "{stderr}");
}

#[test]
fn a_stale_binary_exits_nonzero() {
    let f = Fixture::new("stale", "quaqua is /tmp/bin/quaqua");
    f.install_binary();
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(
        f.checkout.join("crates/quaqua/src/newer.rs"),
        "fn newer() {}",
    )
    .unwrap();
    let out = f.run(&f.bin_dir);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("older than the checkout"), "{stderr}");
}

#[test]
fn a_healthy_setup_exits_zero() {
    let f = Fixture::new("healthy", "quaqua is /tmp/bin/quaqua");
    f.install_binary();
    let out = f.run(&f.bin_dir);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("ok:"), "{stdout}");
}

#[test]
fn a_missing_checkout_says_the_stale_check_was_skipped() {
    let f = Fixture::new("no-checkout", "quaqua is /tmp/bin/quaqua");
    f.install_binary();

    // With a checkout the staleness claim is a real comparison.
    let out = f.run(&f.bin_dir);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("not older than the checkout"), "{stdout}");

    // Without one, the run must not claim that comparison...
    let out = f.run_without_checkout(&f.bin_dir);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(!stdout.contains("not older than the checkout"), "{stdout}");
    // ...and must say the check was skipped and how to enable it.
    assert!(stdout.contains("skipped"), "{stdout}");
    assert!(stdout.contains("QUAQUA_CHECKOUT"), "{stdout}");
}

#[test]
fn a_workspace_that_is_also_an_engine_checkout_warns_and_still_exits_zero() {
    let f = Fixture::new("overlap", "quaqua is /tmp/bin/quaqua");
    f.install_binary();
    // `$HOME/quaqua` before the split: the workspace root held the engine
    // sources and the engine `AGENTS.md`, so the persona never loaded.
    let workspace = f.home.join("quaqua");
    fs::create_dir_all(workspace.join("crates/quaqua/src")).unwrap();
    fs::write(workspace.join("Cargo.toml"), "[workspace]\n").unwrap();
    fs::write(workspace.join("AGENTS.md"), "# quaqua engine\n").unwrap();

    let out = f.run(&f.bin_dir);
    // A broken layout warns; it never fails the run.
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("warning:"), "{stderr}");
    assert!(stderr.contains("Cargo.toml"), "{stderr}");
    assert!(stderr.contains("@identity.md"), "{stderr}");
}

#[test]
fn a_split_workspace_is_quiet() {
    let f = Fixture::new("split", "quaqua is /tmp/bin/quaqua");
    f.install_binary();
    let workspace = f.home.join("quaqua");
    fs::create_dir_all(workspace.join("projects")).unwrap();
    fs::write(workspace.join("projects/INDEX.md"), "").unwrap();
    fs::write(workspace.join("AGENTS.md"), "@identity.md\n@SOUL.md\n").unwrap();

    let out = f.run(&f.bin_dir);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("warning:"), "{stderr}");
}

// `doctor` fails on missing or inactive auditor systemd units on Linux only
// (issue #198); macOS reports manual activation instead, so these three cases
// can only be exercised where the check runs.
#[cfg(target_os = "linux")]
#[test]
fn a_missing_auditor_unit_fails_the_run() {
    let f = Fixture::new("no-units", "quaqua is /tmp/bin/quaqua");
    f.install_binary();
    fs::remove_dir_all(f.home.join(".config/systemd/user")).unwrap();

    let out = f.run(&f.bin_dir);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("auditor systemd unit is missing"),
        "{stderr}"
    );
    assert!(stderr.contains("install-auditor.sh"), "{stderr}");
}

#[cfg(target_os = "linux")]
#[test]
fn an_inactive_auditor_timer_fails_the_run() {
    let f = Fixture::new("inactive-timer", "quaqua is /tmp/bin/quaqua");
    f.install_binary();
    f.install_systemctl("inactive");

    let out = f.run(&f.bin_dir);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("auditor systemd timer is inactive"),
        "{stderr}"
    );
}

/// `QUAQUA_TEST=1` exists so a suite cannot write live workspace state; the
/// auditor check only reads `$HOME` and runs `systemctl`, so the switch must
/// not silence it and hand a broken workstation a green `doctor`.
#[cfg(target_os = "linux")]
#[test]
fn quaqua_test_does_not_disable_the_auditor_check() {
    let f = Fixture::new("test-env", "quaqua is /tmp/bin/quaqua");
    f.install_binary();
    fs::remove_dir_all(f.home.join(".config/systemd/user")).unwrap();

    let out = f.run_env(&f.bin_dir, &[("QUAQUA_TEST", "1")]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("auditor systemd unit is missing"),
        "{stderr}"
    );
}

/// macOS has no systemd user session, so `doctor` notes manual auditor
/// activation instead of failing on units the workstation can never install —
/// even with the fixture's units removed, the run stays green.
#[cfg(target_os = "macos")]
#[test]
fn macos_notes_manual_auditor_activation_instead_of_failing() {
    let f = Fixture::new("macos-auditor", "quaqua is /tmp/bin/quaqua");
    f.install_binary();
    fs::remove_dir_all(f.home.join(".config/systemd/user")).unwrap();

    let out = f.run(&f.bin_dir);
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("auditor: macOS manual activation required"),
        "{stdout}"
    );
}

#[test]
fn doctor_workspace_drift_reports_problems_and_clean_passes() {
    let f = Fixture::new("ws-drift", "quaqua is /tmp/bin/quaqua");
    let ws = f.home.join("quaqua");
    let ws_rules = ws.join("rules/_all");
    let t_rules = f.checkout.join("templates/workspace/rules/_all");
    let plugin_dir = f.checkout.join("plugins/herdr-quaqua");
    let herdr_config = f.home.join(".config/herdr");

    fs::create_dir_all(&ws_rules).unwrap();
    fs::create_dir_all(&t_rules).unwrap();
    fs::create_dir_all(&plugin_dir).unwrap();
    fs::create_dir_all(&herdr_config).unwrap();
    fs::create_dir_all(ws.join("projects")).unwrap();
    fs::write(ws.join("projects/INDEX.md"), "").unwrap();
    fs::write(ws.join("AGENTS.md"), "@identity.md\n").unwrap();

    // Rule drift
    fs::write(t_rules.join("lead-inbox.md"), "template lead inbox content").unwrap();
    fs::write(ws_rules.join("lead-inbox.md"), "stale workspace lead inbox").unwrap();

    // Manifest with 2 hooks
    let manifest = r#"
id = "herdr-quaqua"
name = "herdr-quaqua"

[[events]]
on = "pane.created"
command = ["quaqua", "grid", "hook"]

[[events]]
on = "pane.agent_status_changed"
command = ["quaqua", "events", "hook"]
"#;
    fs::write(plugin_dir.join("herdr-plugin.toml"), manifest).unwrap();

    // plugins.json with only 1 hook (missing pane.agent_status_changed)
    let plugins_json = r#"[
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
    fs::write(herdr_config.join("plugins.json"), plugins_json).unwrap();

    // 1. Run doctor --workspace -> should fail with exit 1 and report both drifts
    let out = f.run_workspace_flag();
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("rule drift: rules/_all/lead-inbox.md differs from template"),
        "{stderr}"
    );
    assert!(stderr.contains("cp "), "{stderr}");
    assert!(
        stderr.contains("plugin hook drift: herdr plugin 'herdr-quaqua' lacks event hook(s)"),
        "{stderr}"
    );
    assert!(stderr.contains("pane.agent_status_changed"), "{stderr}");
    assert!(stderr.contains("herdr plugin link"), "{stderr}");

    // Also fails when XDG_CONFIG_HOME is unset (fallback to HOME/.config/herdr)
    let out_no_xdg = f.run_workspace_flag_no_xdg();
    assert_eq!(out_no_xdg.status.code(), Some(1), "{out_no_xdg:?}");
    let stderr_no_xdg = String::from_utf8_lossy(&out_no_xdg.stderr);
    assert!(
        stderr_no_xdg
            .contains("plugin hook drift: herdr plugin 'herdr-quaqua' lacks event hook(s)"),
        "{stderr_no_xdg}"
    );

    // 2. Fix both drifts
    fs::write(
        ws_rules.join("lead-inbox.md"),
        "template lead inbox content",
    )
    .unwrap();
    let fixed_json = r#"[
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
      }
    ]
  }
]"#;
    fs::write(herdr_config.join("plugins.json"), fixed_json).unwrap();

    // 3. Run doctor --workspace -> should pass with exit 0 (both with XDG_CONFIG_HOME and HOME fallback)
    let out_clean = f.run_workspace_flag();
    assert_eq!(out_clean.status.code(), Some(0), "{out_clean:?}");
    let stdout_clean = String::from_utf8_lossy(&out_clean.stdout);
    assert!(
        stdout_clean.contains(
            "ok: workspace rules and herdr plugin event hooks are in sync with engine templates"
        ),
        "{stdout_clean}"
    );

    let out_clean_no_xdg = f.run_workspace_flag_no_xdg();
    assert_eq!(
        out_clean_no_xdg.status.code(),
        Some(0),
        "{out_clean_no_xdg:?}"
    );
    let stdout_clean_no_xdg = String::from_utf8_lossy(&out_clean_no_xdg.stdout);
    assert!(
        stdout_clean_no_xdg.contains(
            "ok: workspace rules and herdr plugin event hooks are in sync with engine templates"
        ),
        "{stdout_clean_no_xdg}"
    );
}
