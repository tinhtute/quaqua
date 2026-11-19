//! `caller.lookup`: who is the hook process on the other end of the socket.
//!
//! The daemon names every caller from the socket peer (`identity/peer.rs`: the
//! peer pid's ancestry to a herdr pane to that pane's agent); this verb hands
//! that name back to the gate. A hook process is a child of the session's
//! `claude`, so it maps to the session's pane. Fail closed everywhere: a daemon
//! that names no callers, an unmapped peer, an I/O error or a daemon that is
//! down all read as [`Caller::Unknown`].

use std::path::Path;

use serde_json::{json, Value};

use super::Daemon;
use crate::identity::peer::UNMAPPED_PEER;
use crate::rpc::{self, Request, RpcError};

pub const VERB: &str = "caller.lookup";

/// What the gate learns about the process that asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    /// The agent `cos`.
    Cos,
    /// Any other mapped agent (worker, Lead, reviewer).
    Other(String),
    /// Unmapped peer, no daemon answer, or a reply the gate cannot read.
    Unknown,
}

/// Registers the verb. The handler trusts `req.caller` only when the peer gate
/// replaced it (it also sets `caller_pane`): a claim the daemon did not verify
/// never passes.
pub fn register(d: &mut Daemon) {
    let _ = d.register_verb(
        VERB,
        Box::new(|_, req| {
            let Some(who) = identified(req) else {
                return Err(RpcError::new(
                    UNMAPPED_PEER,
                    "caller not identified: the daemon names no callers from the socket peer",
                ));
            };
            Ok(json!({ "caller": who, "cos": who == "cos" }))
        }),
    );
}

/// The caller's name when the peer gate named it (it also sets `caller_pane`);
/// a claim the daemon did not verify is `None`.
pub fn identified(req: &Request) -> Option<&str> {
    req.caller_pane.as_ref()?;
    req.caller.as_deref()
}

/// True only for the agent `cos`, named by the peer gate.
pub fn is_cos(req: &Request) -> bool {
    identified(req) == Some("cos")
}

/// Asks the daemon of `runs_dir`'s workspace who is calling.
pub fn lookup(runs_dir: &Path) -> Caller {
    let socket = rpc::socket_path(runs_dir);
    let Ok(resp) = rpc::call(&socket, &Request::new(VERB, Value::Null)) else {
        return Caller::Unknown;
    };
    let Ok(v) = resp.into_result() else {
        return Caller::Unknown;
    };
    match (
        v.get("cos").and_then(Value::as_bool),
        v.get("caller").and_then(Value::as_str),
    ) {
        (Some(true), _) => Caller::Cos,
        (Some(false), Some(name)) => Caller::Other(name.to_string()),
        _ => Caller::Unknown,
    }
}
