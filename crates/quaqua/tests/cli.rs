//! CLI smoke tests for the built binary: `--version`/`--help`, and the
//! `quaqua doctor` checks against a synthetic PATH and shell.
//! `CARGO_BIN_EXE_quaqua` is the path cargo builds for this crate's binary, so
//! the test never hard-codes a machine-specific path.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

#[test]
fn version_and_help_are_available() {
    let bin = env!("CARGO_BIN_EXE_quaqua");

    let version = Command::new(bin)
        .arg("--version")
        .output()
        .expect("failed to spawn the quaqua binary");
    assert!(version.status.success(), "--version exited non-zero");
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        format!("quaqua {}", env!("CARGO_PKG_VERSION")),
    );

    let help = Command::new(bin)
        .arg("--help")
        .output()
        .expect("failed to spawn the quaqua binary");
    assert!(help.status.success(), "--help exited non-zero");
    let help_text = String::from_utf8_lossy(&help.stdout);
    assert!(
        help_text.contains("Usage: quaqua"),
        "--help does not print a usage line: {help_text}",
    );
}

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("quaqua-cli-{tag}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("temp dir");
    dir
}

fn stub_shell(dir: &Path, output: &str) -> PathBuf {
    let path = dir.join("shell");
    fs::write(&path, format!("#!/bin/sh\nprintf '%s\\n' '{output}'\n")).expect("write shell");
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).expect("chmod shell");
    path
}

/// Run `quaqua doctor` with an isolated PATH/HOME/checkout and the given shell
/// stub. `path_dir` must already contain (or omit) `quaqua`.
fn doctor(path_dir: &Path, home: &Path, cwd: &Path, shell: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_quaqua"))
        .arg("doctor")
        .env("PATH", path_dir)
        .env("HOME", home)
        .env("QUAQUA_SHELL", shell)
        .env_remove("QUAQUA_CHECKOUT")
        // `workspace_root()` prefers these over `$HOME`; the caller's env must
        // not decide which workspace doctor inspects.
        .env_remove("QUAQUA_HOME")
        .env_remove("QUAQUA_ROOT")
        .current_dir(cwd)
        .output()
        .expect("failed to spawn quaqua doctor")
}

#[test]
fn doctor_fails_when_quaqua_is_missing_from_path() {
    let dir = temp_dir("missing");
    let path_dir = dir.join("bin");
    let home = dir.join("home");
    let cwd = dir.join("cwd");
    for d in [&path_dir, &home, &cwd] {
        fs::create_dir_all(d).expect("dir");
    }
    let shell = stub_shell(&dir, "quaqua not found");

    let out = doctor(&path_dir, &home, &cwd, &shell);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("not on PATH"), "{stderr}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn doctor_fails_when_a_shell_function_shadows_quaqua() {
    let dir = temp_dir("shadow");
    let path_dir = dir.join("bin");
    let home = dir.join("home");
    let cwd = dir.join("cwd");
    let checkout = dir.join("checkout");
    for d in [&path_dir, &home, &cwd] {
        fs::create_dir_all(d).expect("dir");
    }
    fs::create_dir_all(checkout.join("crates/quaqua/src")).expect("checkout");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_quaqua"), path_dir.join("quaqua"))
        .expect("symlink quaqua");
    let shell = stub_shell(&dir, "quaqua is a shell function");

    let out = Command::new(env!("CARGO_BIN_EXE_quaqua"))
        .arg("doctor")
        .env("PATH", &path_dir)
        .env("HOME", &home)
        .env("QUAQUA_SHELL", &shell)
        .env("QUAQUA_CHECKOUT", &checkout)
        .env_remove("QUAQUA_HOME")
        .env_remove("QUAQUA_ROOT")
        .current_dir(&cwd)
        .output()
        .expect("spawn doctor");
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("function/alias"), "{stderr}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn doctor_accepts_a_healthy_setup() {
    let dir = temp_dir("healthy");
    let path_dir = dir.join("bin");
    let home = dir.join("home");
    let cwd = dir.join("cwd");
    let checkout = dir.join("checkout");
    for d in [&path_dir, &home, &cwd] {
        fs::create_dir_all(d).expect("dir");
    }
    fs::create_dir_all(checkout.join("crates/quaqua/src")).expect("checkout");
    let installed = path_dir.join("quaqua");
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_quaqua"), &installed).expect("symlink");
    let shell = stub_shell(&dir, "quaqua is /tmp/bin/quaqua");

    let out = Command::new(env!("CARGO_BIN_EXE_quaqua"))
        .arg("doctor")
        .env("PATH", &path_dir)
        .env("HOME", &home)
        .env("QUAQUA_SHELL", &shell)
        .env("QUAQUA_CHECKOUT", &checkout)
        .env_remove("QUAQUA_HOME")
        .env_remove("QUAQUA_ROOT")
        .current_dir(&cwd)
        .output()
        .expect("spawn doctor");
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("ok:"), "{stdout}");
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn doctor_fails_when_the_binary_is_older_than_the_checkout() {
    let dir = temp_dir("stale");
    let path_dir = dir.join("bin");
    let home = dir.join("home");
    let cwd = dir.join("cwd");
    let checkout = dir.join("checkout");
    for d in [&path_dir, &home, &cwd] {
        fs::create_dir_all(d).expect("dir");
    }
    let src = checkout.join("crates/quaqua/src");
    fs::create_dir_all(&src).expect("checkout");
    let installed = path_dir.join("quaqua");
    fs::copy(env!("CARGO_BIN_EXE_quaqua"), &installed).expect("copy quaqua");
    let f = fs::OpenOptions::new()
        .write(true)
        .open(&installed)
        .expect("open");
    f.set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000))
        .expect("set mtime");
    fs::write(src.join("changed.rs"), "fn changed() {}").expect("new source");
    let shell = stub_shell(&dir, "quaqua is /tmp/bin/quaqua");

    let out = Command::new(env!("CARGO_BIN_EXE_quaqua"))
        .arg("doctor")
        .env("PATH", &path_dir)
        .env("HOME", &home)
        .env("QUAQUA_SHELL", &shell)
        .env("QUAQUA_CHECKOUT", &checkout)
        .env_remove("QUAQUA_HOME")
        .env_remove("QUAQUA_ROOT")
        .current_dir(&cwd)
        .output()
        .expect("spawn doctor");
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("older than the checkout"), "{stderr}");
    let _ = fs::remove_dir_all(&dir);
}

