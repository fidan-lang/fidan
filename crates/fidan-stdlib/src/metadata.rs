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
