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
    "hashset",
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
    "hashset",
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
pub enum BuiltinSemantic {
    Print,
    Eprint,
    Input,
    Len,
    Type,
    HashSetConstructor,
    String,
    Integer,
    Float,
    Boolean,
    SharedConstructor,
    WeakSharedConstructor,
    Assert,
    AssertEq,
    AssertNe,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReceiverBuiltinKind {
    Integer,
    Float,
    Boolean,
    String,
    List,
    Dict,
    HashSet,
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
    ReceiverSelf,
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
pub enum ReceiverMethodOp {
    Len,
    IsEmpty,
    Get,
    Set,
    Contains,
    Remove,
    Keys,
    Values,
    Entries,
    Insert,
    ToList,
    Union,
    Intersect,
    Diff,
    ToString,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceiverMemberInfo {
    pub canonical_name: &'static str,
    pub operation: Option<ReceiverMethodOp>,
    pub field_return: Option<ReceiverReturnKind>,
    pub method_return: Option<ReceiverReturnKind>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceiverMemberSpec {
    pub names: &'static [&'static str],
    pub info: ReceiverMemberInfo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReceiverParamInfo {
    pub name: &'static str,
    pub type_name: &'static str,
    pub optional: bool,
    pub variadic: bool,
}

const fn spec(
    names: &'static [&'static str],
    canonical_name: &'static str,
    field_return: Option<ReceiverReturnKind>,
    method_return: Option<ReceiverReturnKind>,
) -> ReceiverMemberSpec {
    spec_with_op(names, canonical_name, None, field_return, method_return)
}

const fn spec_with_op(
    names: &'static [&'static str],
    canonical_name: &'static str,
    operation: Option<ReceiverMethodOp>,
    field_return: Option<ReceiverReturnKind>,
    method_return: Option<ReceiverReturnKind>,
) -> ReceiverMemberSpec {
    ReceiverMemberSpec {
        names,
        info: ReceiverMemberInfo {
            canonical_name,
            operation,
            field_return,
            method_return,
        },
    }
}

const fn param(name: &'static str, type_name: &'static str) -> ReceiverParamInfo {
    ReceiverParamInfo {
        name,
        type_name,
        optional: false,
        variadic: false,
    }
}

const fn optional_param(name: &'static str, type_name: &'static str) -> ReceiverParamInfo {
    ReceiverParamInfo {
        name,
        type_name,
        optional: true,
        variadic: false,
    }
}

const fn variadic_param(name: &'static str, type_name: &'static str) -> ReceiverParamInfo {
    ReceiverParamInfo {
        name,
        type_name,
        optional: false,
        variadic: true,
    }
}

const NO_RECEIVER_PARAMS: &[ReceiverParamInfo] = &[];
const STRING_TEXT_PARAM: &[ReceiverParamInfo] = &[param("text", "string")];
const STRING_PREFIX_PARAM: &[ReceiverParamInfo] = &[param("prefix", "string")];
const STRING_SUFFIX_PARAM: &[ReceiverParamInfo] = &[param("suffix", "string")];
const STRING_SEPARATOR_PARAM: &[ReceiverParamInfo] = &[param("separator", "string")];
const STRING_INDEX_PARAM: &[ReceiverParamInfo] = &[param("index", "integer")];
const STRING_COUNT_PARAM: &[ReceiverParamInfo] = &[param("count", "integer")];
const STRING_VALUES_PARAM: &[ReceiverParamInfo] = &[param("values", "list oftype dynamic")];
const STRING_VALUE_PARAM: &[ReceiverParamInfo] = &[param("value", "string")];
const STRING_REPLACE_PARAMS: &[ReceiverParamInfo] =
    &[param("from", "string"), param("to", "string")];
const STRING_SUBSTRING_PARAMS: &[ReceiverParamInfo] =
    &[param("start", "integer"), optional_param("end", "integer")];
const STRING_WIDTH_AND_PAD_PARAMS: &[ReceiverParamInfo] =
    &[param("width", "integer"), optional_param("pad", "string")];
const STRING_FORMAT_VALUES_PARAM: &[ReceiverParamInfo] = &[variadic_param("values", "dynamic")];
const LIST_VALUE_PARAM: &[ReceiverParamInfo] = &[param("value", "T")];
const LIST_VALUES_PARAM: &[ReceiverParamInfo] = &[variadic_param("values", "T")];
const LIST_INDEX_PARAM: &[ReceiverParamInfo] = &[param("index", "integer")];
const LIST_SEPARATOR_PARAM: &[ReceiverParamInfo] = &[optional_param("separator", "string")];
const LIST_SLICE_PARAMS: &[ReceiverParamInfo] = &[
    optional_param("start", "integer"),
    optional_param("end", "integer"),
    optional_param("step", "integer"),
];
const LIST_ITEMS_PARAM: &[ReceiverParamInfo] = &[param("items", "list oftype T")];
const LIST_CALLBACK_PARAM: &[ReceiverParamInfo] = &[param("callback", "action")];
const LIST_PREDICATE_PARAM: &[ReceiverParamInfo] = &[param("predicate", "action")];
const LIST_REDUCE_PARAMS: &[ReceiverParamInfo] = &[
    param("callback", "action"),
    optional_param("initial", "dynamic"),
];
const DICT_KEY_PARAM: &[ReceiverParamInfo] = &[param("key", "K")];
const DICT_SET_PARAMS: &[ReceiverParamInfo] = &[param("key", "K"), param("value", "V")];
const HASHSET_VALUE_PARAM: &[ReceiverParamInfo] = &[param("value", "T")];
const HASHSET_OTHER_PARAM: &[ReceiverParamInfo] = &[param("other", "hashset oftype T")];
const SHARED_VALUE_PARAM: &[ReceiverParamInfo] = &[param("value", "T")];

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
    spec_with_op(
        &["len", "length", "size"],
        "len",
        Some(ReceiverMethodOp::Len),
        Some(ReceiverReturnKind::Integer),
        Some(ReceiverReturnKind::Integer),
    ),
    spec_with_op(
        &["isEmpty", "is_empty"],
        "isEmpty",
        Some(ReceiverMethodOp::IsEmpty),
        None,
        Some(ReceiverReturnKind::Boolean),
    ),
    spec_with_op(
        &["get"],
        "get",
        Some(ReceiverMethodOp::Get),
        None,
        Some(ReceiverReturnKind::DictValue),
    ),
    spec_with_op(
        &["set", "put", "insert"],
        "set",
        Some(ReceiverMethodOp::Set),
        None,
        Some(ReceiverReturnKind::Nothing),
    ),
    spec_with_op(
        &["contains", "has", "has_key", "containsKey", "contains_key"],
        "containsKey",
        Some(ReceiverMethodOp::Contains),
        None,
        Some(ReceiverReturnKind::Boolean),
    ),
    spec_with_op(
        &["remove", "delete"],
        "remove",
        Some(ReceiverMethodOp::Remove),
        None,
        Some(ReceiverReturnKind::Nothing),
    ),
    spec_with_op(
        &["keys"],
        "keys",
        Some(ReceiverMethodOp::Keys),
        None,
        Some(ReceiverReturnKind::ListOfString),
    ),
    spec_with_op(
        &["values"],
        "values",
        Some(ReceiverMethodOp::Values),
        None,
        Some(ReceiverReturnKind::ListOfDictValue),
    ),
    spec_with_op(
        &["entries", "items"],
        "entries",
        Some(ReceiverMethodOp::Entries),
        None,
        Some(ReceiverReturnKind::ListOfDynamicPairs),
    ),
    spec_with_op(
        &["toString", "to_string"],
        "toString",
        Some(ReceiverMethodOp::ToString),
        None,
        Some(ReceiverReturnKind::String),
    ),
];

const HASHSET_MEMBER_SPECS: &[ReceiverMemberSpec] = &[
    spec_with_op(
        &["len", "length", "size"],
        "len",
        Some(ReceiverMethodOp::Len),
        Some(ReceiverReturnKind::Integer),
        Some(ReceiverReturnKind::Integer),
    ),
    spec_with_op(
        &["isEmpty", "is_empty"],
        "isEmpty",
        Some(ReceiverMethodOp::IsEmpty),
        None,
        Some(ReceiverReturnKind::Boolean),
    ),
    spec_with_op(
        &["insert", "add"],
        "insert",
        Some(ReceiverMethodOp::Insert),
        None,
        Some(ReceiverReturnKind::Nothing),
    ),
    spec_with_op(
        &["remove", "delete"],
        "remove",
        Some(ReceiverMethodOp::Remove),
        None,
        Some(ReceiverReturnKind::Nothing),
    ),
    spec_with_op(
        &["contains", "has"],
        "contains",
        Some(ReceiverMethodOp::Contains),
        None,
        Some(ReceiverReturnKind::Boolean),
    ),
    spec_with_op(
        &["toList", "to_list"],
        "toList",
        Some(ReceiverMethodOp::ToList),
        None,
        Some(ReceiverReturnKind::ListOfReceiverElement),
    ),
    spec_with_op(
        &["union"],
        "union",
        Some(ReceiverMethodOp::Union),
        None,
        Some(ReceiverReturnKind::ReceiverSelf),
    ),
    spec_with_op(
        &["intersect", "intersection"],
        "intersect",
        Some(ReceiverMethodOp::Intersect),
        None,
        Some(ReceiverReturnKind::ReceiverSelf),
    ),
    spec_with_op(
        &["diff", "difference"],
        "diff",
        Some(ReceiverMethodOp::Diff),
        None,
        Some(ReceiverReturnKind::ReceiverSelf),
    ),
    spec_with_op(
        &["toString", "to_string"],
        "toString",
        Some(ReceiverMethodOp::ToString),
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
        Kind::HashSet => HASHSET_MEMBER_SPECS,
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
    pub semantic: Option<BuiltinSemantic>,
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
        semantic: Some(BuiltinSemantic::Print),
        return_kind: Some(BuiltinReturnKind::Nothing),
    },
    BuiltinInfo {
        name: "eprint",
        signature: "eprint(value...) -> nothing",
        doc: "Print values to stderr followed by a newline.",
        semantic: Some(BuiltinSemantic::Eprint),
        return_kind: Some(BuiltinReturnKind::Nothing),
    },
    BuiltinInfo {
        name: "input",
        signature: "input(prompt?) -> string",
        doc: "Read one line of input, optionally after showing a prompt.",
        semantic: Some(BuiltinSemantic::Input),
        return_kind: Some(BuiltinReturnKind::String),
    },
    BuiltinInfo {
        name: "len",
        signature: "len(value) -> integer",
        doc: "Return the length of a string, list, or other countable value.",
        semantic: Some(BuiltinSemantic::Len),
        return_kind: Some(BuiltinReturnKind::Integer),
    },
    BuiltinInfo {
        name: "type",
        signature: "type(value) -> string",
        doc: "Return the runtime type name of a value as a string.",
        semantic: Some(BuiltinSemantic::Type),
        return_kind: Some(BuiltinReturnKind::String),
    },
    BuiltinInfo {
        name: "hashset",
        signature: "hashset(items?) -> hashset",
        doc: "Create a real hashset of unique, hashable values from a list or another hashset.",
        semantic: Some(BuiltinSemantic::HashSetConstructor),
        return_kind: None,
    },
    BuiltinInfo {
        name: "string",
        signature: "string(value) -> string",
        doc: "Convert a value to its string representation.",
        semantic: Some(BuiltinSemantic::String),
        return_kind: Some(BuiltinReturnKind::String),
    },
    BuiltinInfo {
        name: "integer",
        signature: "integer(value) -> integer",
        doc: "Convert a value to an integer when possible.",
        semantic: Some(BuiltinSemantic::Integer),
        return_kind: Some(BuiltinReturnKind::Integer),
    },
    BuiltinInfo {
        name: "float",
        signature: "float(value) -> float",
        doc: "Convert a value to a floating-point number when possible.",
        semantic: Some(BuiltinSemantic::Float),
        return_kind: Some(BuiltinReturnKind::Float),
    },
    BuiltinInfo {
        name: "boolean",
        signature: "boolean(value) -> boolean",
        doc: "Convert a value to a boolean truth value.",
        semantic: Some(BuiltinSemantic::Boolean),
        return_kind: Some(BuiltinReturnKind::Boolean),
    },
    BuiltinInfo {
        name: "Shared",
        signature: "Shared(value) -> Shared",
        doc: "Create a shared, thread-safe wrapper so values can be mutated safely across parallel work.",
        semantic: Some(BuiltinSemantic::SharedConstructor),
        return_kind: None,
    },
    BuiltinInfo {
        name: "WeakShared",
        signature: "WeakShared(shared) -> WeakShared",
        doc: "Create a non-owning weak handle to an existing Shared value. Use `upgrade()` to recover a Shared while it is still alive.",
        semantic: Some(BuiltinSemantic::WeakSharedConstructor),
        return_kind: None,
    },
    BuiltinInfo {
        name: "assert",
        signature: "assert(condition, message?) -> nothing",
        doc: "Fail immediately when the condition is not truthy.",
        semantic: Some(BuiltinSemantic::Assert),
        return_kind: Some(BuiltinReturnKind::Nothing),
    },
    BuiltinInfo {
        name: "assert_eq",
        signature: "assert_eq(left, right, message?) -> nothing",
        doc: "Fail immediately when two values are not equal.",
        semantic: Some(BuiltinSemantic::AssertEq),
        return_kind: Some(BuiltinReturnKind::Nothing),
    },
    BuiltinInfo {
        name: "assert_ne",
        signature: "assert_ne(left, right, message?) -> nothing",
        doc: "Fail immediately when two values are equal.",
        semantic: Some(BuiltinSemantic::AssertNe),
        return_kind: Some(BuiltinReturnKind::Nothing),
    },
];

pub const LANGUAGE_TYPE_NAMES: &[BuiltinInfo] = &[
    BuiltinInfo {
        name: "handle",
        signature: "handle",
        doc: "Opaque native handle type used for extern interop and low-level OS or library handles.",
        semantic: None,
        return_kind: None,
    },
    BuiltinInfo {
        name: "list",
        signature: "list oftype T",
        doc: "Ordered growable collection. Use `list oftype T` for typed lists or bare `list` when element type is dynamic.",
        semantic: None,
        return_kind: None,
    },
    BuiltinInfo {
        name: "dict",
        signature: "dict oftype (K, V)",
        doc: "Key-value map type. Use `dict oftype (K, V)` for typed dictionaries or bare `dict` when both sides are dynamic.",
        semantic: None,
        return_kind: None,
    },
    BuiltinInfo {
        name: "map",
        signature: "map oftype (K, V)",
        doc: "Alias for `dict`. Use `map oftype (K, V)` when you prefer map-style terminology.",
        semantic: None,
        return_kind: None,
    },
    BuiltinInfo {
        name: "hashset",
        signature: "hashset oftype T",
        doc: "Unordered collection of unique, hashable values. Use `hashset oftype T` for typed sets or bare `hashset` for dynamic elements.",
        semantic: None,
        return_kind: None,
    },
    BuiltinInfo {
        name: "tuple",
        signature: "tuple or (T1, T2, ...)",
        doc: "Fixed-position product type. Use bare `tuple` for an untyped tuple value or `(T1, T2, ...)` for a typed tuple shape.",
        semantic: None,
        return_kind: None,
    },
    BuiltinInfo {
        name: "Shared",
        signature: "Shared oftype T",
        doc: "Thread-safe shared wrapper type for values used across parallel work.",
        semantic: None,
        return_kind: None,
    },
    BuiltinInfo {
        name: "WeakShared",
        signature: "WeakShared oftype T",
        doc: "Non-owning weak reference to a `Shared` value. Upgrade it when the shared value is still alive.",
        semantic: None,
        return_kind: None,
    },
    BuiltinInfo {
        name: "Pending",
        signature: "Pending oftype T",
        doc: "Handle type for asynchronous work that will eventually resolve to `T`.",
        semantic: None,
        return_kind: None,
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

pub fn builtin_return_kind(name: &str) -> Option<BuiltinReturnKind> {
    builtin_info(name).and_then(|info| info.return_kind)
}

pub fn builtin_semantic(name: &str) -> Option<BuiltinSemantic> {
    builtin_info(name).and_then(|info| info.semantic)
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

fn receiver_signature_type_name(receiver_kind: ReceiverBuiltinKind) -> &'static str {
    match receiver_kind {
        ReceiverBuiltinKind::Integer => "integer",
        ReceiverBuiltinKind::Float => "float",
        ReceiverBuiltinKind::Boolean => "boolean",
        ReceiverBuiltinKind::String => "string",
        ReceiverBuiltinKind::List => "list",
        ReceiverBuiltinKind::Dict => "dict",
        ReceiverBuiltinKind::HashSet => "hashset",
        ReceiverBuiltinKind::Handle => "handle",
        ReceiverBuiltinKind::Nothing => "nothing",
        ReceiverBuiltinKind::Dynamic => "dynamic",
        ReceiverBuiltinKind::Shared => "Shared",
        ReceiverBuiltinKind::WeakShared => "WeakShared",
        ReceiverBuiltinKind::Pending => "Pending",
        ReceiverBuiltinKind::Function => "action",
    }
}

fn receiver_self_type_name(receiver_kind: ReceiverBuiltinKind) -> &'static str {
    match receiver_kind {
        ReceiverBuiltinKind::List => "list oftype T",
        ReceiverBuiltinKind::Dict => "dict oftype (K, V)",
        ReceiverBuiltinKind::HashSet => "hashset oftype T",
        ReceiverBuiltinKind::Shared => "Shared oftype T",
        ReceiverBuiltinKind::WeakShared => "WeakShared oftype T",
        ReceiverBuiltinKind::Pending => "Pending oftype T",
        _ => receiver_signature_type_name(receiver_kind),
    }
}

fn receiver_return_type_name(
    receiver_kind: ReceiverBuiltinKind,
    return_kind: ReceiverReturnKind,
) -> String {
    match return_kind {
        ReceiverReturnKind::Integer => "integer".to_string(),
        ReceiverReturnKind::Float => "float".to_string(),
        ReceiverReturnKind::Boolean => "boolean".to_string(),
        ReceiverReturnKind::String => "string".to_string(),
        ReceiverReturnKind::Dynamic => "dynamic".to_string(),
        ReceiverReturnKind::Nothing => "nothing".to_string(),
        ReceiverReturnKind::ReceiverSelf => receiver_self_type_name(receiver_kind).to_string(),
        ReceiverReturnKind::ReceiverElement => "T".to_string(),
        ReceiverReturnKind::DictValue => "V".to_string(),
        ReceiverReturnKind::ListOfString => "list oftype string".to_string(),
        ReceiverReturnKind::ListOfInteger => "list oftype integer".to_string(),
        ReceiverReturnKind::ListOfDynamic => "list oftype dynamic".to_string(),
        ReceiverReturnKind::ListOfReceiverElement => "list oftype T".to_string(),
        ReceiverReturnKind::ListOfDictValue => "list oftype V".to_string(),
        ReceiverReturnKind::ListOfDynamicPairs => "list oftype (dynamic, dynamic)".to_string(),
        ReceiverReturnKind::SharedInnerValue => "T".to_string(),
        ReceiverReturnKind::SharedOfInner => "Shared oftype T".to_string(),
        ReceiverReturnKind::WeakSharedOfInner => "WeakShared oftype T".to_string(),
    }
}

fn split_top_level_pair(input: &str) -> Option<(String, String)> {
    let mut depth = 0usize;
    for (index, ch) in input.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                let left = input[..index].trim();
                let right = input[index + 1..].trim();
                if left.is_empty() || right.is_empty() {
                    return None;
                }
                return Some((left.to_string(), right.to_string()));
            }
            _ => {}
        }
    }
    None
}

