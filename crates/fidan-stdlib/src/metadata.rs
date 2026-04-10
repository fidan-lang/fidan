use fidan_config::{ReceiverBuiltinKind, ReceiverReturnKind, infer_receiver_member};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdlibValueKind {
    Integer,
    Float,
    Boolean,
    String,
    List,
    Dict,
    HashSet,
    Dynamic,
    Nothing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MathIntrinsic {
    Sqrt,
    Abs,
    Floor,
    Ceil,
    Trunc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdlibIntrinsic {
    Math(MathIntrinsic),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StdlibMethodInfo {
    pub return_kind: StdlibValueKind,
    pub intrinsic: Option<StdlibIntrinsic>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StdlibTypeSpec {
    Integer,
    Float,
    Boolean,
    String,
    Handle,
    Dynamic,
    Nothing,
    List(Box<StdlibTypeSpec>),
    Dict(Box<StdlibTypeSpec>, Box<StdlibTypeSpec>),
    HashSet(Box<StdlibTypeSpec>),
    Tuple(Vec<StdlibTypeSpec>),
    Shared(Box<StdlibTypeSpec>),
    WeakShared(Box<StdlibTypeSpec>),
    Pending(Box<StdlibTypeSpec>),
    Function,
}

impl StdlibTypeSpec {
    fn is_nothing(&self) -> bool {
        matches!(self, Self::Nothing)
    }

    fn is_dynamic(&self) -> bool {
        matches!(self, Self::Dynamic)
    }

    pub fn list_item_type(&self) -> Option<StdlibTypeSpec> {
        match self {
            Self::List(inner) => Some((**inner).clone()),
            _ => None,
        }
    }

    pub fn hashset_item_type(&self) -> Option<StdlibTypeSpec> {
        match self {
            Self::HashSet(inner) => Some((**inner).clone()),
            _ => None,
        }
    }

    pub fn pending_inner_type(&self) -> Option<StdlibTypeSpec> {
        match self {
            Self::Pending(inner) => Some((**inner).clone()),
            _ => None,
        }
    }

    pub fn merge(&self, other: &StdlibTypeSpec) -> StdlibTypeSpec {
        if self == other {
            return self.clone();
        }
        if self.is_dynamic() || other.is_dynamic() {
            return Self::Dynamic;
        }
        if self.is_nothing() {
            return other.clone();
        }
        if other.is_nothing() {
            return self.clone();
        }

        match (self, other) {
            (Self::Integer, Self::Float) | (Self::Float, Self::Integer) => Self::Float,
            (Self::List(lhs), Self::List(rhs)) => Self::List(Box::new(lhs.merge(rhs))),
            (Self::Dict(lhs_k, lhs_v), Self::Dict(rhs_k, rhs_v)) => {
                Self::Dict(Box::new(lhs_k.merge(rhs_k)), Box::new(lhs_v.merge(rhs_v)))
            }
            (Self::HashSet(lhs), Self::HashSet(rhs)) => Self::HashSet(Box::new(lhs.merge(rhs))),
            (Self::Tuple(lhs), Self::Tuple(rhs)) if lhs.len() == rhs.len() => Self::Tuple(
                lhs.iter()
                    .zip(rhs.iter())
                    .map(|(lhs, rhs)| lhs.merge(rhs))
                    .collect(),
            ),
            (Self::Shared(lhs), Self::Shared(rhs)) => Self::Shared(Box::new(lhs.merge(rhs))),
            (Self::WeakShared(lhs), Self::WeakShared(rhs)) => {
                Self::WeakShared(Box::new(lhs.merge(rhs)))
            }
            (Self::Pending(lhs), Self::Pending(rhs)) => Self::Pending(Box::new(lhs.merge(rhs))),
            _ => Self::Dynamic,
        }
    }
}

impl fmt::Display for StdlibTypeSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            StdlibTypeSpec::Integer => write!(f, "integer"),
            StdlibTypeSpec::Float => write!(f, "float"),
            StdlibTypeSpec::Boolean => write!(f, "boolean"),
            StdlibTypeSpec::String => write!(f, "string"),
            StdlibTypeSpec::Handle => write!(f, "handle"),
            StdlibTypeSpec::Dynamic => write!(f, "dynamic"),
            StdlibTypeSpec::Nothing => write!(f, "nothing"),
            StdlibTypeSpec::List(inner) => write!(f, "list oftype {inner}"),
            StdlibTypeSpec::Dict(key, value) => write!(f, "dict oftype ({key}, {value})"),
            StdlibTypeSpec::HashSet(inner) => write!(f, "hashset oftype {inner}"),
            StdlibTypeSpec::Tuple(elements) => {
                write!(f, "(")?;
                for (index, element) in elements.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{element}")?;
                }
                write!(f, ")")
            }
            StdlibTypeSpec::Shared(inner) => write!(f, "Shared oftype {inner}"),
            StdlibTypeSpec::WeakShared(inner) => write!(f, "WeakShared oftype {inner}"),
            StdlibTypeSpec::Pending(inner) => write!(f, "Pending oftype {inner}"),
            StdlibTypeSpec::Function => write!(f, "action"),
        }
    }
}

