//! `fidan-codegen-llvm` — LLVM AOT backend support for the external helper.

mod compile;
mod context;
#[cfg(feature = "llvm-toolchain-21")]
mod inkwell_backend;
mod model;
#[cfg(feature = "llvm-toolchain-21")]
mod tool;
mod validate;

pub use compile::compile_request;
#[cfg(feature = "llvm-toolchain-21")]
pub(crate) use compile::{dump_ir, env_flag_enabled, trace};
pub use context::{BackendContext, mangle_fn};
pub use model::{
    BackendPayload, CompileRequest, LtoMode, OptLevel, StripMode, ToolchainLayout,
    ToolchainMetadata,
};
pub use validate::validate_toolchain_layout;
