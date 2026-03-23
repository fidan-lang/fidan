//! Escape analysis — identifies MIR locals that do NOT escape their function.
//!
//! A local "escapes" when it can outlive the current activation record:
//!   - stored to a module-level global
//!   - stored into a heap object field, list element, dict entry, or tuple
//!   - captured by a closure (`MakeClosure`)
//!   - passed to a spawned thread (`SpawnExpr`, `SpawnParallel`, `SpawnConcurrent`,
//!     `SpawnDynamic`, `ParallelIter`)
//!   - returned from the function (`Terminator::Return(Some(_))`)
//!   - thrown as an exception (`Terminator::Throw`)
//!   - passed to a dynamic or virtual method call (conservative)
//!
//! Non-escaping locals are pure temporaries that are consumed within the same
//! activation frame.  The Cranelift AOT backend can use this information to:
//!   - elide redundant `fdn_clone` calls on non-escaping values
//!   - stack-allocate small non-heap objects (future optimisation)
//!
//! # Conservatism
//!
//! The analysis is flow-insensitive and fully conservative.  Direct `Callee::Fn`
//! and `Callee::Builtin` arguments are currently marked as escaping (safe but
//! imprecise).  A future inter-procedural pass could refine this.

use fidan_mir::{Callee, Instr, LocalId, MirFunction, MirProgram, Operand, Rvalue, Terminator};
use std::collections::HashSet;

fn mark_operand(escaping: &mut HashSet<u32>, op: &Operand) {
    if let Operand::Local(l) = op {
        escaping.insert(l.0);
    }
}

fn mark_all_operands(escaping: &mut HashSet<u32>, ops: &[Operand]) {
    for op in ops {
        mark_operand(escaping, op);
    }
}

// ── Public types ───────────────────────────────────────────────────────────────

/// Escape analysis result for a single function.
#[derive(Debug, Default, Clone)]
pub struct EscapeInfo {
    /// Locals that provably do NOT escape the current stack frame.
    ///
    /// These are candidates for clone-elision and, in a future optimisation
    /// phase, stack-allocation of small heap-allocated objects.
    pub non_escaping: HashSet<LocalId>,

    /// Total number of SSA locals in the function (includes parameters).
    pub total_locals: usize,
}

impl EscapeInfo {
    /// Number of locals that escape.
    pub fn num_escaping(&self) -> usize {
        self.total_locals - self.non_escaping.len()
    }

    /// Fraction of locals that escape, in `[0.0, 1.0]`.
    pub fn escape_ratio(&self) -> f64 {
        if self.total_locals == 0 {
            return 0.0;
        }
        self.num_escaping() as f64 / self.total_locals as f64
    }

    /// Returns `true` if `local` provably does not escape.
    pub fn is_non_escaping(&self, local: LocalId) -> bool {
        self.non_escaping.contains(&local)
    }
}

// ── Analysis ───────────────────────────────────────────────────────────────────