fn split_tuple_types(ty: &str) -> Option<Vec<&str>> {
    let mut depth = 0usize;
    let mut start = 0usize;
    let mut parts = Vec::new();

    for (idx, ch) in ty.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                parts.push(ty[start..idx].trim());
                start = idx + ch.len_utf8();
            }
            _ => {}
        }
    }

    let tail = ty[start..].trim();
    if tail.is_empty() {
        return None;
    }
    parts.push(tail);
    Some(parts)
}

fn split_oftype_pair(ty: &str) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    for (idx, ch) in ty.char_indices() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            ' ' if depth == 0 && ty[idx..].starts_with(" oftype ") => {
                let left = ty[..idx].trim();
                let right = ty[idx + " oftype ".len()..].trim();
                if left.is_empty() || right.is_empty() {
                    return None;
                }
                return Some((left, right));
            }
            _ => {}
        }
    }
    None
}

pub fn parse_stdlib_type_spec(ty: &str) -> Option<StdlibTypeSpec> {
    let ty = ty.trim();
    match ty {
        "integer" => Some(StdlibTypeSpec::Integer),
        "float" => Some(StdlibTypeSpec::Float),
        "boolean" => Some(StdlibTypeSpec::Boolean),
        "string" => Some(StdlibTypeSpec::String),
        "handle" => Some(StdlibTypeSpec::Handle),
        "dynamic" => Some(StdlibTypeSpec::Dynamic),
        "nothing" => Some(StdlibTypeSpec::Nothing),
        "tuple" => Some(StdlibTypeSpec::Tuple(vec![])),
        "action" => Some(StdlibTypeSpec::Function),
        "Pending" => Some(StdlibTypeSpec::Pending(Box::new(StdlibTypeSpec::Dynamic))),
        _ => {
            if ty.starts_with('(') && ty.ends_with(')') {
                let inner = &ty[1..ty.len() - 1];
                let elems = split_tuple_types(inner)?
                    .into_iter()
                    .map(parse_stdlib_type_spec)
                    .collect::<Option<Vec<_>>>()?;
                return Some(StdlibTypeSpec::Tuple(elems));
            }
            if let Some(inner) = ty.strip_prefix("list oftype ") {
                return parse_stdlib_type_spec(inner)
                    .map(|inner| StdlibTypeSpec::List(Box::new(inner)));
            }
            if let Some(rest) = ty.strip_prefix("dict oftype ") {
                if rest.starts_with('(') && rest.ends_with(')') {
                    let inner = &rest[1..rest.len() - 1];
                    let parts = split_tuple_types(inner)?;
                    if parts.len() != 2 {
                        return None;
                    }
                    return Some(StdlibTypeSpec::Dict(
                        Box::new(parse_stdlib_type_spec(parts[0])?),
                        Box::new(parse_stdlib_type_spec(parts[1])?),
                    ));
                }
                if let Some((key_ty, value_ty)) = split_oftype_pair(rest) {
                    return Some(StdlibTypeSpec::Dict(
                        Box::new(parse_stdlib_type_spec(key_ty)?),
                        Box::new(parse_stdlib_type_spec(value_ty)?),
                    ));
                }
            }
            if let Some(inner) = ty.strip_prefix("hashset oftype ") {
                return parse_stdlib_type_spec(inner)
                    .map(|inner| StdlibTypeSpec::HashSet(Box::new(inner)));
            }
            if let Some(inner) = ty.strip_prefix("Shared oftype ") {
                return parse_stdlib_type_spec(inner)
                    .map(|inner| StdlibTypeSpec::Shared(Box::new(inner)));
            }
            if let Some(inner) = ty.strip_prefix("WeakShared oftype ") {
                return parse_stdlib_type_spec(inner)
                    .map(|inner| StdlibTypeSpec::WeakShared(Box::new(inner)));
            }
            if let Some(inner) = ty.strip_prefix("Pending oftype ") {
                return parse_stdlib_type_spec(inner)
                    .map(|inner| StdlibTypeSpec::Pending(Box::new(inner)));
            }
            None
        }
    }
}

