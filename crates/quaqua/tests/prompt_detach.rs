//! An in-flight `quaqua worker prompt` must survive its coordinator's exit.
//!
//! A coordinator (CoS or Lead) runs the prompt as a job of its Claude Code
//! session; when that session ends, the job's process group gets SIGHUP and the
//! launching parent gets SIGTERM. The wait must keep going and still deliver:
//! the worker's turn is not interrupted, and once the worker writes its report
//! the prompt accepts it (the `.<name>.read` marker is the acceptance).
//!
//! The same holds for `quaqua pipeline`. Once a coordinator is gone, a later
//! `--resume` attaches to the live core instead of starting a second watcher,
//! and Ctrl-C in the foreground leaves the core running and says how to
//! re-attach.
//!
//! Driven through the real binary against a fake herdr; nothing touches a live
//! pane. Verify: `QUAQUA_TEST=1 cargo test --locked -p quaqua --test prompt_detach`

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const NAME: &str = "sude-detachfix";
const TASK: &str = "pdt1";
const PIPE_WORKER: &str = "sude-pdt1";
const REPORT: &str =
    "Outcome:         COMPLETE\nScope:           none\n\n## Blockers\nnone\n\n## Lessons\nnone\n";
const PIPE_REPORT: &str = "Outcome: BLOCKED\nfake build report\n\n## Blockers\n- fake builder stopped early\n\n## Lessons\n- none\n";

#[derive(Clone, Copy, PartialEq)]
enum Cmd {
    /// `quaqua worker prompt --resume <worker>`
    Worker,
    /// `quaqua pipeline --resume <task> <repo>`
    Pipeline,
}

struct Fixture {
    cmd: Cmd,
    root: PathBuf,
    runs: PathBuf,
    herdr: PathBuf,
    repo: PathBuf,
}

impl Fixture {
    fn new(tag: &str) -> Fixture {
        Self::build(Cmd::Worker, tag)
    }

    fn pipeline(tag: &str) -> Fixture {
        let f = Self::build(Cmd::Pipeline, tag);
        fs::create_dir_all(&f.repo).unwrap();
        fs::create_dir_all(f.root.join("scripts")).unwrap();
        let brief = f.root.join("brief.md");
        fs::write(
            &brief,
            "Goal: exercise the pipeline wait\nHard constraints: none\n\
             Current design (open to challenge): fixture\nUnknowns: none\nEvidence: this file\n",
        )
        .unwrap();
        // A resumable `build`-phase state whose prompt has not gone out yet.
        let state = format!(
            "repo={}\nbrief={}\nbuilder=sude\nreviewer=suhuynh\nreview=always\nrounds=2\n\
             base=\nwt={}/.wt/sude-{TASK}\nbase_sha=\nhead_sha=\nplanfile=\ncommit=\n\
             phase=build\nround=0\nprompted=0\nprompt_ts=0\nrounds_run=0\nreview_run=skip\n\
             review_changed=false\nlast_findings=0\nfinished=0\nstart_elapsed=0\n\
             builder_spawned=1\nreviewer_spawned=0\nlead=\nlead_pane=\npipeline_pid=0\n",
            fs::canonicalize(&f.repo).unwrap().display(),
            brief.display(),
            f.root.display(),
        );
        fs::write(f.runs.join(format!("pipeline-{TASK}.state")), state).unwrap();
        fs::write(
            f.runs.join(format!("pipeline-{TASK}.md")),
            format!("# pipeline {TASK}\n"),
        )
        .unwrap();
        f
    }

