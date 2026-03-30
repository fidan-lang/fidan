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

#[derive(Clone, Copy)]
pub struct DecoratorInfo {
    pub name: &'static str,
    pub doc: &'static str,
    pub reserved_only: bool,
}

/// Canonical decorator metadata used by editor/tooling surfaces.
///
/// `reserved_only = true` means the spelling is reserved for future use but is
/// not currently a compiler-recognized built-in decorator.
pub const LANGUAGE_DECORATORS: &[DecoratorInfo] = &[
    DecoratorInfo {
        name: "precompile",
        doc: "Eagerly compile the action before the first hot-path call. Useful for avoiding first-call JIT latency in performance-sensitive code.",
        reserved_only: false,
    },
    DecoratorInfo {
        name: "deprecated",
        doc: "Mark an action as deprecated so callers receive a warning and can migrate to a replacement before removal.",
        reserved_only: false,
    },
    DecoratorInfo {
        name: "extern",
        doc: "Declare a foreign action imported from a native library. Used for ABI-bound native interop.",
        reserved_only: false,
    },
    DecoratorInfo {
        name: "unsafe",
        doc: "Acknowledge an intentionally unsafe boundary, typically alongside native extern usage.",
        reserved_only: false,
    },
    DecoratorInfo {
        name: "gpu",
        doc: "Reserved for future GPU/offload support. This spelling is reserved but not implemented yet.",
        reserved_only: true,
    },
];

pub fn decorator_info(name: &str) -> Option<&'static DecoratorInfo> {
    LANGUAGE_DECORATORS.iter().find(|info| info.name == name)
}

#[cfg(test)]
mod tests {
    use super::{
        BUILTIN_BINDINGS, BUILTIN_DECORATORS, BUILTIN_FUNCTIONS, BUILTIN_VALUE_MODULE,
        LANGUAGE_DECORATORS, decorator_info,
    };

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

    #[test]
    fn builtin_decorators_are_documented() {
        for name in BUILTIN_DECORATORS {
            let info = decorator_info(name).expect("missing decorator metadata");
            assert!(!info.doc.is_empty());
            assert!(!info.reserved_only);
        }
    }

    #[test]
    fn reserved_gpu_decorator_is_documented() {
        let info = decorator_info("gpu").expect("missing gpu decorator metadata");
        assert!(info.reserved_only);
        assert!(!info.doc.is_empty());
    }

    #[test]
    fn language_decorators_are_unique() {
        for (i, info) in LANGUAGE_DECORATORS.iter().enumerate() {
            assert!(
                !LANGUAGE_DECORATORS[i + 1..]
                    .iter()
                    .any(|other| other.name == info.name),
                "duplicate language decorator `{}`",
                info.name
            );
        }
    }
}