fn merge_list_argument_types(args: &[StdlibTypeSpec]) -> StdlibTypeSpec {
    args.iter()
        .filter_map(StdlibTypeSpec::list_item_type)
        .reduce(|acc, ty| acc.merge(&ty))
        .unwrap_or(StdlibTypeSpec::Dynamic)
}

pub fn infer_precise_stdlib_return_type(
    module: &str,
    name: &str,
    args: &[StdlibTypeSpec],
) -> Option<StdlibTypeSpec> {
    match (module, name) {
        ("async", "sleep") => Some(StdlibTypeSpec::Pending(Box::new(StdlibTypeSpec::Nothing))),
        ("async", "ready") => Some(StdlibTypeSpec::Pending(Box::new(
            args.first().cloned().unwrap_or(StdlibTypeSpec::Dynamic),
        ))),
        ("async", "gather") => {
            let item_ty = args
                .first()
                .and_then(StdlibTypeSpec::list_item_type)
                .and_then(|ty| ty.pending_inner_type())
                .unwrap_or(StdlibTypeSpec::Dynamic);
            Some(StdlibTypeSpec::Pending(Box::new(StdlibTypeSpec::List(
                Box::new(item_ty),
            ))))
        }
        ("async", "waitAny") => {
            let item_ty = args
                .first()
                .and_then(StdlibTypeSpec::list_item_type)
                .and_then(|ty| ty.pending_inner_type())
                .unwrap_or(StdlibTypeSpec::Dynamic);
            Some(StdlibTypeSpec::Pending(Box::new(StdlibTypeSpec::Tuple(
                vec![StdlibTypeSpec::Integer, item_ty],
            ))))
        }
        ("async", "timeout") => {
            let item_ty = args
                .first()
                .and_then(StdlibTypeSpec::pending_inner_type)
                .unwrap_or(StdlibTypeSpec::Dynamic);
            Some(StdlibTypeSpec::Pending(Box::new(StdlibTypeSpec::Tuple(
                vec![StdlibTypeSpec::Boolean, item_ty],
            ))))
        }
        ("collections", "range") => Some(StdlibTypeSpec::List(Box::new(StdlibTypeSpec::Integer))),
        ("collections", "hashset") => {
            let elem_ty = args
                .first()
                .and_then(StdlibTypeSpec::list_item_type)
                .or_else(|| args.first().and_then(StdlibTypeSpec::hashset_item_type))
                .unwrap_or(StdlibTypeSpec::Dynamic);
            Some(StdlibTypeSpec::HashSet(Box::new(elem_ty)))
        }
        ("collections", "setUnion") | ("collections", "setIntersect") => {
            let left_ty = args
                .first()
                .and_then(StdlibTypeSpec::hashset_item_type)
                .unwrap_or(StdlibTypeSpec::Dynamic);
            let right_ty = args
                .get(1)
                .and_then(StdlibTypeSpec::hashset_item_type)
                .unwrap_or(StdlibTypeSpec::Dynamic);
            Some(StdlibTypeSpec::HashSet(Box::new(left_ty.merge(&right_ty))))
        }
        ("collections", "setDiff") => Some(StdlibTypeSpec::HashSet(Box::new(
            args.first()
                .and_then(StdlibTypeSpec::hashset_item_type)
                .unwrap_or(StdlibTypeSpec::Dynamic),
        ))),
        ("collections", "setToList") => Some(StdlibTypeSpec::List(Box::new(
            args.first()
                .and_then(StdlibTypeSpec::hashset_item_type)
                .unwrap_or(StdlibTypeSpec::Dynamic),
        ))),
        ("collections", "Queue") | ("collections", "Stack") => {
            let elem_ty = args
                .first()
                .and_then(StdlibTypeSpec::list_item_type)
                .unwrap_or(StdlibTypeSpec::Dynamic);
            Some(StdlibTypeSpec::List(Box::new(elem_ty)))
        }
        ("collections", "flatten") => match args.first() {
            Some(StdlibTypeSpec::List(inner)) => match inner.as_ref() {
                StdlibTypeSpec::List(elem) => {
                    Some(StdlibTypeSpec::List(Box::new((**elem).clone())))
                }
                other => Some(StdlibTypeSpec::List(Box::new(other.clone()))),
            },
            _ => Some(StdlibTypeSpec::List(Box::new(StdlibTypeSpec::Dynamic))),
        },
        ("collections", "zip") => {
            let left_ty = args
                .first()
                .and_then(StdlibTypeSpec::list_item_type)
                .unwrap_or(StdlibTypeSpec::Dynamic);
            let right_ty = args
                .get(1)
                .and_then(StdlibTypeSpec::list_item_type)
                .unwrap_or(StdlibTypeSpec::Dynamic);
            Some(StdlibTypeSpec::List(Box::new(StdlibTypeSpec::Tuple(vec![
                left_ty, right_ty,
            ]))))
        }
        ("collections", "enumerate") => {
            let elem_ty = args
                .first()
                .and_then(StdlibTypeSpec::list_item_type)
                .unwrap_or(StdlibTypeSpec::Dynamic);
            Some(StdlibTypeSpec::List(Box::new(StdlibTypeSpec::Tuple(vec![
                StdlibTypeSpec::Integer,
                elem_ty,
            ]))))
        }
        ("collections", "chunk") | ("collections", "window") => {
            let elem_ty = args
                .first()
                .and_then(StdlibTypeSpec::list_item_type)
                .unwrap_or(StdlibTypeSpec::Dynamic);
            Some(StdlibTypeSpec::List(Box::new(StdlibTypeSpec::List(
                Box::new(elem_ty),
            ))))
        }
        ("collections", "partition") => {
            let elem_ty = args
                .first()
                .and_then(StdlibTypeSpec::list_item_type)
                .unwrap_or(StdlibTypeSpec::Dynamic);
            Some(StdlibTypeSpec::Tuple(vec![
                StdlibTypeSpec::List(Box::new(elem_ty.clone())),
                StdlibTypeSpec::List(Box::new(elem_ty)),
            ]))
        }
        ("collections", "groupBy") => {
            let elem_ty = args
                .first()
                .and_then(StdlibTypeSpec::list_item_type)
                .unwrap_or(StdlibTypeSpec::Dynamic);
            Some(StdlibTypeSpec::Dict(
                Box::new(elem_ty.clone()),
                Box::new(StdlibTypeSpec::List(Box::new(elem_ty))),
            ))
        }
        ("collections", "unique")
        | ("collections", "reverse")
        | ("collections", "sort")
        | ("collections", "slice") => {
            let elem_ty = args
                .first()
                .and_then(StdlibTypeSpec::list_item_type)
                .unwrap_or(StdlibTypeSpec::Dynamic);
            Some(StdlibTypeSpec::List(Box::new(elem_ty)))
        }
        ("collections", "concat") => {
            let elem_ty = merge_list_argument_types(args);
            Some(StdlibTypeSpec::List(Box::new(elem_ty)))
        }
        ("collections", "dequeue")
        | ("collections", "peek")
        | ("collections", "pop")
        | ("collections", "top")
        | ("collections", "first")
        | ("collections", "last") => Some(
            args.first()
                .and_then(StdlibTypeSpec::list_item_type)
                .unwrap_or(StdlibTypeSpec::Dynamic),
        ),
        ("collections", "sum") | ("collections", "product") => Some(
            args.first()
                .and_then(StdlibTypeSpec::list_item_type)
                .and_then(|ty| match ty {
                    StdlibTypeSpec::Integer => Some(StdlibTypeSpec::Integer),
                    StdlibTypeSpec::Float => Some(StdlibTypeSpec::Float),
                    StdlibTypeSpec::Dynamic => Some(StdlibTypeSpec::Dynamic),
                    _ => None,
                })
                .unwrap_or(StdlibTypeSpec::Dynamic),
        ),
        ("collections", "min") | ("collections", "max") => Some(
            args.first()
                .and_then(StdlibTypeSpec::list_item_type)
                .and_then(|ty| match ty {
                    StdlibTypeSpec::Integer
                    | StdlibTypeSpec::Float
                    | StdlibTypeSpec::String
                    | StdlibTypeSpec::Dynamic => Some(ty),
                    _ => None,
                })
                .unwrap_or(StdlibTypeSpec::Dynamic),
        ),
        ("parallel", "parallelFilter") => Some(StdlibTypeSpec::List(Box::new(
            args.first()
                .and_then(StdlibTypeSpec::list_item_type)
                .unwrap_or(StdlibTypeSpec::Dynamic),
        ))),
        ("parallel", "parallelReduce") => {
            Some(args.get(1).cloned().unwrap_or(StdlibTypeSpec::Dynamic))
        }
        ("math", "abs") | ("math", "sign") => Some(
            args.first()
                .and_then(|ty| match ty {
                    StdlibTypeSpec::Integer => Some(StdlibTypeSpec::Integer),
                    StdlibTypeSpec::Float => Some(StdlibTypeSpec::Float),
                    StdlibTypeSpec::Dynamic => Some(StdlibTypeSpec::Dynamic),
                    _ => None,
                })
                .unwrap_or(StdlibTypeSpec::Dynamic),
        ),
        ("math", "min") | ("math", "max") => Some(match (args.first(), args.get(1)) {
            (Some(StdlibTypeSpec::Integer), Some(StdlibTypeSpec::Integer)) => {
                StdlibTypeSpec::Integer
            }
            (Some(StdlibTypeSpec::Float), Some(StdlibTypeSpec::Float))
            | (Some(StdlibTypeSpec::Integer), Some(StdlibTypeSpec::Float))
            | (Some(StdlibTypeSpec::Float), Some(StdlibTypeSpec::Integer)) => StdlibTypeSpec::Float,
            (Some(StdlibTypeSpec::Dynamic), _) | (_, Some(StdlibTypeSpec::Dynamic)) => {
                StdlibTypeSpec::Dynamic
            }
            _ => StdlibTypeSpec::Dynamic,
        }),
        ("math", "clamp") => Some(match (args.first(), args.get(1), args.get(2)) {
            (
                Some(StdlibTypeSpec::Integer),
                Some(StdlibTypeSpec::Integer),
                Some(StdlibTypeSpec::Integer),
            ) => StdlibTypeSpec::Integer,
            (Some(StdlibTypeSpec::Dynamic), _, _)
            | (_, Some(StdlibTypeSpec::Dynamic), _)
            | (_, _, Some(StdlibTypeSpec::Dynamic)) => StdlibTypeSpec::Dynamic,
            _ => StdlibTypeSpec::Float,
        }),
        _ => crate::member_return_type(module, name).and_then(parse_stdlib_type_spec),
    }
}

