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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuiltinReturnKind {
    Nothing,
    String,
    Integer,
    Float,
    Boolean,
    Dynamic,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiverBuiltinKind {
    Integer,
    Float,
    Boolean,
    String,
    List,
    Dict,
    Handle,
    Nothing,
    Dynamic,
    Shared,
    WeakShared,
    Pending,
    Function,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiverReturnKind {
    Integer,
    Float,
    Boolean,
    String,
    Dynamic,
    Nothing,
    ReceiverElement,
    DictValue,
    ListOfString,
    ListOfInteger,
    ListOfDynamic,
    ListOfReceiverElement,
    ListOfDictValue,
    ListOfDynamicPairs,
    SharedInnerValue,
    SharedOfInner,
    WeakSharedOfInner,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceiverMemberInfo {
    pub canonical_name: &'static str,
    pub field_return: Option<ReceiverReturnKind>,
    pub method_return: Option<ReceiverReturnKind>,
}

const fn field(
    canonical_name: &'static str,
    return_kind: ReceiverReturnKind,
) -> ReceiverMemberInfo {
    ReceiverMemberInfo {
        canonical_name,
        field_return: Some(return_kind),
        method_return: None,
    }
}

const fn method(
    canonical_name: &'static str,
    return_kind: ReceiverReturnKind,
) -> ReceiverMemberInfo {
    ReceiverMemberInfo {
        canonical_name,
        field_return: None,
        method_return: Some(return_kind),
    }
}

const fn field_and_method(
    canonical_name: &'static str,
    return_kind: ReceiverReturnKind,
) -> ReceiverMemberInfo {
    ReceiverMemberInfo {
        canonical_name,
        field_return: Some(return_kind),
        method_return: Some(return_kind),
    }
}

#[derive(Clone, Copy)]
pub struct BuiltinInfo {
    pub name: &'static str,
    pub signature: &'static str,
    pub doc: &'static str,
    pub return_kind: Option<BuiltinReturnKind>,
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
        return_kind: Some(BuiltinReturnKind::Nothing),
    },
    BuiltinInfo {
        name: "eprint",
        signature: "eprint(value...) -> nothing",
        doc: "Print values to stderr followed by a newline.",
        return_kind: Some(BuiltinReturnKind::Nothing),
    },
    BuiltinInfo {
        name: "input",
        signature: "input(prompt?) -> string",
        doc: "Read one line of input, optionally after showing a prompt.",
        return_kind: Some(BuiltinReturnKind::String),
    },
    BuiltinInfo {
        name: "len",
        signature: "len(value) -> integer",
        doc: "Return the length of a string, list, or other countable value.",
        return_kind: Some(BuiltinReturnKind::Integer),
    },
    BuiltinInfo {
        name: "type",
        signature: "type(value) -> string",
        doc: "Return the runtime type name of a value as a string.",
        return_kind: Some(BuiltinReturnKind::String),
    },
    BuiltinInfo {
        name: "string",
        signature: "string(value) -> string",
        doc: "Convert a value to its string representation.",
        return_kind: Some(BuiltinReturnKind::String),
    },
    BuiltinInfo {
        name: "integer",
        signature: "integer(value) -> integer",
        doc: "Convert a value to an integer when possible.",
        return_kind: Some(BuiltinReturnKind::Integer),
    },
    BuiltinInfo {
        name: "float",
        signature: "float(value) -> float",
        doc: "Convert a value to a floating-point number when possible.",
        return_kind: Some(BuiltinReturnKind::Float),
    },
    BuiltinInfo {
        name: "boolean",
        signature: "boolean(value) -> boolean",
        doc: "Convert a value to a boolean truth value.",
        return_kind: Some(BuiltinReturnKind::Boolean),
    },
    BuiltinInfo {
        name: "Shared",
        signature: "Shared(value) -> Shared",
        doc: "Create a shared, thread-safe wrapper so values can be mutated safely across parallel work.",
        return_kind: None,
    },
    BuiltinInfo {
        name: "WeakShared",
        signature: "WeakShared(shared) -> WeakShared",
        doc: "Create a non-owning weak handle to an existing Shared value. Use `upgrade()` to recover a Shared while it is still alive.",
        return_kind: None,
    },
    BuiltinInfo {
        name: "assert",
        signature: "assert(condition, message?) -> nothing",
        doc: "Fail immediately when the condition is not truthy.",
        return_kind: Some(BuiltinReturnKind::Nothing),
    },
    BuiltinInfo {
        name: "assert_eq",
        signature: "assert_eq(left, right, message?) -> nothing",
        doc: "Fail immediately when two values are not equal.",
        return_kind: Some(BuiltinReturnKind::Nothing),
    },
    BuiltinInfo {
        name: "assert_ne",
        signature: "assert_ne(left, right, message?) -> nothing",
        doc: "Fail immediately when two values are equal.",
        return_kind: Some(BuiltinReturnKind::Nothing),
    },
];

pub const LANGUAGE_TYPE_NAMES: &[BuiltinInfo] = &[BuiltinInfo {
    name: "handle",
    signature: "handle",
    doc: "Opaque native handle type used for extern interop and low-level OS or library handles.",
    return_kind: None,
}];

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

pub fn builtin_return_kind(name: &str) -> Option<BuiltinReturnKind> {
    builtin_info(name).and_then(|info| info.return_kind)
}

pub fn infer_receiver_member(
    receiver_kind: ReceiverBuiltinKind,
    name: &str,
) -> Option<ReceiverMemberInfo> {
    use ReceiverBuiltinKind as Kind;
    use ReceiverReturnKind as Return;

    match receiver_kind {
        Kind::String => match name {
            "len" | "length" => Some(field_and_method("len", Return::Integer)),
            "byteLen" | "byte_len" => Some(field_and_method("byteLen", Return::Integer)),
            "lower" | "toLower" | "to_lower" => Some(method("lower", Return::String)),
            "upper" | "toUpper" | "to_upper" => Some(method("upper", Return::String)),
            "capitalize" => Some(method("capitalize", Return::String)),
            "trim" => Some(method("trim", Return::String)),
            "trimStart" | "ltrim" | "trim_start" => Some(method("trimStart", Return::String)),
            "trimEnd" | "rtrim" | "trim_end" => Some(method("trimEnd", Return::String)),
            "split" => Some(method("split", Return::ListOfString)),
            "lines" => Some(method("lines", Return::ListOfString)),
            "chars" => Some(method("chars", Return::ListOfString)),
            "join" => Some(method("join", Return::String)),
            "contains" => Some(method("contains", Return::Boolean)),
            "startsWith" | "starts_with" => Some(method("startsWith", Return::Boolean)),
            "endsWith" | "ends_with" => Some(method("endsWith", Return::Boolean)),
            "indexOf" | "index_of" | "find" => Some(method("indexOf", Return::Integer)),
            "lastIndexOf" | "last_index_of" => Some(method("lastIndexOf", Return::Integer)),
            "replace" => Some(method("replace", Return::String)),
            "replaceAll" | "replace_all" => Some(method("replaceAll", Return::String)),
            "replaceFirst" | "replace_first" => Some(method("replaceFirst", Return::String)),
            "repeat" => Some(method("repeat", Return::String)),
            "reverse" | "reversed" => Some(method("reverse", Return::String)),
            "charAt" | "char_at" => Some(method("charAt", Return::String)),
            "substring" | "substr" | "slice" => Some(method("substring", Return::String)),
            "toInt" | "to_int" | "parseInt" | "parse_int" => Some(method("toInt", Return::Integer)),
            "toFloat" | "to_float" | "parseFloat" | "parse_float" => {
                Some(method("toFloat", Return::Float))
            }
            "toBool" | "to_bool" => Some(method("toBool", Return::Boolean)),
            "toString" | "to_string" => Some(method("toString", Return::String)),
            "padStart" | "pad_start" => Some(method("padStart", Return::String)),
            "padEnd" | "pad_end" => Some(method("padEnd", Return::String)),
            "bytes" => Some(method("bytes", Return::ListOfInteger)),
            "charCode" | "char_code" => Some(method("charCode", Return::Integer)),
            "format" => Some(method("format", Return::String)),
            "isEmpty" | "is_empty" => Some(method("isEmpty", Return::Boolean)),
            _ => None,
        },
        Kind::List => match name {
            "len" | "length" | "size" => Some(field_and_method("len", Return::Integer)),
            "isEmpty" | "is_empty" => Some(method("isEmpty", Return::Boolean)),
            "append" | "push" | "add" => Some(method("append", Return::Nothing)),
            "pop" => Some(method("pop", Return::ReceiverElement)),
            "first" | "head" => Some(method("first", Return::ReceiverElement)),
            "last" => Some(method("last", Return::ReceiverElement)),
            "get" => Some(method("get", Return::ReceiverElement)),
            "contains" => Some(method("contains", Return::Boolean)),
            "indexOf" | "index_of" => Some(method("indexOf", Return::Integer)),
            "reverse" => Some(method("reverse", Return::Nothing)),
            "reversed" => Some(method("reversed", Return::ListOfReceiverElement)),
            "sort" => Some(method("sort", Return::Nothing)),
            "join" => Some(method("join", Return::String)),
            "slice" => Some(method("slice", Return::ListOfReceiverElement)),
            "flatten" => Some(method("flatten", Return::ListOfDynamic)),
            "extend" | "concat" => Some(method("extend", Return::Nothing)),
            "toString" | "to_string" => Some(method("toString", Return::String)),
            "forEach" | "for_each" | "each" => Some(method("forEach", Return::Nothing)),
            "map" | "transform" | "collect" => Some(method("map", Return::ListOfDynamic)),
            "filter" | "where_" | "select" => Some(method("filter", Return::ListOfReceiverElement)),
            "find" => Some(method("find", Return::Dynamic)),
            "firstWhere" | "first_where" => Some(method("firstWhere", Return::ReceiverElement)),
            "remove" => Some(method("remove", Return::ReceiverElement)),
            "reduce" | "fold" => Some(method("reduce", Return::Dynamic)),
            _ => None,
        },
        Kind::Dict => match name {
            "len" | "length" | "size" => Some(field_and_method("len", Return::Integer)),
            "isEmpty" | "is_empty" => Some(method("isEmpty", Return::Boolean)),
            "get" => Some(method("get", Return::DictValue)),
            "set" | "put" | "insert" => Some(method("set", Return::Nothing)),
            "contains" | "has" | "has_key" | "containsKey" | "contains_key" => {
                Some(method("containsKey", Return::Boolean))
            }
            "remove" | "delete" => Some(method("remove", Return::Nothing)),
            "keys" => Some(method("keys", Return::ListOfString)),
            "values" => Some(method("values", Return::ListOfDictValue)),
            "entries" | "items" => Some(method("entries", Return::ListOfDynamicPairs)),
            "toString" | "to_string" => Some(method("toString", Return::String)),
            _ => None,
        },
        Kind::Shared => match name {
            "type" => Some(field("type", Return::String)),
            "get" => Some(method("get", Return::SharedInnerValue)),
            "set" => Some(method("set", Return::Nothing)),
            "weak" | "downgrade" => Some(method("weak", Return::WeakSharedOfInner)),
            _ => None,
        },
        Kind::WeakShared => match name {
            "type" => Some(field("type", Return::String)),
            "upgrade" => Some(method("upgrade", Return::SharedOfInner)),
            "isAlive" | "is_alive" | "alive" => Some(method("isAlive", Return::Boolean)),
            _ => None,
        },
        Kind::Function => match name {
            "name" => Some(field("name", Return::String)),
            _ => None,
        },
        _ => None,
    }
}

pub fn type_name_info(name: &str) -> Option<&'static BuiltinInfo> {
    LANGUAGE_TYPE_NAMES.iter().find(|info| info.name == name)
}

pub fn editor_symbol_info(name: &str) -> Option<&'static BuiltinInfo> {
    builtin_info(name).or_else(|| type_name_info(name))
}