fn receiver_type_bindings(
    receiver_kind: ReceiverBuiltinKind,
    receiver_type_name: &str,
) -> (Option<String>, Option<String>, Option<String>) {
    let trimmed = receiver_type_name.trim();
    match receiver_kind {
        ReceiverBuiltinKind::List => (
            trimmed
                .strip_prefix("list oftype ")
                .map(str::trim)
                .filter(|inner| !inner.is_empty())
                .map(ToOwned::to_owned),
            None,
            None,
        ),
        ReceiverBuiltinKind::HashSet => (
            trimmed
                .strip_prefix("hashset oftype ")
                .map(str::trim)
                .filter(|inner| !inner.is_empty())
                .map(ToOwned::to_owned),
            None,
            None,
        ),
        ReceiverBuiltinKind::Dict => {
            let Some(inner) = trimmed
                .strip_prefix("dict oftype ")
                .or_else(|| trimmed.strip_prefix("map oftype "))
                .map(str::trim)
            else {
                return (None, None, None);
            };
            let Some(tuple) = inner
                .strip_prefix('(')
                .and_then(|value| value.strip_suffix(')'))
            else {
                return (None, None, None);
            };
            let Some((key_ty, value_ty)) = split_top_level_pair(tuple) else {
                return (None, None, None);
            };
            (None, Some(key_ty), Some(value_ty))
        }
        ReceiverBuiltinKind::Shared => (
            trimmed
                .strip_prefix("Shared oftype ")
                .map(str::trim)
                .filter(|inner| !inner.is_empty())
                .map(ToOwned::to_owned),
            None,
            None,
        ),
        ReceiverBuiltinKind::WeakShared => (
            trimmed
                .strip_prefix("WeakShared oftype ")
                .map(str::trim)
                .filter(|inner| !inner.is_empty())
                .map(ToOwned::to_owned),
            None,
            None,
        ),
        ReceiverBuiltinKind::Pending => (
            trimmed
                .strip_prefix("Pending oftype ")
                .map(str::trim)
                .filter(|inner| !inner.is_empty())
                .map(ToOwned::to_owned),
            None,
            None,
        ),
        _ => (None, None, None),
    }
}

