// fidan-passes/src/constant_folding.rs
//
// Constant folding + strength reduction:
//   - `Binary(Const, Const)` and `Unary(Const)` → computed literal
//   - `x + 0`, `x * 1`, `x ** 0`, `x && true`, etc. → simpler rvalue

use fidan_ast::{BinOp, UnOp};
use fidan_mir::{Instr, LocalId, MirLit, MirTy, Operand, Rvalue};
use rustc_hash::{FxHashMap, FxHashSet};

pub struct ConstantFolding;

impl crate::Pass for ConstantFolding {
    fn run(&self, prog: &mut fidan_mir::MirProgram) {
        for func in &mut prog.functions {
            let dropped: FxHashSet<_> = func
                .blocks
                .iter()
                .flat_map(|bb| &bb.instructions)
                .filter_map(|instr| match instr {
                    Instr::Drop { local } => Some(*local),
                    _ => None,
                })
                .collect();
            // Parameter annotations (even certain) do not validate runtime types
            // for flexible calls. Only literal values and their SSA copies prove
            // types here; globals, phis and other definitions remain unknown.
            let mut known_types = FxHashMap::default();
            for bb in &mut func.blocks {
                for instr in &mut bb.instructions {
                    if let Instr::Assign { dest, ty, rhs } = instr {
                        if let Some(reduced) = try_reduce(rhs, ty, &known_types) {
                            *rhs = reduced;
                        }
                        let known_type = match rhs {
                            Rvalue::Literal(MirLit::Int(_)) => Some(MirTy::Integer),
                            Rvalue::Literal(MirLit::Bool(_)) => Some(MirTy::Boolean),
                            Rvalue::Use(operand) => operand_type(operand, &known_types),
                            _ => None,
                        };
                        if let Some(known_type) = known_type
                            && !dropped.contains(dest)
                            && (matches!(ty, MirTy::Dynamic | MirTy::Error) || *ty == known_type)
                        {
                            known_types.insert(*dest, known_type);
                        }
                    }
                }
            }
        }
    }
}

/// Returns a simplified `Rvalue` if any folding or strength reduction applies,
/// or `None` if the expression should be left unchanged.
fn try_reduce(
    rhs: &Rvalue,
    result_ty: &MirTy,
    known_types: &FxHashMap<LocalId, MirTy>,
) -> Option<Rvalue> {
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
        Rvalue::Binary { op, lhs, rhs } => strength_reduce(*op, lhs, rhs, result_ty, known_types),

        // ── Identity unary : +x → x ────────────────────────────────────────
        Rvalue::Unary {
            op: UnOp::Pos,
            operand,
        } => Some(Rvalue::Use(operand.clone())),

        _ => None,
    }
}

/// Strength-reduce one binary operand being a known constant.
fn strength_reduce(
    op: BinOp,
    lhs: &Operand,
    rhs: &Operand,
    result_ty: &MirTy,
    known_types: &FxHashMap<LocalId, MirTy>,
) -> Option<Rvalue> {
    use MirLit::*;
    // Helper: is operand a specific integer?
    let is_int = |op: &Operand, n: i64| matches!(op, Operand::Const(Int(v)) if *v == n);
    let is_bool = |op: &Operand, b: bool| matches!(op, Operand::Const(Bool(v)) if *v == b);

    // Only matching integer/boolean values permit these identities. Float
    // rewrites can change signed zero/NaN; mixed operations can change types.
    match (
        operand_type(lhs, known_types),
        operand_type(rhs, known_types),
    ) {
        (Some(MirTy::Integer), Some(MirTy::Integer))
            if matches!(result_ty, MirTy::Integer | MirTy::Dynamic | MirTy::Error) =>
        {
            match op {
                // x + 0  or  0 + x  →  x
                BinOp::Add if is_int(rhs, 0) => Some(Rvalue::Use(lhs.clone())),
                BinOp::Add if is_int(lhs, 0) => Some(Rvalue::Use(rhs.clone())),

                // x - 0  →  x
                BinOp::Sub if is_int(rhs, 0) => Some(Rvalue::Use(lhs.clone())),

                // x * 1  or  1 * x  →  x
                BinOp::Mul if is_int(rhs, 1) => Some(Rvalue::Use(lhs.clone())),
                BinOp::Mul if is_int(lhs, 1) => Some(Rvalue::Use(rhs.clone())),

                // x * 0  or  0 * x  →  0  (integers only; floats have -0.0/NaN edge cases)
                BinOp::Mul if is_int(rhs, 0) => Some(Rvalue::Literal(Int(0))),
                BinOp::Mul if is_int(lhs, 0) => Some(Rvalue::Literal(Int(0))),

                // x / 1  →  x
                BinOp::Div if is_int(rhs, 1) => Some(Rvalue::Use(lhs.clone())),

                // x ** 0  →  1  (integer base only)
                BinOp::Pow if is_int(rhs, 0) => Some(Rvalue::Literal(Int(1))),
                // x ** 1  →  x
                BinOp::Pow if is_int(rhs, 1) => Some(Rvalue::Use(lhs.clone())),
                _ => None,
            }
        }

        (Some(MirTy::Boolean), Some(MirTy::Boolean))
            if matches!(result_ty, MirTy::Boolean | MirTy::Dynamic | MirTy::Error) =>
        {
            match op {
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
                BinOp::Or if is_bool(rhs, true) || is_bool(lhs, true) => {
                    Some(Rvalue::Literal(Bool(true)))
                }

                _ => None,
            }
        }
        _ => None,
    }
}

