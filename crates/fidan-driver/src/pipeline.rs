use crate::{CompileOptions, Session};
use anyhow::Result;

pub fn compile(_session: &Session, _opts: &CompileOptions) -> Result<()> {
    // AOT compilation backend not yet implemented (Phase 8/11).
    // The CLI handles interpret / check / test modes directly.
    eprintln!("note: AOT backend not yet implemented");
    Ok(())
}
