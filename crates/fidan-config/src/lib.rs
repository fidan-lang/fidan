//! `fidan-config` — shared language/runtime configuration constants.

/// Current 1.0 contract for the native `@extern` ABI.
///
/// Native extern calls are lowered as direct scalar ABI calls across the
/// interpreter and AOT backends, so the supported parameter count is
/// intentionally bounded and shared across the compiler/runtime.
pub const MAX_NATIVE_EXTERN_PARAMS: usize = 4;
