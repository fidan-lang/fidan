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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceiverMemberSpec {
    pub names: &'static [&'static str],
    pub info: ReceiverMemberInfo,
}

const fn spec(
    names: &'static [&'static str],
    canonical_name: &'static str,
    field_return: Option<ReceiverReturnKind>,
    method_return: Option<ReceiverReturnKind>,
) -> ReceiverMemberSpec {
    ReceiverMemberSpec {
        names,
        info: ReceiverMemberInfo {
            canonical_name,
            field_return,
            method_return,
        },
    }
}

const INTEGER_MEMBER_SPECS: &[ReceiverMemberSpec] = &[
    spec(&["abs"], "abs", None, Some(ReceiverReturnKind::Integer)),
    spec(&["sqrt"], "sqrt", None, Some(ReceiverReturnKind::Float)),
    spec(
        &["toFloat", "to_float"],
        "toFloat",
        None,
        Some(ReceiverReturnKind::Float),
    ),
    spec(
        &["toString", "to_string"],
        "toString",
        None,
        Some(ReceiverReturnKind::String),
    ),
];

const FLOAT_MEMBER_SPECS: &[ReceiverMemberSpec] = &[
    spec(&["abs"], "abs", None, Some(ReceiverReturnKind::Float)),
    spec(&["sqrt"], "sqrt", None, Some(ReceiverReturnKind::Float)),
    spec(&["floor"], "floor", None, Some(ReceiverReturnKind::Integer)),
    spec(&["ceil"], "ceil", None, Some(ReceiverReturnKind::Integer)),
    spec(&["round"], "round", None, Some(ReceiverReturnKind::Integer)),
    spec(
        &["toInt", "to_int"],
        "toInt",
        None,
        Some(ReceiverReturnKind::Integer),
    ),
    spec(
        &["toString", "to_string"],
        "toString",
        None,
        Some(ReceiverReturnKind::String),
    ),
];

const STRING_MEMBER_SPECS: &[ReceiverMemberSpec] = &[
    spec(
        &["len", "length"],
        "len",
        Some(ReceiverReturnKind::Integer),
        Some(ReceiverReturnKind::Integer),
    ),
    spec(
        &["byteLen", "byte_len"],
        "byteLen",
        Some(ReceiverReturnKind::Integer),
        Some(ReceiverReturnKind::Integer),
    ),
    spec(
        &["lower", "toLower", "to_lower"],
        "lower",
        None,
        Some(ReceiverReturnKind::String),
    ),
    spec(
        &["upper", "toUpper", "to_upper"],
        "upper",
        None,
        Some(ReceiverReturnKind::String),
    ),
    spec(
        &["capitalize"],
        "capitalize",
        None,
        Some(ReceiverReturnKind::String),
    ),
    spec(&["trim"], "trim", None, Some(ReceiverReturnKind::String)),
    spec(
        &["trimStart", "ltrim", "trim_start"],
        "trimStart",
        None,
        Some(ReceiverReturnKind::String),
    ),
    spec(
        &["trimEnd", "rtrim", "trim_end"],
        "trimEnd",
        None,
        Some(ReceiverReturnKind::String),
    ),
    spec(
        &["split"],
        "split",
        None,
        Some(ReceiverReturnKind::ListOfString),
    ),
    spec(
        &["lines"],
        "lines",
        None,
        Some(ReceiverReturnKind::ListOfString),
    ),
    spec(
        &["chars"],
        "chars",
        None,
        Some(ReceiverReturnKind::ListOfString),
    ),
    spec(&["join"], "join", None, Some(ReceiverReturnKind::String)),
    spec(
        &["contains"],
        "contains",
        None,
        Some(ReceiverReturnKind::Boolean),
    ),
    spec(
        &["startsWith", "starts_with"],
        "startsWith",
        None,
        Some(ReceiverReturnKind::Boolean),
    ),
    spec(
        &["endsWith", "ends_with"],
        "endsWith",
        None,
        Some(ReceiverReturnKind::Boolean),
    ),
    spec(
        &["indexOf", "index_of", "find"],
        "indexOf",
        None,
        Some(ReceiverReturnKind::Integer),
    ),
    spec(
        &["lastIndexOf", "last_index_of"],
        "lastIndexOf",
        None,
        Some(ReceiverReturnKind::Integer),
    ),
    spec(
        &["replace"],
        "replace",
        None,
        Some(ReceiverReturnKind::String),
    ),
    spec(
        &["replaceAll", "replace_all"],
        "replaceAll",
        None,
        Some(ReceiverReturnKind::String),
    ),
    spec(
        &["replaceFirst", "replace_first"],
        "replaceFirst",
        None,
        Some(ReceiverReturnKind::String),
    ),
    spec(
        &["repeat"],
        "repeat",
        None,
        Some(ReceiverReturnKind::String),
    ),
    spec(
        &["reverse", "reversed"],
        "reverse",
        None,
        Some(ReceiverReturnKind::String),
    ),
    spec(
        &["charAt", "char_at"],
        "charAt",
        None,
        Some(ReceiverReturnKind::String),
    ),
    spec(
        &["substring", "substr", "slice"],
        "substring",
        None,
        Some(ReceiverReturnKind::String),
    ),
    spec(
        &["toInt", "to_int", "parseInt", "parse_int"],
        "toInt",
        None,
        Some(ReceiverReturnKind::Integer),
    ),
    spec(
        &["toFloat", "to_float", "parseFloat", "parse_float"],
        "toFloat",
        None,
        Some(ReceiverReturnKind::Float),
    ),
    spec(
        &["toBool", "to_bool"],
        "toBool",
        None,
        Some(ReceiverReturnKind::Boolean),
    ),
    spec(
        &["toString", "to_string"],
        "toString",
        None,
        Some(ReceiverReturnKind::String),
    ),
    spec(
        &["padStart", "pad_start"],
        "padStart",
        None,
        Some(ReceiverReturnKind::String),
    ),
    spec(
        &["padEnd", "pad_end"],
        "padEnd",
        None,
        Some(ReceiverReturnKind::String),
    ),
    spec(
        &["bytes"],
        "bytes",
        None,
        Some(ReceiverReturnKind::ListOfInteger),
    ),
    spec(
        &["charCode", "char_code"],
        "charCode",
        None,
        Some(ReceiverReturnKind::Integer),
    ),
    spec(
        &["format"],
        "format",
        None,
        Some(ReceiverReturnKind::String),
    ),
    spec(
        &["isEmpty", "is_empty"],
        "isEmpty",
        None,
        Some(ReceiverReturnKind::Boolean),
    ),
];