pub fn specialize_receiver_type_template(
    receiver_kind: ReceiverBuiltinKind,
    receiver_type_name: &str,
    template: &str,
) -> String {
    let (t_binding, k_binding, v_binding) =
        receiver_type_bindings(receiver_kind, receiver_type_name);

    let mut specialized = template.to_string();
    if let Some(key_ty) = k_binding {
        specialized = specialized.replace("K", &key_ty);
    }
    if let Some(value_ty) = v_binding {
        specialized = specialized.replace("V", &value_ty);
    }
    if let Some(elem_ty) = t_binding {
        specialized = specialized.replace("T", &elem_ty);
    }

    specialized
}

pub fn receiver_member_return_type_name_for_type_name(
    receiver_kind: ReceiverBuiltinKind,
    receiver_type_name: &str,
    name: &str,
) -> Option<String> {
    let info = infer_receiver_member(receiver_kind, name)?;
    let return_kind = info.method_return.or(info.field_return)?;
    if matches!(return_kind, ReceiverReturnKind::ReceiverSelf) {
        return Some(receiver_type_name.to_string());
    }

    Some(specialize_receiver_type_template(
        receiver_kind,
        receiver_type_name,
        &receiver_return_type_name(receiver_kind, return_kind),
    ))
}

pub fn receiver_member_param_type_names_for_type_name(
    receiver_kind: ReceiverBuiltinKind,
    receiver_type_name: &str,
    name: &str,
) -> Option<Vec<String>> {
    let info = infer_receiver_member(receiver_kind, name)?;
    let params =
        receiver_member_params(receiver_kind, info.canonical_name).unwrap_or(NO_RECEIVER_PARAMS);
    Some(
        params
            .iter()
            .map(|param| {
                specialize_receiver_type_template(
                    receiver_kind,
                    receiver_type_name,
                    param.type_name,
                )
            })
            .collect(),
    )
}