    fn build(cmd: Cmd, tag: &str) -> Fixture {
        let root = std::env::temp_dir().join(format!("quaqua-detach-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let runs = root.join("home/quaqua/runs");
        fs::create_dir_all(&runs).unwrap();
        let herdr = root.join("herdr");
        let (calls, release, ppids) =
            (root.join("calls"), root.join("release"), root.join("ppids"));
        // Every watcher poll logs its parent: the watching quaqua process.
        // The agent reads as "working" until `release` exists, then idle.
        let snapshot = r#"{"snapshot":{"workspaces":[{"workspace_id":"ws1","label":"q"}],
 "tabs":[{"tab_id":"t1","workspace_id":"ws1","label":"lead-tab","pane_count":3}],
 "panes":[
   {"pane_id":"%lead","tab_id":"t1","workspace_id":"ws1","agent_status":"idle",
    "tokens":{"grid_role":"lead","grid_owner":"cos","grid_ready":"1"}},
   {"pane_id":"%a","tab_id":"t1","workspace_id":"ws1","agent_status":"working",
    "tokens":{"grid_role":"worker","grid_owner":"cos","grid_group":"repo","grid_ready":"1"}},
   {"pane_id":"%b","tab_id":"t1","workspace_id":"ws1","agent_status":"idle",
    "tokens":{"grid_role":"worker","grid_owner":"cos","grid_group":"repo","grid_ready":"1"}}
 ],
 "layouts":[{"tab_id":"t1","workspace_id":"ws1","area":{"x":0,"y":0,"width":200,"height":50},
             "panes":[{"pane_id":"%lead"},{"pane_id":"%a"},{"pane_id":"%b"}]}],
 "agents":[{"name":"cos","pane_id":"%lead","agent_status":"idle"}]}}"#;
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
  "agent wait") echo $PPID >> "{ppids}"; sleep 0.3; echo '{{"result": {{"agent": {{"agent_status": "'$status'", "state_change_seq": 1}}}}}}' ;;
  "agent get") echo $PPID >> "{ppids}"; echo '{{"result": {{"agent": {{"name": "'$3'", "agent_status": "'$status'"}}}}}}' ;;
  "api snapshot") cat <<'EOF'
{snapshot}
EOF
  ;;
  *) echo '{{"result": {{}}}}' ;;
