//! An in-flight `quaqua worker prompt` must survive its coordinator's exit.
//!
//! A coordinator (CoS or Lead) runs the prompt as a job of its Claude Code
//! session; when that session ends, the job's process group gets SIGHUP and the
//! launching parent gets SIGTERM. The wait must keep going and still deliver:
//! the worker's turn is not interrupted, and once the worker writes its report
//! the prompt accepts it (the `.<name>.read` marker is the acceptance).
//!
//! Driven through the real binary against a fake herdr; nothing touches a live
//! pane. Verify: `QUAQUA_TEST=1 cargo test --locked -p quaqua --test prompt_detach`

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const NAME: &str = "sude-detachfix";
const REPORT: &str =
    "Outcome:         COMPLETE\nScope:           none\n\n## Blockers\nnone\n\n## Lessons\nnone\n";

struct Fixture {
    root: PathBuf,
    runs: PathBuf,
    herdr: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!("quaqua-detach-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let runs = root.join("home/quaqua/runs");
        fs::create_dir_all(&runs).unwrap();
        let herdr = root.join("herdr");
        let (calls, release) = (root.join("calls"), root.join("release"));
        // The agent reads as "working" until `release` exists, then idle.
        fs::write(
            &herdr,
            format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> "{calls}"
status=working
if [ -e "{release}" ]; then
  status=idle
fi
case "$1 $2" in
  "agent wait") sleep 0.3; echo '{{"result": {{"agent": {{"agent_status": "'$status'", "state_change_seq": 1}}}}}}' ;;
  "agent get") echo '{{"result": {{"agent": {{"name": "'$3'", "agent_status": "'$status'"}}}}}}' ;;
  *) echo '{{"result": {{}}}}' ;;
esac
"#,
                calls = calls.display(),
                release = release.display(),
            ),
        )
        .unwrap();
        fs::set_permissions(&herdr, fs::Permissions::from_mode(0o755)).unwrap();
        Fixture { root, runs, herdr }
    }

    fn calls(&self) -> String {
        fs::read_to_string(self.root.join("calls")).unwrap_or_default()
    }

    /// Start `quaqua worker prompt --resume` under a launcher shell that leads
    /// its own process group, like a coordinator's background job.
    fn launch(&self) -> std::process::Child {
        let script = format!(
            "\"{}\" worker prompt --resume {NAME}; echo $? > \"{}\"",
            env!("CARGO_BIN_EXE_quaqua"),
            self.root.join("launcher-exit").display()
        );
        Command::new("sh")
            .args(["-c", &script])
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", self.root.join("home"))
            .env("QUAQUA_HOME", self.root.join("home/quaqua"))
            .env("QUAQUA_RUNS_DIR", &self.runs)
            .env("QUAQUA_TEST", "1")
            .env("GRID_SKIP", "1")
            .env("HERDR_BIN_PATH", &self.herdr)
            .env("PROMPT_WORKER_POLL_SEC", "1")
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("launch")
    }

    fn wait_for(&self, what: &str, f: impl Fn() -> bool) {
        let end = Instant::now() + Duration::from_secs(20);
        while !f() {
            assert!(Instant::now() < end, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn watching(&self) -> bool {
        self.calls().lines().any(|l| l.starts_with("agent "))
    }

    fn accepted(&self) -> bool {
        self.runs.join(format!(".{NAME}.read")).exists()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// The worker finishing its turn: the report lands atomically (tmp + mv),
/// whether or not anything is still watching.
fn release_worker(f: &Fixture) {
    let tmp = f.runs.join(format!(".{NAME}.tmp"));
    fs::write(&tmp, REPORT).unwrap();
    fs::rename(&tmp, f.runs.join(format!("{NAME}.md"))).unwrap();
    fs::write(f.root.join("release"), "").unwrap();
}

fn kill(args: &[&str]) {
    let st = Command::new("kill").args(args).status().expect("kill");
    assert!(st.success(), "kill {args:?} failed");
}

fn assert_turn_untouched(f: &Fixture) {
    let calls = f.calls();
    for bad in ["send-keys", "interrupt", "stop", "close"] {
        assert!(!calls.contains(bad), "worker was touched ({bad}):\n{calls}");
    }
}

#[test]
fn test_prompt_accepts_the_report_when_nobody_kills_it() {
    let f = Fixture::new("control");
    let mut child = f.launch();
    f.wait_for("the wait to start", || f.watching());
    release_worker(&f);
    f.wait_for("acceptance", || f.accepted());
    assert!(child.wait().unwrap().success());
    assert_eq!(
        fs::read_to_string(f.root.join("launcher-exit"))
            .unwrap()
            .trim(),
        "0"
    );
}

#[test]
fn test_prompt_survives_its_coordinator_exiting() {
    let f = Fixture::new("coordinator-exit");
    let mut child = f.launch();
    let (parent, pgid) = (child.id().to_string(), format!("-{}", child.id()));
    f.wait_for("the wait to start", || f.watching());

    // The coordinator exits: SIGHUP to its process group, SIGTERM to the parent.
    kill(&["-HUP", "--", &pgid]);
    kill(&["-TERM", &parent]);
    let _ = child.wait();

    // The worker finishes after its coordinator is gone.
    std::thread::sleep(Duration::from_secs(1));
    release_worker(&f);
    f.wait_for("acceptance after the coordinator exit", || f.accepted());

    assert_turn_untouched(&f);
    assert_eq!(
        fs::read_to_string(f.runs.join(format!("{NAME}.md"))).unwrap(),
        REPORT
    );
}