pub fn receiver_member_signature_for_type_name(
    receiver_kind: ReceiverBuiltinKind,
    receiver_type_name: &str,
    name: &str,
) -> Option<String> {
    let info = infer_receiver_member(receiver_kind, name)?;
    let params =
        receiver_member_params(receiver_kind, info.canonical_name).unwrap_or(NO_RECEIVER_PARAMS);
    let params = params
        .iter()
        .map(|param| {
            let ty_name = specialize_receiver_type_template(
                receiver_kind,
                receiver_type_name,
                param.type_name,
            );
            let mut rendered = format!("{} oftype {}", param.name, ty_name);
            if param.variadic {
                rendered.push_str("...");
            }
            if param.optional {
                rendered.push('?');
            }
            rendered
        })
        .collect::<Vec<_>>()
        .join(", ");

    if info.method_return.is_some() {
        let return_type = receiver_member_return_type_name_for_type_name(
            receiver_kind,
            receiver_type_name,
            info.canonical_name,
        )?;
        return Some(format!(
            "{}.{}({}) -> {}",
            receiver_signature_type_name(receiver_kind),
            info.canonical_name,
            params,
            return_type
        ));
    }

    let return_type = receiver_member_return_type_name_for_type_name(
        receiver_kind,
        receiver_type_name,
        info.canonical_name,
    )?;
    Some(format!(
        "{}.{} -> {}",
        receiver_signature_type_name(receiver_kind),
        info.canonical_name,
        return_type
    ))
}

