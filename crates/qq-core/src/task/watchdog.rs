//! What the watchdog does when a worker's provider fails or the worker dies:
//! how many resumes an error earns, and the stub report a dead worker leaves
//! so `runs/<name>.md` always exists for whoever reads it next.

use std::path::Path;

use super::prompt::{Exit, ProviderError};

/// The stub's marker line. A stub is a death notice, not a report: the next
/// `worker prompt` (a fresh brief or `--resume`) removes it before it waits,
/// so it can never stand in for the report of the worker it resumes.
const MARKER: &str = "Stub: quaqua watchdog";

/// Resumes (a `continue` after a backoff) a provider error earns before a
/// human is asked. A transient error gets the full ladder, or one resume when
/// the harness already spent its own. An API error nobody could name is not
/// known to be fatal, so it gets one. A named fatal (signed out, quota,
/// billing) gets none: a resume repeats it.
pub fn resume_cap(error: ProviderError, harness_exhausted: bool, retry_max: u32) -> u32 {
    match error {
        ProviderError::Retryable(_) if !harness_exhausted => retry_max,
        ProviderError::Retryable(_) | ProviderError::Fatal("api-error") => 1,
        ProviderError::Fatal(_) => 0,
    }
}

/// Exits that mean the worker is dead (not slow): a provider error that the
/// resume ladder could not clear, and a core that died without an end record.
fn is_death(exit: Exit) -> bool {
    matches!(exit, Exit::ProviderError | Exit::CoreDied)
}

/// Leaves `runs/<name>.md` naming the cause when the worker died without a
/// report. Never overwrites one: a file that is already there is the worker's.
/// `tail` is the pane tail block the caller already printed.
pub fn write_death_stub(runs: &Path, name: &str, exit: Exit, tail: &str) {
    if !is_death(exit) {
        return;
    }
    let cause = match exit {
        Exit::ProviderError => "provider error (resumes spent or not allowed)",
        _ => "the agent is gone without an end record",
    };
    let body = format!(
        "Outcome: BLOCKED\n## Changes\nnone recorded: the worker died before it wrote a report.\n## Verify\nnot run.\n## Risks\nWatchdog exit {} for {name}: {cause}. The worktree may hold unreported work.\n\n{tail}\n## Lessons\nnone\n\n{MARKER}\n",
        exit.code()
    );
    let dest = runs.join(format!("{name}.md"));
    let tmp = runs.join(format!(".{name}.stub.{}", std::process::id()));
    if std::fs::write(&tmp, body).is_ok() {
        // hard_link fails when the report exists: an atomic no-clobber publish
        let _ = std::fs::hard_link(&tmp, &dest);
        let _ = std::fs::remove_file(&tmp);
    }
}

/// Removes `runs/<name>.md` when it is a watchdog stub (and only then).
pub fn clear_stub(runs: &Path, name: &str) {
    let path = runs.join(format!("{name}.md"));
    if std::fs::read_to_string(&path).is_ok_and(|t| t.lines().any(|l| l == MARKER)) {
        let _ = std::fs::remove_file(path);
    }
}
