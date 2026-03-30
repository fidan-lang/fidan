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
use fidan_mir::{Callee, GlobalId, Instr, LocalId, MirLit, MirProgram, Operand, Rvalue};
use rustc_hash::{FxHashMap, FxHashSet};

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
    let async_namespace_globals = async_namespace_globals(prog, interner);
    let async_consumer_fn_globals = async_consumer_fn_globals(prog, interner);

    for func in &prog.functions {
        let mut pending_locals: FxHashSet<LocalId> = FxHashSet::default();
        for block in &func.blocks {
            for instr in &block.instructions {
                if let Instr::SpawnExpr { dest, .. } | Instr::SpawnDynamic { dest, .. } = instr {
                    pending_locals.insert(*dest);
                }
            }
        }

        // ── Build a shallow copy-alias map ────────────────────────────────────
        // For `Assign { dest: %d, rhs: Use(Local(%src)) }`, record %d → %src.
        // Also extend through globals: `store_global g = %l` followed by
        // `%d = load_global g` is treated as `%d` being a copy of %l.
        // This handles the common pattern `var h = spawn foo()` where `h` is a
        // module-level global — the spawn result goes through a StoreGlobal /
        // LoadGlobal pair before reaching the AwaitPending instruction.
        let mut copy_of: FxHashMap<LocalId, LocalId> = FxHashMap::default();
        let mut global_origin: FxHashMap<GlobalId, LocalId> = FxHashMap::default();
        let mut container_roots: FxHashMap<LocalId, FxHashSet<LocalId>> = FxHashMap::default();
        let mut global_container_roots: FxHashMap<GlobalId, FxHashSet<LocalId>> =
            FxHashMap::default();
        let mut async_namespace_locals: FxHashSet<LocalId> = FxHashSet::default();
        let mut async_consumer_fn_locals: FxHashMap<LocalId, String> = FxHashMap::default();
        let mut global_async_consumer_fns: FxHashMap<GlobalId, String> = FxHashMap::default();

        for block in &func.blocks {
            for instr in &block.instructions {
                match instr {
                    Instr::Assign {
                        dest,
                        rhs: Rvalue::Literal(MirLit::Namespace(module)),
                        ..
                    } if module == "async" => {
                        async_namespace_locals.insert(*dest);
                    }
                    Instr::Assign {
                        dest,
                        rhs: Rvalue::Literal(MirLit::StdlibFn { module, name }),
                        ..
                    } if module == "async" && async_consumes_pending(name) => {
                        async_consumer_fn_locals.insert(*dest, name.clone());
                    }
                    Instr::Assign {
                        dest,
                        rhs: Rvalue::Use(Operand::Local(src)),
                        ..
                    } => {
                        let root = chase(*src, &copy_of);
                        copy_of.insert(*dest, root);
                        if async_namespace_locals.contains(&root) {
                            async_namespace_locals.insert(*dest);
                        }
                        if let Some(name) = async_consumer_fn_locals.get(&root).cloned() {
                            async_consumer_fn_locals.insert(*dest, name);
                        }
                        if let Some(roots) = container_roots.get(&root).cloned() {
                            container_roots.insert(*dest, roots);
                        }
                    }
                    Instr::Assign {
                        dest,
                        rhs: Rvalue::List(items) | Rvalue::Tuple(items),
                        ..
                    } => {
                        let roots = consumed_pending_roots(
                            items,
                            &pending_locals,
                            &copy_of,
                            &container_roots,
                        );
                        if !roots.is_empty() {
                            container_roots.insert(*dest, roots);
                        }
                    }
                    Instr::GetField {
                        dest,
                        object: Operand::Local(local),
                        field,
                    } if async_namespace_locals.contains(&chase(*local, &copy_of)) => {
                        let name = interner.resolve(*field);
                        if async_consumes_pending(name.as_ref()) {
                            async_consumer_fn_locals.insert(*dest, name.to_string());
                        }
                    }
                    // Track which local's value is stored in each global,
                    // chasing any existing copy aliases first.
                    Instr::StoreGlobal {
                        global,
                        value: Operand::Local(l),
                    } => {
                        let root = chase(*l, &copy_of);
                        global_origin.insert(*global, root);
                        if let Some(roots) = container_roots.get(&root).cloned() {
                            global_container_roots.insert(*global, roots);
                        }
                        if let Some(name) = async_consumer_fn_locals.get(&root).cloned() {
                            global_async_consumer_fns.insert(*global, name);
                        }
                    }
                    // A load of a global with a known origin extends the copy chain.
                    Instr::LoadGlobal { dest, global } => {
                        if let Some(&origin) = global_origin.get(global) {
                            copy_of.insert(*dest, origin);
                        }
                        if async_namespace_globals.contains(global) {
                            async_namespace_locals.insert(*dest);
                        }
                        if let Some(name) = async_consumer_fn_globals.get(global).cloned() {
                            async_consumer_fn_locals.insert(*dest, name);
                        }
                        if let Some(name) = global_async_consumer_fns.get(global).cloned() {
                            async_consumer_fn_locals.insert(*dest, name);
                        }
                        if let Some(roots) = global_container_roots.get(global).cloned() {
                            container_roots.insert(*dest, roots);
                        }
                    }
                    _ => {}
                }
            }
        }

        // ── Collect pending-producing and awaited locals ───────────────────────
        let mut awaited_locals: FxHashSet<LocalId> = FxHashSet::default();

        for block in &func.blocks {
            for instr in &block.instructions {
                match instr {
                    Instr::AwaitPending {
                        handle: Operand::Local(l),
                        ..
                    } => {
                        // Chase copy-aliases so `await h` where `h = spawn`
                        // correctly resolves back to the SpawnExpr dest.
                        awaited_locals.insert(chase(*l, &copy_of));
                    }
                    Instr::Call { callee, args, .. } => {
                        if let Some(name) = async_consumer_name(
                            callee,
                            interner,
                            &async_namespace_locals,
                            &async_consumer_fn_locals,
                            &copy_of,
                        ) {
                            mark_async_consumed(
                                &mut awaited_locals,
                                &name,
                                args,
                                &pending_locals,
                                &copy_of,
                                &container_roots,
                            );
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
fn chase(mut l: LocalId, copy_of: &FxHashMap<LocalId, LocalId>) -> LocalId {
    for _ in 0..32 {
        match copy_of.get(&l) {
            Some(&src) => l = src,
            None => break,
        }
    }
    l
}

fn async_namespace_globals(prog: &MirProgram, interner: &SymbolInterner) -> FxHashSet<GlobalId> {
    let async_aliases: FxHashSet<String> = prog
        .use_decls
        .iter()
        .filter(|decl| decl.is_stdlib && decl.module == "async" && decl.specific_names.is_none())
        .map(|decl| decl.alias.clone())
        .collect();
    prog.globals
        .iter()
        .enumerate()
        .filter_map(|(index, global)| {
            async_aliases
                .contains(interner.resolve(global.name).as_ref())
                .then_some(GlobalId(index as u32))
        })
        .collect()
}

fn async_consumer_fn_globals(
    prog: &MirProgram,
    interner: &SymbolInterner,
) -> FxHashMap<GlobalId, String> {
    let mut imported_names: FxHashSet<String> = FxHashSet::default();
    for decl in &prog.use_decls {
        if decl.is_stdlib
            && decl.module == "async"
            && let Some(names) = &decl.specific_names
        {
            for name in names {
                if async_consumes_pending(name) {
                    imported_names.insert(name.clone());
                }
            }
        }
    }

    prog.globals
        .iter()
        .enumerate()
        .filter_map(|(index, global)| {
            let name = interner.resolve(global.name);
            imported_names
                .get(name.as_ref())
                .cloned()
                .map(|imported| (GlobalId(index as u32), imported))
        })
        .collect()
}

fn async_consumes_pending(name: &str) -> bool {
    matches!(
        name,
        "gather" | "waitAll" | "wait_all" | "waitAny" | "wait_any" | "timeout"
    )
}

fn async_consumer_name(
    callee: &Callee,
    interner: &SymbolInterner,
    async_namespace_locals: &FxHashSet<LocalId>,
    async_consumer_fn_locals: &FxHashMap<LocalId, String>,
    copy_of: &FxHashMap<LocalId, LocalId>,
) -> Option<String> {
    match callee {
        Callee::Method {
            receiver: Operand::Local(local),
            method,
        } if async_namespace_locals.contains(&chase(*local, copy_of)) => {
            let name = interner.resolve(*method);
            async_consumes_pending(name.as_ref()).then(|| name.to_string())
        }
        Callee::Dynamic(Operand::Local(local)) => async_consumer_fn_locals
            .get(&chase(*local, copy_of))
            .cloned(),
        Callee::Dynamic(Operand::Const(MirLit::StdlibFn { module, name }))
            if module == "async" && async_consumes_pending(name) =>
        {
            Some(name.clone())
        }
        _ => None,
    }
}

fn mark_async_consumed(
    awaited_locals: &mut FxHashSet<LocalId>,
    name: &str,
    args: &[Operand],
    pending_locals: &FxHashSet<LocalId>,
    copy_of: &FxHashMap<LocalId, LocalId>,
    container_roots: &FxHashMap<LocalId, FxHashSet<LocalId>>,
) {
    match name {
        "gather" | "waitAll" | "wait_all" | "waitAny" | "wait_any" => {
            if let Some(arg) = args.first() {
                awaited_locals.extend(consumed_pending_roots(
                    std::slice::from_ref(arg),
                    pending_locals,
                    copy_of,
                    container_roots,
                ));
            }
        }
        "timeout" => {
            if let Some(arg) = args.first() {
                awaited_locals.extend(consumed_pending_roots(
                    std::slice::from_ref(arg),
                    pending_locals,
                    copy_of,
                    container_roots,
                ));
            }
        }
        _ => {}
    }
}

fn consumed_pending_roots(
    operands: &[Operand],
    pending_locals: &FxHashSet<LocalId>,
    copy_of: &FxHashMap<LocalId, LocalId>,
    container_roots: &FxHashMap<LocalId, FxHashSet<LocalId>>,
) -> FxHashSet<LocalId> {
    let mut roots = FxHashSet::default();
    for operand in operands {
        if let Operand::Local(local) = operand {
            let root = chase(*local, copy_of);
            if pending_locals.contains(&root) {
                roots.insert(root);
            }
            if let Some(nested) = container_roots.get(&root) {
                roots.extend(nested.iter().copied());
            }
        }
    }
    roots
}
