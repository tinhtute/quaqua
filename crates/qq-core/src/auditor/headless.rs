//! The one-shot, read-only command line of a harness.
//!
//! This is per-harness behaviour and belongs in `driver/` (a `HarnessDriver`
//! method); it lives here only because `driver/` is outside this module's
//! remit. The harness comes from the driver's `herdr_kind()`, model and effort
//! from config via the driver's `launch_args`. Move this match into `driver/`
//! when the Supervisor (T63) absorbs scheduled audits.

/// A program and its argv, prompt included.
pub struct Headless {
    pub program: String,
    pub args: Vec<String>,
}

/// `None`: no headless form is known for this agent kind.
pub fn command(herdr_kind: &str, launch_args: Vec<String>, prompt: &str) -> Option<Headless> {
    match herdr_kind {
        "claude" => {
            let mut args = vec!["-p".to_string()];
            args.extend(launch_args);
            args.extend(
                [
                    "--permission-mode",
                    "dontAsk",
                    "--setting-sources",
                    "",
                    "--allowedTools",
                    "Read,Grep,Glob,Bash(git log:*),Bash(git diff:*),Bash(git show:*)",
                    "--disallowedTools",
                    "Bash(git *--output*)",
                    "--",
                ]
                .map(String::from),
            );
            args.push(prompt.to_string());
            Some(Headless {
                program: herdr_kind.to_string(),
                args,
            })
        }
        _ => None,
    }
}
