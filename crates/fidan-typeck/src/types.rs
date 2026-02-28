#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FidanType {
    Integer, Float, Boolean, String, Nothing, Dynamic,
    List(Box<FidanType>),
    Dict(Box<FidanType>, Box<FidanType>),
    Object(fidan_lexer::Symbol),
    Shared(Box<FidanType>),
    Pending(Box<FidanType>),
    Function,
    Unknown,
}
