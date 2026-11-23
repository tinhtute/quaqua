//! One way to run a child process with a time limit. A child that outlives
//! the limit is killed and the call returns [`io::ErrorKind::TimedOut`], so a
//! hung `herdr`, `gh` or `git` cannot block its caller for good.

use std::io::{self, Read};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

/// How long to wait for the output readers after the child is gone. A
/// grandchild that inherited the pipes can hold them open past the kill.
const DRAIN_GRACE: Duration = Duration::from_millis(500);

/// Spawns `cmd` with stdin closed and stdout and stderr captured, and waits at
/// most `limit`. On expiry the child is killed and reaped and the error kind is
/// `TimedOut`. Pipes are drained on threads so a large output cannot stall the
/// child.
pub fn output_within(cmd: &mut Command, limit: Duration) -> io::Result<Output> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut p) = pipe {
                let _ = p.read_to_end(&mut buf);
            }
            let _ = tx.send(buf);
        });
        rx
    };
    let out_rx = drain(child.stdout.take().map(|p| Box::new(p) as _));
    let err_rx = drain(child.stderr.take().map(|p| Box::new(p) as _));

    let deadline = Instant::now() + limit;
    let status = loop {
        match child.try_wait()? {
            Some(s) => break Some(s),
            None if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(10)),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let Some(status) = status else {
        return Err(io::Error::new(
            io::ErrorKind::TimedOut,
            format!("child did not finish within {}s", limit.as_secs_f32()),
        ));
    };
    let stdout = out_rx.recv_timeout(DRAIN_GRACE).unwrap_or_default();
    let stderr = err_rx.recv_timeout(DRAIN_GRACE).unwrap_or_default();
    Ok(Output {
        status,
        stdout,
        stderr,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str) -> Command {
        let mut c = Command::new("sh");
        c.args(["-c", script]);
        c
    }

    #[test]
    fn captures_output_and_status() {
        let o = output_within(&mut sh("echo out; echo err >&2; exit 3"), Duration::from_secs(10))
            .unwrap();
        assert_eq!(o.status.code(), Some(3));
        assert_eq!(o.stdout, b"out\n");
        assert_eq!(o.stderr, b"err\n");
    }

    #[test]
    fn a_large_output_does_not_stall_the_child() {
        let o = output_within(
            &mut sh("head -c 1000000 /dev/zero"),
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!(o.stdout.len(), 1_000_000);
    }

    #[test]
    fn a_hung_child_is_killed_at_the_limit() {
        let t = Instant::now();
        let e = output_within(&mut sh("sleep 60"), Duration::from_millis(300)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::TimedOut);
        assert!(t.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn a_missing_program_is_a_spawn_error() {
        let e = output_within(&mut Command::new("qq-no-such-bin"), Duration::from_secs(1))
            .unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::NotFound);
    }
}
