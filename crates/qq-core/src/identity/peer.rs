//! Caller identity from the socket peer, never from the request.
//!
//! Data shape: the kernel's [`PeerCred`] `{pid, uid}` for the connected socket
//! -> [`PeerResolver::resolve`] walks the pid's ancestry to the herdr pane whose
//! shell or foreground process it is -> that pane's agent is the [`CallerId`].
//! A peer of the daemon's own uid that reaches no pane is the [`PRINCIPAL`], the
//! owner of the farm; it names no agent, so the daemon keeps it to a few verbs.
//! Every other step fails closed: a peer in a pane with no agent, or from
//! another uid, is refused whatever the request claims.

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

/// The name a same-uid peer that reaches no herdr pane speaks as: the
/// Principal, the uid that owns the farm. Named by the socket peer (the uid)
/// plus the absence of a pane, never claimed by the request.
///
/// The Principal runs the farm from a plain terminal, so the daemon opens
/// `ping`, `status`, `stop`, `config` and `roster` to it and nothing else: it
/// names no agent, so no agent verb can be reached through it.
pub const PRINCIPAL: &str = "owner";

/// Refusal code for an agent-only verb called by the Principal. Distinct from
/// [`UNMAPPED_PEER`] (the daemon could name nobody) so a script can tell "not
/// allowed" from "not known" and from a daemon that is down.
pub const AGENT_ONLY: &str = "agent_only";

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
    /// The agent the peer speaks as. A process in no pane, or in a pane with no
    /// agent, is an error here: never taken for an agent.
    fn resolve(&self, cred: PeerCred) -> Result<CallerId, RpcError>;

    /// As [`PeerResolver::resolve`], plus the pane the peer was mapped to.
    /// `Some` pane is for placement (where to split a new pane); `None` says
    /// the peer reached no pane, which the daemon reads as the [`PRINCIPAL`].
    fn resolve_pane(&self, cred: PeerCred) -> Result<(CallerId, Option<String>), RpcError> {
        self.resolve(cred).map(|c| (c, None))
    }
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
    /// Workspace `runs/` whose database gets the CoS agent row on the first
    /// call from a `grid_role=cos` pane; `None` seeds nothing.
    cos_runs: Option<std::path::PathBuf>,
}

impl HerdrPeers {
    pub fn new(herdr: Arc<dyn HerdrClient>) -> Self {
        HerdrPeers {
            herdr,
            cos_runs: None,
        }
    }

    /// As [`HerdrPeers::new`], and a call that maps to a cos-tagged pane makes
    /// sure the database holds one active `cos` agent row (a fresh workspace
    /// has none). The row is only ever made on that mapping, so an unmapped
    /// peer is still refused before this runs.
    pub fn with_cos_bootstrap(herdr: Arc<dyn HerdrClient>, runs_dir: std::path::PathBuf) -> Self {
        HerdrPeers {
            herdr,
            cos_runs: Some(runs_dir),
        }
    }

    fn ensure_cos(&self, pane: &str) -> Result<(), RpcError> {
        let Some(runs) = &self.cos_runs else {
            return Ok(());
        };
        let conn = crate::db::open(&runs.join(crate::db::path::DB_FILE))
            .map_err(|e| refuse(format!("cos bootstrap: {e}")))?;
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        // One statement: two first calls cannot both insert.
        conn.execute(
            "INSERT INTO agent (name, role, started_ms, pane_id) \
             SELECT 'cos', 'cos', ?1, ?2 \
             WHERE NOT EXISTS (SELECT 1 FROM agent WHERE name = 'cos' AND role = 'cos' AND ended_ms IS NULL)",
            rusqlite::params![now, pane],
        )
        .map_err(|e| refuse(format!("cos bootstrap: {e}")))?;
        Ok(())
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
        self.resolve_pane(cred).map(|(c, _)| c)
    }

    fn resolve_pane(&self, cred: PeerCred) -> Result<(CallerId, Option<String>), RpcError> {
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
            self.ensure_cos(pane)?;
            return Ok((CallerId("cos".into()), Some(pane.to_string())));
        }
        let agents = json(self.herdr.agent_list(), "agent list")?;
        agents
            .pointer("/result/agents")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .find(|a| a.get("pane_id").and_then(Value::as_str) == Some(pane))
            .and_then(|a| a.get("name").and_then(Value::as_str))
            .map(|n| (CallerId(n.to_string()), Some(pane.to_string())))
            .ok_or_else(|| refuse(format!("pane {pane} hosts no known agent")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::herdr::{FakeHerdr, HerdrClient};

    fn resolver(herdr: &Arc<FakeHerdr>) -> HerdrPeers {
        HerdrPeers::new(Arc::clone(herdr) as Arc<dyn HerdrClient>)
    }

    /// A pane herdr knows, run by some other process tree, with an agent in it.
    fn pane_belonging_to_another_tree(herdr: &FakeHerdr) -> String {
        let pane = herdr.seed_pane("w", None, &[]);
        herdr.agent_start("w-1", "claude", &pane, 0, &[]).unwrap();
        herdr.set_pane_pids(&pane, 1, &[]);
        pane
    }

    fn me() -> PeerCred {
        PeerCred {
            pid: std::process::id() as i32,
            uid: own_uid(),
        }
    }

    /// Issue #368: a same-uid process that reaches no herdr pane is the
    /// Principal of this uid, not an unnameable peer. The daemon decides which
    /// verbs it may run; naming it here is what lets `quaqua ping` answer from
    /// a plain terminal.
    #[test]
    fn a_same_uid_pid_in_no_pane_is_the_principal() {
        let herdr = Arc::new(FakeHerdr::new());
        pane_belonging_to_another_tree(&herdr);
        let (who, pane) = resolver(&herdr)
            .resolve_pane(me())
            .expect("a peer in no pane is the principal");
        assert_eq!(who.as_str(), PRINCIPAL);
        assert_eq!(pane, None, "the principal sits in no pane");
    }

    /// `resolve` is the agent-only door (`quaqua worker spawn` reads it): the
    /// principal must stay refused there, so no spawn is ever attributed to it.
    #[test]
    fn the_principal_is_no_agent() {
        let herdr = Arc::new(FakeHerdr::new());
        pane_belonging_to_another_tree(&herdr);
        let e = resolver(&herdr).resolve(me()).unwrap_err();
        assert_eq!(e.code, UNMAPPED_PEER);
    }

    /// A process inside a pane that hosts no known agent is still refused: only
    /// a peer that reaches no pane at all is the principal.
    #[test]
    fn a_pane_with_no_agent_is_still_refused() {
        let herdr = Arc::new(FakeHerdr::new());
        let pane = herdr.seed_pane("h", Some("zsh"), &[]);
        herdr.set_pane_pids(&pane, std::process::id(), &[]);
        let e = resolver(&herdr).resolve_pane(me()).unwrap_err();
        assert_eq!(e.code, UNMAPPED_PEER);
        assert!(e.message.contains("hosts no known agent"), "{}", e.message);
    }
}
