//! `fidan-codegen-llvm` — LLVM AOT backend.
//! Used only for `fidan build --release`. Requires the `llvm` feature flag.
//! Not compiled unless LLVM is installed and the feature is enabled.

#[cfg(feature = "llvm")]
mod aot;

#[cfg(feature = "llvm")]
pub use aot::AotCompiler;
