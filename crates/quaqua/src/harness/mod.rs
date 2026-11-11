//! Harness registry: the closed facts about each agent harness quaqua launches
//! (name, effort vocabulary, stop keys, idle detection) in one place, so adding
//! a harness edits this table instead of string lists across the crate.
//!
//! Launch args, auditor and usage CLI calls stay with their callers.

/// An agent harness quaqua can launch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    Claude,
    Opencode,
    Omp,
    Pi,
    Agy,
}

impl Harness {
    pub const ALL: [Harness; 5] = [
        Harness::Opencode,
        Harness::Omp,
        Harness::Claude,
        Harness::Pi,
        Harness::Agy,
    ];

    /// The harness named exactly `s` (team.toml `harness`, herdr `agent`).
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|h| h.as_str() == s)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Harness::Claude => "claude",
            Harness::Opencode => "opencode",
            Harness::Omp => "omp",
            Harness::Pi => "pi",
            Harness::Agy => "agy",
        }
    }

    /// The effort values this harness accepts. opencode effort is a model *variant*
    /// (`opencode --help` lists no `--model`; the interactive TUI reads the model
    /// from config as `provider/model#variant`, and the variant vocabulary is the
    /// `ReasoningEffort` set). omp's is `omp --help` `--thinking`. claude's is
    /// `claude --help` `--effort <level> (low, medium, high, xhigh, max)`.
    /// pi's is `pi --help` `--thinking` (pi 1.3.0). agy's is `agy --help`
    /// `--effort (low|medium|high|xhigh|max)` (agy 1.5.5).
    pub fn effort_allowed(self, effort: &str) -> bool {
        let allowed: &[&str] = match self {
            Harness::Opencode => &["none", "minimal", "low", "medium", "high", "xhigh", "max"],
            Harness::Omp => &[
                "off", "minimal", "low", "medium", "high", "xhigh", "max", "auto",
            ],
            Harness::Claude => &["low", "medium", "high", "xhigh", "max"],
            Harness::Pi => &["off", "minimal", "low", "medium", "high", "xhigh", "max"],
            Harness::Agy => &["low", "medium", "high", "xhigh", "max"],
        };
        allowed.contains(&effort)
    }

    /// Keys that interrupt a busy worker of this harness, in order — never
    /// `ctrl+c` (it kills opencode). `None` = no verified sequence.
    pub fn stop_keys(self) -> Option<&'static [&'static str]> {
        match self {
            Harness::Opencode => Some(&["esc", "esc"]),
            Harness::Omp => Some(&["esc"]),
            // pi: no interrupt sequence verified on a live pane yet (H3a).
            Harness::Claude | Harness::Pi => None,
            // agy: `esc` stops a running turn on a live pane ("Interrupted ·
            // What should Antigravity CLI do instead?").
            Harness::Agy => Some(&["esc"]),
        }
    }

    /// True when herdr's hook-based idle signal is not trusted for this harness,
    /// so the watchdog uses the longer fallback idle limit. pi reports
    /// working/blocked/idle through herdr's pi integration
    /// (`~/.pi/agent/extensions/herdr-agent-state.ts`, loaded with `-e`).
    /// agy has no lifecycle hook: its herdr integration
    /// (`~/.gemini/config/hooks/herdr-agent-state.sh`) only reports the
    /// conversation for resume, and state comes from herdr's screen detection.
    pub fn fallback_idle(self) -> bool {
        matches!(self, Harness::Opencode | Harness::Omp | Harness::Agy)
    }
}

/// True when `s` mentions any fallback-idle harness by name (substring match on
/// a kind or label, as the watchdog callers have always done).
pub fn mentions_fallback_idle(s: &str) -> bool {
    Harness::ALL
        .into_iter()
        .any(|h| h.fallback_idle() && s.contains(h.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_round_trips_every_harness() {
        for h in Harness::ALL {
            assert_eq!(Harness::parse(h.as_str()), Some(h));
        }
        assert_eq!(Harness::parse("aider"), None);
        assert_eq!(Harness::parse(""), None);
        assert_eq!(Harness::parse("Claude"), None);
    }

    #[test]
    fn effort_sets_per_harness() {
        assert!(Harness::Opencode.effort_allowed("none"));
        assert!(!Harness::Opencode.effort_allowed("auto"));
        assert!(Harness::Omp.effort_allowed("auto"));
        assert!(Harness::Omp.effort_allowed("off"));
        assert!(Harness::Claude.effort_allowed("xhigh"));
        assert!(!Harness::Claude.effort_allowed("minimal"));
        assert!(Harness::Pi.effort_allowed("max"));
        assert!(Harness::Pi.effort_allowed("off"));
        assert!(!Harness::Pi.effort_allowed("auto"));
        assert!(!Harness::Pi.effort_allowed("none"));
        assert!(Harness::Agy.effort_allowed("high"));
        assert!(Harness::Agy.effort_allowed("max"));
        assert!(!Harness::Agy.effort_allowed("off"));
        assert!(!Harness::Agy.effort_allowed("minimal"));
        assert!(!Harness::Agy.effort_allowed("auto"));
    }

    #[test]
    fn fallback_idle_is_opencode_omp_and_agy() {
        assert!(Harness::Opencode.fallback_idle());
        assert!(Harness::Omp.fallback_idle());
        assert!(!Harness::Claude.fallback_idle());
        assert!(!Harness::Pi.fallback_idle());
        assert!(Harness::Agy.fallback_idle());
        assert!(Harness::Pi.stop_keys().is_none());
        assert_eq!(Harness::Agy.stop_keys(), Some(&["esc"][..]));
        assert!(mentions_fallback_idle("opencode"));
        assert!(mentions_fallback_idle("x-omp-y"));
        assert!(mentions_fallback_idle("agy"));
        assert!(!mentions_fallback_idle("tieude"));
        assert!(!mentions_fallback_idle("claude"));
    }
}
