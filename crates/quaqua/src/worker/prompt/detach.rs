//! Run the prompt's wait in its own session so it outlives its coordinator.
//!
//! A coordinator runs `quaqua worker prompt` as a job of its own session; when
//! that session ends, the job's process group is signalled and the wait dies
//! mid-turn. The CLI therefore re-executes itself in a new session (stdio to
//! logs under `runs/`, never a pipe the dying foreground could break) and only
//! follows it: it mirrors the core's output and exit code, so a live caller
//! sees what it always saw, and a dead one leaves the core to deliver.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

/// Set in the detached core so it runs the prompt instead of detaching again.
pub const ENV: &str = "QUAQUA_PROMPT_DETACHED";

extern "C" {
    fn setsid() -> i32;
}

/// Where the detached core's stdout (`out`) and stderr (`err`) are written.
pub fn log_path(runs_dir: &Path, name: &str, stream: &str) -> PathBuf {
    runs_dir.join(format!(".{name}.prompt.{stream}"))
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

/// Re-execute this command in a new session and follow it. `Ok(code)` is the
/// core's exit code; `Err` means it could not be started, and the caller
/// falls back to waiting in-process.
pub fn run(runs_dir: &Path, name: &str) -> std::io::Result<i32> {
    std::fs::create_dir_all(runs_dir)?;
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
    let mut child = cmd.spawn()?;
    let (mut out_tail, mut err_tail) = (Tail::new(&out_path)?, Tail::new(&err_path)?);
    let (mut stdout, mut stderr) = (std::io::stdout(), std::io::stderr());
    loop {
        let status = child.try_wait()?;
        out_tail.pump(&mut stdout);
        err_tail.pump(&mut stderr);
        if let Some(status) = status {
            // A core killed by a signal is a failed wait, not a success.
            return Ok(status.code().unwrap_or(1));
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}
