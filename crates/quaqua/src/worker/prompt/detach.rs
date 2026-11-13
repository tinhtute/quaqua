//! Run the prompt's wait in its own session so it outlives its coordinator.
//!
//! A coordinator runs `quaqua worker prompt` as a job of its own session; when
//! that session ends, the job's process group is signalled and the wait dies
//! mid-turn. The CLI therefore re-executes itself in a new session (stdio to
//! logs under `runs/`, never a pipe the dying foreground could break) and only
//! follows it: it mirrors the core's output and exit code, so a live caller
//! sees what it always saw, and a dead one leaves the core to deliver.
//!
//! The core records its pid (`.<name>.prompt.pid`) while it runs and its exit
//! code (`.<name>.prompt.exit`) when it ends. A `--resume` that finds a live
//! core follows that one instead of starting a second watcher on the same
//! report.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::Duration;

/// Set in the detached core so it runs the prompt instead of detaching again.
pub const ENV: &str = "QUAQUA_PROMPT_DETACHED";

extern "C" {
    fn setsid() -> i32;
    fn kill(pid: i32, sig: i32) -> i32;
    fn signal(signum: i32, handler: extern "C" fn(i32)) -> usize;
    fn flock(fd: i32, op: i32) -> i32;
}

const LOCK_EX: i32 = 2;
/// Exit code of a follower whose core died without leaving an exit record
/// (SIGKILL, OOM, panic): distinct from 0 and from the 1 of a failed prompt.
pub const EXIT_CORE_DIED: i32 = 8;
/// The status a Rust process exits with when it panics: it leaves no exit
/// record, so a parent that reaps it reports it like any other dead core.
const EXIT_PANIC: i32 = 101;

const SIGINT: i32 = 2;
/// The conventional exit code of a process ended by Ctrl-C.
const EXIT_INTERRUPTED: i32 = 130;

static INTERRUPTED: AtomicBool = AtomicBool::new(false);

extern "C" fn on_sigint(_: i32) {
    INTERRUPTED.store(true, Ordering::SeqCst);
}

/// Where the detached core's stdout (`out`) and stderr (`err`) are written.
pub fn log_path(runs_dir: &Path, name: &str, stream: &str) -> PathBuf {
    runs_dir.join(format!(".{name}.prompt.{stream}"))
}

/// The live core's pid.
pub fn pid_path(runs_dir: &Path, name: &str) -> PathBuf {
    runs_dir.join(format!(".{name}.prompt.pid"))
}

/// The finished core's exit code.
pub fn exit_path(runs_dir: &Path, name: &str) -> PathBuf {
    runs_dir.join(format!(".{name}.prompt.exit"))
}

fn pid_alive(pid: i32) -> bool {
    // SAFETY: signal 0 only probes; pid > 0 never addresses a group.
    pid > 0 && unsafe { kill(pid, 0) } == 0
}

/// The pid of the core that is running for `name`, if any. A pidfile whose
/// process is gone (a core killed before it could clean up) is not a core.
pub fn live_core(runs_dir: &Path, name: &str) -> Option<i32> {
    let pid = std::fs::read_to_string(pid_path(runs_dir, name))
        .ok()?
        .trim()
        .parse()
        .ok()?;
    pid_alive(pid).then_some(pid)
}

static CORE: OnceLock<(PathBuf, String)> = OnceLock::new();

fn lock_path(runs_dir: &Path, name: &str) -> PathBuf {
    runs_dir.join(format!(".{name}.prompt.lock"))
}

