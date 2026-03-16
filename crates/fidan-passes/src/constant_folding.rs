// fidan-passes/src/constant_folding.rs
//
// Constant folding + strength reduction:
//   - `Binary(Const, Const)` and `Unary(Const)` → computed literal
//   - `x + 0`, `x * 1`, `x ** 0`, `x && true`, etc. → simpler rvalue

use fidan_ast::{BinOp, UnOp};
use fidan_mir::{Instr, MirLit, Operand, Rvalue};

pub struct ConstantFolding;

impl crate::Pass for ConstantFolding {
    fn run(&self, prog: &mut fidan_mir::MirProgram) {
        for func in &mut prog.functions {
            for bb in &mut func.blocks {
                for instr in &mut bb.instructions {
                    if let Instr::Assign { rhs, .. } = instr
                        && let Some(reduced) = try_reduce(rhs)
                    {
                        *rhs = reduced;
                    }
                }
            }
        }
    }
}

/// Returns a simplified `Rvalue` if any folding or strength reduction applies,
/// or `None` if the expression should be left unchanged.
fn try_reduce(rhs: &Rvalue) -> Option<Rvalue> {
    match rhs {
        // ── Full constant fold ─────────────────────────────────────────────
        Rvalue::Binary {
            op,
            lhs: Operand::Const(a),
            rhs: Operand::Const(b),
        } => fold_binary(*op, a, b).map(Rvalue::Literal),

        Rvalue::Unary {
            op,
            operand: Operand::Const(a),
        } => fold_unary(*op, a).map(Rvalue::Literal),

        // ── Strength reduction: Binary with one constant operand ───────────
        Rvalue::Binary { op, lhs, rhs } => strength_reduce(*op, lhs, rhs),

        // ── Identity unary : +x → x ────────────────────────────────────────
        Rvalue::Unary {
            op: UnOp::Pos,
            operand,
        } => Some(Rvalue::Use(operand.clone())),

        _ => None,
    }
}

/// Strength-reduce one binary operand being a known constant.
fn strength_reduce(op: BinOp, lhs: &Operand, rhs: &Operand) -> Option<Rvalue> {
    use MirLit::*;
    // Helper: is operand a specific integer?
    let is_int = |op: &Operand, n: i64| matches!(op, Operand::Const(Int(v)) if *v == n);
    let is_float = |op: &Operand, v: f64| matches!(op, Operand::Const(Float(f)) if *f == v);
    let is_bool = |op: &Operand, b: bool| matches!(op, Operand::Const(Bool(v)) if *v == b);

    match op {
        // x + 0  or  0 + x  →  x
        BinOp::Add if is_int(rhs, 0) || is_float(rhs, 0.0) => Some(Rvalue::Use(lhs.clone())),
        BinOp::Add if is_int(lhs, 0) || is_float(lhs, 0.0) => Some(Rvalue::Use(rhs.clone())),

        // x - 0  →  x
        BinOp::Sub if is_int(rhs, 0) || is_float(rhs, 0.0) => Some(Rvalue::Use(lhs.clone())),

        // x * 1  or  1 * x  →  x
        BinOp::Mul if is_int(rhs, 1) || is_float(rhs, 1.0) => Some(Rvalue::Use(lhs.clone())),
        BinOp::Mul if is_int(lhs, 1) || is_float(lhs, 1.0) => Some(Rvalue::Use(rhs.clone())),

        // x * 0  or  0 * x  →  0  (integers only; floats have -0.0/NaN edge cases)
        BinOp::Mul if is_int(rhs, 0) => Some(Rvalue::Literal(Int(0))),
        BinOp::Mul if is_int(lhs, 0) => Some(Rvalue::Literal(Int(0))),

        // x / 1  →  x
        BinOp::Div if is_int(rhs, 1) || is_float(rhs, 1.0) => Some(Rvalue::Use(lhs.clone())),

        // x ** 0  →  1  (integer base only)
        BinOp::Pow if is_int(rhs, 0) => Some(Rvalue::Literal(Int(1))),
        // x ** 1  →  x
        BinOp::Pow if is_int(rhs, 1) => Some(Rvalue::Use(lhs.clone())),

        // x && true  or  true && x  →  x
        BinOp::And if is_bool(rhs, true) => Some(Rvalue::Use(lhs.clone())),
        BinOp::And if is_bool(lhs, true) => Some(Rvalue::Use(rhs.clone())),
        // x && false  or  false && x  →  false
        BinOp::And if is_bool(rhs, false) || is_bool(lhs, false) => {
            Some(Rvalue::Literal(Bool(false)))
        }

        // x || false  or  false || x  →  x
        BinOp::Or if is_bool(rhs, false) => Some(Rvalue::Use(lhs.clone())),
        BinOp::Or if is_bool(lhs, false) => Some(Rvalue::Use(rhs.clone())),
        // x || true  or  true || x  →  true
        BinOp::Or if is_bool(rhs, true) || is_bool(lhs, true) => Some(Rvalue::Literal(Bool(true))),

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