pub fn infer_stdlib_method(
    module: &str,
    name: &str,
    arg_kinds: &[StdlibValueKind],
) -> Option<StdlibMethodInfo> {
    match module {
        "math" => crate::math::method_info(name, arg_kinds),
        _ => None,
    }
}

pub fn infer_receiver_method(
    receiver_kind: StdlibValueKind,
    name: &str,
    _arg_kinds: &[StdlibValueKind],
) -> Option<StdlibMethodInfo> {
    let receiver_kind = stdlib_kind_to_receiver_kind(receiver_kind)?;
    let info = infer_receiver_member(receiver_kind, name)?;
    let return_kind = info.method_return.or(info.field_return)?;
    Some(StdlibMethodInfo {
        return_kind: receiver_return_to_stdlib_kind(receiver_kind, return_kind),
        intrinsic: None,
    })
}

fn stdlib_kind_to_receiver_kind(kind: StdlibValueKind) -> Option<ReceiverBuiltinKind> {
    match kind {
        StdlibValueKind::Integer => Some(ReceiverBuiltinKind::Integer),
        StdlibValueKind::Float => Some(ReceiverBuiltinKind::Float),
        StdlibValueKind::Boolean => Some(ReceiverBuiltinKind::Boolean),
        StdlibValueKind::String => Some(ReceiverBuiltinKind::String),
        StdlibValueKind::List => Some(ReceiverBuiltinKind::List),
        StdlibValueKind::Dict => Some(ReceiverBuiltinKind::Dict),
        StdlibValueKind::HashSet => Some(ReceiverBuiltinKind::HashSet),
        StdlibValueKind::Dynamic => Some(ReceiverBuiltinKind::Dynamic),
        StdlibValueKind::Nothing => Some(ReceiverBuiltinKind::Nothing),
    }
}

