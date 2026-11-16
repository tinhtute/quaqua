//! Caller identity from the socket peer, never from the request.
//!
//! Data shape: the kernel's [`PeerCred`] `{pid, uid}` for the connected socket
//! -> [`PeerResolver::resolve`] walks the pid's ancestry to the herdr pane whose
//! shell or foreground process it is -> that pane's agent is the [`CallerId`].
//! Every step fails closed: an unmapped peer is refused, whatever the request
//! claims.

use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::sync::Arc;

use serde_json::Value;

use crate::herdr::{HerdrClient, Reply};
use crate::rpc::RpcError;

/// Refusal code for a peer that maps to no pane or no agent.
pub const UNMAPPED_PEER: &str = "unmapped_peer";

/// Longest ancestry walked from the peer pid.
const MAX_DEPTH: usize = 64;

/// Refusal code for a peer owned by another uid than the daemon.
pub const FOREIGN_UID: &str = "foreign_uid";

/// The daemon's own effective uid.
pub fn own_uid() -> u32 {
    extern "C" {
        fn geteuid() -> u32;
    }
    // SAFETY: geteuid takes no arguments and cannot fail.
    unsafe { geteuid() }
}

/// What the kernel says about the process on the other end of a socket.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerCred {
    pub pid: i32,
    pub uid: u32,
}

/// The agent name a request is attributed to; what `Request.caller` carries
/// once the daemon has replaced the client's claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallerId(pub String);

impl CallerId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Maps a peer to the caller it speaks as.
pub trait PeerResolver: Send + Sync {
    fn resolve(&self, cred: PeerCred) -> Result<CallerId, RpcError>;
}

#[repr(C)]
#[cfg(target_os = "linux")]
struct Ucred {
    pid: i32,
    uid: u32,
    gid: u32,
}

extern "C" {
    fn getsockopt(fd: i32, level: i32, name: i32, val: *mut std::ffi::c_void, len: *mut u32)
        -> i32;
    #[cfg(target_os = "macos")]
    fn getpeereid(fd: i32, uid: *mut u32, gid: *mut u32) -> i32;
}

