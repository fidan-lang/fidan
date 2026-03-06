use crate::{CompileOptions, Session};
use anyhow::Result;
use fidan_diagnostics::{Severity, render_message_to_stderr};

pub fn compile(_session: &Session, _opts: &CompileOptions) -> Result<()> {
    // AOT compilation backend not yet implemented (Phase 8/11).
    // The CLI handles interpret / check / test modes directly.
    render_message_to_stderr(Severity::Note, "", "AOT backend not yet implemented");
    Ok(())
}
