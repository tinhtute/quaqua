//! The grid lock: one placement lists and claims panes at a time, across
//! threads and processes. A directory made with `mkdir` plus a pid file; a
//! lock whose holder is gone is reclaimed.

use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const WAIT: Duration = Duration::from_secs(15);
const POLL: Duration = Duration::from_millis(10);

pub struct GridLock {
    dir: PathBuf,
}

fn alive(pid: &str) -> bool {
    Command::new("kill")
        .args(["-0", pid])
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(true)
}

impl GridLock {
    /// Takes `<dir>/grid.lock`, waiting up to 15 s for another holder.
    pub fn take(dir: &Path) -> io::Result<GridLock> {
        std::fs::create_dir_all(dir)?;
        let lock = dir.join("grid.lock");
        let start = Instant::now();
        loop {
            match std::fs::create_dir(&lock) {
                Ok(()) => {
                    std::fs::write(lock.join("pid"), std::process::id().to_string())?;
                    return Ok(GridLock { dir: lock });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    let holder = std::fs::read_to_string(lock.join("pid")).unwrap_or_default();
                    let holder = holder.trim();
                    let own = holder == std::process::id().to_string();
                    if !holder.is_empty() && !own && !alive(holder) {
                        let _ = std::fs::remove_file(lock.join("pid"));
                        let _ = std::fs::remove_dir(&lock);
                        continue;
                    }
                    if start.elapsed() > WAIT {
                        return Err(io::Error::other("grid lock busy"));
                    }
                    std::thread::sleep(POLL);
                }
                Err(e) => return Err(e),
            }
        }
    }
}

impl Drop for GridLock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(self.dir.join("pid"));
        let _ = std::fs::remove_dir(&self.dir);
    }
}
