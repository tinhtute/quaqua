//! `quaqua init` tests: the embedded-template contract driven through the built
//! binary. Every test injects its own fixture tree through `QUAQUA_TEMPLATE_DIR`
//! (the documented test-only override — the production path reads the table
//! compiled in by `build.rs`) and points `PATH` at an empty dir, so no real
//! `git`/`herdr` is ever touched unless a test deliberately links `git` in.
//! `HERDR_BIN_PATH` is pinned per invocation, so an ambient one cannot leak.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use quaqua::hash::md5_hex;

#[path = "common/gitfix.rs"]
mod gitfix;

/// Stub `herdr` for `HERDR_BIN_PATH`: logs argv, answers the pane queries as an
/// unregistered `claude` agent in this checkout (the pane the CoS tag is for).
const HERDR_STUB: &str = r#"#!/bin/sh
echo "$*" >> @LOG@
case "$1 $2" in
  "pane current") echo '{"result":{"pane":{"pane_id":"w1:p1"}}}' ;;
  "pane get") echo '{"result":{"pane":{"pane_id":"w1:p1","agent":"claude","cwd":"@CWD@"}}}' ;;
  "agent list") echo '{"result":{"agents":[{"pane_id":"w1:p1","agent":"claude"}]}}' ;;
esac
exit 0
"#;

/// Decoy `herdr` for `PATH`: records that it ran and reports an agent, so a
/// PATH lookup is visible in the audit prompt instead of silently passing.
const HERDR_DECOY: &str = r#"#!/bin/sh
: > "@MARKER@"
echo '{"result":{"agents":[{"name":"sude-decoy","agent_status":"idle","pane_id":"p9"}]}}'
exit 0
"#;

fn write_script(path: &Path, body: &str) {
    std::fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn write_file(path: &Path, body: &str, mode: u32) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
}

fn path_mode(path: &Path) -> u32 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).unwrap().permissions().mode() & 0o777
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        0o644
    }
}

/// One sandbox: a canonical scratch root holding the template fixture, an empty
/// `PATH` dir, and whatever target workspace a test asks for.
struct Sandbox {
    root: PathBuf,
    tpl: PathBuf,
}

impl Sandbox {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("quaqua-init-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        // macOS /tmp is a symlink: compare against the real path everywhere.
        let root = std::fs::canonicalize(&root).unwrap();
        std::fs::create_dir_all(root.join("nobin")).unwrap();
        let sb = Sandbox {
            tpl: root.join("tpl"),
            root,
        };
        sb.tpl_write("workspace/AGENTS.md", "# agents v1\n", 0o644);
        sb.tpl_write("workspace/rules/_all/core.md", "rule v1\n", 0o644);
        sb.tpl_write(
            "workspace/scripts/hooks/herdr-gate.sh",
            "#!/bin/sh\n",
            0o755,
        );
        sb.tpl_write("seed/memory-rules.md", "# rules v1\n", 0o644);
        sb.tpl_write("seed/projects-INDEX.md", "| slug |\n", 0o644);
        sb.tpl_write("seed/projects-_template.md", "# p\n", 0o644);
        sb
    }

    fn tpl_write(&self, rel: &str, body: &str, mode: u32) {
        write_file(&self.tpl.join(rel), body, mode);
    }

    fn tpl_remove(&self, rel: &str) {
        std::fs::remove_file(self.tpl.join(rel)).unwrap();
    }

    fn target(&self, rel: &str) -> PathBuf {
        self.root.join(rel)
    }

    /// A `quaqua` command: fixture template, empty PATH, herdr pinned away, and
    /// no host git config (a global `core.hooksPath` would change the hooks it
    /// installs).
    fn cmd(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_quaqua"));
        cmd.env("QUAQUA_TEMPLATE_DIR", &self.tpl)
            .env("PATH", self.root.join("nobin"))
            .env("HERDR_BIN_PATH", self.root.join("nobin/herdr"))
            .env_remove("HERDR_PANE_ID")
            .env_remove("QUAQUA_HOME")
            .current_dir(&self.root);
        gitfix::isolate(&mut cmd);
        cmd
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd()
            .args(args)
            .output()
            .expect("failed to spawn the quaqua binary")
    }

    fn lock(&self, ws: &Path) -> serde_json::Value {
        let body = std::fs::read_to_string(ws.join(".quaqua/template.lock")).expect("no lock");
        serde_json::from_str(&body).expect("lock is not JSON")
    }
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).to_string()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_default()
}

#[test]
fn init_writes_missing_files_with_modes_and_a_lock() {
    let sb = Sandbox::new("boot");
    let ws = sb.target("ws");

    let out = sb.run(&["init", ws.to_str().unwrap()]);
    assert!(out.status.success(), "init failed: {}", stderr(&out));

    for d in ["memory", "projects", "raw", "runs", "reports"] {
        assert!(ws.join(d).is_dir(), "missing dir {d}/");
    }
    assert_eq!(read(&ws.join("AGENTS.md")), "# agents v1\n");
    assert_eq!(read(&ws.join("rules/_all/core.md")), "rule v1\n");
    assert_eq!(read(&ws.join("memory/rules.md")), "# rules v1\n");
    assert_eq!(read(&ws.join("projects/INDEX.md")), "| slug |\n");
    assert_eq!(read(&ws.join("projects/_template.md")), "# p\n");

    // the exec bit survives the embed
    assert_eq!(
        path_mode(&ws.join("scripts/hooks/herdr-gate.sh")) & 0o111,
        0o111
    );
    assert_eq!(path_mode(&ws.join("AGENTS.md")) & 0o111, 0);

    let lock = sb.lock(&ws);
    let files = lock["files"].as_object().expect("lock.files");
    let agents = &files["AGENTS.md"];
    assert_eq!(agents["md5"], md5_hex(b"# agents v1\n"));
    assert_eq!(agents["mode"], 420);
    assert_eq!(agents["mode"].as_u64(), Some(0o644));
    assert!(files.contains_key("memory/rules.md"), "seed not recorded");
    assert!(
        lock["engine"]
            .as_str()
            .unwrap_or("")
            .starts_with(&format!("{}+", env!("CARGO_PKG_VERSION"))),
        "engine id missing: {lock}",
    );

    let out = stdout(&out);
    assert!(out.contains("workspace ready in"), "no ready line: {out}");
    assert!(
        out.contains("cargo install --path <checkout>/crates/quaqua --locked"),
        "install line missing: {out}",
    );

    let _ = std::fs::remove_dir_all(&sb.root);
}

#[test]
fn init_never_clobbers_an_existing_file() {
    let sb = Sandbox::new("idempotent");
    let ws = sb.target("ws");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("AGENTS.md"), "# mine\n").unwrap();

    let out = sb.run(&["init", ws.to_str().unwrap()]);
    assert!(out.status.success(), "init failed: {}", stderr(&out));
    assert_eq!(read(&ws.join("AGENTS.md")), "# mine\n");

    let out = sb.run(&["init", ws.to_str().unwrap()]);
    assert!(out.status.success(), "re-run failed");

    // the pre-existing divergent file is left out of the lock, so the first
    // --update reports it instead of overwriting it
    let lock = sb.lock(&ws);
    assert!(
        lock["files"].get("AGENTS.md").is_none(),
        "a divergent file was baselined into the lock: {lock}",
    );

    let _ = std::fs::remove_dir_all(&sb.root);
}

#[test]
fn init_defaults_to_the_workspace_root() {
    let sb = Sandbox::new("default-root");
    let home = sb.target("home");

    let out = sb
        .cmd()
        .env("QUAQUA_HOME", &home)
        .arg("init")
        .output()
        .expect("spawn");
    assert!(out.status.success(), "init failed: {}", stderr(&out));
    assert_eq!(read(&home.join("AGENTS.md")), "# agents v1\n");

    let _ = std::fs::remove_dir_all(&sb.root);
}