/// Holds the exclusive lock that makes "is a core live? else start one" atomic
/// between processes. Released when dropped (the fd closes).
struct StartLock(#[allow(dead_code)] Option<File>);

/// A filesystem that cannot lock (NFS without lockd: ENOLCK; no flock: EOPNOTSUPP).
fn lock_unsupported(e: &std::io::Error) -> bool {
    matches!(e.raw_os_error(), Some(37) | Some(95))
}

impl StartLock {
    /// Blocks until held. Where locking is unsupported it warns and proceeds
    /// unlocked: still detached, only the simultaneous-resume race is open.
    fn take(runs_dir: &Path, name: &str) -> std::io::Result<StartLock> {
        use std::os::fd::AsRawFd;
        let f = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(lock_path(runs_dir, name))?;
        // SAFETY: the fd is open for the lifetime of `f`.
        if unsafe { flock(f.as_raw_fd(), LOCK_EX) } != 0 {
            let e = std::io::Error::last_os_error();
            if lock_unsupported(&e) {
                eprintln!("WATCHDOG start-lock-unsupported | name={name} {e} — starting unlocked");
                return Ok(StartLock(None));
            }
            return Err(e);
        }
        Ok(StartLock(Some(f)))
    }
}

/// Called by the detached core as it starts: publish its pid.
pub fn core_begin(runs_dir: &Path, name: &str) {
    let _ = std::fs::remove_file(exit_path(runs_dir, name));
    let _ = std::fs::write(
        pid_path(runs_dir, name),
        format!("{}\n", std::process::id()),
    );
    let _ = CORE.set((runs_dir.to_path_buf(), name.to_string()));
}

/// Called by the detached core with its result, before it returns it: record
/// the exit code for a follower that is not the core's parent, then retire the
/// pidfile. Liveness is the pid, not the file, so output the caller prints
/// after this is still seen.
pub fn core_end(res: &Result<(), (i32, String)>) {
    if let Some((runs_dir, name)) = CORE.get() {
        let code = res.as_ref().err().map_or(0, |(c, _)| *c);
        let _ = std::fs::write(exit_path(runs_dir, name), format!("{code}\n"));
        let _ = std::fs::remove_file(pid_path(runs_dir, name));
    }
}

fn open_log(path: &Path) -> std::io::Result<File> {
    OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
}

/// Mirrors whatever was appended to `file` since the last call.
struct Tail {
    file: File,
    pos: u64,
}

impl Tail {
    fn new(path: &Path) -> std::io::Result<Self> {
        Ok(Tail {
            file: File::open(path)?,
            pos: 0,
        })
    }

    fn pump(&mut self, to: &mut dyn Write) {
        let mut buf = Vec::new();
        if self.file.seek(SeekFrom::Start(self.pos)).is_ok() {
            if let Ok(n) = self.file.read_to_end(&mut buf) {
                self.pos += n as u64;
                let _ = to.write_all(&buf);
                let _ = to.flush();
            }
        }
    }
}

/// Follow a core: mirror its logs and return its exit code. `child` is the
/// core when this process started it; otherwise it is a core someone else
/// started, found by pid, whose exit code it reads from the file the core left.
fn follow(
    runs_dir: &Path,
    name: &str,
    pid: i32,
    reattach: &str,
    mut child: Option<Child>,
) -> std::io::Result<i32> {
    let (mut out_tail, mut err_tail) = (
        Tail::new(&log_path(runs_dir, name, "out"))?,
        Tail::new(&log_path(runs_dir, name, "err"))?,
    );
    let (mut stdout, mut stderr) = (std::io::stdout(), std::io::stderr());
    // Ctrl-C ends this tail only. The core is in a session of its own and
    // keeps the in-flight report; say so and how to get back to it.
    // SAFETY: the handler only stores to an atomic.
    unsafe { signal(SIGINT, on_sigint) };
    loop {
        if INTERRUPTED.load(Ordering::SeqCst) {
            out_tail.pump(&mut stdout);
            err_tail.pump(&mut stderr);
            eprintln!("Ctrl-C: core pid {pid} keeps running; re-attach with `{reattach}`");
            return Ok(EXIT_INTERRUPTED);
        }
        let done = match child.as_mut() {
            // A core killed by a signal never recorded a result.
            Some(c) => c.try_wait()?.map(|s| match s.code() {
                Some(EXIT_PANIC) | None => Err(()),
                Some(code) => Ok(code),
            }),
            None if pid_alive(pid) => None,
            None => Some(
                std::fs::read_to_string(exit_path(runs_dir, name))
                    .ok()
                    .and_then(|s| s.trim().parse().ok())
                    .ok_or(()),
            ),
        };
        out_tail.pump(&mut stdout);
        err_tail.pump(&mut stderr);
        match done {
            Some(Ok(code)) => return Ok(code),
            Some(Err(())) => {
                eprintln!(
                    "core pid {pid} died without an exit record (killed?); the worker may still be running — re-run `{reattach}` to start a new wait"
                );
                return Ok(EXIT_CORE_DIED);
            }
            None => {}
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Run this command's wait in a new session and follow it, or, when
/// `attach_if_live` and a core is already running for `name`, follow that one.
/// `reattach` is the command Ctrl-C tells the operator to run to follow the core
/// again. `Ok(code)` is the core's exit code; `Err` means no core could be followed
/// or started, and the caller falls back to waiting in-process.
pub fn run(
    runs_dir: &Path,
    name: &str,
    reattach: &str,
    attach_if_live: bool,
) -> std::io::Result<i32> {
    std::fs::create_dir_all(runs_dir)?;
    // Check-and-start under one lock, or two resumes at the same instant both
    // miss the core the other is starting.
    let lock = StartLock::take(runs_dir, name)?;
    if attach_if_live {
        if let Some(pid) = live_core(runs_dir, name) {
            drop(lock);
            if let Ok(code) = follow(runs_dir, name, pid, reattach, None) {
                return Ok(code);
            }
            return run(runs_dir, name, reattach, false);
        }
    }
    let (out_path, err_path) = (
        log_path(runs_dir, name, "out"),
        log_path(runs_dir, name, "err"),
    );
    let (out, err) = (open_log(&out_path)?, open_log(&err_path)?);
    let mut cmd = Command::new(std::env::current_exe()?);
    cmd.args(std::env::args_os().skip(1))
        .env(ENV, "1")
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err);
    // SAFETY: `setsid` is async-signal-safe and touches no Rust state.
    unsafe {
        cmd.pre_exec(|| {
            if setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = cmd.spawn()?;
    let pid = child.id() as i32;
    // Publish the pid before the lock goes: the core's own `core_begin` comes
    // later, after its startup.
    let _ = std::fs::remove_file(exit_path(runs_dir, name));
    let _ = std::fs::write(pid_path(runs_dir, name), format!("{pid}\n"));
    drop(lock);
    follow(runs_dir, name, pid, reattach, Some(child))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detach_lock_unsupported_errnos_proceed_unlocked() {
        // ENOLCK and EOPNOTSUPP mean "cannot lock here"; anything else is a real error.
        assert!(lock_unsupported(&std::io::Error::from_raw_os_error(37)));
        assert!(lock_unsupported(&std::io::Error::from_raw_os_error(95)));
        assert!(!lock_unsupported(&std::io::Error::from_raw_os_error(9)));
    }

    #[test]
    fn test_detach_parent_reports_a_panicking_core_as_died() {
        let dir = std::env::temp_dir().join(format!("qq-detach-panic-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for s in ["out", "err"] {
            std::fs::write(log_path(&dir, "w", s), "").unwrap();
        }
        let child = Command::new("sh").args(["-c", "exit 101"]).spawn().unwrap();
        let pid = child.id() as i32;
        let code = follow(&dir, "w", pid, "re", Some(child)).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(code, EXIT_CORE_DIED);
    }
}