fn receiver_return_to_stdlib_kind(
    receiver_kind: ReceiverBuiltinKind,
    return_kind: ReceiverReturnKind,
) -> StdlibValueKind {
    match return_kind {
        ReceiverReturnKind::Integer => StdlibValueKind::Integer,
        ReceiverReturnKind::Float => StdlibValueKind::Float,
        ReceiverReturnKind::Boolean => StdlibValueKind::Boolean,
        ReceiverReturnKind::String => StdlibValueKind::String,
        ReceiverReturnKind::Dynamic
        | ReceiverReturnKind::ReceiverElement
        | ReceiverReturnKind::DictValue
        | ReceiverReturnKind::SharedInnerValue
        | ReceiverReturnKind::SharedOfInner
        | ReceiverReturnKind::WeakSharedOfInner => StdlibValueKind::Dynamic,
        ReceiverReturnKind::Nothing => StdlibValueKind::Nothing,
        ReceiverReturnKind::ReceiverSelf => match receiver_kind {
            ReceiverBuiltinKind::Integer => StdlibValueKind::Integer,
            ReceiverBuiltinKind::Float => StdlibValueKind::Float,
            ReceiverBuiltinKind::Boolean => StdlibValueKind::Boolean,
            ReceiverBuiltinKind::String => StdlibValueKind::String,
            ReceiverBuiltinKind::List => StdlibValueKind::List,
            ReceiverBuiltinKind::Dict => StdlibValueKind::Dict,
            ReceiverBuiltinKind::HashSet => StdlibValueKind::HashSet,
            _ => StdlibValueKind::Dynamic,
        },
        ReceiverReturnKind::ListOfString
        | ReceiverReturnKind::ListOfInteger
        | ReceiverReturnKind::ListOfDynamic
        | ReceiverReturnKind::ListOfReceiverElement
        | ReceiverReturnKind::ListOfDictValue
        | ReceiverReturnKind::ListOfDynamicPairs => StdlibValueKind::List,
    }
}

#[cfg(test)]
mod tests {
    use fidan_config::{ReceiverBuiltinKind, ReceiverReturnKind, infer_receiver_member};

    #[test]
    fn string_members_are_centralized_in_metadata() {
        let len =
            infer_receiver_member(ReceiverBuiltinKind::String, "len").expect("string len metadata");
        assert_eq!(len.field_return, Some(ReceiverReturnKind::Integer));
        assert_eq!(len.method_return, Some(ReceiverReturnKind::Integer));

        let filter = infer_receiver_member(ReceiverBuiltinKind::String, "filter");
        assert!(filter.is_none());
    }

    #[test]
    fn shared_members_are_centralized_in_metadata() {
        let downgrade = infer_receiver_member(ReceiverBuiltinKind::Shared, "downgrade")
            .expect("shared downgrade metadata");
        assert_eq!(
            downgrade.method_return,
            Some(ReceiverReturnKind::WeakSharedOfInner)
        );
    }
}
