//! `fidan-codegen-cranelift` — Cranelift JIT backend.
//! Used only for `@precompile` and interpreter hot-path elevation.
//! NOT used for `fidan build` release binaries (that is LLVM's job).

mod jit;

pub use jit::JitCompiler;