pub fn receiver_member_params(
    receiver_kind: ReceiverBuiltinKind,
    name: &str,
) -> Option<&'static [ReceiverParamInfo]> {
    let canonical = infer_receiver_member(receiver_kind, name)?.canonical_name;

    let params = match receiver_kind {
        ReceiverBuiltinKind::Integer | ReceiverBuiltinKind::Float => match canonical {
            "abs" | "sqrt" | "floor" | "ceil" | "round" | "toFloat" | "toInt" | "toString" => {
                NO_RECEIVER_PARAMS
            }
            _ => return None,
        },
        ReceiverBuiltinKind::Boolean => match canonical {
            "toString" => NO_RECEIVER_PARAMS,
            _ => return None,
        },
        ReceiverBuiltinKind::String => match canonical {
            "len" | "byteLen" | "lower" | "upper" | "capitalize" | "trim" | "trimStart"
            | "trimEnd" | "lines" | "chars" | "toInt" | "toFloat" | "toBool" | "toString"
            | "reverse" | "bytes" | "charCode" | "isEmpty" => NO_RECEIVER_PARAMS,
            "split" => STRING_SEPARATOR_PARAM,
            "join" => STRING_VALUES_PARAM,
            "contains" => STRING_TEXT_PARAM,
            "startsWith" => STRING_PREFIX_PARAM,
            "endsWith" => STRING_SUFFIX_PARAM,
            "indexOf" | "lastIndexOf" => STRING_VALUE_PARAM,
            "replace" | "replaceAll" | "replaceFirst" => STRING_REPLACE_PARAMS,
            "repeat" => STRING_COUNT_PARAM,
            "charAt" => STRING_INDEX_PARAM,
            "substring" => STRING_SUBSTRING_PARAMS,
            "padStart" | "padEnd" => STRING_WIDTH_AND_PAD_PARAMS,
            "format" => STRING_FORMAT_VALUES_PARAM,
            _ => return None,
        },
        ReceiverBuiltinKind::List => match canonical {
            "len" | "isEmpty" | "pop" | "first" | "last" | "reverse" | "reversed" | "sort"
            | "flatten" | "toString" => NO_RECEIVER_PARAMS,
            "append" => LIST_VALUES_PARAM,
            "get" | "remove" => LIST_INDEX_PARAM,
            "contains" | "indexOf" | "find" => LIST_VALUE_PARAM,
            "join" => LIST_SEPARATOR_PARAM,
            "slice" => LIST_SLICE_PARAMS,
            "extend" => LIST_ITEMS_PARAM,
            "forEach" | "map" => LIST_CALLBACK_PARAM,
            "filter" | "firstWhere" => LIST_PREDICATE_PARAM,
            "reduce" => LIST_REDUCE_PARAMS,
            _ => return None,
        },
        ReceiverBuiltinKind::Dict => match canonical {
            "len" | "isEmpty" | "keys" | "values" | "entries" | "toString" => NO_RECEIVER_PARAMS,
            "get" | "containsKey" | "remove" => DICT_KEY_PARAM,
            "set" => DICT_SET_PARAMS,
            _ => return None,
        },
        ReceiverBuiltinKind::HashSet => match canonical {
            "len" | "isEmpty" | "toList" | "toString" => NO_RECEIVER_PARAMS,
            "insert" | "remove" | "contains" => HASHSET_VALUE_PARAM,
            "union" | "intersect" | "diff" => HASHSET_OTHER_PARAM,
            _ => return None,
        },
        ReceiverBuiltinKind::Shared => match canonical {
            "get" | "weak" => NO_RECEIVER_PARAMS,
            "set" => SHARED_VALUE_PARAM,
            _ => return None,
        },
        ReceiverBuiltinKind::WeakShared => match canonical {
            "upgrade" | "isAlive" => NO_RECEIVER_PARAMS,
            _ => return None,
        },
        ReceiverBuiltinKind::Function => match canonical {
            "name" => NO_RECEIVER_PARAMS,
            _ => return None,
        },
        ReceiverBuiltinKind::Handle
        | ReceiverBuiltinKind::Nothing
        | ReceiverBuiltinKind::Dynamic
        | ReceiverBuiltinKind::Pending => return None,
    };

    Some(params)
}

