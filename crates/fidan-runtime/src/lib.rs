//! `fidan-runtime` — Value types, memory model (OwnedRef/SharedRef/COW), object model.

use std::sync::{OnceLock, RwLock};

mod dict;
pub mod ffi;
mod hashset;
mod list;
mod object;
mod owned_ref;
pub mod parallel;
mod shared_ref;
pub mod stdlib;
mod string;
mod value;

pub use dict::FidanDict;
pub use hashset::{FidanHashKey, FidanHashSet, HashKeyError};
pub use list::FidanList;
pub use object::{FidanClass, FidanObject, FieldDef};
pub use owned_ref::OwnedRef;
pub use parallel::{FidanPending, ParallelArgs, ParallelCapture};
pub use shared_ref::{SharedRef, WeakSharedRef};
pub use string::FidanString;
pub use value::{FidanValue, FunctionId, display};

static PROGRAM_ARGS_OVERRIDE: OnceLock<RwLock<Option<Vec<String>>>> = OnceLock::new();

fn program_args_override() -> &'static RwLock<Option<Vec<String>>> {
    PROGRAM_ARGS_OVERRIDE.get_or_init(|| RwLock::new(None))
}

/// Temporary scoped override for the argv visible to `std.env.args()` /
/// `std.io.args()` inside Fidan programs. Interpreted `fidan run` uses this to
/// expose script-facing arguments instead of the host CLI's own subcommand
/// arguments.
pub struct ProgramArgsGuard(Option<Vec<String>>);

impl Drop for ProgramArgsGuard {
    fn drop(&mut self) {
        let mut slot = program_args_override()
            .write()
            .expect("program argv override lock poisoned");
        *slot = self.0.take();
    }
}

pub fn push_program_args(args: Vec<String>) -> ProgramArgsGuard {
    let mut slot = program_args_override()
        .write()
        .expect("program argv override lock poisoned");
    let previous = slot.replace(args);
    ProgramArgsGuard(previous)
}

pub fn current_program_args() -> Vec<String> {
    let slot = program_args_override()
        .read()
        .expect("program argv override lock poisoned");
    slot.clone().unwrap_or_else(|| std::env::args().collect())
}
