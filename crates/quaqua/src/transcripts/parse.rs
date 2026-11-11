//! JSONL reader for Claude Code session transcripts.
//!
//! One line of `<projects>/<project>/<session>.jsonl` is one entry. Only two
//! kinds carry conversation text a CoS would search: `type: "user"` and
//! `type: "assistant"`, whose `message.content` is either a string or an array
//! of blocks. Everything else in the file (`attachment`, `system`, `mode`,
//! `file-history-snapshot`, …) is bookkeeping and is skipped.
//!
//! One rule decides what counts as a message, and the index and `tail` share
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

/// One message worth indexing: the entry's own line number, its role, and the
/// text blocks joined by a newline.
#[derive(Debug, Clone)]
pub struct Entry {
    /// 1-based line number in the source file, for `path:line` anchors.
    pub line: u64,
    /// `user` or `assistant`.
    pub role: &'static str,
    /// The entry's ISO-8601 `timestamp`, or `""` when it has none.
    pub ts: String,
    /// The entry's `cwd`, or `""`.
    pub cwd: String,
    /// The entry's `sessionId`, falling back to the file stem.
    pub session: String,
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
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(256 * 1024, file);
    let mut buf = Vec::new();
    let mut scan = Scan::default();
    let mut line_no = 0u64;
    loop {
        buf.clear();
        if reader.read_until(b'\n', &mut buf)? == 0 {
            break;
        }
        line_no += 1;
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
                if let Some(entry) = entry_from(&v, line_no, &stem) {
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
fn entry_from(v: &Value, line: u64, stem: &str) -> Option<Entry> {
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
        line,
        role,
        ts: v
            .get("timestamp")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        cwd: v
            .get("cwd")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        session: v
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or(stem)
            .to_string(),
        text,
        sidechain: v
            .get("isSidechain")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(tag: &str, body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "quaqua-transcripts-parse-{tag}-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s1.jsonl");
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn a_string_content_is_a_user_message() {
        let p = write(
            "str",
            r#"{"type":"user","sessionId":"s1","timestamp":"2026-11-01T10:00:00Z","cwd":"/w","message":{"role":"user","content":"hello there"}}"#,
        );
        let scan = read_entries(&p).unwrap();
        assert_eq!(scan.entries.len(), 1);
        assert_eq!(scan.entries[0].text, "hello there");
        assert_eq!(scan.entries[0].role, "user");
        assert_eq!(scan.entries[0].line, 1);
        assert_eq!(scan.entries[0].ts, "2026-11-01T10:00:00Z");
        assert_eq!(scan.entries[0].cwd, "/w");
        assert_eq!(scan.entries[0].session, "s1");
        assert!(!scan.entries[0].sidechain);
    }

    #[test]
    fn tool_results_thinking_and_tool_use_are_not_text() {
        let p = write(
            "blocks",
            concat!(
                r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","content":"x"}]}}"#,
                "\n",
                r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"hmm"},{"type":"tool_use","name":"Read"},{"type":"text","text":"done"}]}}"#,
            ),
        );
        let scan = read_entries(&p).unwrap();
        assert_eq!(scan.entries.len(), 1, "only the assistant text survives");
        assert_eq!(scan.entries[0].role, "assistant");
        assert_eq!(scan.entries[0].text, "done");
        assert_eq!(scan.entries[0].line, 2);
    }

    #[test]
    fn a_user_entry_with_a_tool_result_is_dropped_whole() {
        let p = write(
            "mixed",
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"and?"},{"type":"tool_result","content":"x"}]}}"#,
        );
        let scan = read_entries(&p).unwrap();
        assert!(scan.entries.is_empty());
    }

    #[test]
    fn bookkeeping_entries_and_bad_lines_are_skipped() {
        let p = write(
            "bad",
            concat!(
                r#"{"type":"attachment","attachment":{"type":"x"}}"#,
                "\n",
                r#"{"type":"mode","mode":"normal"}"#,
                "\n",
                "{\"type\":\"user\",\"message\":{\"role\":\"user\",\"content\":\"trunc",
                "\n",
                r#"{"type":"user","message":{"role":"user","content":"kept"}}"#,
            ),
        );
        let scan = read_entries(&p).unwrap();
        assert_eq!(scan.entries.len(), 1);
        assert_eq!(scan.entries[0].text, "kept");
        assert_eq!(scan.entries[0].line, 4);
        assert_eq!(scan.unparseable, 1);
    }

    #[test]
    fn harness_wrappers_are_not_user_speech() {
        let p = write(
            "injected",
            concat!(
                r#"{"type":"user","message":{"role":"user","content":"<system-reminder>be careful</system-reminder>"}}"#,
                "\n",
                r#"{"type":"user","message":{"role":"user","content":"<task-notification>worker done</task-notification>"}}"#,
                "\n",
                r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"<local-command-stdout>ok</local-command-stdout>"},{"type":"text","text":"the real question"}]}}"#,
                "\n",
                r#"{"type":"user","message":{"role":"user","content":"[Request interrupted by user]"}}"#,
                "\n",
                r#"{"type":"user","message":{"role":"user","content":"<pasted_content id=\"x\">kept</pasted_content>"}}"#,
            ),
        );
        let scan = read_entries(&p).unwrap();
        let texts: Vec<&str> = scan.entries.iter().map(|e| e.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                "the real question",
                "<pasted_content id=\"x\">kept</pasted_content>"
            ],
            "only harness wrappers are dropped; pasted content is the Principal's"
        );
    }

    #[test]
    fn sidechain_entries_are_flagged_not_dropped() {
        let p = write(
            "side",
            r#"{"type":"user","isSidechain":true,"message":{"role":"user","content":"sub"}}"#,
        );
        let scan = read_entries(&p).unwrap();
        assert_eq!(scan.entries.len(), 1);
        assert!(scan.entries[0].sidechain);
    }
}