pub fn receiver_member_signature(receiver_kind: ReceiverBuiltinKind, name: &str) -> Option<String> {
    let info = infer_receiver_member(receiver_kind, name)?;
    let receiver_name = receiver_signature_type_name(receiver_kind);
    let params =
        receiver_member_params(receiver_kind, info.canonical_name).unwrap_or(NO_RECEIVER_PARAMS);
    let params = params
        .iter()
        .map(|param| {
            let mut rendered = format!("{} oftype {}", param.name, param.type_name);
            if param.variadic {
                rendered.push_str("...");
            }
            if param.optional {
                rendered.push('?');
            }
            rendered
        })
        .collect::<Vec<_>>()
        .join(", ");

    if let Some(return_kind) = info.method_return {
        return Some(format!(
            "{}.{}({}) -> {}",
            receiver_name,
            info.canonical_name,
            params,
            receiver_return_type_name(receiver_kind, return_kind)
        ));
    }

    info.field_return.map(|return_kind| {
        format!(
            "{}.{} -> {}",
            receiver_name,
            info.canonical_name,
            receiver_return_type_name(receiver_kind, return_kind)
        )
    })
}

pub fn receiver_method_arity_bounds(
    receiver_kind: ReceiverBuiltinKind,
    name: &str,
) -> Option<(usize, Option<usize>)> {
    let info = infer_receiver_member(receiver_kind, name)?;
    info.method_return?;

    let params =
        receiver_member_params(receiver_kind, info.canonical_name).unwrap_or(NO_RECEIVER_PARAMS);
    let min_args = params.iter().filter(|param| !param.optional).count();
    let max_args = if params.iter().any(|param| param.variadic) {
        None
    } else {
        Some(params.len())
    };

    Some((min_args, max_args))
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
        BuiltinSemantic, LANGUAGE_BUILTINS, LANGUAGE_DECORATORS, LANGUAGE_TYPE_NAMES,
        ReceiverBuiltinKind, ReceiverMethodOp, ReceiverReturnKind, builtin_info,
        builtin_return_kind, builtin_semantic, decorator_info, editor_symbol_info,
        infer_receiver_member, receiver_member_params, receiver_member_signature,
        receiver_member_specs, receiver_method_arity_bounds, type_name_info,
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
    fn builtin_semantics_are_centralized() {
        assert_eq!(
            builtin_semantic("hashset"),
            Some(BuiltinSemantic::HashSetConstructor)
        );
        assert_eq!(
            builtin_semantic("Shared"),
            Some(BuiltinSemantic::SharedConstructor)
        );
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
        assert_eq!(dict_contains.operation, Some(ReceiverMethodOp::Contains));

        let hashset_insert = infer_receiver_member(ReceiverBuiltinKind::HashSet, "add")
            .expect("hashset add alias metadata");
        assert_eq!(hashset_insert.canonical_name, "insert");
        assert_eq!(hashset_insert.operation, Some(ReceiverMethodOp::Insert));

        let list_append = infer_receiver_member(ReceiverBuiltinKind::List, "add")
            .expect("list append alias metadata");
        assert_eq!(list_append.canonical_name, "append");

        let list_reversed = infer_receiver_member(ReceiverBuiltinKind::List, "reversed")
            .expect("list reversed metadata");
        assert_eq!(list_reversed.canonical_name, "reversed");

        assert!(infer_receiver_member(ReceiverBuiltinKind::String, "filter").is_none());
    }

    #[test]
    fn receiver_method_arity_bounds_follow_canonical_aliases() {
        assert_eq!(
            receiver_method_arity_bounds(ReceiverBuiltinKind::HashSet, "add"),
            Some((1, Some(1)))
        );
        assert_eq!(
            receiver_method_arity_bounds(ReceiverBuiltinKind::List, "add"),
            Some((1, None))
        );
        assert_eq!(
            receiver_method_arity_bounds(ReceiverBuiltinKind::Dict, "has"),
            Some((1, Some(1)))
        );
    }

    #[test]
    fn receiver_method_signatures_are_typed_and_centralized() {
        assert_eq!(
            receiver_member_signature(ReceiverBuiltinKind::HashSet, "contains").as_deref(),
            Some("hashset.contains(value oftype T) -> boolean")
        );
        assert_eq!(
            receiver_member_signature(ReceiverBuiltinKind::Dict, "get").as_deref(),
            Some("dict.get(key oftype K) -> V")
        );
        assert_eq!(
            receiver_member_signature(ReceiverBuiltinKind::List, "slice").as_deref(),
            Some(
                "list.slice(start oftype integer?, end oftype integer?, step oftype integer?) -> list oftype T"
            )
        );
    }

    #[test]
    fn receiver_method_signature_metadata_covers_every_builtin_receiver_member() {
        let receiver_kinds = [
            ReceiverBuiltinKind::Integer,
            ReceiverBuiltinKind::Float,
            ReceiverBuiltinKind::Boolean,
            ReceiverBuiltinKind::String,
            ReceiverBuiltinKind::List,
            ReceiverBuiltinKind::Dict,
            ReceiverBuiltinKind::HashSet,
            ReceiverBuiltinKind::Shared,
            ReceiverBuiltinKind::WeakShared,
            ReceiverBuiltinKind::Function,
        ];

        for receiver_kind in receiver_kinds {
            for spec in receiver_member_specs(receiver_kind) {
                let signature = receiver_member_signature(receiver_kind, spec.info.canonical_name)
                    .unwrap_or_else(|| {
                        panic!(
                            "missing signature metadata for {:?}.{}",
                            receiver_kind, spec.info.canonical_name
                        )
                    });
                assert!(
                    signature.contains(spec.info.canonical_name),
                    "signature should mention canonical member name: {signature}"
                );

                if spec.info.method_return.is_some() {
                    let params = receiver_member_params(receiver_kind, spec.info.canonical_name)
                        .unwrap_or(&[]);
                    if let Some((min_args, max_args)) =
                        receiver_method_arity_bounds(receiver_kind, spec.info.canonical_name)
                    {
                        let required_args = params.iter().filter(|param| !param.optional).count();
                        assert_eq!(
                            required_args, min_args,
                            "required arg count mismatch for {:?}.{}",
                            receiver_kind, spec.info.canonical_name
                        );
                        if let Some(max_args) = max_args {
                            assert_eq!(
                                params.len(),
                                max_args,
                                "max arg count mismatch for {:?}.{}",
                                receiver_kind,
                                spec.info.canonical_name
                            );
                        }
                    }
                }
            }
        }
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