const LIST_MEMBER_SPECS: &[ReceiverMemberSpec] = &[
    spec(
        &["len", "length", "size"],
        "len",
        Some(ReceiverReturnKind::Integer),
        Some(ReceiverReturnKind::Integer),
    ),
    spec(
        &["isEmpty", "is_empty"],
        "isEmpty",
        None,
        Some(ReceiverReturnKind::Boolean),
    ),
    spec(
        &["append", "push", "add"],
        "append",
        None,
        Some(ReceiverReturnKind::Nothing),
    ),
    spec(
        &["pop"],
        "pop",
        None,
        Some(ReceiverReturnKind::ReceiverElement),
    ),
    spec(
        &["first", "head"],
        "first",
        None,
        Some(ReceiverReturnKind::ReceiverElement),
    ),
    spec(
        &["last"],
        "last",
        None,
        Some(ReceiverReturnKind::ReceiverElement),
    ),
    spec(
        &["get"],
        "get",
        None,
        Some(ReceiverReturnKind::ReceiverElement),
    ),
    spec(
        &["contains"],
        "contains",
        None,
        Some(ReceiverReturnKind::Boolean),
    ),
    spec(
        &["indexOf", "index_of"],
        "indexOf",
        None,
        Some(ReceiverReturnKind::Integer),
    ),
    spec(
        &["reverse"],
        "reverse",
        None,
        Some(ReceiverReturnKind::Nothing),
    ),
    spec(
        &["reversed"],
        "reversed",
        None,
        Some(ReceiverReturnKind::ListOfReceiverElement),
    ),
    spec(&["sort"], "sort", None, Some(ReceiverReturnKind::Nothing)),
    spec(&["join"], "join", None, Some(ReceiverReturnKind::String)),
    spec(
        &["slice"],
        "slice",
        None,
        Some(ReceiverReturnKind::ListOfReceiverElement),
    ),
    spec(
        &["flatten"],
        "flatten",
        None,
        Some(ReceiverReturnKind::ListOfDynamic),
    ),
    spec(
        &["extend", "concat"],
        "extend",
        None,
        Some(ReceiverReturnKind::Nothing),
    ),
    spec(
        &["toString", "to_string"],
        "toString",
        None,
        Some(ReceiverReturnKind::String),
    ),
    spec(
        &["forEach", "for_each", "each"],
        "forEach",
        None,
        Some(ReceiverReturnKind::Nothing),
    ),
    spec(
        &["map", "transform", "collect"],
        "map",
        None,
        Some(ReceiverReturnKind::ListOfDynamic),
    ),
    spec(
        &["filter", "where_", "select"],
        "filter",
        None,
        Some(ReceiverReturnKind::ListOfReceiverElement),
    ),
    spec(&["find"], "find", None, Some(ReceiverReturnKind::Dynamic)),
    spec(
        &["firstWhere", "first_where"],
        "firstWhere",
        None,
        Some(ReceiverReturnKind::ReceiverElement),
    ),
    spec(
        &["remove"],
        "remove",
        None,
        Some(ReceiverReturnKind::ReceiverElement),
    ),
    spec(
        &["reduce", "fold"],
        "reduce",
        None,
        Some(ReceiverReturnKind::Dynamic),
    ),
];

