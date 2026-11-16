//! JSONL reader for Claude Code session transcripts.
//!
//! One line of `<projects>/<project>/<session>.jsonl` is one entry. Only two
//! kinds carry conversation text a CoS would search: `type: "user"` and
//! `type: "assistant"`, whose `message.content` is either a string or an array
//! of blocks. Everything else in the file (`attachment`, `system`, `mode`,
//! `file-history-snapshot`, …) is bookkeeping and is skipped.
//!
//! One rule decides what counts as a message, and `tail` uses
//! it: `thinking`, `tool_use` and `tool_result` blocks are never text, and a
//! `user` entry carrying any `tool_result` block is a tool round-trip rather
//! than something the Principal said, so it is dropped whole.
//!
//! A session being written right now can end mid-line, so an unparseable line
//! is counted and skipped — never fatal.

use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;

use serde_json::Value;

/// One message worth reading: its role and the
/// text blocks joined by a newline.
#[derive(Debug, Clone)]
pub struct Entry {
    /// `user` or `assistant`.
    pub role: &'static str,
    /// The `text` blocks joined by `\n`.
    pub text: String,
    /// `isSidechain == true`: a subagent's entry, not the main conversation.
    pub sidechain: bool,
}

/// The messages of one file plus the count of lines that would not parse.
#[derive(Debug, Default)]
pub struct Scan {
    pub entries: Vec<Entry>,
    pub unparseable: u64,
}

/// Reads every message of `path`, streaming the file.
///
/// The buffer holds one line at a time: the largest live session is ~88 MB, so
/// nothing here may depend on reading a file whole.
pub fn read_entries(path: &Path) -> std::io::Result<Scan> {
    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(256 * 1024, file);
    let mut buf = Vec::new();
    let mut scan = Scan::default();
    loop {
        buf.clear();
        if reader.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        let raw = trim_eol(&buf);
        if raw.is_empty() {
            continue;
        }
        let text = String::from_utf8_lossy(raw);
        // Cheap pre-filter: a message entry always names its role somewhere, so
        // a line carrying neither word cannot be one. Kept as a word test (not
        // an exact `"type":"user"` match) so a writer that adds spaces between
        // JSON tokens cannot silently empty the index.
        if !text.contains("user") && !text.contains("assistant") {
            continue;
        }
        match serde_json::from_str::<Value>(&text) {
            Ok(v) => {
                if let Some(entry) = entry_from(&v) {
                    scan.entries.push(entry);
                }
            }
            Err(_) => scan.unparseable += 1,
        }
    }
    Ok(scan)
}

/// Strips the trailing `\n` and an optional `\r`.
fn trim_eol(buf: &[u8]) -> &[u8] {
    let mut end = buf.len();
    if end > 0 && buf[end - 1] == b'\n' {
        end -= 1;
    }
    if end > 0 && buf[end - 1] == b'\r' {
        end -= 1;
    }
    &buf[..end]
}

/// Harness-injected wrappers that arrive as `user` entries but are not
/// something the Principal said. The live corpus carries them in bulk
/// (`<task-notification>`, `<local-command-*>`, `<command-name>`, …), and
/// without this they dominate a search and crowd out the real prompts in a
/// `tail`. `<pasted_content>` is deliberately absent: the Principal pasted it.
const INJECTED_PREFIXES: &[&str] = &[
    "<system-reminder>",
    "<local-command-caveat>",
    "<local-command-stdout>",
    "<command-name>",
    "<command-message>",
    "<task-notification>",
    "<bash-input>",
    "<bash-stdout>",
    "<user-prompt-submit-hook>",
    "[Request interrupted by user",
];

/// True when a text block is one of the harness's own wrappers.
fn is_injected(text: &str) -> bool {
    let t = text.trim_start();
    INJECTED_PREFIXES.iter().any(|p| t.starts_with(p))
}

/// The message an entry carries, or `None` when it is not one.
fn entry_from(v: &Value) -> Option<Entry> {
    let ty = v.get("type")?.as_str()?;
    let msg = v.get("message")?;
    let role = match msg.get("role").and_then(Value::as_str) {
        Some("user") => "user",
        Some("assistant") => "assistant",
        _ => match ty {
            "user" => "user",
            "assistant" => "assistant",
            _ => return None,
        },
    };

    let mut text = String::new();
    let mut has_tool_result = false;
    match msg.get("content")? {
        Value::String(s) => {
            if role == "user" && is_injected(s) {
                return None;
            }
            text.push_str(s);
        }
        Value::Array(blocks) => {
            for block in blocks {
                match block.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(t) = block.get("text").and_then(Value::as_str) {
                            if role == "user" && is_injected(t) {
                                continue;
                            }
                            if !text.is_empty() {
                                text.push('\n');
                            }
                            text.push_str(t);
                        }
                    }
                    Some("tool_result") => has_tool_result = true,
                    _ => {}
                }
            }
        }
        _ => return None,
    }
    if text.trim().is_empty() {
        return None;
    }
    if role == "user" && has_tool_result {
        return None;
    }

    Some(Entry {
        role,
        text,
        sidechain: v
            .get("isSidechain")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}
