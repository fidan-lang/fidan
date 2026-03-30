//! `fidan-config` — shared language/runtime configuration constants.

/// Pseudo-module used by MIR/lowerings for top-level builtins like `print`
/// or `assert_eq`.
pub const BUILTIN_VALUE_MODULE: &str = "__builtin__";

/// All top-level builtin bindings reserved by the language.
///
/// This list is the canonical source for:
/// - typechecker builtin registration / name reservation
/// - MIR lowering of builtin values
/// - editor-facing builtin completion subsets
pub const BUILTIN_BINDINGS: &[&str] = &[
    "print",
    "eprint",
    "input",
    "len",
    "type",
    "string",
    "integer",
    "float",
    "boolean",
    "Shared",
    "assert",
    "assert_eq",
    "assert_ne",
];

/// Builtin callables that should show up as function-like editor completions.
///
/// This intentionally excludes constructor-ish values like `Shared`, which the
/// LSP already surfaces through keyword completion.
pub const BUILTIN_FUNCTIONS: &[&str] = &[
    "print",
    "eprint",
    "input",
    "len",
    "type",
    "string",
    "integer",
    "float",
    "boolean",
    "assert",
    "assert_eq",
    "assert_ne",
];

/// Compiler-recognized decorator names reserved by the language/toolchain.
pub const BUILTIN_DECORATORS: &[&str] = &["precompile", "deprecated", "extern", "unsafe"];

#[cfg(test)]
mod tests {
    use super::{BUILTIN_BINDINGS, BUILTIN_DECORATORS, BUILTIN_FUNCTIONS, BUILTIN_VALUE_MODULE};

    #[test]
    fn builtin_module_name_is_stable() {
        assert_eq!(BUILTIN_VALUE_MODULE, "__builtin__");
    }

    #[test]
    fn builtin_functions_are_subset_of_bindings() {
        for builtin in BUILTIN_FUNCTIONS {
            assert!(BUILTIN_BINDINGS.contains(builtin));
        }
    }

    #[test]
    fn shared_is_reserved_but_not_function_completion() {
        assert!(BUILTIN_BINDINGS.contains(&"Shared"));
        assert!(!BUILTIN_FUNCTIONS.contains(&"Shared"));
    }

    #[test]
    fn builtin_decorators_are_unique() {
        for (i, name) in BUILTIN_DECORATORS.iter().enumerate() {
            assert!(
                !BUILTIN_DECORATORS[i + 1..].contains(name),
                "duplicate builtin decorator `{name}`"
            );
        }
    }
}
