#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StdlibValueKind {
    Integer,
    Float,
    Boolean,
    String,
    List,
    Dict,
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
            if let Some(rest) = ty.strip_prefix("dict oftype ")
                && let Some((key_ty, value_ty)) = split_oftype_pair(rest)
            {
                return Some(StdlibTypeSpec::Dict(
                    Box::new(parse_stdlib_type_spec(key_ty)?),
                    Box::new(parse_stdlib_type_spec(value_ty)?),
                ));
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
        ("collections", "Set")
        | ("collections", "setUnion")
        | ("collections", "setIntersect")
        | ("collections", "setDiff") => Some(StdlibTypeSpec::Dict(
            Box::new(StdlibTypeSpec::String),
            Box::new(StdlibTypeSpec::Boolean),
        )),
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
                Box::new(StdlibTypeSpec::String),
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
    use StdlibValueKind as Kind;

    let info = |return_kind| StdlibMethodInfo {
        return_kind,
        intrinsic: None,
    };

    match receiver_kind {
        Kind::String => match name {
            "upper" | "to_upper" | "lower" | "to_lower" | "trim" | "trim_start" | "ltrim"
            | "trim_end" | "rtrim" | "replace" | "substr" | "slice" | "char_at" | "reverse"
            | "reversed" => Some(info(Kind::String)),
            "len" | "length" | "find" | "index_of" => Some(info(Kind::Integer)),
            "contains" | "starts_with" | "startsWith" | "ends_with" | "endsWith" => {
                Some(info(Kind::Boolean))
            }
            "split" => Some(info(Kind::List)),
            _ => None,
        },
        Kind::List => match name {
            "append" | "push" | "add" | "insert" | "reverse" | "sort" => Some(info(Kind::Nothing)),
            "len" | "length" | "find" | "index_of" => Some(info(Kind::Integer)),
            "contains" => Some(info(Kind::Boolean)),
            "join" => Some(info(Kind::String)),
            "reversed" => Some(info(Kind::List)),
            "get" | "pop" | "remove" => Some(info(Kind::Dynamic)),
            _ => None,
        },
        Kind::Dict => match name {
            "set" | "insert" => Some(info(Kind::Nothing)),
            "len" | "length" => Some(info(Kind::Integer)),
            "keys" | "values" => Some(info(Kind::List)),
            "contains" | "has_key" => Some(info(Kind::Boolean)),
            "get" => Some(info(Kind::Dynamic)),
            _ => None,
        },
        _ => None,
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
