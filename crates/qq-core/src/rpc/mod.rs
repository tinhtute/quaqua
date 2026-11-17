//! Daemon RPC: one Unix socket per workspace, one JSON object per line.
//!
//! A connection carries any number of request lines; each gets exactly one
//! response line, in order. A request is refused or answered immediately.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const SOCKET_FILE: &str = "qq.sock";

/// Longest socket path used as is. `sun_path` holds 108 bytes on Linux and
/// 104 on macOS; stay under both with room for the NUL.
pub const MAX_SOCKET_PATH: usize = 100;

/// Refusal code for a verb the daemon has no handler for.
pub const UNKNOWN_VERB: &str = "unknown_verb";
/// Refusal code for a request line that is not a request object.
pub const BAD_REQUEST: &str = "bad_request";

/// One request line. `caller` is carried for the authority check that a later
/// task adds; this layer does not verify it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub verb: String,
    #[serde(default)]
    pub args: Value,
    #[serde(default)]
    pub caller: Option<String>,
    /// The pane the daemon mapped the peer to. Set only by the daemon, never
    /// read off the wire; used for placement, never as authority.
    #[serde(skip)]
    pub caller_pane: Option<String>,
}

impl Request {
    pub fn new(verb: &str, args: Value) -> Self {
        Request {
            verb: verb.to_string(),
            args,
            caller: None,
            caller_pane: None,
        }
    }
}

/// A refusal: a stable `code` callers branch on and a human `message`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RpcError {
    pub code: String,
    pub message: String,
}

impl RpcError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        RpcError {
            code: code.to_string(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for RpcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for RpcError {}

/// One response line: `{"ok":true,"result":..}` or `{"ok":false,"error":{..}}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    #[serde(default)]
    pub result: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

impl Response {
    pub fn ok(result: Value) -> Self {
        Response {
            ok: true,
            result,
            error: None,
        }
    }

    pub fn err(e: RpcError) -> Self {
        Response {
            ok: false,
            result: Value::Null,
            error: Some(e),
        }
    }

    pub fn into_result(self) -> Result<Value, RpcError> {
        match (self.ok, self.error) {
            (true, _) => Ok(self.result),
            (false, Some(e)) => Err(e),
            (false, None) => Err(RpcError::new("unknown", "failure without an error")),
        }
    }
}

/// The socket for a runs dir: `<runs>/qq.sock`, or a short `/tmp` path keyed by
/// the runs dir when that would not fit `sun_path`. Pure: creates nothing.
pub fn socket_path(runs_dir: &Path) -> PathBuf {
    let direct = runs_dir.join(SOCKET_FILE);
    if direct.as_os_str().len() <= MAX_SOCKET_PATH {
        return direct;
    }
    let canon = runs_dir
        .canonicalize()
        .unwrap_or_else(|_| runs_dir.to_path_buf());
    // FNV-1a: stable across runs and processes, unlike `DefaultHasher`.
    let mut h: u64 = 0xcbf29ce484222325;
    for b in canon.as_os_str().as_encoded_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x100000001b3);
    }
    PathBuf::from(format!("/tmp/qq-{h:016x}.sock"))
}

/// Reads one line; `Ok(None)` at end of stream.
pub fn read_line<R: BufRead>(r: &mut R) -> io::Result<Option<String>> {
    let mut line = String::new();
    if r.read_line(&mut line)? == 0 {
        return Ok(None);
    }
    Ok(Some(line.trim_end_matches(['\n', '\r']).to_string()))
}

/// Writes `v` as one line.
pub fn write_line<W: Write, T: Serialize>(w: &mut W, v: &T) -> io::Result<()> {
    let mut s = serde_json::to_string(v).map_err(io::Error::other)?;
    s.push('\n');
    w.write_all(s.as_bytes())?;
    w.flush()
}

/// Client side: one request, one response, then the connection closes. The
/// error kind is `NotFound` or `ConnectionRefused` when no daemon listens.
pub fn call(socket: &Path, req: &Request) -> io::Result<Response> {
    let mut stream = UnixStream::connect(socket)?;
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    write_line(&mut stream, req)?;
    let mut reader = BufReader::new(stream);
    let line = read_line(&mut reader)?
        .ok_or_else(|| io::Error::new(io::ErrorKind::UnexpectedEof, "daemon closed the socket"))?;
    serde_json::from_str(&line).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_runs_dir_keeps_socket_inside_it() {
        assert_eq!(
            socket_path(Path::new("/w/runs")),
            PathBuf::from("/w/runs/qq.sock")
        );
    }

    #[test]
    fn long_runs_dir_falls_back_to_short_tmp_path_stable_per_dir() {
        let long = PathBuf::from(format!("/nonexistent/{}", "d".repeat(120)));
        let a = socket_path(&long);
        assert!(a.starts_with("/tmp"));
        assert!(a.as_os_str().len() <= MAX_SOCKET_PATH);
        assert_eq!(a, socket_path(&long));
        assert_ne!(a, socket_path(&long.join("other")));
    }
}