const DICT_MEMBER_SPECS: &[ReceiverMemberSpec] = &[
    spec(
        &["len", "length", "size"],
        "len",
        Some(ReceiverReturnKind::Integer),
        Some(ReceiverReturnKind::Integer),
    ),
    spec(
        &["isEmpty", "is_empty"],
        "isEmpty",
        None,
        Some(ReceiverReturnKind::Boolean),
    ),
    spec(&["get"], "get", None, Some(ReceiverReturnKind::DictValue)),
    spec(
        &["set", "put", "insert"],
        "set",
        None,
        Some(ReceiverReturnKind::Nothing),
    ),
    spec(
        &["contains", "has", "has_key", "containsKey", "contains_key"],
        "containsKey",
        None,
        Some(ReceiverReturnKind::Boolean),
    ),
    spec(
        &["remove", "delete"],
        "remove",
        None,
        Some(ReceiverReturnKind::Nothing),
    ),
    spec(
        &["keys"],
        "keys",
        None,
        Some(ReceiverReturnKind::ListOfString),
    ),
    spec(
        &["values"],
        "values",
        None,
        Some(ReceiverReturnKind::ListOfDictValue),
    ),
    spec(
        &["entries", "items"],
        "entries",
        None,
        Some(ReceiverReturnKind::ListOfDynamicPairs),
    ),
    spec(
        &["toString", "to_string"],
        "toString",
        None,
        Some(ReceiverReturnKind::String),
    ),
];

const SHARED_MEMBER_SPECS: &[ReceiverMemberSpec] = &[
    spec(&["type"], "type", Some(ReceiverReturnKind::String), None),
    spec(
        &["get"],
        "get",
        None,
        Some(ReceiverReturnKind::SharedInnerValue),
    ),
    spec(&["set"], "set", None, Some(ReceiverReturnKind::Nothing)),
    spec(
        &["weak", "downgrade"],
        "weak",
        None,
        Some(ReceiverReturnKind::WeakSharedOfInner),
    ),
];

const WEAK_SHARED_MEMBER_SPECS: &[ReceiverMemberSpec] = &[
    spec(&["type"], "type", Some(ReceiverReturnKind::String), None),
    spec(
        &["upgrade"],
        "upgrade",
        None,
        Some(ReceiverReturnKind::SharedOfInner),
    ),
    spec(
        &["isAlive", "is_alive", "alive"],
        "isAlive",
        None,
        Some(ReceiverReturnKind::Boolean),
    ),
];

const FUNCTION_MEMBER_SPECS: &[ReceiverMemberSpec] = &[spec(
    &["name"],
    "name",
    Some(ReceiverReturnKind::String),
    None,
)];

pub fn receiver_member_specs(receiver_kind: ReceiverBuiltinKind) -> &'static [ReceiverMemberSpec] {
    use ReceiverBuiltinKind as Kind;

    match receiver_kind {
        Kind::Integer => INTEGER_MEMBER_SPECS,
        Kind::Float => FLOAT_MEMBER_SPECS,
        Kind::String => STRING_MEMBER_SPECS,
        Kind::List => LIST_MEMBER_SPECS,
        Kind::Dict => DICT_MEMBER_SPECS,
        Kind::Shared => SHARED_MEMBER_SPECS,
        Kind::WeakShared => WEAK_SHARED_MEMBER_SPECS,
        Kind::Function => FUNCTION_MEMBER_SPECS,
        _ => &[],
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
    receiver_member_specs(receiver_kind)
        .iter()
        .find(|spec| spec.names.contains(&name))
        .map(|spec| spec.info)
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