/// `quaqua worker spawn` with `<kind>` or `<repo-path>` missing must name the
/// missing argument, not only print the usage block.
#[test]
fn worker_spawn_names_the_missing_argument() {
    let dir = temp_dir("spawnargs");
    let repo = dir.join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    let bin = env!("CARGO_BIN_EXE_quaqua");

    // kind omitted: `worker spawn <name> <repo>`
    let kind_omitted = Command::new(bin)
        .args(["worker", "spawn", "sude-task"])
        .arg(&repo)
        .current_dir(&dir)
        .output()
        .expect("spawn worker spawn");
    assert_eq!(kind_omitted.status.code(), Some(1), "{kind_omitted:?}");
    let stderr = String::from_utf8_lossy(&kind_omitted.stderr);
    assert!(stderr.contains("missing <kind>"), "kind omitted: {stderr}");

    // repo omitted: `worker spawn <name> <kind>`
    let repo_omitted = Command::new(bin)
        .args(["worker", "spawn", "sude-task", "sude"])
        .current_dir(&dir)
        .output()
        .expect("spawn worker spawn");
    assert_eq!(repo_omitted.status.code(), Some(1), "{repo_omitted:?}");
    let stderr = String::from_utf8_lossy(&repo_omitted.stderr);
    assert!(
        stderr.contains("missing <repo-path>"),
        "repo omitted: {stderr}"
    );

    let _ = fs::remove_dir_all(&dir);
}

/// `quaqua worker prompt --help` documents the exit-code vocabulary a caller
/// branches on: one line per code, with its meaning.
#[test]
fn worker_prompt_help_lists_the_exit_codes() {
    let out = Command::new(env!("CARGO_BIN_EXE_quaqua"))
        .args(["worker", "prompt", "--help"])
        .output()
        .expect("spawn worker prompt --help");
    assert!(out.status.success(), "{out:?}");
    let help = String::from_utf8_lossy(&out.stdout);
    for (code, meaning) in [
        ("0", "report printed"),
        ("1", "usage error"),
        ("2", "hard cap"),
        ("3", "stall"),
        ("4", "time budget"),
        ("5", "blocked"),
        ("6", "malformed report"),
        ("7", "provider error"),
    ] {
        let listed = help.lines().any(|line| {
            let body = line.trim_start_matches('#').trim_start();
            body.starts_with(code) && body.contains(meaning)
        });
        assert!(
            listed,
            "exit {code} ({meaning}) is missing from --help: {help}"
        );
    }
}