/// Analyse the escape behaviour of every local in `mf`.
pub fn analyze_function(mf: &MirFunction) -> EscapeInfo {
    let total_locals = mf.local_count as usize;
    let mut escaping: HashSet<u32> = HashSet::new();

    for bb in &mf.blocks {
        for instr in &bb.instructions {
            match instr {
                // ── Stores that push a value onto the heap or into global scope ──

                // Stored to a module global → survives past the current frame.
                Instr::StoreGlobal { value, .. } => mark_operand(&mut escaping, value),

                // Stored into an object field → the field owner may outlive the caller.
                Instr::SetField { value, .. } => mark_operand(&mut escaping, value),

                // Stored into a collection slot → the collection may outlive the caller.
                Instr::SetIndex { value, .. } => mark_operand(&mut escaping, value),

                // ── Rvalue assignments ─────────────────────────────────────────
                Instr::Assign { rhs, .. } => match rhs {
                    // Collection constructors heap-allocate their elements.
                    Rvalue::List(elems) | Rvalue::Tuple(elems) => {
                        mark_all_operands(&mut escaping, elems)
                    }

                    Rvalue::Dict(pairs) => {
                        for (k, v) in pairs {
                            mark_operand(&mut escaping, k);
                            mark_operand(&mut escaping, v);
                        }
                    }

                    // Object field initialisers escape into the heap object.
                    Rvalue::Construct { fields, .. } => {
                        for (_, v) in fields {
                            mark_operand(&mut escaping, v);
                        }
                    }

                    // Closure captures escape into the heap closure object.
                    Rvalue::MakeClosure { captures, .. } => {
                        mark_all_operands(&mut escaping, captures)
                    }

                    // Function calls: all arguments escape conservatively.
                    Rvalue::Call { callee, args } => {
                        mark_all_operands(&mut escaping, args);
                        if let Callee::Method { receiver, .. } = callee {
                            mark_operand(&mut escaping, receiver);
                        }
                        if let Callee::Dynamic(fn_op) = callee {
                            mark_operand(&mut escaping, fn_op);
                        }
                    }

                    _ => {}
                },

                // ── Call instruction (non-rvalue) ──────────────────────────────
                Instr::Call { callee, args, .. } => {
                    mark_all_operands(&mut escaping, args);
                    if let Callee::Method { receiver, .. } = callee {
                        mark_operand(&mut escaping, receiver);
                    }
                    if let Callee::Dynamic(fn_op) = callee {
                        mark_operand(&mut escaping, fn_op);
                    }
                }

                // ── Concurrency: args cross thread boundaries ──────────────────
                Instr::SpawnExpr { args, .. }
                | Instr::SpawnConcurrent { args, .. }
                | Instr::SpawnParallel { args, .. } => mark_all_operands(&mut escaping, args),

                Instr::SpawnDynamic { args, .. } => mark_all_operands(&mut escaping, args),

                Instr::ParallelIter {
                    collection,
                    closure_args,
                    ..
                } => {
                    mark_operand(&mut escaping, collection);
                    mark_all_operands(&mut escaping, closure_args);
                }

                _ => {}
            }
        }

        // ── Terminators ────────────────────────────────────────────────────────

        match &bb.terminator {
            // Return value escapes to the caller's frame.
            Terminator::Return(Some(op)) => mark_operand(&mut escaping, op),

            // Thrown value escapes to the nearest catch handler (may be in caller).
            Terminator::Throw { value } => mark_operand(&mut escaping, value),

            _ => {}
        }
    }

    let non_escaping = (0..total_locals as u32)
        .filter(|i| !escaping.contains(i))
        .map(LocalId)
        .collect();

    EscapeInfo {
        non_escaping,
        total_locals,
    }
}

/// Analyse all functions in `program` and return one `EscapeInfo` per function,
/// indexed by `FunctionId.0`.
pub fn analyze(program: &MirProgram) -> Vec<EscapeInfo> {
    program.functions.iter().map(analyze_function).collect()
}