fn operand_type(operand: &Operand, known_types: &FxHashMap<LocalId, MirTy>) -> Option<MirTy> {
    match operand {
        Operand::Local(local) => known_types.get(local).cloned(),
        Operand::Const(MirLit::Int(_)) => Some(MirTy::Integer),
        Operand::Const(MirLit::Bool(_)) => Some(MirTy::Boolean),
        _ => None,
    }
}

fn fold_binary(op: BinOp, l: &MirLit, r: &MirLit) -> Option<MirLit> {
    use MirLit::*;
    Some(match (op, l, r) {
        // Integer arithmetic
        (BinOp::Add, Int(a), Int(b)) => Int(a.checked_add(*b)?),
        (BinOp::Sub, Int(a), Int(b)) => Int(a.checked_sub(*b)?),
        (BinOp::Mul, Int(a), Int(b)) => Int(a.checked_mul(*b)?),
        (BinOp::Div, Int(a), Int(b)) if *b != 0 => Int(a.checked_div(*b)?),
        (BinOp::Rem, Int(a), Int(b)) if *b != 0 => Int(a.checked_rem(*b)?),
        (BinOp::Pow, Int(a), Int(b)) => match fidan_runtime::integer::power(*a, *b).ok()? {
            fidan_runtime::integer::Power::Integer(value) => Int(value),
            fidan_runtime::integer::Power::Float(value) => Float(value),
        },
        // Float arithmetic
        (BinOp::Add, Float(a), Float(b)) => Float(a + b),
        (BinOp::Sub, Float(a), Float(b)) => Float(a - b),
        (BinOp::Mul, Float(a), Float(b)) => Float(a * b),
        (BinOp::Div, Float(a), Float(b)) if *b != 0.0 => Float(a / b),
        (BinOp::Rem, Float(a), Float(b)) if *b != 0.0 => Float(a % b),
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
        (UnOp::Neg, Int(a)) => Int(a.checked_neg()?),
        (UnOp::Neg, Float(a)) => Float(-a),
        (UnOp::Not, Bool(a)) => Bool(!a),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn constant_folding_preserves_runtime_arithmetic_errors() {
        for (op, a, b) in [
            (BinOp::Add, i64::MAX, 1),
            (BinOp::Sub, i64::MIN, 1),
            (BinOp::Mul, i64::MAX, 2),
            (BinOp::Div, i64::MIN, -1),
            (BinOp::Rem, i64::MIN, -1),
            (BinOp::Div, 1, 0),
            (BinOp::Rem, 1, 0),
            (BinOp::Pow, 2, 63),
            (BinOp::Pow, 0, -1),
            (BinOp::Pow, 2, 4_294_967_296),
        ] {
            assert!(fold_binary(op, &MirLit::Int(a), &MirLit::Int(b)).is_none());
        }
        assert!(fold_unary(UnOp::Neg, &MirLit::Int(i64::MIN)).is_none());
        assert!(matches!(
            fold_binary(BinOp::Pow, &MirLit::Int(-2), &MirLit::Int(63)),
            Some(MirLit::Int(i64::MIN))
        ));
    }
    #[test]
    fn constant_shifts_mask_signed_counts_to_six_bits() {
        for (a, b, left, right) in [
            (1, 0, 1, 1),
            (1, 63, i64::MIN, 0),
            (1, 64, 1, 1),
            (1, 65, 2, 0),
            (8, 65, 16, 4),
            (1, -1, i64::MIN, 0),
            (-8, 65, -16, -4),
        ] {
            assert!(
                matches!(fold_binary(BinOp::Shl, &MirLit::Int(a), &MirLit::Int(b)), Some(MirLit::Int(value)) if value == left)
            );
            assert!(
                matches!(fold_binary(BinOp::Shr, &MirLit::Int(a), &MirLit::Int(b)), Some(MirLit::Int(value)) if value == right)
            );
        }
    }
}
