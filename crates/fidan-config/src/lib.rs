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
    "WeakShared",
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
pub struct BuiltinInfo {
    pub name: &'static str,
    pub signature: &'static str,
    pub doc: &'static str,
}

#[derive(Clone, Copy)]
pub struct DecoratorInfo {
    pub name: &'static str,
    pub doc: &'static str,
    pub reserved_only: bool,
}

pub const LANGUAGE_BUILTINS: &[BuiltinInfo] = &[
    BuiltinInfo {
        name: "print",
        signature: "print(value...) -> nothing",
        doc: "Print values to stdout followed by a newline.",
    },
    BuiltinInfo {
        name: "eprint",
        signature: "eprint(value...) -> nothing",
        doc: "Print values to stderr followed by a newline.",
    },
    BuiltinInfo {
        name: "input",
        signature: "input(prompt?) -> string",
        doc: "Read one line of input, optionally after showing a prompt.",
    },
    BuiltinInfo {
        name: "len",
        signature: "len(value) -> integer",
        doc: "Return the length of a string, list, or other countable value.",
    },
    BuiltinInfo {
        name: "type",
        signature: "type(value) -> string",
        doc: "Return the runtime type name of a value as a string.",
    },
    BuiltinInfo {
        name: "string",
        signature: "string(value) -> string",
        doc: "Convert a value to its string representation.",
    },
    BuiltinInfo {
        name: "integer",
        signature: "integer(value) -> integer",
        doc: "Convert a value to an integer when possible.",
    },
    BuiltinInfo {
        name: "float",
        signature: "float(value) -> float",
        doc: "Convert a value to a floating-point number when possible.",
    },
    BuiltinInfo {
        name: "boolean",
        signature: "boolean(value) -> boolean",
        doc: "Convert a value to a boolean truth value.",
    },
    BuiltinInfo {
        name: "Shared",
        signature: "Shared(value) -> Shared",
        doc: "Create a shared, thread-safe wrapper so values can be mutated safely across parallel work.",
    },
    BuiltinInfo {
        name: "WeakShared",
        signature: "WeakShared(shared) -> WeakShared",
        doc: "Create a non-owning weak handle to an existing Shared value. Use `upgrade()` to recover a Shared while it is still alive.",
    },
    BuiltinInfo {
        name: "assert",
        signature: "assert(condition, message?) -> nothing",
        doc: "Fail immediately when the condition is not truthy.",
    },
    BuiltinInfo {
        name: "assert_eq",
        signature: "assert_eq(left, right, message?) -> nothing",
        doc: "Fail immediately when two values are not equal.",
    },
    BuiltinInfo {
        name: "assert_ne",
        signature: "assert_ne(left, right, message?) -> nothing",
        doc: "Fail immediately when two values are equal.",
    },
];

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

pub fn builtin_info(name: &str) -> Option<&'static BuiltinInfo> {
    LANGUAGE_BUILTINS.iter().find(|info| info.name == name)
}

#[cfg(test)]
mod tests {
    use super::{
        BUILTIN_BINDINGS, BUILTIN_DECORATORS, BUILTIN_FUNCTIONS, BUILTIN_VALUE_MODULE,
        LANGUAGE_BUILTINS, LANGUAGE_DECORATORS, builtin_info, decorator_info,
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
    fn builtin_bindings_are_documented() {
        for builtin in BUILTIN_BINDINGS {
            assert!(
                builtin_info(builtin).is_some(),
                "missing builtin metadata for `{builtin}`"
            );
        }
        assert_eq!(LANGUAGE_BUILTINS.len(), BUILTIN_BINDINGS.len());
    }

    #[test]
    fn shared_constructors_are_reserved_but_not_function_completion() {
        assert!(BUILTIN_BINDINGS.contains(&"Shared"));
        assert!(BUILTIN_BINDINGS.contains(&"WeakShared"));
        assert!(!BUILTIN_FUNCTIONS.contains(&"Shared"));
        assert!(!BUILTIN_FUNCTIONS.contains(&"WeakShared"));
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
