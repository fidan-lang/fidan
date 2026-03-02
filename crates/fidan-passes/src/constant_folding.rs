// fidan-passes/src/constant_folding.rs
//
// Constant folding: replace `Binary(Const(a), Const(b))` and `Unary(Const(a))`
// rvalues with their statically-computed result.

use fidan_ast::{BinOp, UnOp};
use fidan_mir::{Instr, MirLit, Operand, Rvalue};

pub struct ConstantFolding;

impl crate::Pass for ConstantFolding {
    fn run(&self, prog: &mut fidan_mir::MirProgram) {
        for func in &mut prog.functions {
            for bb in &mut func.blocks {
                for instr in &mut bb.instructions {
                    if let Instr::Assign { rhs, .. } = instr {
                        if let Some(folded) = try_fold(rhs) {
                            *rhs = Rvalue::Literal(folded);
                        }
                    }
                }
            }
        }
    }
}

fn try_fold(rhs: &Rvalue) -> Option<MirLit> {
    match rhs {
        Rvalue::Binary {
            op,
            lhs: Operand::Const(a),
            rhs: Operand::Const(b),
        } => fold_binary(*op, a, b),
        Rvalue::Unary {
            op,
            operand: Operand::Const(a),
        } => fold_unary(*op, a),
        _ => None,
    }
}

fn fold_binary(op: BinOp, l: &MirLit, r: &MirLit) -> Option<MirLit> {
    use MirLit::*;
    Some(match (op, l, r) {
        // Integer arithmetic
        (BinOp::Add, Int(a), Int(b)) => Int(a.wrapping_add(*b)),
        (BinOp::Sub, Int(a), Int(b)) => Int(a.wrapping_sub(*b)),
        (BinOp::Mul, Int(a), Int(b)) => Int(a.wrapping_mul(*b)),
        (BinOp::Div, Int(a), Int(b)) if *b != 0 => Int(a.wrapping_div(*b)),
        (BinOp::Rem, Int(a), Int(b)) if *b != 0 => Int(a.wrapping_rem(*b)),
        (BinOp::Pow, Int(a), Int(b)) if *b >= 0 => Int(a.wrapping_pow(*b as u32)),
        // Float arithmetic
        (BinOp::Add, Float(a), Float(b)) => Float(a + b),
        (BinOp::Sub, Float(a), Float(b)) => Float(a - b),
        (BinOp::Mul, Float(a), Float(b)) => Float(a * b),
        (BinOp::Div, Float(a), Float(b)) => Float(a / b),
        (BinOp::Rem, Float(a), Float(b)) => Float(a % b),
        // Integer comparisons
        (BinOp::Eq, Int(a), Int(b)) => Bool(a == b),
        (BinOp::NotEq, Int(a), Int(b)) => Bool(a != b),
        (BinOp::Lt, Int(a), Int(b)) => Bool(a < b),
        (BinOp::LtEq, Int(a), Int(b)) => Bool(a <= b),
        (BinOp::Gt, Int(a), Int(b)) => Bool(a > b),
        (BinOp::GtEq, Int(a), Int(b)) => Bool(a >= b),
        // Float comparisons
        (BinOp::Eq, Float(a), Float(b)) => Bool(a == b),
        (BinOp::NotEq, Float(a), Float(b)) => Bool(a != b),
        (BinOp::Lt, Float(a), Float(b)) => Bool(a < b),
        (BinOp::LtEq, Float(a), Float(b)) => Bool(a <= b),
        (BinOp::Gt, Float(a), Float(b)) => Bool(a > b),
        (BinOp::GtEq, Float(a), Float(b)) => Bool(a >= b),
        // Bool logic
        (BinOp::And, Bool(a), Bool(b)) => Bool(*a && *b),
        (BinOp::Or, Bool(a), Bool(b)) => Bool(*a || *b),
        (BinOp::Eq, Bool(a), Bool(b)) => Bool(a == b),
        (BinOp::NotEq, Bool(a), Bool(b)) => Bool(a != b),
        // String concatenation
        (BinOp::Add, Str(a), Str(b)) => Str(format!("{}{}", a, b)),
        (BinOp::Eq, Str(a), Str(b)) => Bool(a == b),
        (BinOp::NotEq, Str(a), Str(b)) => Bool(a != b),
        // Bitwise
        (BinOp::BitAnd, Int(a), Int(b)) => Int(a & b),
        (BinOp::BitOr, Int(a), Int(b)) => Int(a | b),
        (BinOp::BitXor, Int(a), Int(b)) => Int(a ^ b),
        (BinOp::Shl, Int(a), Int(b)) => Int(a << (b & 63)),
        (BinOp::Shr, Int(a), Int(b)) => Int(a >> (b & 63)),
        _ => return None,
    })
}

fn fold_unary(op: UnOp, val: &MirLit) -> Option<MirLit> {
    use MirLit::*;
    Some(match (op, val) {
        (UnOp::Pos, Int(a)) => Int(*a),
        (UnOp::Pos, Float(a)) => Float(*a),
        (UnOp::Neg, Int(a)) => Int(-a),
        (UnOp::Neg, Float(a)) => Float(-a),
        (UnOp::Not, Bool(a)) => Bool(!a),
        _ => return None,
    })
}
