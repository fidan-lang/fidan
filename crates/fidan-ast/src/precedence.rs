//! Shared parser/formatter binding powers. Higher values bind tighter.
use crate::BinOp;

pub const TERNARY: u8 = 0;
pub const NULL_COALESCE: u8 = 1;
pub const COMPARISON: u8 = 6;
pub const PREFIX: u8 = 14;
pub const POSTFIX: u8 = 15;

impl BinOp {
    /// All binary operators associate left, including the existing `**` contract.
    /// A larger right binding power prevents consuming another equal-tier operator.
    pub const fn binding_power(self) -> (u8, u8) {
        use BinOp::*;
        let left = match self {
            Range | RangeInclusive => 2,
            Or => 3,
            Xor => 4,
            And => 5,
            Eq | NotEq | Lt | LtEq | Gt | GtEq => COMPARISON,
            BitOr => 7,
            BitXor => 8,
            BitAnd => 9,
            Shl | Shr => 10,
            Add | Sub => 11,
            Mul | Div | Rem => 12,
            Pow => 13,
        };
        (left, left + 1)
    }
}
