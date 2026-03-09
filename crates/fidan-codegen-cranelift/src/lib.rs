//! `fidan-codegen-cranelift` — Cranelift JIT + AOT backend.
//! JIT:  `@precompile` and interpreter hot-path elevation.
//! AOT:  `fidan build --backend cranelift` — zero-dependency native binary.

mod aot;
mod jit;

pub use aot::{
    AotCompiler as CraneliftAotCompiler, AotOptions as CraneliftAotOptions,
    OptLevel as CraneliftOptLevel,
};
pub use jit::{JitCompiler, JitFnEntry, call_jit_fn};