// ── Tests ──────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use fidan_lexer::SymbolInterner;
    use fidan_mir::*;
    use std::sync::Arc;

    fn interner() -> Arc<SymbolInterner> {
        Arc::new(SymbolInterner::new())
    }

    /// Build a minimal MirFunction with a given set of instructions and
    /// terminator.
    fn make_fn(
        interner: &SymbolInterner,
        local_count: u32,
        instrs: Vec<Instr>,
        term: Terminator,
    ) -> MirFunction {
        let name = interner.intern("test_fn");
        MirFunction {
            id: FunctionId(1),
            name,
            params: vec![],
            return_ty: MirTy::Nothing,
            local_count,
            blocks: vec![BasicBlock {
                id: BlockId(0),
                phis: vec![],
                instructions: instrs,
                terminator: term,
            }],
            precompile: false,
            extern_decl: None,
            custom_decorators: vec![],
        }
    }

    // All locals that are only used as conditions / arithmetic never escape.
    #[test]
    fn pure_arithmetic_no_escape() {
        let si = interner();
        let mf = make_fn(
            &si,
            3,
            vec![
                Instr::Assign {
                    dest: LocalId(0),
                    ty: MirTy::Integer,
                    rhs: Rvalue::Literal(MirLit::Int(10)),
                },
                Instr::Assign {
                    dest: LocalId(1),
                    ty: MirTy::Integer,
                    rhs: Rvalue::Literal(MirLit::Int(20)),
                },
                Instr::Assign {
                    dest: LocalId(2),
                    ty: MirTy::Integer,
                    rhs: Rvalue::Binary {
                        op: fidan_ast::BinOp::Add,
                        lhs: Operand::Local(LocalId(0)),
                        rhs: Operand::Local(LocalId(1)),
                    },
                },
            ],
            Terminator::Return(None),
        );

        let info = analyze_function(&mf);
        // No locals should escape (3 pure arithmetic temps, void return).
        assert_eq!(info.non_escaping.len(), 3);
        assert_eq!(info.num_escaping(), 0);
        assert!(info.is_non_escaping(LocalId(0)));
        assert!(info.is_non_escaping(LocalId(1)));
        assert!(info.is_non_escaping(LocalId(2)));
    }

    // The returned local escapes.
    #[test]
    fn returned_local_escapes() {
        let si = interner();
        let mf = make_fn(
            &si,
            1,
            vec![Instr::Assign {
                dest: LocalId(0),
                ty: MirTy::Integer,
                rhs: Rvalue::Literal(MirLit::Int(42)),
            }],
            Terminator::Return(Some(Operand::Local(LocalId(0)))),
        );

        let info = analyze_function(&mf);
        assert_eq!(info.num_escaping(), 1);
        assert!(!info.is_non_escaping(LocalId(0)));
    }

    // A value pushed into a list escapes.
    #[test]
    fn list_element_escapes() {
        let si = interner();
        let mf = make_fn(
            &si,
            2,
            vec![
                Instr::Assign {
                    dest: LocalId(0),
                    ty: MirTy::Integer,
                    rhs: Rvalue::Literal(MirLit::Int(7)),
                },
                Instr::Assign {
                    dest: LocalId(1),
                    ty: MirTy::Dynamic,
                    rhs: Rvalue::List(vec![Operand::Local(LocalId(0))]),
                },
            ],
            Terminator::Return(None),
        );

        let info = analyze_function(&mf);
        // Local 0 escapes into the list; local 1 (the list itself) does not escape (void return).
        assert!(!info.is_non_escaping(LocalId(0)));
        assert!(info.is_non_escaping(LocalId(1)));
    }

    // A value stored to a global escapes.
    #[test]
    fn global_store_escapes() {
        let si = interner();
        let mf = make_fn(
            &si,
            1,
            vec![
                Instr::Assign {
                    dest: LocalId(0),
                    ty: MirTy::Integer,
                    rhs: Rvalue::Literal(MirLit::Int(99)),
                },
                Instr::StoreGlobal {
                    global: GlobalId(0),
                    value: Operand::Local(LocalId(0)),
                },
            ],
            Terminator::Return(None),
        );

        let info = analyze_function(&mf);
        assert!(!info.is_non_escaping(LocalId(0)));
    }

    // Thrown value escapes.
    #[test]
    fn thrown_value_escapes() {
        let si = interner();
        let mf = make_fn(
            &si,
            1,
            vec![Instr::Assign {
                dest: LocalId(0),
                ty: MirTy::Dynamic,
                rhs: Rvalue::Literal(MirLit::Str("oops".to_owned())),
            }],
            Terminator::Throw {
                value: Operand::Local(LocalId(0)),
            },
        );

        let info = analyze_function(&mf);
        assert!(!info.is_non_escaping(LocalId(0)));
    }

    // Empty function → all locals non-escaping (vacuously true).
    #[test]
    fn empty_function_all_non_escaping() {
        let si = interner();
        let mf = make_fn(&si, 4, vec![], Terminator::Return(None));
        let info = analyze_function(&mf);
        assert_eq!(info.non_escaping.len(), 4);
        assert_eq!(info.num_escaping(), 0);
        assert!((info.escape_ratio() - 0.0).abs() < f64::EPSILON);
    }
}