#[test]
fn init_resolves_a_relative_dir_against_the_cwd() {
    let sb = Sandbox::new("relative");
    let out = sb.run(&["init", "rel/ws"]);
    assert!(out.status.success(), "init failed: {}", stderr(&out));
    assert_eq!(read(&sb.target("rel/ws/AGENTS.md")), "# agents v1\n");
    let _ = std::fs::remove_dir_all(&sb.root);
}

#[test]
fn init_fails_closed_on_an_empty_template_dir() {
    let sb = Sandbox::new("empty-tpl");
    let empty = sb.target("empty-tpl");
    std::fs::create_dir_all(&empty).unwrap();

    let out = sb
        .cmd()
        .env("QUAQUA_TEMPLATE_DIR", &empty)
        .args(["init", sb.target("ws").to_str().unwrap()])
        .output()
        .expect("spawn");
    assert!(!out.status.success(), "expected a failure");
    assert!(
        stderr(&out).contains("holds no workspace/ or seed/ files"),
        "unclear failure: {}",
        stderr(&out),
    );
    let _ = std::fs::remove_dir_all(&sb.root);
}

/// Table row 1: file missing in the workspace -> write T, record.
#[test]
fn update_writes_a_missing_managed_file() {
    let sb = Sandbox::new("row1");
    let ws = sb.target("ws");
    assert!(sb.run(&["init", ws.to_str().unwrap()]).status.success());
    std::fs::remove_file(ws.join("rules/_all/core.md")).unwrap();

    let out = sb.run(&["init", ws.to_str().unwrap(), "--update"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(read(&ws.join("rules/_all/core.md")), "rule v1\n");
    assert!(
        stdout(&out).contains("write rules/_all/core.md"),
        "{}",
        stdout(&out)
    );

    let _ = std::fs::remove_dir_all(&sb.root);
}

/// Table row 2: W == T -> record only.
#[test]
fn update_records_an_already_current_file() {
    let sb = Sandbox::new("row2");
    let ws = sb.target("ws");
    assert!(sb.run(&["init", ws.to_str().unwrap()]).status.success());

    let before = read(&ws.join("AGENTS.md"));
    let out = sb.run(&["init", ws.to_str().unwrap(), "--update"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("record AGENTS.md"),
        "{}",
        stdout(&out)
    );
    assert_eq!(read(&ws.join("AGENTS.md")), before);

    let lock = sb.lock(&ws);
    assert_eq!(lock["files"]["AGENTS.md"]["md5"], md5_hex(b"# agents v1\n"));

    let _ = std::fs::remove_dir_all(&sb.root);
}

/// Table row 3: W == L, T != L (untouched locally) -> overwrite with T, record.
#[test]
fn update_overwrites_an_untouched_file() {
    let sb = Sandbox::new("row3");
    let ws = sb.target("ws");
    assert!(sb.run(&["init", ws.to_str().unwrap()]).status.success());

    sb.tpl_write("workspace/AGENTS.md", "# agents v2\n", 0o644);
    let out = sb.run(&["init", ws.to_str().unwrap(), "--update"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(
        stdout(&out).contains("overwrite AGENTS.md"),
        "{}",
        stdout(&out)
    );
    assert_eq!(read(&ws.join("AGENTS.md")), "# agents v2\n");
    assert_eq!(
        sb.lock(&ws)["files"]["AGENTS.md"]["md5"],
        md5_hex(b"# agents v2\n"),
    );
    assert!(!ws.join(".quaqua/pending/AGENTS.md").exists());

    let _ = std::fs::remove_dir_all(&sb.root);
}

/// Table row 4 (a): W != L (changed locally) -> never overwrite; pending + exit 3.
#[test]
fn update_keeps_a_locally_changed_file_and_pends_the_new_one() {
    let sb = Sandbox::new("row4a");
    let ws = sb.target("ws");
    assert!(sb.run(&["init", ws.to_str().unwrap()]).status.success());
    std::fs::write(ws.join("AGENTS.md"), "# mine\n").unwrap();

    sb.tpl_write("workspace/AGENTS.md", "# agents v2\n", 0o644);
    let out = sb.run(&["init", ws.to_str().unwrap(), "--update"]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "expected exit 3: {}",
        stderr(&out)
    );

    assert_eq!(read(&ws.join("AGENTS.md")), "# mine\n", "local edit lost");
    assert_eq!(
        read(&ws.join(".quaqua/pending/AGENTS.md")),
        "# agents v2\n",
        "new template not saved to pending/",
    );
    // the lock keeps L: the conflict is re-reported until resolved
    assert_eq!(
        sb.lock(&ws)["files"]["AGENTS.md"]["md5"],
        md5_hex(b"# agents v1\n"),
    );
    let out_text = stdout(&out);
    assert!(
        out_text.contains("conflict AGENTS.md -> .quaqua/pending/AGENTS.md"),
        "{out_text}",
    );
    assert!(
        out_text.contains("1 conflicts — see .quaqua/pending/"),
        "{out_text}",
    );

    let _ = std::fs::remove_dir_all(&sb.root);
}

/// Table row 4 (b): no lock entry and W != T -> never overwrite; exit 3.
#[test]
fn update_treats_a_preexisting_divergent_file_as_a_conflict() {
    let sb = Sandbox::new("row4b");
    let ws = sb.target("ws");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("AGENTS.md"), "# foreign\n").unwrap();

    let out = sb.run(&["init", ws.to_str().unwrap(), "--update"]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "expected exit 3: {}",
        stderr(&out)
    );
    assert_eq!(read(&ws.join("AGENTS.md")), "# foreign\n");
    assert_eq!(read(&ws.join(".quaqua/pending/AGENTS.md")), "# agents v1\n");

    let _ = std::fs::remove_dir_all(&sb.root);
}

/// Taking the pending copy and re-running records W == T.
#[test]
fn update_accepts_a_pending_copy_on_the_next_run() {
    let sb = Sandbox::new("row4-take");
    let ws = sb.target("ws");
    assert!(sb.run(&["init", ws.to_str().unwrap()]).status.success());
    std::fs::write(ws.join("AGENTS.md"), "# mine\n").unwrap();
    sb.tpl_write("workspace/AGENTS.md", "# agents v2\n", 0o644);
    assert_eq!(
        sb.run(&["init", ws.to_str().unwrap(), "--update"])
            .status
            .code(),
        Some(3),
    );

    std::fs::copy(ws.join(".quaqua/pending/AGENTS.md"), ws.join("AGENTS.md")).unwrap();
    let out = sb.run(&["init", ws.to_str().unwrap(), "--update"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(read(&ws.join("AGENTS.md")), "# agents v2\n");

    let _ = std::fs::remove_dir_all(&sb.root);
}

/// Table row 5: path in the lock, gone from the template -> never delete.
#[test]
fn update_lists_a_file_removed_upstream_and_keeps_it() {
    let sb = Sandbox::new("row5");
    let ws = sb.target("ws");
    assert!(sb.run(&["init", ws.to_str().unwrap()]).status.success());
    sb.tpl_remove("workspace/rules/_all/core.md");

    let out = sb.run(&["init", ws.to_str().unwrap(), "--update"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert!(ws.join("rules/_all/core.md").is_file(), "file was deleted");
    assert!(
        stdout(&out).contains("removed upstream rules/_all/core.md"),
        "{}",
        stdout(&out),
    );

    let _ = std::fs::remove_dir_all(&sb.root);
}

/// Table row 6: seed files are never updated.
#[test]
fn update_never_touches_a_seed() {
    let sb = Sandbox::new("row6");
    let ws = sb.target("ws");
    assert!(sb.run(&["init", ws.to_str().unwrap()]).status.success());
    std::fs::write(ws.join("memory/rules.md"), "# mine\n").unwrap();

    sb.tpl_write("seed/memory-rules.md", "# rules v2\n", 0o644);
    let out = sb.run(&["init", ws.to_str().unwrap(), "--update"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    assert_eq!(read(&ws.join("memory/rules.md")), "# mine\n");
    assert!(!ws.join(".quaqua/pending/memory/rules.md").exists());

    let _ = std::fs::remove_dir_all(&sb.root);
}

/// `--dry-run` prints the decisions and writes nothing: no pending copy, no
/// lock, not even the private dirs on a plain init.
#[test]
fn dry_run_writes_nothing() {
    let sb = Sandbox::new("dryrun");
    let ws = sb.target("ws");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("AGENTS.md"), "# foreign\n").unwrap();

    let out = sb.run(&["init", ws.to_str().unwrap(), "--update", "--dry-run"]);
    assert_eq!(
        out.status.code(),
        Some(3),
        "expected exit 3: {}",
        stderr(&out)
    );
    let text = stdout(&out);
    assert!(text.contains("conflict AGENTS.md"), "{text}");
    assert!(text.contains("write rules/_all/core.md"), "{text}");
    assert!(text.contains("nothing written"), "{text}");
    assert!(!ws.join(".quaqua").exists(), "dry run created .quaqua/");
    assert_eq!(read(&ws.join("AGENTS.md")), "# foreign\n");

    // plain init --dry-run: no dirs, no files, exit 0
    let fresh = sb.target("fresh");
    let out = sb.run(&["init", fresh.to_str().unwrap(), "--dry-run"]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));
    assert!(!fresh.exists(), "dry run created the target dir");
    assert!(stdout(&out).contains("would create memory/"));

    let _ = std::fs::remove_dir_all(&sb.root);
}

#[test]
fn init_fails_when_it_cannot_wire_hooks() {
    let Some(git) = find_git() else {
        return; // no git on PATH: nothing to assert
    };
    let sb = Sandbox::new("hookfail");
    let ws = sb.target("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let ok = gitfix::cmd(&git)
        .args(["init", "-q"])
        .arg(&ws)
        .status()
        .expect("git init");
    assert!(ok.success(), "git init failed");

    // A controlled PATH with only git, so no real herdr is reachable.
    let bin = sb.target("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(&git, bin.join("git")).unwrap();
    // Point git's config file at a directory: reads report "unset", the write fails.
    let configdir = sb.target("configdir");
    std::fs::create_dir_all(&configdir).unwrap();

    let out = sb
        .cmd()
        .env("PATH", &bin)
        .env("GIT_CONFIG", &configdir)
        .args(["init", ws.to_str().unwrap()])
        .output()
        .expect("spawn");
    assert!(
        !out.status.success(),
        "expected failure when core.hooksPath cannot be written",
    );
    assert!(
        stderr(&out).contains("failed to set core.hooksPath"),
        "unclear failure: {}",
        stderr(&out),
    );
    assert!(
        !stdout(&out).contains("workspace ready"),
        "run reported success despite unwired hooks: {}",
        stdout(&out),
    );

    let _ = std::fs::remove_dir_all(&sb.root);
}

#[test]
fn init_wires_embedded_hook_and_migrates_legacy() {
    let Some(git) = find_git() else {
        return;
    };
    let sb = Sandbox::new("wirehook");
    let ws = sb.target("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let ok = gitfix::cmd(&git)
        .args(["init", "-q"])
        .arg(&ws)
        .status()
        .expect("git init");
    assert!(ok.success(), "git init failed");

    let bin = sb.target("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(&git, bin.join("git")).unwrap();

    let run = |args: &[&str]| {
        sb.cmd()
            .env("PATH", &bin)
            .args(args)
            .output()
            .expect("spawn")
    };

    // 1. Fresh target: wires .git/quaqua-hooks/pre-commit (+exec) and sets core.hooksPath
    let out = run(&["init", ws.to_str().unwrap()]);
    assert!(out.status.success(), "init failed: {}", stderr(&out));

    let hook = ws.join(".git/quaqua-hooks/pre-commit");
    assert!(hook.is_file(), "hook was not created");
    assert_eq!(path_mode(&hook) & 0o111, 0o111, "hook is not executable");
    assert!(read(&hook).contains("portable-paths.md"));

    let push_hook = ws.join(".git/quaqua-hooks/pre-push");
    assert!(push_hook.is_file(), "pre-push hook was not created");
    assert_eq!(
        path_mode(&push_hook) & 0o111,
        0o111,
        "pre-push hook is not executable"
    );
    assert!(read(&push_hook).contains("QUAQUA_OWNER"));

    let hooks_path = gitfix::cmd(&git)
        .arg("-C")
        .arg(&ws)
        .args(["config", "--get", "core.hooksPath"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&hooks_path.stdout).trim(),
        ws.join(".git/quaqua-hooks").to_string_lossy().trim()
    );

    // 2. Modified hook is refreshed + logged
    std::fs::write(&hook, "tampered").unwrap();
    let out = run(&["init", ws.to_str().unwrap()]);
    assert!(out.status.success());
    assert!(
        stdout(&out).contains("hooks: refreshed"),
        "no refresh log: {}",
        stdout(&out),
    );
    assert!(read(&hook).contains("portable-paths.md"));

    // 3. Legacy migration: core.hooksPath set to scripts/hooks
    let legacy = ws.join("scripts/hooks");
    let ok = gitfix::cmd(&git)
        .arg("-C")
        .arg(&ws)
        .args(["config", "core.hooksPath", &legacy.to_string_lossy()])
        .status()
        .unwrap();
    assert!(ok.success());

    let out = run(&["init", ws.to_str().unwrap()]);
    assert!(out.status.success());
    assert!(
        stdout(&out).contains("migrated core.hooksPath"),
        "no migration log: {}",
        stdout(&out),
    );

    // 4. Idempotent re-run is quiet
    let out = run(&["init", ws.to_str().unwrap()]);
    assert!(out.status.success());
    assert!(!stdout(&out).contains("migrated"));
    assert!(!stdout(&out).contains("core.hooksPath="));

    let _ = std::fs::remove_dir_all(&sb.root);
}

/// Every herdr exec `quaqua init` makes — the pane queries (pane current/get,
/// agent list) and the CoS tag (`pane report-metadata`) — must run the binary
/// `HERDR_BIN_PATH` names, not one found through `PATH`.
#[test]
fn init_resolves_herdr_through_herdr_bin_path_only() {
    let sb = Sandbox::new("herdrbin");
    let ws = sb.target("ws");

    let stub = sb.target("stub/herdr");
    std::fs::create_dir_all(stub.parent().unwrap()).unwrap();
    let log = sb.target("stub.calls");
    write_script(
        &stub,
        &HERDR_STUB
            .replace("@LOG@", &log.display().to_string())
            .replace("@CWD@", &ws.display().to_string()),
    );

    // 1. No herdr on PATH at all: only HERDR_BIN_PATH can answer, so every
    //    query and the tag itself must have gone through the stub.
    let out = sb
        .cmd()
        .env("HERDR_BIN_PATH", &stub)
        .args(["init", ws.to_str().unwrap()])
        .output()
        .expect("spawn");
    assert!(out.status.success(), "init failed: {}", stderr(&out));
    let calls = std::fs::read_to_string(&log).unwrap_or_default();
    for want in ["pane current", "pane get w1:p1", "agent list"] {
        assert!(
            calls.contains(want),
            "HERDR_BIN_PATH binary missed '{want}': {calls}",
        );
    }
    // The CoS tag carries the farm of the workspace `init` just created.
    let want_metadata = format!(
        "pane report-metadata w1:p1 --source herdr-quaqua \
         --token grid_role=cos --token grid_owner=cos --token grid_farm={}",
        quaqua::workspace::farm_id_of(&ws),
    );
    assert!(
        calls.contains(&want_metadata),
        "HERDR_BIN_PATH binary missed '{want_metadata}': {calls}",
    );

    // 2. A decoy herdr FIRST on PATH: with HERDR_BIN_PATH set it must never be
    //    executed, and the tag must still come from the stub.
    let marker = sb.target("decoy.ran");
    let decoy_dir = sb.target("decoy");
    std::fs::create_dir_all(&decoy_dir).unwrap();
    write_script(
        &decoy_dir.join("herdr"),
        &HERDR_DECOY.replace("@MARKER@", &marker.display().to_string()),
    );
    std::fs::remove_file(&log).unwrap();
    let out = sb
        .cmd()
        .env("PATH", &decoy_dir)
        .env("HERDR_BIN_PATH", &stub)
        .args(["init", ws.to_str().unwrap()])
        .output()
        .expect("spawn");
    assert!(out.status.success(), "init failed: {}", stderr(&out));
    assert!(
        !marker.exists(),
        "the PATH herdr was executed despite HERDR_BIN_PATH",
    );
    let calls = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        calls.contains("pane report-metadata w1:p1"),
        "the HERDR_BIN_PATH binary did not tag the CoS pane: {calls}",
    );

    let _ = std::fs::remove_dir_all(&sb.root);
}

/// `init --git-hooks <repo>` installs the same embedded pre-commit into an
/// existing repo and does nothing else: no workspace dirs, no template lock,
/// no working-tree writes, and a re-run is a no-op.
#[test]
fn init_git_hooks_wires_an_existing_repo_only() {
    let Some(git) = find_git() else {
        return; // no git on PATH: nothing to assert
    };
    let sb = Sandbox::new("githooks");
    let repo = sb.target("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let ok = gitfix::cmd(&git)
        .args(["init", "-q"])
        .arg(&repo)
        .status()
        .expect("git init");
    assert!(ok.success(), "git init failed");
    std::fs::write(repo.join("README.md"), "engine\n").unwrap();
    assert!(gitfix::cmd(&git)
        .arg("-C")
        .arg(&repo)
        .args(["add", "README.md"])
        .status()
        .unwrap()
        .success());

    let bin = sb.target("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(&git, bin.join("git")).unwrap();
    let run = |args: &[&str]| {
        sb.cmd()
            .env("PATH", &bin)
            .args(args)
            .output()
            .expect("spawn")
    };

    let out = run(&["init", "--git-hooks", repo.to_str().unwrap()]);
    assert!(out.status.success(), "stderr: {}", stderr(&out));

    let hook = repo.join(".git/quaqua-hooks/pre-commit");
    assert!(hook.is_file(), "hook was not created");
    assert_eq!(path_mode(&hook) & 0o111, 0o111, "hook is not executable");
    assert!(read(&hook).contains("portable-paths.md"));

    let push_hook = repo.join(".git/quaqua-hooks/pre-push");
    assert!(push_hook.is_file(), "pre-push hook was not created");
    assert_eq!(
        path_mode(&push_hook) & 0o111,
        0o111,
        "pre-push hook is not executable"
    );
    assert!(read(&push_hook).contains("QUAQUA_OWNER"));

    let hooks_path = gitfix::cmd(&git)
        .arg("-C")
        .arg(&repo)
        .args(["config", "--get", "core.hooksPath"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&hooks_path.stdout).trim(),
        repo.join(".git/quaqua-hooks").to_string_lossy().trim()
    );

    // hook-only: no workspace skeleton, no lock, no CoS tag side effects
    for rel in [
        "AGENTS.md",
        "memory",
        "projects",
        "runs",
        "reports",
        ".quaqua",
    ] {
        assert!(!repo.join(rel).exists(), "workspace file written: {rel}");
    }
    let status = gitfix::cmd(&git)
        .arg("-C")
        .arg(&repo)
        .args(["status", "--porcelain"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&status.stdout).trim(),
        "A  README.md",
        "the working tree/index was touched",
    );

    // idempotent: a second run rewrites neither the hook nor the config
    let config_before = read(&repo.join(".git/config"));
    let hook_before = read(&hook);
    let out = run(&["init", "--git-hooks", repo.to_str().unwrap()]);
    assert!(out.status.success(), "re-run failed: {}", stderr(&out));
    assert_eq!(
        read(&repo.join(".git/config")),
        config_before,
        "config rewritten"
    );
    assert_eq!(read(&hook), hook_before, "hook rewritten");

    let _ = std::fs::remove_dir_all(&sb.root);
}

#[test]
fn init_git_hooks_refuses_a_foreign_hooks_path() {
    let Some(git) = find_git() else {
        return;
    };
    let sb = Sandbox::new("githooks-foreign");
    let repo = sb.target("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(gitfix::cmd(&git)
        .args(["init", "-q"])
        .arg(&repo)
        .status()
        .expect("git init")
        .success());
    assert!(gitfix::cmd(&git)
        .arg("-C")
        .arg(&repo)
        .args(["config", "core.hooksPath", "/their/hooks"])
        .status()
        .unwrap()
        .success());

    let bin = sb.target("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(&git, bin.join("git")).unwrap();
    let out = sb
        .cmd()
        .env("PATH", &bin)
        .args(["init", "--git-hooks", repo.to_str().unwrap()])
        .output()
        .expect("spawn");

    assert!(
        !out.status.success(),
        "a foreign core.hooksPath was accepted"
    );
    assert!(
        stderr(&out).contains("already sets core.hooksPath='/their/hooks'"),
        "unclear refusal: {}",
        stderr(&out),
    );
    let hooks_path = gitfix::cmd(&git)
        .arg("-C")
        .arg(&repo)
        .args(["config", "--get", "core.hooksPath"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&hooks_path.stdout).trim(),
        "/their/hooks",
        "the foreign value was overwritten",
    );
    assert!(
        !repo.join(".git/quaqua-hooks").exists(),
        "a refused repo was still given a quaqua hook dir",
    );

    let _ = std::fs::remove_dir_all(&sb.root);
}

#[test]
fn init_git_hooks_rejects_a_non_git_directory() {
    let Some(git) = find_git() else {
        return;
    };
    let sb = Sandbox::new("githooks-nonrepo");
    let plain = sb.target("plain");
    std::fs::create_dir_all(&plain).unwrap();

    let bin = sb.target("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(&git, bin.join("git")).unwrap();
    let out = sb
        .cmd()
        .env("PATH", &bin)
        .args(["init", "--git-hooks", plain.to_str().unwrap()])
        .output()
        .expect("spawn");

    assert!(!out.status.success(), "a non-repo target was accepted");
    assert!(
        stderr(&out).contains("is not a git repository"),
        "unclear error: {}",
        stderr(&out),
    );
    assert!(
        std::fs::read_dir(&plain).unwrap().next().is_none(),
        "a non-repo target was written to",
    );

    let _ = std::fs::remove_dir_all(&sb.root);
}

fn find_git() -> Option<PathBuf> {
    let path = std::env::var("PATH").ok()?;
    for dir in path.split(':') {
        if dir.is_empty() {
            continue;
        }
        let cand = Path::new(dir).join("git");
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

/// `worker spawn`'s hook step: only the push guard, in git's effective hooks
/// dir, with `core.hooksPath` untouched. A foreign `core.hooksPath` and an
/// existing foreign `pre-push` are left alone — spawn never redirects or
/// overwrites a hook setup it did not create.
#[test]
fn worker_push_guard_installs_pre_push_only() {
    let Some(_git) = find_git() else {
        return;
    };
    let sb = Sandbox::new("pushguard");
    let repo = sb.target("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(gitfix::cmd("git")
        .args(["init", "-q"])
        .arg(&repo)
        .status()
        .unwrap()
        .success());

    quaqua::init::ensure_worker_push_guard(&repo).expect("push guard install");

    let hook = repo.join(".git/hooks/pre-push");
    assert!(hook.is_file(), "pre-push guard was not installed");
    assert_eq!(
        path_mode(&hook) & 0o111,
        0o111,
        "push guard is not executable"
    );
    assert!(
        !repo.join(".git/hooks/pre-commit").exists(),
        "spawn installed the opt-in pre-commit",
    );
    let hooks_path = gitfix::cmd("git")
        .arg("-C")
        .arg(&repo)
        .args(["config", "--get", "core.hooksPath"])
        .output()
        .unwrap();
    assert!(
        hooks_path.stdout.is_empty(),
        "spawn redirected core.hooksPath: {}",
        String::from_utf8_lossy(&hooks_path.stdout),
    );

    let run = |owner: Option<&str>| {
        let mut cmd = Command::new(&hook);
        cmd.current_dir(&repo)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        match owner {
            Some(v) => {
                cmd.env("QUAQUA_OWNER", v);
            }
            None => {
                cmd.env_remove("QUAQUA_OWNER");
            }
        }
        cmd.status().unwrap()
    };
    assert!(!run(Some("tieude-x")).success(), "worker push must fail");
    assert!(run(None).success(), "principal push must pass");

    // A foreign core.hooksPath: refused with a warning, config and dir untouched.
    let foreign = sb.target("foreign-repo");
    std::fs::create_dir_all(&foreign).unwrap();
    assert!(gitfix::cmd("git")
        .args(["init", "-q"])
        .arg(&foreign)
        .status()
        .unwrap()
        .success());
    let theirs = sb.target("their-hooks");
    assert!(gitfix::cmd("git")
        .arg("-C")
        .arg(&foreign)
        .args(["config", "core.hooksPath", theirs.to_str().unwrap()])
        .status()
        .unwrap()
        .success());
    quaqua::init::ensure_worker_push_guard(&foreign).expect("foreign hooksPath is a no-op");
    let got = gitfix::cmd("git")
        .arg("-C")
        .arg(&foreign)
        .args(["config", "--get", "core.hooksPath"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&got.stdout).trim(),
        theirs.to_string_lossy().trim(),
        "the foreign core.hooksPath was overwritten",
    );
    assert!(!theirs.exists(), "wrote into a foreign hooks dir");

    // An existing foreign pre-push is never overwritten.
    let occupied = sb.target("occupied-repo");
    std::fs::create_dir_all(&occupied).unwrap();
    assert!(gitfix::cmd("git")
        .args(["init", "-q"])
        .arg(&occupied)
        .status()
        .unwrap()
        .success());
    let their_hook = occupied.join(".git/hooks/pre-push");
    write_file(&their_hook, "#!/bin/sh\necho theirs\n", 0o755);
    quaqua::init::ensure_worker_push_guard(&occupied).expect("occupied pre-push is a no-op");
    assert_eq!(
        read(&their_hook),
        "#!/bin/sh\necho theirs\n",
        "an existing pre-push was overwritten",
    );

    let _ = std::fs::remove_dir_all(&sb.root);
}

/// The guard as installed before the "Installed by" comment was added: same
/// behaviour, different bytes. A comment edit must not make an older install
/// look foreign.
const OLD_PRE_PUSH: &str = r#"#!/usr/bin/env bash
# pre-push — a worker must never push: pushes and merges belong to the
# Principal (issue #4). `quaqua worker spawn` exports QUAQUA_OWNER into the
# pane, so every process the worker starts carries it, git included. Run the
# push from a shell without QUAQUA_OWNER (the Lead/CoS) and it passes.
if [[ -n "${QUAQUA_OWNER-}" ]]; then
  echo "pre-push: refused — QUAQUA_OWNER='$QUAQUA_OWNER' marks a worker; workers never push." >&2
  echo "Hand the branch to the Lead; it merges via the queue." >&2
  exit 1
fi
exit 0
"#;

/// An older revision of the quaqua guard is quaqua's own: it is recognised and
/// refreshed to the current one, and still guards. The current revision is left
/// as it is.
#[test]
fn worker_push_guard_refreshes_an_older_revision() {
    let Some(_git) = find_git() else {
        return;
    };
    let sb = Sandbox::new("pushguard-old");
    let init_repo = |name: &str| {
        let repo = sb.target(name);
        std::fs::create_dir_all(&repo).unwrap();
        assert!(gitfix::cmd("git")
            .args(["init", "-q"])
            .arg(&repo)
            .status()
            .unwrap()
            .success());
        repo
    };

    // A fresh install is the refresh target: the current revision.
    let fresh = init_repo("fresh");
    quaqua::init::ensure_worker_push_guard(&fresh).expect("fresh install");
    let fresh_hook = fresh.join(".git/hooks/pre-push");
    let current = read(&fresh_hook);

    // An older revision of the same guard: refreshed, never called foreign.
    let old = init_repo("old");
    let hook = old.join(".git/hooks/pre-push");
    write_file(&hook, OLD_PRE_PUSH, 0o755);
    quaqua::init::ensure_worker_push_guard(&old).expect("an older revision is refreshed");
    let refreshed = read(&hook);
    assert_ne!(
        refreshed, OLD_PRE_PUSH,
        "the older revision was left in place",
    );
    assert_eq!(
        refreshed, current,
        "the older revision was not refreshed to the current one",
    );
    assert_eq!(
        path_mode(&hook) & 0o111,
        0o111,
        "the refreshed guard is not executable",
    );

    // ... and it still guards: a worker is refused, the principal passes.
    let run = |owner: Option<&str>| {
        let mut cmd = Command::new(&hook);
        cmd.current_dir(&old)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        match owner {
            Some(v) => {
                cmd.env("QUAQUA_OWNER", v);
            }
            None => {
                cmd.env_remove("QUAQUA_OWNER");
            }
        }
        cmd.status().unwrap()
    };
    assert!(
        !run(Some("tieude-x")).success(),
        "worker push must fail after the refresh",
    );
    assert!(
        run(None).success(),
        "principal push must pass after the refresh",
    );

    // The current revision is already right: a second call changes nothing.
    quaqua::init::ensure_worker_push_guard(&fresh).expect("a current guard is a no-op");
    assert_eq!(
        read(&fresh_hook),
        current,
        "the current guard was rewritten",
    );

    let _ = std::fs::remove_dir_all(&sb.root);
}

/// A worker pane carries `QUAQUA_OWNER`; the pre-push hook must reject that and
/// pass a plain shell. Runs the installed hook directly — no remote needed.
#[test]
fn pre_push_hook_rejects_workers_only() {
    let Some(git) = find_git() else {
        return;
    };
    let sb = Sandbox::new("prepush");
    let repo = sb.target("repo");
    std::fs::create_dir_all(&repo).unwrap();
    assert!(gitfix::cmd(&git)
        .args(["init", "-q"])
        .arg(&repo)
        .status()
        .unwrap()
        .success());
    let bin = sb.target("bin");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(&git, bin.join("git")).unwrap();
    let out = sb
        .cmd()
        .env("PATH", &bin)
        .args(["init", "--git-hooks", repo.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success(), "stderr: {}", stderr(&out));

    let hook = repo.join(".git/quaqua-hooks/pre-push");
    let run = |owner: Option<&str>| {
        let mut cmd = Command::new(&hook);
        cmd.current_dir(&repo)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        match owner {
            Some(v) => {
                cmd.env("QUAQUA_OWNER", v);
            }
            None => {
                cmd.env_remove("QUAQUA_OWNER");
            }
        }
        cmd.status().unwrap()
    };
    assert!(
        !run(Some("tieude-hardwork")).success(),
        "worker push must fail"
    );
    assert!(run(None).success(), "principal push must pass");
}

#[test]
fn s6_acceptance_5_fresh_init_has_agents_coordinator_lines_and_playbooks() {
    let root = std::env::temp_dir().join(format!("quaqua-init-s6-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let ws = root.join("ws");

    let nobin = root.join("nobin");
    std::fs::create_dir_all(&nobin).unwrap();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_quaqua"));
    cmd.env("PATH", &nobin)
        .env("HERDR_BIN_PATH", nobin.join("herdr"))
        .env_remove("HERDR_PANE_ID")
        .env_remove("QUAQUA_HOME")
        .env_remove("QUAQUA_TEMPLATE_DIR")
        .current_dir(&root);

    let out = cmd
        .args(["init", ws.to_str().unwrap()])
        .output()
        .expect("spawn init");
    assert!(
        out.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let agents_md = std::fs::read_to_string(ws.join("AGENTS.md")).expect("read AGENTS.md");
    assert!(
        agents_md.contains("## Coordinator Rules"),
        "missing Coordinator Rules section in AGENTS.md"
    );
    assert!(
        agents_md.contains("Route (binds CoS and Lead)"),
        "missing Route rule in AGENTS.md"
    );
    assert!(
        agents_md.contains("Never edit code or config (binds CoS and Lead)"),
        "missing Never edit code rule in AGENTS.md"
    );
    assert!(
        agents_md.contains("Attack the premise (binds CoS and Lead)"),
        "missing Attack the premise rule in AGENTS.md"
    );
    assert!(
        agents_md.contains("Background wait and drain points (binds CoS and Lead)"),
        "missing Background wait rule in AGENTS.md"
    );
    assert!(
        agents_md.contains("Budget (binds CoS and Lead)"),
        "missing Budget rule in AGENTS.md"
    );
    assert!(
        agents_md.contains("Pilot and liveness (binds CoS and Lead)"),
        "missing Pilot and liveness rule in AGENTS.md"
    );
    assert!(
        agents_md.contains("rules/_all/worker-timebox.md"),
        "missing link to worker-timebox.md in AGENTS.md"
    );

    // Also assert playbooks and principles in fresh init
    assert!(ws.join("playbooks/bug-fix.md").is_file());
    assert!(ws.join("playbooks/feature.md").is_file());
    assert!(ws.join("playbooks/figure-it-out.md").is_file());
    assert!(ws.join("playbooks/runtime-forensics.md").is_file());
    assert!(ws.join("playbooks/trace-forensics.md").is_file());
    assert!(ws.join("playbooks/visual-parity.md").is_file());
    assert!(ws.join("playbooks/eval.md").is_file());
    assert!(ws.join("playbooks/shipping.md").is_file());
    assert!(ws.join("playbooks/babysit.md").is_file());
    assert!(ws.join("playbooks/worktree-cleanup.md").is_file());
    assert!(
        !ws.join("playbooks/fixture.md").exists(),
        "fixture.md must not be installed in playbooks/"
    );
    assert!(
        !ws.join("principles/fixture.md").exists(),
        "fixture.md must not be installed in principles/"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn f1_fresh_init_has_no_fixture() {
    let root = std::env::temp_dir().join(format!("quaqua-init-f1-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let ws = root.join("ws");

    let nobin = root.join("nobin");
    std::fs::create_dir_all(&nobin).unwrap();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_quaqua"));
    cmd.env("PATH", &nobin)
        .env("HERDR_BIN_PATH", nobin.join("herdr"))
        .env_remove("HERDR_PANE_ID")
        .env_remove("QUAQUA_HOME")
        .env_remove("QUAQUA_TEMPLATE_DIR")
        .current_dir(&root);

    let out = cmd
        .args(["init", ws.to_str().unwrap()])
        .output()
        .expect("spawn init");
    assert!(
        out.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        !ws.join("playbooks/fixture.md").exists(),
        "fixture.md must not be installed in playbooks/"
    );
    assert!(
        !ws.join("principles/fixture.md").exists(),
        "fixture.md must not be installed in principles/"
    );
    assert!(ws.join("playbooks/bug-fix.md").is_file());
    assert!(ws.join("playbooks/feature.md").is_file());
    assert!(ws.join("playbooks/figure-it-out.md").is_file());
    assert!(ws.join("playbooks/runtime-forensics.md").is_file());
    assert!(ws.join("playbooks/trace-forensics.md").is_file());
    assert!(ws.join("playbooks/visual-parity.md").is_file());
    assert!(ws.join("playbooks/eval.md").is_file());
    assert!(ws.join("playbooks/shipping.md").is_file());
    assert!(ws.join("playbooks/babysit.md").is_file());
    assert!(ws.join("playbooks/worktree-cleanup.md").is_file());

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn s6b_acceptance_1_fresh_init_has_poteto_mode_skill() {
    let root = std::env::temp_dir().join(format!("quaqua-init-s6b-skill-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let ws = root.join("ws");

    let nobin = root.join("nobin");
    std::fs::create_dir_all(&nobin).unwrap();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_quaqua"));
    cmd.env("PATH", &nobin)
        .env("HERDR_BIN_PATH", nobin.join("herdr"))
        .env_remove("HERDR_PANE_ID")
        .env_remove("QUAQUA_HOME")
        .env_remove("QUAQUA_TEMPLATE_DIR")
        .current_dir(&root);

    let out = cmd
        .args(["init", ws.to_str().unwrap()])
        .output()
        .expect("spawn init");
    assert!(
        out.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let skill_path = ws.join(".claude/skills/poteto-mode/SKILL.md");
    assert!(
        skill_path.is_file(),
        "fresh quaqua init workspace must have .claude/skills/poteto-mode/SKILL.md"
    );

    let content = std::fs::read_to_string(&skill_path).expect("read poteto-mode/SKILL.md");
    assert!(
        content.contains("name: poteto-mode"),
        "skill must be named poteto-mode"
    );
    assert!(
        content.contains("Never invent a playbook name"),
        "skill must require never inventing a playbook name"
    );
    assert!(
        content.contains("Never route inside a worker"),
        "skill must forbid routing inside a worker"
    );
    assert!(
        content.contains("routes to `feature`"),
        "code route must route to feature"
    );
    assert!(
        content.contains("routes to `babysit`"),
        "F2: PR-status route must be babysit now that P2d landed"
    );
    assert!(
        content.contains("routes to `shipping`"),
        "F2: land-or-ship route must be shipping now that P2d landed"
    );
    assert!(
        content.contains("routes to `worktree-cleanup`"),
        "F2: disk-reclaim route must be worktree-cleanup"
    );
    assert!(
        content.contains("routes to `multi-phase-plan`"),
        "multi-phase-plan route must route to multi-phase-plan"
    );
    assert!(
        content.contains("Deferred routes with quaqua mappings (P2):"),
        "F5: deferred routes with quaqua mappings must be present"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn s6b_acceptance_2_agents_route_line_names_poteto_mode() {
    let root = std::env::temp_dir().join(format!("quaqua-init-s6b-agents-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let ws = root.join("ws");

    let nobin = root.join("nobin");
    std::fs::create_dir_all(&nobin).unwrap();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_quaqua"));
    cmd.env("PATH", &nobin)
        .env("HERDR_BIN_PATH", nobin.join("herdr"))
        .env_remove("HERDR_PANE_ID")
        .env_remove("QUAQUA_HOME")
        .env_remove("QUAQUA_TEMPLATE_DIR")
        .current_dir(&root);

    let out = cmd
        .args(["init", ws.to_str().unwrap()])
        .output()
        .expect("spawn init");
    assert!(
        out.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let agents_md = std::fs::read_to_string(ws.join("AGENTS.md")).expect("read AGENTS.md");
    assert!(
        agents_md.contains("Before every brief, route through `/poteto-mode`."),
        "AGENTS.md must carry the route line: Before every brief, route through `/poteto-mode`."
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn s6b_f3_skill_carries_agents_approval_list() {
    let root = std::env::temp_dir().join(format!("quaqua-init-s6b-f3-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let ws = root.join("ws");

    let nobin = root.join("nobin");
    std::fs::create_dir_all(&nobin).unwrap();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_quaqua"));
    cmd.env("PATH", &nobin)
        .env("HERDR_BIN_PATH", nobin.join("herdr"))
        .env_remove("HERDR_PANE_ID")
        .env_remove("QUAQUA_HOME")
        .env_remove("QUAQUA_TEMPLATE_DIR")
        .current_dir(&root);

    let out = cmd
        .args(["init", ws.to_str().unwrap()])
        .output()
        .expect("spawn init");
    assert!(
        out.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let skill_path = ws.join(".claude/skills/poteto-mode/SKILL.md");
    let content = std::fs::read_to_string(&skill_path).expect("read poteto-mode/SKILL.md");

    assert!(
        content
            .contains("Never approve on anh iu's behalf without explicit approval (`AGENTS.md`):"),
        "skill must cite AGENTS.md approval rule"
    );
    assert!(
        content.contains("Deleting data"),
        "skill must require approval for deleting data"
    );
    assert!(
        content.contains("Push or force-push"),
        "skill must require approval for push or force-push"
    );
    assert!(
        content.contains("Branch delete or merge to main"),
        "skill must require approval for branch delete or merge to main"
    );
    assert!(
        content.contains("Deploy"),
        "skill must require approval for deploy"
    );
    assert!(
        content.contains("Spending money"),
        "skill must require approval for spending money"
    );
    assert!(
        content.contains("Sending or publishing anything outward"),
        "skill must require approval for outward publishing/sending"
    );
    assert!(
        content.contains("never bypass anh iu's approval for the irreversible actions above"),
        "session overrides must never bypass approval for irreversible actions"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn s6b_f4_skill_carries_agents_resume_first_rule() {
    let root = std::env::temp_dir().join(format!("quaqua-init-s6b-f4-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let ws = root.join("ws");

    let nobin = root.join("nobin");
    std::fs::create_dir_all(&nobin).unwrap();

    let mut cmd = Command::new(env!("CARGO_BIN_EXE_quaqua"));
    cmd.env("PATH", &nobin)
        .env("HERDR_BIN_PATH", nobin.join("herdr"))
        .env_remove("HERDR_PANE_ID")
        .env_remove("QUAQUA_HOME")
        .env_remove("QUAQUA_TEMPLATE_DIR")
        .current_dir(&root);

    let out = cmd
        .args(["init", ws.to_str().unwrap()])
        .output()
        .expect("spawn init");
    assert!(
        out.status.success(),
        "init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    let skill_path = ws.join(".claude/skills/poteto-mode/SKILL.md");
    let content = std::fs::read_to_string(&skill_path).expect("read poteto-mode/SKILL.md");

    assert!(
        content.contains("Resume vs fresh:"),
        "skill must carry Resume vs fresh header"
    );
    assert!(
        content.contains("Resume is the default for existing work"),
        "skill must set resume as default for existing work"
    );
    assert!(
        content.contains("A half-done, stalled or killed worker resumes the same session (`quaqua worker prompt --resume`)"),
        "skill must state half-done/stalled/killed workers resume"
    );
    assert!(
        content.contains("fresh spawn only when the goal changed or the worktree is unrecoverable"),
        "skill must restrict fresh spawn per AGENTS.md:43"
    );

    let _ = std::fs::remove_dir_all(&root);
}

/// The missing half of the hooks contract: not just "init installed a hook", but
/// what the installed pre-commit does with a diff (rules/_all/portable-paths.md).
/// A commit whose ADDED lines carry a machine-specific absolute path is refused
/// and names the file, the line and the policy; the allowlist is exact; the
/// portable spellings, a pure rename, binary content and existing violations all
/// still commit.
///
/// Runs the hook `init --git-hooks` installed, against a throwaway repo — the
/// real workspace index is never touched.
#[test]
fn pre_commit_hook_refuses_added_machine_paths() {
    let Some(bash) = find_bash() else {
        return; // the hook is a bash script; without bash there is nothing to run
    };
    let Some(git_bin) = find_git() else {
        return;
    };

    let sb = Sandbox::new("precommit");
    let repo = sb.target("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let bin = sb.target("bin");
    std::fs::create_dir_all(&bin).unwrap();
    // `quaqua init --git-hooks` resolves git through PATH, so the sandbox bin dir
    // offers exactly one real git; bash comes from the system PATH appended below.
    std::os::unix::fs::symlink(&git_bin, bin.join("git")).unwrap();
    let path = format!("{}:/usr/bin:/bin", bin.display());
    let git = |args: &[&str]| {
        let out = gitfix::cmd("git")
            .args(["-C", repo.to_str().unwrap()])
            .args(args)
            .env("PATH", &path)
            .output()
            .expect("spawn git");
        (out.status.success(), stdout(&out) + &stderr(&out))
    };
    // The fixtures must carry a machine-looking path, but this file may not
    // spell one (the hook under test scans added lines, this repo's included),
    // so `@HOME@` / `@USERS@` stand in for the machine-specific prefix.
    let home = concat!("/", "home");
    let users = concat!("/", "Users");
    let write = |rel: &str, body: &str| {
        let p = repo.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, body.replace("@HOME@", home).replace("@USERS@", users)).unwrap();
    };

    assert!(git(&["init", "-q"]).0, "git init failed");
    assert!(git(&["config", "user.email", "test@example.invalid"]).0);
    assert!(git(&["config", "user.name", "test"]).0);

    let out = sb
        .cmd()
        .env("PATH", &bin)
        .args(["init", "--git-hooks", repo.to_str().unwrap()])
        .output()
        .expect("spawn quaqua init --git-hooks");
    assert!(
        out.status.success(),
        "init --git-hooks failed: {}",
        stderr(&out)
    );

    let hookdir = repo.join(".git/quaqua-hooks");
    let hook = hookdir.join("pre-commit");
    assert!(hook.is_file(), "no generated pre-commit at {hook:?}");

    let hook_out = |dir: &Path| {
        let out = Command::new(&bash)
            .arg(&hook)
            .current_dir(dir)
            .env("PATH", &path)
            .output()
            .expect("run the pre-commit hook");
        (
            out.status.code().unwrap_or(-1),
            stdout(&out) + &stderr(&out),
        )
    };
    let hook_rc = || hook_out(&repo).0;
    let stage = || assert!(git(&["add", "-A"]).0, "git add failed");
    let commit_rc = || -> i32 {
        let (ok, text) = git(&["commit", "-qm", "subject"]);
        std::fs::write(sb.target("last-commit.out"), &text).unwrap();
        if ok {
            0
        } else {
            1
        }
    };
    let last_commit = || read(&sb.target("last-commit.out"));
    let reset = || {
        let _ = git(&["reset", "-q", "--hard", "HEAD"]);
        let _ = git(&["clean", "-qfd"]);
    };

    // baseline: the two allowlisted paths plus ordinary sources
    write("rules/_all/portable-paths.md", "# portable paths\n");
    write("tests/fixture.sh", "#!/usr/bin/env bash\necho baseline\n");
    write("src/tool.sh", "#!/usr/bin/env bash\necho baseline\n");
    write("README.md", "baseline\n");
    stage();
    assert_eq!(commit_rc(), 0, "baseline commit refused: {}", last_commit());

    // 1. an added machine path in a script is refused, naming file/line/policy
    write(
        "src/tool.sh",
        "#!/usr/bin/env bash\ncd @HOME@/alice/project\n",
    );
    stage();
    assert_eq!(hook_rc(), 1, "added /home/<user>/ path was not refused");
    let text = hook_out(&repo).1;
    for want in ["src/tool.sh", "home/alice/project", "portable-paths"] {
        assert!(text.contains(want), "hook output lacks {want:?}: {text}");
    }
    // and through a real commit, so the installed hook really fires
    assert_ne!(commit_rc(), 0, "git commit accepted a machine path");
    assert!(last_commit().contains("src/tool.sh"), "{}", last_commit());
    reset();

    // 2. the macOS form is refused too
    write(
        "src/tool.sh",
        "#!/usr/bin/env bash\nopen @USERS@/bob/notes\n",
    );
    stage();
    assert_eq!(hook_rc(), 1, "added /Users/<user>/ path was not refused");
    assert!(hook_out(&repo).1.contains("Users/bob/notes"));
    reset();

    // 3. the allowlist is the two paths, and it is exact rather than a prefix
    write(
        "tests/fixture.sh",
        "#!/usr/bin/env bash\ncd @HOME@/alice/project\n",
    );
    stage();
    assert_eq!(hook_rc(), 0, "a tests/ fixture must be allowlisted");
    write("rules/_all/portable-paths.md", "- never @HOME@/alice/\n");
    stage();
    assert_eq!(hook_rc(), 0, "the policy file naming the pattern must pass");
    write(
        "tests-helper.sh",
        "#!/usr/bin/env bash\ncd @HOME@/alice/project\n",
    );
    stage();
    assert_eq!(
        hook_rc(),
        1,
        "tests-helper.sh is outside tests/ and must be checked"
    );
    reset(); // `clean -fd` drops the untracked tests-helper.sh again

    // 4. portable spellings and placeholders pass
    write(
        "src/tool.sh",
        "#!/usr/bin/env bash\n\
         a=$HOME/thing\n\
         b=~/thing\n\
         c=$(dirname \"$0\")/thing\n\
         d=/home/<user>/thing\n\
         e=/Users/<name>/thing\n\
         f=/etc/hosts\n\
         g=/usr/local/bin\n",
    );
    stage();
    assert_eq!(
        hook_rc(),
        0,
        "portable spellings refused: {}",
        hook_out(&repo).1
    );
    reset();

    // 5. only added lines are judged: a legacy violation still commits, and a
    //    clean edit to that file still passes
    write(
        "src/legacy.sh",
        "#!/usr/bin/env bash\ncd @HOME@/legacy/project\n",
    );
    stage();
    let (ok, text) = git(&["commit", "-q", "--no-verify", "-m", "legacy"]);
    assert!(ok, "seeding the legacy violation failed: {text}");
    write(
        "src/legacy.sh",
        "#!/usr/bin/env bash\ncd @HOME@/legacy/project\necho more\n",
    );
    stage();
    assert_eq!(hook_rc(), 0, "a clean edit of a legacy file was refused");
    assert!(read(&repo.join("src/legacy.sh")).contains("echo more"));
    reset();

    // 6. a pure rename re-judges nothing; rename + added lines is re-checked
    //    under the new name
    assert!(git(&["mv", "src/legacy.sh", "src/renamed.sh"]).0);
    stage();
    assert_eq!(hook_rc(), 0, "a pure rename re-judged existing content");
    write(
        "src/renamed.sh",
        "#!/usr/bin/env bash\ncd @HOME@/legacy/project\ncd @HOME@/carol/other\n",
    );
    stage();
    assert_eq!(hook_rc(), 1, "rename + added machine path was not refused");
    assert!(
        hook_out(&repo).1.contains("src/renamed.sh"),
        "the new path was not named"
    );
    reset();

    // 7. binary content carries no added lines and passes
    let mut blob = vec![0u8, 1, 2];
    blob.extend(format!("{home}/alice/inside").bytes());
    blob.extend([0u8, 3]);
    std::fs::write(repo.join("blob.bin"), &blob).unwrap();
    stage();
    assert_eq!(hook_rc(), 0, "a binary diff was refused");
    reset(); // `clean -fd` drops the untracked blob.bin again

    // 8. a clean index passes; an unborn branch's first commit is still judged
    assert_eq!(hook_rc(), 0, "a clean index was refused");
    let fresh = sb.target("fresh");
    std::fs::create_dir_all(&fresh).unwrap();
    assert!(gitfix::cmd("git")
        .args(["-C", fresh.to_str().unwrap(), "init", "-q"])
        .env("PATH", &path)
        .status()
        .expect("git init")
        .success());
    assert!(gitfix::cmd("git")
        .args(["-C", fresh.to_str().unwrap(), "config", "core.hooksPath"])
        .arg(&hookdir)
        .env("PATH", &path)
        .status()
        .expect("git config")
        .success());
    std::fs::write(
        fresh.join("first.sh"),
        format!("#!/usr/bin/env bash\ncd {home}/dave/x\n"),
    )
    .unwrap();
    assert!(gitfix::cmd("git")
        .args(["-C", fresh.to_str().unwrap(), "add", "-A"])
        .env("PATH", &path)
        .status()
        .expect("git add")
        .success());
    let (code, text) = hook_out(&fresh);
    assert_eq!(
        code, 1,
        "an unborn-branch first commit with a machine path was allowed"
    );
    assert!(
        text.contains("first.sh"),
        "the first commit did not name the file: {text}"
    );

    // 9. the shipped hook itself stays portable and executable
    for line in read(&hook).lines() {
        for marker in ["/home/", "/Users/"] {
            for rest in line.split(marker).skip(1) {
                let machine = rest
                    .chars()
                    .next()
                    .map(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
                    .unwrap_or(false);
                assert!(
                    !machine,
                    "the generated hook carries a machine path: {line}"
                );
            }
        }
    }
    assert_eq!(
        path_mode(&hook) & 0o111,
        0o111,
        "the hook is not executable"
    );

    // 10. a workspace init and `--git-hooks` install the same hook body: one
    //     embedded template, not a second copy that can drift.
    let ws = sb.target("ws");
    std::fs::create_dir_all(&ws).unwrap();
    assert!(gitfix::cmd("git")
        .args(["-C", ws.to_str().unwrap(), "init", "-q"])
        .env("PATH", &path)
        .status()
        .expect("git init")
        .success());
    let out = sb
        .cmd()
        .env("PATH", &bin)
        .args(["init", ws.to_str().unwrap()])
        .output()
        .expect("spawn quaqua init");
    assert!(
        out.status.success(),
        "workspace init failed: {}",
        stderr(&out)
    );
    assert_eq!(
        read(&ws.join(".git/quaqua-hooks/pre-commit")),
        read(&hook),
        "--git-hooks installed a different hook body than a workspace init"
    );

    let _ = std::fs::remove_dir_all(&sb.root);
}

fn find_bash() -> Option<PathBuf> {
    let path = std::env::var("PATH").ok()?;
    for dir in path.split(':') {
        if dir.is_empty() {
            continue;
        }
        let cand = Path::new(dir).join("bash");
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}
