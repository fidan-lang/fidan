// fidan-passes/src/unawaited_pending.rs
//
// Unawaited `Pending` value detection (W1004).
//
// For each function in the MIR program, finds every `SpawnExpr` or
// `SpawnDynamic` instruction (both produce a `Pending` handle local) and
// verifies that the produced local is used as the `handle` operand of some
// `AwaitPending` instruction within the same function.
//
// A `var h = spawn foo()` + `await h` pattern creates an indirection:
//   SpawnExpr { dest: %d }
//   Assign { dest: %h, rhs: Rvalue::Use(Local(%d)) }   ← SSA copy
//   AwaitPending { handle: Local(%h) }
//
// We resolve the copy chain so that `%d` is recognised as awaited even
// when the await references the SSA alias `%h` rather than `%d` directly.

use fidan_lexer::SymbolInterner;
use fidan_mir::{GlobalId, Instr, LocalId, MirProgram, Operand, Rvalue};
use std::collections::HashMap;
use std::collections::HashSet;

/// One detected unawaited-Pending diagnostic.
pub struct UnawaitedPendingDiag {
    /// Name of the function that spawns but never awaits.
    pub fn_name: String,
    /// Number of unawaited spawns in that function.
    pub count: usize,
}

/// Run the unawaited-Pending check over the entire program.
///
/// Returns one `UnawaitedPendingDiag` for each function that contains at
/// least one `spawn` expression whose result local is never passed to `await`.
pub fn check(prog: &MirProgram, interner: &SymbolInterner) -> Vec<UnawaitedPendingDiag> {
    let mut diags = Vec::new();

    for func in &prog.functions {
        // ── Build a shallow copy-alias map ────────────────────────────────────
        // For `Assign { dest: %d, rhs: Use(Local(%src)) }`, record %d → %src.
        // Also extend through globals: `store_global g = %l` followed by
        // `%d = load_global g` is treated as `%d` being a copy of %l.
        // This handles the common pattern `var h = spawn foo()` where `h` is a
        // module-level global — the spawn result goes through a StoreGlobal /
        // LoadGlobal pair before reaching the AwaitPending instruction.
        let mut copy_of: HashMap<LocalId, LocalId> = HashMap::new();
        let mut global_origin: HashMap<GlobalId, LocalId> = HashMap::new();

        for block in &func.blocks {
            for instr in &block.instructions {
                match instr {
                    Instr::Assign {
                        dest,
                        rhs: Rvalue::Use(Operand::Local(src)),
                        ..
                    } => {
                        copy_of.insert(*dest, *src);
                    }
                    // Track which local's value is stored in each global,
                    // chasing any existing copy aliases first.
                    Instr::StoreGlobal {
                        global,
                        value: Operand::Local(l),
                    } => {
                        global_origin.insert(*global, chase(*l, &copy_of));
                    }
                    // A load of a global with a known origin extends the copy chain.
                    Instr::LoadGlobal { dest, global } => {
                        if let Some(&origin) = global_origin.get(global) {
                            copy_of.insert(*dest, origin);
                        }
                    }
                    _ => {}
                }
            }
        }

        // ── Collect pending-producing and awaited locals ───────────────────────
        let mut pending_locals: HashSet<LocalId> = HashSet::new();
        let mut awaited_locals: HashSet<LocalId> = HashSet::new();

        for block in &func.blocks {
            for instr in &block.instructions {
                match instr {
                    Instr::SpawnExpr { dest, .. } | Instr::SpawnDynamic { dest, .. } => {
                        pending_locals.insert(*dest);
                    }
                    Instr::AwaitPending { handle, .. } => {
                        if let Operand::Local(l) = handle {
                            // Chase copy-aliases so `await h` where `h = spawn`
                            // correctly resolves back to the SpawnExpr dest.
                            awaited_locals.insert(chase(*l, &copy_of));
                        }
                    }
                    _ => {}
                }
            }
        }

        let unawaited_count = pending_locals
            .iter()
            .filter(|l| !awaited_locals.contains(l))
            .count();

        if unawaited_count > 0 {
            let fn_name = interner.resolve(func.name).to_string();
            diags.push(UnawaitedPendingDiag {
                fn_name,
                count: unawaited_count,
            });
        }
    }

    diags
}

/// Chase the copy-alias chain to find the root local that a given local
/// was ultimately copied from.  Cuts off after 32 hops to avoid cycles
/// (which cannot occur in valid SSA, but defensive programming costs nothing).
fn chase(mut l: LocalId, copy_of: &HashMap<LocalId, LocalId>) -> LocalId {
    for _ in 0..32 {
        match copy_of.get(&l) {
            Some(&src) => l = src,
            None => break,
        }
    }
    l
}
