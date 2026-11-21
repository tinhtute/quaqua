//! Supervisor module: empty seam the wave fills in.

use crate::cli::{Install, Registry};
use crate::daemon::Daemon;

pub const INSTALL: Install = install;

fn install(_: &mut Registry, _: &mut Daemon) -> Result<(), String> {
    Ok(())
}