/// `SO_PEERCRED` (Linux) / `LOCAL_PEERPID` + `getpeereid` (macOS) of `stream`.
#[cfg(target_os = "linux")]
pub fn peer_cred(stream: &UnixStream) -> io::Result<PeerCred> {
    const SOL_SOCKET: i32 = 1;
    const SO_PEERCRED: i32 = 17;
    let mut c = Ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut len = std::mem::size_of::<Ucred>() as u32;
    // SAFETY: `c` and `len` outlive the call; `len` is the size of `c`.
    let rc = unsafe {
        getsockopt(
            stream.as_raw_fd(),
            SOL_SOCKET,
            SO_PEERCRED,
            (&mut c as *mut Ucred).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(PeerCred {
        pid: c.pid,
        uid: c.uid,
    })
}

#[cfg(target_os = "macos")]
pub fn peer_cred(stream: &UnixStream) -> io::Result<PeerCred> {
    const SOL_LOCAL: i32 = 0;
    const LOCAL_PEERPID: i32 = 2;
    let fd = stream.as_raw_fd();
    let mut pid: i32 = 0;
    let mut len = std::mem::size_of::<i32>() as u32;
    // SAFETY: `pid` and `len` outlive the call; `len` is the size of `pid`.
    let rc = unsafe {
        getsockopt(
            fd,
            SOL_LOCAL,
            LOCAL_PEERPID,
            (&mut pid as *mut i32).cast(),
            &mut len,
        )
    };
    if rc != 0 {
        return Err(io::Error::last_os_error());
    }
    let (mut uid, mut gid) = (0u32, 0u32);
    // SAFETY: both out-pointers are valid for the call.
    if unsafe { getpeereid(fd, &mut uid, &mut gid) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(PeerCred { pid, uid })
}

/// The parent of `pid`, or `None` when it is gone or is pid 1's parent.
#[cfg(target_os = "linux")]
pub fn parent_pid(pid: i32) -> Option<i32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // `pid (comm) S ppid ...`: comm may hold spaces and parens, so cut at the last ')'.
    let rest = &stat[stat.rfind(')')? + 1..];
    let ppid: i32 = rest.split_whitespace().nth(1)?.parse().ok()?;
    (ppid > 0).then_some(ppid)
}

#[cfg(target_os = "macos")]
pub fn parent_pid(pid: i32) -> Option<i32> {
    let out = std::process::Command::new("ps")
        .args(["-o", "ppid=", "-p", &pid.to_string()])
        .output()
        .ok()?;
    let ppid: i32 = String::from_utf8_lossy(&out.stdout).trim().parse().ok()?;
    (ppid > 0).then_some(ppid)
}

/// `pid` and its ancestors, nearest first.
fn ancestry(pid: i32) -> Vec<i32> {
    let mut chain = vec![pid];
    while chain.len() < MAX_DEPTH {
        match parent_pid(*chain.last().unwrap_or(&pid)) {
            Some(p) if p > 1 && !chain.contains(&p) => chain.push(p),
            _ => break,
        }
    }
    chain
}

/// Resolves a peer through herdr: pane process info for the pid, then the
/// agent hosted by that pane.
pub struct HerdrPeers {
    herdr: Arc<dyn HerdrClient>,
}

impl HerdrPeers {
    pub fn new(herdr: Arc<dyn HerdrClient>) -> Self {
        HerdrPeers { herdr }
    }
}

fn refuse(why: impl std::fmt::Display) -> RpcError {
    RpcError::new(UNMAPPED_PEER, format!("caller not identified: {why}"))
}

fn json(r: io::Result<Reply>, what: &str) -> Result<Value, RpcError> {
    let r = r.map_err(|e| refuse(format!("herdr {what}: {e}")))?;
    if !r.ok {
        return Err(refuse(format!("herdr {what}: {}", r.body.trim())));
    }
    serde_json::from_str(&r.body).map_err(|e| refuse(format!("herdr {what}: {e}")))
}

impl PeerResolver for HerdrPeers {
    fn resolve(&self, cred: PeerCred) -> Result<CallerId, RpcError> {
        let chain = ancestry(cred.pid);
        let panes = json(self.herdr.pane_list(), "pane list")?;
        let panes = panes
            .pointer("/result/panes")
            .and_then(Value::as_array)
            .ok_or_else(|| refuse("herdr pane list has no panes"))?;
        // (pane id, is cos, pids the pane runs)
        let mut owned: Vec<(&str, bool, Vec<i64>)> = Vec::new();
        for p in panes {
            let Some(id) = p.get("pane_id").and_then(Value::as_str) else {
                continue;
            };
            // A pane whose process info is unavailable cannot own the peer.
            let Ok(info) = json(self.herdr.pane_process_info(id), "pane process-info") else {
                continue;
            };
            let info = info.pointer("/result/process_info");
            let mut pids: Vec<i64> = info
                .and_then(|i| i.get("shell_pid"))
                .and_then(Value::as_i64)
                .into_iter()
                .collect();
            pids.extend(
                info.and_then(|i| i.get("foreground_processes"))
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|f| f.get("pid").and_then(Value::as_i64)),
            );
            let cos = p.pointer("/tokens/grid_role").and_then(Value::as_str) == Some("cos");
            owned.push((id, cos, pids));
        }
        // The nearest ancestor that a pane runs decides.
        let (pane, cos) = chain
            .iter()
            .find_map(|a| {
                owned
                    .iter()
                    .find(|(_, _, pids)| pids.contains(&i64::from(*a)))
                    .map(|(id, cos, _)| (*id, *cos))
            })
            .ok_or_else(|| refuse(format!("pid {} is in no herdr pane", cred.pid)))?;
        if cos {
            return Ok(CallerId("cos".into()));
        }
        let agents = json(self.herdr.agent_list(), "agent list")?;
        agents
            .pointer("/result/agents")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|a| a.get("pane_id").and_then(Value::as_str) == Some(pane))
            .and_then(|a| a.get("name").and_then(Value::as_str))
            .map(|n| CallerId(n.to_string()))
            .ok_or_else(|| refuse(format!("pane {pane} hosts no known agent")))
    }
}
