//! `fidan-codegen-cranelift` — Cranelift JIT + AOT backend.
//! JIT:  `@precompile` and interpreter hot-path elevation.
//! AOT:  `fidan build --backend cranelift` — zero-dependency native binary.

mod aot;
mod jit;

pub use aot::{
    AotCompiler as CraneliftAotCompiler, AotOptions as CraneliftAotOptions,
    LtoMode as CraneliftLtoMode, OptLevel as CraneliftOptLevel, StripMode as CraneliftStripMode,
};
pub use jit::{
    JitCompiler, JitFnEntry, JitRuntimeHooks, call_jit_fn, decode_jit_abi_value,
    encode_jit_abi_value, register_jit_runtime_hooks, with_jit_runtime_context,
};