esac
"#,
                calls = calls.display(),
                release = release.display(),
                ppids = ppids.display(),
            ),
        )
        .unwrap();
        fs::set_permissions(&herdr, fs::Permissions::from_mode(0o755)).unwrap();
        Fixture {
            cmd,
            repo: root.join("repo"),
            root,
            runs,
            herdr,
        }
    }

    fn calls(&self) -> String {
        fs::read_to_string(self.root.join("calls")).unwrap_or_default()
    }

    /// Pids of the quaqua processes that polled the agent so far.
    fn watchers(&self) -> Vec<u32> {
        let mut pids: Vec<u32> = fs::read_to_string(self.root.join("ppids"))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.trim().parse().ok())
            .collect();
        pids.sort_unstable();
        pids.dedup();
        pids
    }

    fn quaqua(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_quaqua"));
        match self.cmd {
            Cmd::Worker => cmd.args(["worker", "prompt", "--resume", NAME]),
            Cmd::Pipeline => cmd.args([
                "pipeline",
                "--resume",
                TASK,
                &fs::canonicalize(&self.repo).unwrap().to_string_lossy(),
            ]),
        };
        cmd.current_dir(if self.cmd == Cmd::Pipeline {
            &self.repo
        } else {
            &self.root
        })
        .env_clear()
        .env(
            "PATH",
            format!(
                "{}:{}",
                self.root.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .env("HOME", self.root.join("home"))
        .env("QUAQUA_HOME", self.root.join("home/quaqua"))
        .env("QUAQUA_RUNS_DIR", &self.runs)
        .env("HERDR_BIN_PATH", &self.herdr)
        .env("PROMPT_WORKER_POLL_SEC", "1");
        if self.cmd == Cmd::Worker {
            cmd.env("QUAQUA_TEST", "1").env("GRID_SKIP", "1");
        } else {
            cmd.env("QUAQUA_SCRIPT_DIR", self.root.join("scripts"))
                .env("PROMPT_WORKER_AGENT_GONE_SEC", "0")
                .env("GRID_CHECK_TIMEOUT_SEC", "0");
        }
        cmd
    }

    /// Start the command under a launcher shell that leads its own process
    /// group, like a coordinator's background job.
    fn launch(&self, tag: &str) -> Child {
        let mut parts = vec![format!("\"{}\"", env!("CARGO_BIN_EXE_quaqua"))];
        let q = self.quaqua();
        parts.extend(q.get_args().map(|a| format!("\"{}\"", a.to_string_lossy())));
        let script = format!(
            "{}; echo $? > \"{}\"",
            parts.join(" "),
            self.root.join(format!("launcher-exit-{tag}")).display()
        );
        let mut cmd = Command::new("sh");
        cmd.args(["-c", &script]);
        cmd.env_clear()
            .envs(q.get_envs().filter_map(|(k, v)| v.map(|v| (k, v))))
            .current_dir(q.get_current_dir().unwrap())
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("launch")
    }

    /// Start the command directly (no shell), so signals reach the foreground
    /// quaqua itself. Its stderr is captured.
    fn spawn_direct(&self) -> Child {
        self.quaqua()
            .process_group(0)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn")
    }

    fn launcher_exit(&self, tag: &str) -> Option<String> {
        fs::read_to_string(self.root.join(format!("launcher-exit-{tag}")))
            .ok()
            .map(|s| s.trim().to_string())
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

    /// The report was consumed: a worker prompt writes its `.read` marker, a
    /// pipeline parks the builder's report as `<worker>.build.md`.
    fn accepted(&self) -> bool {
        match self.cmd {
            Cmd::Worker => self.runs.join(format!(".{NAME}.read")).exists(),
            Cmd::Pipeline => self.runs.join(format!("{PIPE_WORKER}.build.md")).exists(),
        }
    }

    fn report_path(&self) -> PathBuf {
        let w = if self.cmd == Cmd::Worker {
            NAME
        } else {
            PIPE_WORKER
        };
        self.runs.join(format!("{w}.md"))
    }

    /// The worker finishing its turn: the report lands atomically (tmp + mv),
    /// whether or not anything is still watching.
    fn release_worker(&self) {
        let tmp = self.runs.join(".fake-worker.tmp");
        let body = if self.cmd == Cmd::Worker {
            REPORT
        } else {
            PIPE_REPORT
        };
        fs::write(&tmp, body).unwrap();
        fs::rename(&tmp, self.report_path()).unwrap();
        fs::write(self.root.join("release"), "").unwrap();
    }

    fn state_field(&self, key: &str) -> String {
        let content = fs::read_to_string(self.runs.join(format!("pipeline-{TASK}.state")))
            .unwrap_or_default();
        let prefix = format!("{key}=");
        content
            .lines()
            .find_map(|l| l.strip_prefix(prefix.as_str()))
            .unwrap_or_default()
            .to_string()
    }

    /// The coordinator exits: SIGHUP to its process group, SIGTERM to the parent.
    fn coordinator_exits(&self, mut launcher: Child) {
        let (parent, pgid) = (launcher.id().to_string(), format!("-{}", launcher.id()));
        kill(&["-HUP", "--", &pgid]);
        kill(&["-TERM", &parent]);
        let _ = launcher.wait();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        // Only processes this test started: the watchers that polled its fake herdr.
        for pid in self.watchers() {
            let _ = Command::new("kill")
                .args(["-9", &pid.to_string()])
                .stderr(Stdio::null())
                .status();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn kill(args: &[&str]) {
    let st = Command::new("kill").args(args).status().expect("kill");
    assert!(st.success(), "kill {args:?} failed");
}

fn alive(pid: u32) -> bool {
    Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
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
    let mut child = f.launch("a");
    f.wait_for("the wait to start", || f.watching());
    f.release_worker();
    f.wait_for("acceptance", || f.accepted());
    assert!(child.wait().unwrap().success());
    assert_eq!(f.launcher_exit("a").as_deref(), Some("0"));
}

#[test]
fn test_pipeline_consumes_the_report_when_nobody_kills_it() {
    let f = Fixture::pipeline("pipe-control");
    let mut child = f.launch("a");
    f.wait_for("the pipeline to wait for its worker", || f.watching());
    f.release_worker();
    f.wait_for("the pipeline to consume the report", || f.accepted());
    assert!(child.wait().unwrap().success());
    assert_eq!(f.launcher_exit("a").as_deref(), Some("0"));
}

#[test]
fn test_prompt_survives_its_coordinator_exiting() {
    let f = Fixture::new("coordinator-exit");
    let launcher = f.launch("a");
    f.wait_for("the wait to start", || f.watching());
    f.coordinator_exits(launcher);

    // The worker finishes after its coordinator is gone.
    std::thread::sleep(Duration::from_secs(1));
    f.release_worker();
    f.wait_for("acceptance after the coordinator exit", || f.accepted());

    assert_turn_untouched(&f);
    assert_eq!(fs::read_to_string(f.report_path()).unwrap(), REPORT);
}

#[test]
fn test_pipeline_survives_its_coordinator_exiting() {
    let f = Fixture::pipeline("pipe-exit");
    let launcher = f.launch("a");
    f.wait_for("the pipeline to wait for its worker", || f.watching());
    f.coordinator_exits(launcher);

    std::thread::sleep(Duration::from_secs(1));
    f.release_worker();
    f.wait_for("the pipeline to consume the report", || f.accepted());
    let pfile = f.runs.join(format!("pipeline-{TASK}.md"));
    f.wait_for("the pipeline to record its outcome", || {
        fs::read_to_string(&pfile).is_ok_and(|c| c.contains("status: blocked"))
    });
    assert_turn_untouched(&f);
}

/// A `--resume` after the coordinator exit must join the live core.
fn assert_resume_joins_the_live_core(f: &Fixture) {
    let launcher = f.launch("a");
    f.wait_for("the wait to start", || f.watching());
    f.coordinator_exits(launcher);
    let cores = f.watchers();
    assert_eq!(cores.len(), 1, "expected one core before the resume");

    let mut second = f.spawn_direct();
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(
        f.watchers(),
        cores,
        "the resume started a second watcher instead of joining the live core"
    );
    assert!(
        second.try_wait().unwrap().is_none(),
        "the resume must keep following the live core"
    );

    f.release_worker();
    f.wait_for("acceptance", || f.accepted());
    let status = second.wait().unwrap();
    assert_eq!(status.code(), Some(0), "the core's exit code is mirrored");
}

#[test]
fn test_resume_joins_the_live_core_instead_of_starting_another() {
    let f = Fixture::new("resume-join");
    assert_resume_joins_the_live_core(&f);
}

#[test]
fn test_pipeline_resume_joins_the_live_core_and_leaves_state_alone() {
    let f = Fixture::pipeline("pipe-join");
    let launcher = f.launch("a");
    f.wait_for("the wait to start", || f.watching());
    f.coordinator_exits(launcher);
    let pid = f.state_field("pipeline_pid");
    let pfile_path = f.runs.join(format!("pipeline-{TASK}.md"));
    let before = fs::read_to_string(&pfile_path).unwrap();

    let mut second = f.spawn_direct();
    std::thread::sleep(Duration::from_secs(3));
    assert_eq!(f.watchers().len(), 1, "a second pipeline core was started");
    assert_eq!(f.state_field("pipeline_pid"), pid, "state was rewritten");
    assert_eq!(
        fs::read_to_string(&pfile_path).unwrap(),
        before,
        "pfile was appended"
    );

    f.release_worker();
    f.wait_for("the pipeline to consume the report", || f.accepted());
    assert_eq!(second.wait().unwrap().code(), Some(0));
}

/// Ctrl-C in the foreground leaves the core running, exits 130 and says how
/// to re-attach.
fn assert_ctrl_c_leaves_the_core(f: &Fixture, reattach: &str) {
    let mut fg = f.spawn_direct();
    f.wait_for("the wait to start", || f.watching());
    let core = {
        f.wait_for("the core to poll", || !f.watchers().is_empty());
        f.watchers()[0]
    };
    kill(&["-INT", &fg.id().to_string()]);
    let end = Instant::now() + Duration::from_secs(10);
    while fg.try_wait().unwrap().is_none() {
        assert!(Instant::now() < end, "the foreground ignored Ctrl-C");
        std::thread::sleep(Duration::from_millis(50));
    }
    let mut err = String::new();
    std::io::Read::read_to_string(fg.stderr.as_mut().unwrap(), &mut err).unwrap();
    assert_eq!(fg.wait().unwrap().code(), Some(130), "stderr: {err}");
    assert!(alive(core), "Ctrl-C must not stop the core");
    let lines: Vec<&str> = err
        .lines()
        .filter(|l| l.contains(&core.to_string()))
        .collect();
    assert_eq!(lines.len(), 1, "one line naming the core pid, got: {err}");
    assert!(
        lines[0].contains(reattach),
        "re-attach command in: {}",
        lines[0]
    );

    // The in-flight report is not lost.
    f.release_worker();
    f.wait_for("acceptance after Ctrl-C", || f.accepted());
}

#[test]
fn test_ctrl_c_leaves_the_prompt_core_running() {
    let f = Fixture::new("ctrl-c");
    assert_ctrl_c_leaves_the_core(&f, &format!("quaqua worker prompt --resume {NAME}"));
}

#[test]
fn test_ctrl_c_leaves_the_pipeline_core_running() {
    let f = Fixture::pipeline("pipe-ctrl-c");
    let repo = fs::canonicalize(&f.repo).unwrap();
    assert_ctrl_c_leaves_the_core(
        &f,
        &format!("quaqua pipeline --resume {TASK} {}", repo.display()),
    );
}
