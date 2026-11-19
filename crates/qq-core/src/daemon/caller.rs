//! `caller.lookup`: who is the hook process on the other end of the socket.

use std::path::Path;

/// What the gate learns about the process that asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    Cos,
    Other(String),
    Unknown,
}

pub fn lookup(_runs_dir: &Path) -> Caller {
    Caller::Unknown
}
