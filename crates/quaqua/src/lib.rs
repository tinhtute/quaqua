//! `quaqua` library surface. The clap binary in `main.rs` is a thin shell over
//! this module set, which lets tests call load/resolve/validate/get in-process.

pub mod auditor;
pub mod bus;
pub mod cap;
pub mod config;
pub mod cos;
pub mod doctor;
pub mod engine;
pub mod gate;
pub mod git;
pub mod grid;
pub mod harness;
pub mod hash;
pub mod herdr;
pub mod identity;
pub mod init;
pub mod launch;
pub mod layout;
pub mod lead;
pub mod liveness;
pub mod merge;
pub mod pipeline;
pub mod playbook;
pub mod rules;
pub mod state;
pub mod status;
pub mod taskforce;
pub mod usage;
pub mod util;
pub mod worker;
pub mod workspace;

/// Process-global env is shared by every test in this binary; tests that
/// mutate it take [`test_env::lock_env`] and restore through its guard.
#[cfg(test)]
pub(crate) mod test_env;
