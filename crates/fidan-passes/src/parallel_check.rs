// fidan-passes/src/parallel_check.rs
//
// Parallel data-race detection pass (E0401).
//
// Algorithm
// ---------
// For each function F in the MIR program:
//   1. Build a map: spawn-handle local → task FunctionId for every
//      SpawnParallel / SpawnConcurrent instruction encountered.
//   2. For each JoinAll instruction, collect the participating task functions
//      referenced by the joined handles.
//   3. For each *pair* of tasks (A, B) in the same JoinAll group:
//      - `write_set(A)` = set of GlobalIds written by A  (StoreGlobal)
//      - `access_set(B)` = set of GlobalIds read or written by B
//      - Intersection of write_set(A) ∩ access_set(B) → E0401
//   4. For each ParallelIter body function: any StoreGlobal in the body is a
//      race across iterations → E0401.
//
// Only module-level globals can race: task-private captures are value-copied
// per invocation and mutations are discarded after the task returns.
// `Shared` globals are deliberately safe (Arc<Mutex> internals), but since
// we cannot distinguish them at the GlobalId level yet we report all races
// and the fix suggestion directs users to `Shared`.

use fidan_lexer::SymbolInterner;
use fidan_mir::{FunctionId, GlobalId, Instr, LocalId, MirProgram};
use rustc_hash::{FxHashMap, FxHashSet};

/// One detected data-race diagnostic.
pub struct ParallelRaceDiag {
    /// Name of the global variable involved in the race.
    pub var_name: String,
    /// Short description of where the race occurs.
    pub context: String,
}

/// Run the parallel data-race check over the entire program.
///
/// Returns one `ParallelRaceDiag` per distinct (global, group) pair where a
/// module-level global is unsafely shared between two or more parallel tasks.
pub fn check(prog: &MirProgram, interner: &SymbolInterner) -> Vec<ParallelRaceDiag> {
    let mut diags = Vec::new();

    for func in &prog.functions {
        // ── Map: spawned-handle local → task FunctionId ───────────────────────
        let mut handle_map: FxHashMap<LocalId, FunctionId> = FxHashMap::default();

        for block in &func.blocks {
            for instr in &block.instructions {
                match instr {
                    Instr::SpawnParallel {
                        handle, task_fn, ..
                    }
                    | Instr::SpawnConcurrent {
                        handle, task_fn, ..
                    } => {
                        handle_map.insert(*handle, *task_fn);
                    }
                    _ => {}
                }
            }
        }

        // ── Check each JoinAll group ──────────────────────────────────────────
        for block in &func.blocks {
            for instr in &block.instructions {
                if let Instr::JoinAll { handles } = instr {
                    let tasks: Vec<FunctionId> = handles
                        .iter()
                        .filter_map(|h| handle_map.get(h).copied())
                        .collect();
                    if tasks.len() < 2 {
                        continue;
                    }
                    check_task_group(prog, &tasks, interner, &mut diags);
                }

                // ── ParallelIter: each iteration is its own parallel task ─────
                if let Instr::ParallelIter { body_fn, .. } = instr {
                    check_parallel_for_body(prog, *body_fn, interner, &mut diags);
                }
            }
        }
    }

    diags
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn global_sets(prog: &MirProgram, fn_id: FunctionId) -> (FxHashSet<GlobalId>, FxHashSet<GlobalId>) {
    let mut writes: FxHashSet<GlobalId> = FxHashSet::default();
    let mut accesses: FxHashSet<GlobalId> = FxHashSet::default();

    if fn_id.0 as usize >= prog.functions.len() {
        return (writes, accesses);
    }
    let func = &prog.functions[fn_id.0 as usize];
    for block in &func.blocks {
        for instr in &block.instructions {
            match instr {
                Instr::StoreGlobal { global, .. } => {
                    writes.insert(*global);
                    accesses.insert(*global);
                }
                Instr::LoadGlobal { global, .. } => {
                    accesses.insert(*global);
                }
                _ => {}
            }
        }
    }
    (writes, accesses)
}

fn resolve_global_name(prog: &MirProgram, gid: GlobalId, interner: &SymbolInterner) -> String {
    if (gid.0 as usize) < prog.globals.len() {
        interner
            .resolve(prog.globals[gid.0 as usize].name)
            .to_string()
    } else {
        format!("global#{}", gid.0)
    }
}

fn check_task_group(
    prog: &MirProgram,
    tasks: &[FunctionId],
    interner: &SymbolInterner,
    diags: &mut Vec<ParallelRaceDiag>,
) {
    // Build per-task sets.
    let sets: Vec<(FxHashSet<GlobalId>, FxHashSet<GlobalId>)> =
        tasks.iter().map(|&fid| global_sets(prog, fid)).collect();

    let mut reported: FxHashSet<GlobalId> = FxHashSet::default();

    for i in 0..tasks.len() {
        for j in 0..tasks.len() {
            if i == j {
                continue;
            }
            let (ref writes_i, _) = sets[i];
            let (_, ref access_j) = sets[j];

            for &gid in writes_i {
                if reported.contains(&gid) {
                    continue;
                }
                if access_j.contains(&gid) {
                    let var_name = resolve_global_name(prog, gid, interner);
                    diags.push(ParallelRaceDiag {
                        var_name: var_name.clone(),
                        context: format!(
                            "written by task `{}` and accessed by task `{}`; \
                             wrap in `Shared oftype T` to allow safe concurrent mutation",
                            interner.resolve(prog.functions[tasks[i].0 as usize].name),
                            interner.resolve(prog.functions[tasks[j].0 as usize].name),
                        ),
                    });
                    reported.insert(gid);
                }
            }
        }
    }
}

fn check_parallel_for_body(
    prog: &MirProgram,
    body_fn: FunctionId,
    interner: &SymbolInterner,
    diags: &mut Vec<ParallelRaceDiag>,
) {
    if body_fn.0 as usize >= prog.functions.len() {
        return;
    }
    let body = &prog.functions[body_fn.0 as usize];
    let mut reported: FxHashSet<GlobalId> = FxHashSet::default();

    for block in &body.blocks {
        for instr in &block.instructions {
            if let Instr::StoreGlobal { global, .. } = instr {
                if reported.insert(*global) {
                    let var_name = resolve_global_name(prog, *global, interner);
                    diags.push(ParallelRaceDiag {
                        var_name: var_name.clone(),
                        context: format!(
                            "`{}` is mutated inside a `parallel for` body — \
                             each iteration runs concurrently and writes are \
                             silently lost; use `Shared oftype T` and `.update()` \
                             for safe accumulation",
                            var_name
                        ),
                    });
                }
            }
        }
    }
}