pub fn is_type_like_name(name: &str) -> bool {
    type_name_info(name).is_some()
}

#[cfg(test)]
mod tests {
    use super::{
        BUILTIN_BINDINGS, BUILTIN_DECORATORS, BUILTIN_FUNCTIONS, BUILTIN_VALUE_MODULE,
        LANGUAGE_BUILTINS, LANGUAGE_DECORATORS, LANGUAGE_TYPE_NAMES, ReceiverBuiltinKind,
        ReceiverReturnKind, builtin_info, builtin_return_kind, decorator_info, editor_symbol_info,
        infer_receiver_member, type_name_info,
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
    fn builtin_return_kinds_are_exposed_from_metadata() {
        assert_eq!(
            builtin_return_kind("print"),
            Some(super::BuiltinReturnKind::Nothing)
        );
        assert_eq!(
            builtin_return_kind("input"),
            Some(super::BuiltinReturnKind::String)
        );
        assert_eq!(builtin_return_kind("Shared"), None);
    }

    #[test]
    fn receiver_member_metadata_is_centralized() {
        let len =
            infer_receiver_member(ReceiverBuiltinKind::String, "len").expect("string len metadata");
        assert_eq!(len.canonical_name, "len");
        assert_eq!(len.field_return, Some(ReceiverReturnKind::Integer));
        assert_eq!(len.method_return, Some(ReceiverReturnKind::Integer));

        let downgrade = infer_receiver_member(ReceiverBuiltinKind::Shared, "downgrade")
            .expect("shared downgrade metadata");
        assert_eq!(downgrade.canonical_name, "weak");

        let dict_contains = infer_receiver_member(ReceiverBuiltinKind::Dict, "has_key")
            .expect("dict contains alias metadata");
        assert_eq!(dict_contains.canonical_name, "containsKey");

        let list_append = infer_receiver_member(ReceiverBuiltinKind::List, "add")
            .expect("list append alias metadata");
        assert_eq!(list_append.canonical_name, "append");

        let list_reversed = infer_receiver_member(ReceiverBuiltinKind::List, "reversed")
            .expect("list reversed metadata");
        assert_eq!(list_reversed.canonical_name, "reversed");

        assert!(infer_receiver_member(ReceiverBuiltinKind::String, "filter").is_none());
    }

    #[test]
    fn builtin_type_names_are_documented() {
        for builtin in LANGUAGE_TYPE_NAMES {
            assert_eq!(
                type_name_info(builtin.name).map(|info| info.name),
                Some(builtin.name)
            );
            assert_eq!(
                editor_symbol_info(builtin.name).map(|info| info.name),
                Some(builtin.name)
            );
            assert!(!builtin.doc.is_empty());
        }
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
