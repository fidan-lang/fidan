// fidan-interp/src/mir_interp.rs
//
// Phase 6: MIR interpreter.
//
// Executes a `MirProgram` by walking its SSA/CFG representation.
// All non-local control flow (exceptions) is handled via an explicit
// per-call-frame catch stack, mirroring the `PushCatch`/`PopCatch`
// instructions emitted by the MIR lowerer.

use std::sync::Arc;

use fidan_ast::{BinOp, UnOp};
use fidan_lexer::{Symbol, SymbolInterner};
use fidan_mir::{
    BlockId, Callee, FunctionId, Instr, LocalId, MirLit, MirObjectInfo, MirProgram, MirStringPart,
    Operand, Rvalue, Terminator,
};
use fidan_runtime::{
    FidanClass, FidanDict, FidanList, FidanObject, FidanPending, FidanString, FidanValue, FieldDef,
    FunctionId as RuntimeFnId, OwnedRef, ParallelArgs, ParallelCapture,
};
use fidan_source::{SourceMap, Span};
use fidan_stdlib::{StdlibResult, parallel::ParallelOp};

use crate::builtins;
use crate::interp::{RunError, TraceFrame};

// ── Object class table ────────────────────────────────────────────────────────

/// Build `Arc<FidanClass>` instances from `MirObjectInfo` metadata.
/// Parent classes are resolved recursively; cycles are silently broken.
fn build_class_table(
    objects: &[MirObjectInfo],
    interner: &SymbolInterner,
) -> std::collections::HashMap<fidan_lexer::Symbol, Arc<FidanClass>> {
    use std::collections::HashMap;
    let mut table: HashMap<fidan_lexer::Symbol, Arc<FidanClass>> = HashMap::new();

    // Build in the order objects appear (HIR outputs parents before children).
    for obj in objects {
        // Collect inherited fields from parent chain first, then own fields.
        let mut all_field_names: Vec<fidan_lexer::Symbol> = Vec::new();
        if let Some(parent_sym) = obj.parent {
            if let Some(parent_class) = table.get(&parent_sym) {
                // Add parent fields (in their original order).
                for fd in &parent_class.fields {
                    if !all_field_names.contains(&fd.name) {
                        all_field_names.push(fd.name);
                    }
                }
            }
        }
        for &f in &obj.field_names {
            if !all_field_names.contains(&f) {
                all_field_names.push(f);
            }
        }

        let field_defs: Vec<FieldDef> = all_field_names
            .iter()
            .enumerate()
            .map(|(i, &sym)| FieldDef {
                name: sym,
                index: i,
            })
            .collect();
        let mut method_map = std::collections::HashMap::new();
        for (&sym, &fid) in &obj.methods {
            method_map.insert(sym, RuntimeFnId(fid.0));
        }
        let parent = obj.parent.and_then(|p| table.get(&p).cloned());
        let name_str = interner.resolve(obj.name);
        let class = Arc::new(FidanClass {
            name: obj.name,
            name_str,
            parent,
            fields: field_defs,
            methods: method_map,
        });
        table.insert(obj.name, class);
    }
    table
}

// ── Call frame ────────────────────────────────────────────────────────────────

struct CallFrame {
    /// SSA locals — sized to `func.local_count` on entry.
    locals: Vec<FidanValue>,
    /// Exception-handler stack: `PushCatch` pushes, `PopCatch` pops.
    catch_stack: Vec<BlockId>,
    /// Set by `Terminator::Throw` before jumping to a catch block.
    current_exception: Option<FidanValue>,
    /// Name for stack-trace messages.
    #[allow(dead_code)]
    fn_name: String,
}

impl CallFrame {
    fn new(local_count: u32, fn_name: String) -> Self {
        Self {
            locals: vec![FidanValue::Nothing; local_count as usize],
            catch_stack: vec![],
            current_exception: None,
            fn_name,
        }
    }

    fn load(&self, local: LocalId) -> FidanValue {
        self.locals
            .get(local.0 as usize)
            .cloned()
            .unwrap_or(FidanValue::Nothing)
    }

    fn store(&mut self, local: LocalId, value: FidanValue) {
        if let Some(slot) = self.locals.get_mut(local.0 as usize) {
            *slot = value;
        }
    }
}

// ── MirMachine ────────────────────────────────────────────────────────────────

/// The MIR interpreter.  Stateless between calls; state lives in `CallFrame`.
pub struct MirMachine {
    program: Arc<MirProgram>,
    interner: Arc<SymbolInterner>,
    classes: std::collections::HashMap<Symbol, Arc<FidanClass>>,
    /// Source map for resolving spans to file/line/col in stack traces.
    source_map: Arc<SourceMap>,
    /// Names + call-site spans of all currently executing functions, outermost first.
    /// Each entry is `(full_label, call_site_span)` where the span is where *this*
    /// function was called from (i.e. the `Instr::Call` span in the caller).
    call_stack: Vec<(String, Option<Span>)>,
    /// Span of the `Instr::Call` currently being dispatched — consumed by the
    /// next `call_function` invocation to annotate its stack frame.
    pending_call_span: Option<Span>,
    /// Call stack snapshot at the point of the first uncaught panic/throw,
    /// innermost first.  Populated once and never overwritten.
    panic_trace: Vec<TraceFrame>,
    /// Maps free-imported function names (e.g. `readFile`) to their stdlib module
    /// (e.g. `"io"`).  Populated from `use std.io.{readFile}` declarations.
    stdlib_free_fns: std::collections::HashMap<Arc<str>, Arc<str>>,
    /// Set of stdlib module names/aliases known in this program (e.g. `"io"`, `"math"`).
    /// O(1) lookup used by `dispatch_method` to distinguish stdlib vs user namespaces.
    stdlib_modules: std::collections::HashSet<Arc<str>>,
    /// Maps merged free-function names to their `FunctionId`.
    /// Used for `use mymod` / `test2.add(...)` user-module namespace dispatch.
    user_fn_map: std::collections::HashMap<Symbol, FunctionId>,
    /// Module-level global variables, shared across all threads.
    /// Indexed by `GlobalId` (same index as `MirProgram::globals`).
    globals: Arc<std::sync::Mutex<Vec<FidanValue>>>,
    /// Accumulated test results when running in `fidan test` mode.
    #[allow(dead_code)]
    pub test_results: Vec<TestResult>,
}

/// A single test result recorded during `fidan test`.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct TestResult {
    pub name: String,
    pub passed: bool,
    pub message: Option<String>,
}

// SAFETY: All fields are either `Arc<T>` (Send+Sync when T:Send+Sync) or a
// `HashMap` of `Arc<FidanClass>` (Send+Sync).  `MirMachine` itself holds no
// `Rc` — frame-local `Rc` only lives on the call stack of a single thread.
unsafe impl Send for MirMachine {}

impl MirMachine {
    pub fn new(
        program: Arc<MirProgram>,
        interner: Arc<SymbolInterner>,
        source_map: Arc<SourceMap>,
    ) -> Self {
        let classes = build_class_table(&program.objects, &interner);

        // Build the free-function import map from `use std.module.{fn}` declarations.
        let mut stdlib_free_fns: std::collections::HashMap<Arc<str>, Arc<str>> =
            std::collections::HashMap::new();
        // Build the stdlib module set: module names AND aliases, for O(1) is-stdlib lookup.
        let mut stdlib_modules: std::collections::HashSet<Arc<str>> =
            std::collections::HashSet::new();
        for decl in &program.use_decls {
            if let Some(ref names) = decl.specific_names {
                let module: Arc<str> = Arc::from(decl.module.as_str());
                for name in names {
                    let fn_name: Arc<str> = Arc::from(name.as_str());
                    stdlib_free_fns.insert(fn_name, Arc::clone(&module));
                }
                stdlib_modules.insert(Arc::clone(&module));
            } else {
                // `use std.io` / `use std.io as myIo` — register both canonical name and alias.
                let module: Arc<str> = Arc::from(decl.module.as_str());
                let alias: Arc<str> = Arc::from(decl.alias.as_str());
                stdlib_modules.insert(Arc::clone(&module));
                stdlib_modules.insert(alias);
            }
        }

        let globals_count = program.globals.len();

        // Build user-module function name map: all merged free functions (non-init,
        // non-method). Methods have `this` as their first param; we detect that.
        let this_sym = interner.intern("this");
        let mut user_fn_map: std::collections::HashMap<Symbol, FunctionId> =
            std::collections::HashMap::new();
        for (i, func) in program.functions.iter().enumerate() {
            if i == 0 {
                continue; // skip init fn
            }
            // Skip methods: their first param is always named `this`.
            if func
                .params
                .first()
                .map(|p| p.name == this_sym)
                .unwrap_or(false)
            {
                continue;
            }
            user_fn_map.insert(func.name, FunctionId(i as u32));
        }

        Self {
            program,
            interner,
            classes,
            source_map,
            call_stack: Vec::new(),
            pending_call_span: None,
            panic_trace: Vec::new(),
            stdlib_free_fns,
            stdlib_modules,
            user_fn_map,
            globals: Arc::new(std::sync::Mutex::new(vec![
                FidanValue::Nothing;
                globals_count
            ])),
            test_results: Vec::new(),
        }
    }

    /// Create a lightweight clone of this machine for use on a parallel thread.
    ///
    /// `Arc` fields (`program`, `interner`) are bumped by a single atomic
    /// refcount.  The `classes` map clones its `Arc<FidanClass>` pointers —
    /// O(n-classes), but class tables are small.
    fn clone_for_thread(&self) -> MirMachine {
        MirMachine {
            program: Arc::clone(&self.program),
            interner: Arc::clone(&self.interner),
            classes: self.classes.clone(),
            source_map: Arc::clone(&self.source_map),
            call_stack: Vec::new(),
            pending_call_span: None,
            panic_trace: Vec::new(),
            stdlib_free_fns: self.stdlib_free_fns.clone(),
            stdlib_modules: self.stdlib_modules.clone(),
            user_fn_map: self.user_fn_map.clone(),
            globals: Arc::clone(&self.globals),
            test_results: Vec::new(),
        }
    }

    /// Resolve a `Symbol` to its string.
    fn sym_str(&self, sym: Symbol) -> Arc<str> {
        self.interner.resolve(sym)
    }

    // ── Entry point ──────────────────────────────────────────────────────────

    /// Execute the main (top-level init) function.
    pub fn run(&mut self) -> Result<(), RunError> {
        self.call_stack.clear();
        self.panic_trace.clear();
        let entry = FunctionId(0);
        match self.call_function(entry, vec![]) {
            Ok(_) => Ok(()),
            Err(MirSignal::Throw(v)) => Err(RunError {
                code: fidan_diagnostics::diag_code!("R0001"),
                message: format!("unhandled exception: {}", builtins::display(&v)),
                trace: std::mem::take(&mut self.panic_trace),
            }),
            Err(MirSignal::Panic(msg)) => Err(RunError {
                code: fidan_diagnostics::diag_code!("R0001"),
                message: msg,
                trace: std::mem::take(&mut self.panic_trace),
            }),
            Err(MirSignal::ParallelFail(msg)) => Err(RunError {
                code: fidan_diagnostics::diag_code!("R9001"),
                message: msg,
                trace: std::mem::take(&mut self.panic_trace),
            }),
        }
    }

    // ── Function call ─────────────────────────────────────────────────────────

    fn call_function(&mut self, fn_id: FunctionId, args: Vec<FidanValue>) -> MirResult {
        let func = self.program.function(fn_id);
        let fn_name = self.sym_str(func.name).to_string();
        let local_count = func.local_count;

        let mut frame = CallFrame::new(local_count, fn_name.clone());

        // Bind parameters and build a rich call-site label: `name(param = val, ...)`.
        let mut arg_parts: Vec<String> = Vec::new();
        for (i, param) in func.params.iter().enumerate() {
            let val = args.get(i).cloned().unwrap_or(FidanValue::Nothing);
            let pname = self.sym_str(param.name).to_string();
            let vdisplay = match &val {
                fidan_runtime::FidanValue::String(_) => {
                    format!("{:?}", builtins::display(&val))
                }
                _ => builtins::display(&val),
            };
            arg_parts.push(format!("{pname} = {vdisplay}"));
            frame.store(param.local, val);
        }
        let full_label = if arg_parts.is_empty() {
            format!("{fn_name}()")
        } else {
            format!("{fn_name}({})", arg_parts.join(", "))
        };

        // Consume the call-site span set by exec_instr just before calling us.
        let call_site_span = self.pending_call_span.take();
        self.call_stack.push((full_label, call_site_span));

        // Block-level execution starting at entry (BlockId(0)).
        let result = self.run_function(fn_id, &mut frame);

        // Capture the trace at the innermost frame (only once — don't overwrite).
        // Exclude the module-level entry function (FunctionId(0)) from the trace
        // since it is not a user-visible named function.
        if result.is_err() && self.panic_trace.is_empty() {
            self.panic_trace = self
                .call_stack
                .iter()
                .enumerate()
                .filter(|(i, _)| *i > 0) // skip entry function at index 0
                .rev()
                .map(|(_, (label, span))| {
                    let location = span.map(|s| {
                        let file = self.source_map.get(s.file);
                        let (line, col) = file.line_col(s.start);
                        format!("{}:{}:{}", file.name, line, col)
                    });
                    TraceFrame {
                        label: label.clone(),
                        location,
                    }
                })
                .collect();
        }

        self.call_stack.pop();

        result.map(|v| v.unwrap_or(FidanValue::Nothing))
    }

    fn run_function(
        &mut self,
        fn_id: FunctionId,
        frame: &mut CallFrame,
    ) -> Result<Option<FidanValue>, MirSignal> {
        let mut bb_id = BlockId(0);
        let mut prev_bb: Option<BlockId> = None;

        'outer: loop {
            // Evaluate phi-nodes using `prev_bb`.
            let phis: Vec<(LocalId, FidanValue)> = {
                let func = self.program.function(fn_id);
                let bb = func.block(bb_id);
                bb.phis
                    .iter()
                    .map(|phi| {
                        let val = if let Some(p) = prev_bb {
                            phi.operands
                                .iter()
                                .find(|(src, _)| *src == p)
                                .map(|(_, op)| self.eval_operand(op, frame))
                                .unwrap_or(FidanValue::Nothing)
                        } else {
                            FidanValue::Nothing
                        };
                        (phi.result, val)
                    })
                    .collect()
            };
            for (dest, val) in phis {
                frame.store(dest, val);
            }

            // Execute instructions.
            let instr_count = { self.program.function(fn_id).block(bb_id).instructions.len() };

            for i in 0..instr_count {
                let instr = self.program.function(fn_id).block(bb_id).instructions[i].clone();
                match self.exec_instr(instr, frame) {
                    Ok(Some(ret)) => return Ok(Some(ret)),
                    Ok(None) => {}
                    // A callee threw — route through the *caller's* catch stack.
                    Err(MirSignal::Throw(v)) => {
                        if let Some(catch_bb) = frame.catch_stack.pop() {
                            frame.current_exception = Some(v);
                            prev_bb = Some(bb_id);
                            bb_id = catch_bb;
                            continue 'outer;
                        } else {
                            return Err(MirSignal::Throw(v));
                        }
                    }
                    Err(e) => return Err(e),
                }
            }

            // Handle terminator.
            let term = self.program.function(fn_id).block(bb_id).terminator.clone();
            match term {
                Terminator::Return(op) => {
                    let val = op.as_ref().map(|o| self.eval_operand(o, frame));
                    return Ok(val);
                }
                Terminator::Goto(target) => {
                    prev_bb = Some(bb_id);
                    bb_id = target;
                }
                Terminator::Branch {
                    cond,
                    then_bb,
                    else_bb,
                } => {
                    let cv = self.eval_operand(&cond, frame);
                    prev_bb = Some(bb_id);
                    bb_id = if cv.truthy() { then_bb } else { else_bb };
                }
                Terminator::Throw { value } => {
                    let v = self.eval_operand(&value, frame);
                    if let Some(catch_bb) = frame.catch_stack.pop() {
                        frame.current_exception = Some(v);
                        prev_bb = Some(bb_id);
                        bb_id = catch_bb;
                    } else {
                        return Err(MirSignal::Throw(v));
                    }
                }
                Terminator::Unreachable => {
                    return Err(MirSignal::Panic("reached unreachable block".to_string()));
                }
            }
        }
    }

    // ── Instruction execution ─────────────────────────────────────────────────

    /// Returns `Some(FidanValue)` only if the instruction causes an early return
    /// (which only happens inside `Instr::Call` to a user function that returns).
    fn exec_instr(
        &mut self,
        instr: Instr,
        frame: &mut CallFrame,
    ) -> Result<Option<FidanValue>, MirSignal> {
        match instr {
            Instr::Assign { dest, rhs, .. } => {
                let val = self.eval_rvalue(rhs, frame)?;
                frame.store(dest, val);
            }
            Instr::Call {
                dest,
                callee,
                args,
                span,
            } => {
                // Record the call-site span so call_function can attach it to the frame.
                self.pending_call_span = Some(span);
                let arg_vals: Vec<FidanValue> =
                    args.iter().map(|a| self.eval_operand(a, frame)).collect();
                let result = self.dispatch_call(&callee, arg_vals, frame)?;
                if let Some(d) = dest {
                    frame.store(d, result);
                }
            }
            Instr::SetField {
                object,
                field,
                value,
            } => {
                let mut obj_val = self.eval_operand(&object, frame);
                let val = self.eval_operand(&value, frame);
                self.set_field(&mut obj_val, field, val);
                // Write back — the object operand should be a local.
                if let Operand::Local(l) = object {
                    frame.store(l, obj_val);
                }
            }
            Instr::GetField {
                dest,
                object,
                field,
            } => {
                let obj_val = self.eval_operand(&object, frame);
                let val = self.get_field(&obj_val, field);
                frame.store(dest, val);
            }
            Instr::GetIndex {
                dest,
                object,
                index,
            } => {
                let obj_val = self.eval_operand(&object, frame);
                let idx_val = self.eval_operand(&index, frame);
                let val = self.index_get(obj_val, idx_val)?;
                frame.store(dest, val);
            }
            Instr::SetIndex {
                object,
                index,
                value,
            } => {
                let obj_val = self.eval_operand(&object, frame);
                let idx_val = self.eval_operand(&index, frame);
                let val = self.eval_operand(&value, frame);
                self.index_set(obj_val, idx_val, val)?;
            }
            Instr::Drop { .. } => {
                // Values are reference-counted; explicit Drop is a no-op here.
            }
            Instr::Nop => {}
            Instr::PushCatch(catch_bb) => {
                frame.catch_stack.push(catch_bb);
            }
            Instr::PopCatch => {
                frame.catch_stack.pop();
            }
            // ── Module-level globals ──────────────────────────────────────────
            Instr::LoadGlobal { dest, global } => {
                let val = self
                    .globals
                    .lock()
                    .unwrap()
                    .get(global.0 as usize)
                    .cloned()
                    .unwrap_or(FidanValue::Nothing);
                frame.store(dest, val);
            }
            Instr::StoreGlobal { global, value } => {
                let val = self.eval_operand(&value, frame);
                if let Some(slot) = self.globals.lock().unwrap().get_mut(global.0 as usize) {
                    *slot = val;
                }
            }
            // ── Concurrency / Parallelism ─────────────────────────────────────
            //
            // SpawnParallel / SpawnConcurrent: launch the task on a real OS
            // thread.  The caller is expected to `JoinAll` the handles later.
            //
            // SpawnExpr: same, but the result is `await`-ed explicitly.
            //
            // Each thread gets its own `MirMachine` (clone_for_thread is O(1)
            // for Arc fields).  Captured values go through `parallel_capture()`
            // to produce fresh `Rc<RefCell<T>>` wrappers around the shared CoW
            // `Arc<…>` inner data — zero data copying until first mutation.
            //
            // IMPORTANT: closures capture `ParallelArgs` (whole type, Send) and
            // unwrap via `.into_vec()` method call — NOT `.0` field access.
            // Rust 2021 partial-capture would otherwise see `!Send FidanValue`.
            Instr::SpawnConcurrent {
                handle,
                task_fn,
                args,
            }
            | Instr::SpawnParallel {
                handle,
                task_fn,
                args,
            } => {
                // Capture the task name while `self` is still in scope so that
                // failure messages can include the source-level task name.
                let task_name = {
                    let func = self.program.function(task_fn);
                    self.sym_str(func.name).to_string()
                };
                let args_bundle = ParallelArgs::from_captures(
                    args.iter()
                        .map(|a| ParallelCapture(self.eval_operand(a, frame).parallel_capture())),
                );
                let mut child = self.clone_for_thread();
                let pending = FidanPending::spawn_fallible(args_bundle, move |bundle| {
                    child
                        .call_function(task_fn, bundle.into_vec())
                        .map_err(|sig| match sig {
                            MirSignal::Panic(m) => {
                                format!("task `{}` panicked: {}", task_name, m)
                            }
                            MirSignal::Throw(v) => {
                                format!(
                                    "task `{}` threw an uncaught error: {}",
                                    task_name,
                                    crate::builtins::display(&v)
                                )
                            }
                            MirSignal::ParallelFail(m) => {
                                format!("task `{}` failed: {}", task_name, m)
                            }
                        })
                });
                frame.store(handle, FidanValue::Pending(pending));
            }

            Instr::SpawnExpr {
                dest,
                task_fn,
                args,
            } => {
                let args_bundle = ParallelArgs::from_captures(
                    args.iter()
                        .map(|a| ParallelCapture(self.eval_operand(a, frame).parallel_capture())),
                );
                let mut child = self.clone_for_thread();
                let pending = FidanPending::spawn_with_args(args_bundle, move |bundle| {
                    child
                        .call_function(task_fn, bundle.into_vec())
                        .unwrap_or(FidanValue::Nothing)
                });
                frame.store(dest, FidanValue::Pending(pending));
            }

            Instr::SpawnDynamic { dest, method, args } => {
                // Evaluate all args (args[0] = receiver or fn-value) in the
                // current frame and capture them for the new thread.
                let caps: Vec<ParallelCapture> = args
                    .iter()
                    .map(|a| ParallelCapture(self.eval_operand(a, frame).parallel_capture()))
                    .collect();
                // Look up the method name before moving `self` into the closure.
                let method_name: Option<Arc<str>> = method.map(|sym| self.sym_str(sym));
                let bundle = ParallelArgs::from_captures(caps);
                let mut child = self.clone_for_thread();
                let pending = FidanPending::spawn_with_args(bundle, move |bundle| {
                    let mut vals = bundle.into_vec();
                    if vals.is_empty() {
                        return FidanValue::Nothing;
                    }
                    let first = vals.remove(0);
                    match method_name {
                        Some(ref name) => {
                            // Method dispatch: first value is the receiver.
                            child
                                .dispatch_method(first, name, vals)
                                .unwrap_or(FidanValue::Nothing)
                        }
                        None => match first {
                            // Dynamic fn-value dispatch.
                            FidanValue::Function(RuntimeFnId(id)) => child
                                .call_function(FunctionId(id), vals)
                                .unwrap_or(FidanValue::Nothing),
                            _ => FidanValue::Nothing,
                        },
                    }
                });
                frame.store(dest, FidanValue::Pending(pending));
            }

            Instr::JoinAll { handles } => {
                // Wait for every handle in declaration order.  Results are
                // written back into the same local slots (Pending → resolved).
                // Task failures are collected and reported together as R9001.
                let mut failures: Vec<String> = Vec::new();
                let resolved: Vec<(LocalId, FidanValue)> = handles
                    .iter()
                    .map(|&local| {
                        let val = frame.load(local);
                        let result = if let FidanValue::Pending(p) = &val {
                            match p.try_join() {
                                Ok(v) => v,
                                Err(e) => {
                                    failures.push(e);
                                    FidanValue::Nothing
                                }
                            }
                        } else {
                            val // already resolved (sequential fallback)
                        };
                        (local, result)
                    })
                    .collect();
                for (local, result) in resolved {
                    frame.store(local, result);
                }
                if !failures.is_empty() {
                    let n = failures.len();
                    let pl = if n == 1 { "" } else { "s" };
                    let details = failures
                        .iter()
                        .map(|f| format!("  {}", f))
                        .collect::<Vec<_>>()
                        .join("\n");
                    return Err(MirSignal::ParallelFail(format!(
                        "{} task{} failed in `parallel` block:\n{}",
                        n, pl, details
                    )));
                }
            }

            Instr::AwaitPending { dest, handle } => {
                let val = self.eval_operand(&handle, frame);
                let resolved = if let FidanValue::Pending(p) = &val {
                    p.join()
                } else {
                    val
                };
                frame.store(dest, resolved);
            }

            Instr::ParallelIter {
                collection,
                body_fn,
                closure_args,
            } => {
                let coll = self.eval_operand(&collection, frame);
                // Capture the shared "environment" args once; per-item bundles
                // below each include a capture-clone of these.
                let env_caps: Vec<ParallelCapture> = closure_args
                    .iter()
                    .map(|a| ParallelCapture(self.eval_operand(a, frame).parallel_capture()))
                    .collect();

                if let FidanValue::List(list_ref) = coll {
                    // Snapshot the list before spawning (immutable during iter).
                    let items: Vec<FidanValue> = list_ref.borrow().iter().cloned().collect();

                    // Collect the first error from any iteration.
                    // All threads are joined when the scope exits, so it is safe
                    // to read this slot immediately after the scope.
                    let first_err: std::sync::Arc<std::sync::Mutex<Option<String>>> =
                        std::sync::Arc::new(std::sync::Mutex::new(None));

                    std::thread::scope(|s| {
                        for item in &items {
                            // Build per-thread bundle using whole-struct captures.
                            let mut caps = vec![ParallelCapture(item.parallel_capture())];
                            caps.extend(
                                env_caps
                                    .iter()
                                    .map(|c| ParallelCapture(c.0.parallel_capture())),
                            );
                            let bundle = ParallelArgs::from_captures(caps);
                            let mut child = self.clone_for_thread();
                            let err_slot = std::sync::Arc::clone(&first_err);
                            s.spawn(move || {
                                if let Err(sig) = child.call_function(body_fn, bundle.into_vec()) {
                                    let msg = match sig {
                                        MirSignal::Panic(m) => m,
                                        MirSignal::Throw(v) => {
                                            format!(
                                                "uncaught throw in parallel iteration: {}",
                                                crate::builtins::display(&v)
                                            )
                                        }
                                        MirSignal::ParallelFail(m) => m,
                                    };
                                    let mut slot = err_slot.lock().unwrap();
                                    if slot.is_none() {
                                        *slot = Some(msg);
                                    }
                                }
                            });
                        }
                        // Scope joins all threads implicitly on drop.
                    });

                    // Propagate the first iteration error (if any).
                    if let Some(msg) = first_err.lock().unwrap().take() {
                        return Err(MirSignal::Panic(msg));
                    }
                }
            }
        }
        Ok(None)
    }

    // ── Rvalue evaluation ─────────────────────────────────────────────────────

    fn eval_rvalue(&mut self, rhs: Rvalue, frame: &mut CallFrame) -> Result<FidanValue, MirSignal> {
        match rhs {
            Rvalue::Use(op) => Ok(self.eval_operand(&op, frame)),
            Rvalue::Literal(lit) => Ok(mir_lit_to_value(lit)),
            Rvalue::Binary { op, lhs, rhs } => {
                let l = self.eval_operand(&lhs, frame);
                let r = self.eval_operand(&rhs, frame);
                eval_binary(op, l, r)
            }
            Rvalue::Unary { op, operand } => {
                let v = self.eval_operand(&operand, frame);
                eval_unary(op, v)
            }
            Rvalue::NullCoalesce { lhs, rhs } => {
                let l = self.eval_operand(&lhs, frame);
                if l.is_nothing() {
                    Ok(self.eval_operand(&rhs, frame))
                } else {
                    Ok(l)
                }
            }
            Rvalue::Call { callee, args } => {
                let arg_vals: Vec<FidanValue> =
                    args.iter().map(|a| self.eval_operand(a, frame)).collect();
                self.dispatch_call(&callee, arg_vals, frame)
            }
            Rvalue::Construct { ty, fields } => {
                let field_vals: Vec<(Symbol, FidanValue)> = fields
                    .iter()
                    .map(|(sym, op)| (*sym, self.eval_operand(op, frame)))
                    .collect();
                self.construct_object(ty, field_vals)
            }
            Rvalue::List(elems) => {
                let mut list = FidanList::new();
                for e in &elems {
                    list.append(self.eval_operand(e, frame));
                }
                Ok(FidanValue::List(OwnedRef::new(list)))
            }
            Rvalue::Dict(pairs) => {
                let mut dict = FidanDict::new();
                for (k, v) in &pairs {
                    let key = self.eval_operand(k, frame);
                    let val = self.eval_operand(v, frame);
                    let key_str = FidanString::new(&builtins::display(&key));
                    dict.insert(key_str, val);
                }
                Ok(FidanValue::Dict(OwnedRef::new(dict)))
            }
            Rvalue::Tuple(elems) => {
                let items: Vec<FidanValue> =
                    elems.iter().map(|e| self.eval_operand(e, frame)).collect();
                Ok(FidanValue::Tuple(items))
            }
            Rvalue::StringInterp(parts) => {
                let mut s = String::new();
                for part in &parts {
                    match part {
                        MirStringPart::Literal(lit) => s.push_str(lit),
                        MirStringPart::Operand(op) => {
                            let v = self.eval_operand(op, frame);
                            s.push_str(&builtins::display(&v));
                        }
                    }
                }
                Ok(FidanValue::String(FidanString::new(&s)))
            }
            Rvalue::CatchException => Ok(frame
                .current_exception
                .take()
                .unwrap_or(FidanValue::Nothing)),
        }
    }

    // ── Operand evaluation ────────────────────────────────────────────────────

    fn eval_operand(&self, op: &Operand, frame: &CallFrame) -> FidanValue {
        match op {
            Operand::Local(l) => frame.load(*l),
            Operand::Const(lit) => mir_lit_to_value(lit.clone()),
        }
    }

    // ── Call dispatch ─────────────────────────────────────────────────────────

    fn dispatch_call(
        &mut self,
        callee: &Callee,
        args: Vec<FidanValue>,
        frame: &mut CallFrame,
    ) -> Result<FidanValue, MirSignal> {
        match callee {
            Callee::Fn(fn_id) => self.call_function(*fn_id, args),
            Callee::Builtin(sym) => {
                let name: Arc<str> = self.sym_str(*sym);
                // Check if this is a free-imported stdlib function (e.g. `use std.io.{readFile}`).
                if let Some(module) = self.stdlib_free_fns.get(&name).cloned() {
                    return self.dispatch_stdlib_call(&module, &name, args);
                }
                // Constructor builtins (e.g. `Shared(val)`) take priority; then
                // true language builtins (print, input, len, type conversions, math).
                // String/list/dict receiver methods are NOT free functions and must
                // be invoked via `receiver.method()` — they live in call_bootstrap_method.
                builtins::call_builtin_constructor(&name, args.clone())
                    .or_else(|| builtins::call_builtin(&name, args))
                    .ok_or_else(|| MirSignal::Panic(format!("unknown builtin `{}`", name)))
            }
            Callee::Method { receiver, method } => {
                let recv = self.eval_operand(receiver, frame);
                let method_name = self.sym_str(*method);
                self.dispatch_method(recv, &method_name, args)
            }
            Callee::Dynamic(op) => {
                let v = self.eval_operand(op, frame);
                match v {
                    FidanValue::Function(RuntimeFnId(id)) => {
                        self.call_function(FunctionId(id), args)
                    }
                    FidanValue::StdlibFn(ref module, ref name) => {
                        let m = Arc::clone(module);
                        let n = Arc::clone(name);
                        self.dispatch_stdlib_call(&m, &n, args)
                    }
                    _ => Err(MirSignal::Panic(format!(
                        "cannot call value of type `{}`",
                        v.type_name()
                    ))),
                }
            }
        }
    }

    fn dispatch_method(
        &mut self,
        receiver: FidanValue,
        method: &str,
        args: Vec<FidanValue>,
    ) -> Result<FidanValue, MirSignal> {
        // Stdlib namespace dispatch: `io.readFile(...)`, `math.sin(...)`, etc.
        if let FidanValue::Namespace(ref module) = receiver {
            // O(1) lookup: is this a stdlib module or a user-defined namespace?
            if !self.stdlib_modules.contains(module.as_ref()) {
                let method_sym = self.interner.intern(method);
                if let Some(&fn_id) = self.user_fn_map.get(&method_sym) {
                    return self.call_function(fn_id, args);
                }
                return Err(MirSignal::Panic(format!(
                    "no function `{}` in user module `{}`",
                    method, module
                )));
            }
            return self.dispatch_stdlib_call(module, method, args);
        }
        // Shared<T> built-in methods.
        if let FidanValue::Shared(ref sr) = receiver {
            match method {
                "get" => return Ok(sr.0.lock().unwrap().clone()),
                "set" => {
                    let val = args.into_iter().next().unwrap_or(FidanValue::Nothing);
                    *sr.0.lock().unwrap() = val;
                    return Ok(FidanValue::Nothing);
                }
                _ => {}
            }
        }
        // Check user-defined methods on objects first.
        if let FidanValue::Object(ref obj_ref) = receiver {
            let class = obj_ref.borrow().class.clone();
            let method_sym = self.interner.intern(method);
            if let Some(RuntimeFnId(id)) = class.find_method(method_sym) {
                let mut fn_args = vec![receiver];
                fn_args.extend(args);
                return self.call_function(FunctionId(id), fn_args);
            }
        }
        // Fall through: bootstrap stdlib methods (pre-Phase 7 stdlib).
        crate::bootstrap::call_bootstrap_method(receiver, method, args)
            .ok_or_else(|| MirSignal::Panic(format!("no method `{}` found", method)))
    }

    // ── Stdlib dispatch ───────────────────────────────────────────────────────

    fn dispatch_stdlib_call(
        &mut self,
        module: &str,
        name: &str,
        args: Vec<FidanValue>,
    ) -> Result<FidanValue, MirSignal> {
        match fidan_stdlib::dispatch_stdlib(module, name, args) {
            Some(StdlibResult::Value(v)) => {
                // Check for test assertion failures encoded as `__test_fail__: msg`.
                if let FidanValue::String(ref s) = v {
                    let s_str = s.as_str();
                    if let Some(msg) = s_str.strip_prefix("__test_fail__: ") {
                        return Err(MirSignal::Panic(format!("assertion failed: {}", msg)));
                    }
                }
                Ok(v)
            }
            Some(StdlibResult::NeedsCallbackDispatch(op)) => self.exec_parallel_op(op),
            None => Err(MirSignal::Panic(format!(
                "no function `{}` in stdlib module `{}`",
                name, module
            ))),
        }
    }

    /// Execute a `ParallelOp` — iterate through the list serially (the MIR
    /// interpreter is single-threaded; true parallelism requires the Cranelift
    /// or LLVM backend).
    fn exec_parallel_op(&mut self, op: ParallelOp) -> Result<FidanValue, MirSignal> {
        match op {
            ParallelOp::Map { list, fn_id } => {
                let mut out = fidan_runtime::FidanList::new();
                for elem in list {
                    let v = self.call_function(FunctionId(fn_id.0), vec![elem])?;
                    out.append(v);
                }
                Ok(FidanValue::List(OwnedRef::new(out)))
            }
            ParallelOp::Filter { list, fn_id } => {
                let mut out = fidan_runtime::FidanList::new();
                for elem in list {
                    let keep = self.call_function(FunctionId(fn_id.0), vec![elem.clone()])?;
                    if keep.truthy() {
                        out.append(elem);
                    }
                }
                Ok(FidanValue::List(OwnedRef::new(out)))
            }
            ParallelOp::ForEach { list, fn_id } => {
                for elem in list {
                    self.call_function(FunctionId(fn_id.0), vec![elem])?;
                }
                Ok(FidanValue::Nothing)
            }
            ParallelOp::Reduce { list, init, fn_id } => {
                let mut acc = init;
                for elem in list {
                    acc = self.call_function(FunctionId(fn_id.0), vec![acc, elem])?;
                }
                Ok(acc)
            }
        }
    }

    // ── Object construction ───────────────────────────────────────────────────

    fn construct_object(
        &mut self,
        ty: Symbol,
        fields: Vec<(Symbol, FidanValue)>,
    ) -> Result<FidanValue, MirSignal> {
        let class = if let Some(c) = self.classes.get(&ty).cloned() {
            c
        } else {
            // Unknown class: build a minimal one from the provided fields.
            let field_defs: Vec<FieldDef> = fields
                .iter()
                .enumerate()
                .map(|(i, (sym, _))| FieldDef {
                    name: *sym,
                    index: i,
                })
                .collect();
            let name_str = self.interner.resolve(ty);
            let class = Arc::new(FidanClass {
                name: ty,
                name_str,
                parent: None,
                fields: field_defs,
                methods: Default::default(),
            });
            self.classes.insert(ty, Arc::clone(&class));
            class
        };

        let mut obj = FidanObject::new(class);
        for (sym, val) in fields {
            obj.set_field(sym, val);
        }
        Ok(FidanValue::Object(OwnedRef::new(obj)))
    }

    // ── Field access ──────────────────────────────────────────────────────────

    fn get_field(&self, val: &FidanValue, field: Symbol) -> FidanValue {
        match val {
            FidanValue::Object(obj_ref) => obj_ref
                .borrow()
                .get_field(field)
                .cloned()
                .unwrap_or(FidanValue::Nothing),
            _ => FidanValue::Nothing,
        }
    }

    fn set_field(&self, val: &mut FidanValue, field: Symbol, new_val: FidanValue) {
        if let FidanValue::Object(obj_ref) = val {
            obj_ref.borrow_mut().set_field(field, new_val);
        }
    }

    // ── Indexing ──────────────────────────────────────────────────────────────

    fn index_get(&self, obj: FidanValue, idx: FidanValue) -> Result<FidanValue, MirSignal> {
        match (obj, idx) {
            (FidanValue::List(r), FidanValue::Integer(i)) => {
                let list = r.borrow();
                let len = list.len() as i64;
                let norm = if i < 0 { len + i } else { i };
                list.get(norm as usize)
                    .cloned()
                    .ok_or_else(|| MirSignal::Panic(format!("list index {} out of range", i)))
            }
            (FidanValue::Dict(r), key) => {
                let key_str = FidanString::new(&builtins::display(&key));
                Ok(r.borrow()
                    .get(&key_str)
                    .cloned()
                    .unwrap_or(FidanValue::Nothing))
            }
            (FidanValue::String(s), FidanValue::Integer(i)) => {
                let chars: Vec<char> = s.as_str().chars().collect();
                let len = chars.len() as i64;
                let norm = if i < 0 { len + i } else { i };
                chars
                    .get(norm as usize)
                    .map(|c| FidanValue::String(FidanString::new(&c.to_string())))
                    .ok_or_else(|| MirSignal::Panic(format!("string index {} out of range", i)))
            }
            (FidanValue::Tuple(items), FidanValue::Integer(i)) => items
                .into_iter()
                .nth(i as usize)
                .ok_or_else(|| MirSignal::Panic(format!("tuple index {} out of range", i))),
            (obj, idx) => Err(MirSignal::Panic(format!(
                "cannot index `{}` with `{}`",
                obj.type_name(),
                idx.type_name()
            ))),
        }
    }

    fn index_set(
        &self,
        obj: FidanValue,
        idx: FidanValue,
        val: FidanValue,
    ) -> Result<(), MirSignal> {
        match (obj, idx) {
            (FidanValue::List(r), FidanValue::Integer(i)) => {
                let norm = {
                    let list = r.borrow();
                    let len = list.len() as i64;
                    (if i < 0 { len + i } else { i }) as usize
                };
                let mut list = r.borrow_mut();
                if norm < list.len() {
                    list.set_at(norm, val);
                    Ok(())
                } else {
                    Err(MirSignal::Panic(format!("list index {} out of range", i)))
                }
            }
            (FidanValue::Dict(r), key) => {
                let key_str = FidanString::new(&builtins::display(&key));
                r.borrow_mut().insert(key_str, val);
                Ok(())
            }
            (obj, idx) => Err(MirSignal::Panic(format!(
                "cannot index-set `{}` with `{}`",
                obj.type_name(),
                idx.type_name()
            ))),
        }
    }
}

// ── Signals ───────────────────────────────────────────────────────────────────

enum MirSignal {
    /// Uncaught Throw propagation.
    Throw(FidanValue),
    /// Internal panic (interpreter bug or user-initiated panic).
    Panic(String),
    /// One or more tasks failed inside a `parallel` / `concurrent` block (R9001).
    /// Not catchable by `attempt / catch` — the parallel block itself fails.
    ParallelFail(String),
}

type MirResult = Result<FidanValue, MirSignal>;

// ── Arithmetic / logic helpers ────────────────────────────────────────────────

fn mir_lit_to_value(lit: MirLit) -> FidanValue {
    match lit {
        MirLit::Int(n) => FidanValue::Integer(n),
        MirLit::Float(f) => FidanValue::Float(f),
        MirLit::Bool(b) => FidanValue::Boolean(b),
        MirLit::Str(s) => FidanValue::String(FidanString::new(&s)),
        MirLit::Nothing => FidanValue::Nothing,
        MirLit::FunctionRef(id) => FidanValue::Function(RuntimeFnId(id)),
        MirLit::Namespace(m) => FidanValue::Namespace(Arc::from(m.as_str())),
        MirLit::StdlibFn { module, name } => {
            FidanValue::StdlibFn(Arc::from(module.as_str()), Arc::from(name.as_str()))
        }
    }
}

fn eval_binary(op: BinOp, l: FidanValue, r: FidanValue) -> Result<FidanValue, MirSignal> {
    use FidanValue::*;
    Ok(match (op, &l, &r) {
        // Arithmetic — integer
        (BinOp::Add, Integer(a), Integer(b)) => Integer(a + b),
        (BinOp::Sub, Integer(a), Integer(b)) => Integer(a - b),
        (BinOp::Mul, Integer(a), Integer(b)) => Integer(a * b),
        (BinOp::Div, Integer(a), Integer(b)) => {
            if *b == 0 {
                return Err(MirSignal::Panic("division by zero".into()));
            }
            Integer(a / b)
        }
        (BinOp::Rem, Integer(a), Integer(b)) => {
            if *b == 0 {
                return Err(MirSignal::Panic("modulo by zero".into()));
            }
            Integer(a % b)
        }
        (BinOp::Pow, Integer(a), Integer(b)) => Integer(a.wrapping_pow(*b as u32)),
        (BinOp::Pow, Float(a), Float(b)) => Float(a.powf(*b)),
        (BinOp::Pow, Integer(a), Float(b)) => Float((*a as f64).powf(*b)),
        (BinOp::Pow, Float(a), Integer(b)) => Float(a.powf(*b as f64)),
        // Arithmetic — float
        (BinOp::Add, Float(a), Float(b)) => Float(a + b),
        (BinOp::Sub, Float(a), Float(b)) => Float(a - b),
        (BinOp::Mul, Float(a), Float(b)) => Float(a * b),
        (BinOp::Div, Float(a), Float(b)) => Float(a / b),
        (BinOp::Rem, Float(a), Float(b)) => Float(a % b),
        // Mixed int/float
        (BinOp::Add, Integer(a), Float(b)) => Float(*a as f64 + b),
        (BinOp::Add, Float(a), Integer(b)) => Float(a + *b as f64),
        (BinOp::Sub, Integer(a), Float(b)) => Float(*a as f64 - b),
        (BinOp::Sub, Float(a), Integer(b)) => Float(a - *b as f64),
        (BinOp::Mul, Integer(a), Float(b)) => Float(*a as f64 * b),
        (BinOp::Mul, Float(a), Integer(b)) => Float(a * *b as f64),
        (BinOp::Div, Integer(a), Float(b)) => Float(*a as f64 / b),
        (BinOp::Div, Float(a), Integer(b)) => Float(a / *b as f64),
        // String concatenation
        (BinOp::Add, String(a), String(b)) => {
            let mut s = a.as_str().to_string();
            s.push_str(b.as_str());
            String(FidanString::new(&s))
        }
        // Comparison — integer
        (BinOp::Eq, Integer(a), Integer(b)) => Boolean(a == b),
        (BinOp::NotEq, Integer(a), Integer(b)) => Boolean(a != b),
        (BinOp::Lt, Integer(a), Integer(b)) => Boolean(a < b),
        (BinOp::LtEq, Integer(a), Integer(b)) => Boolean(a <= b),
        (BinOp::Gt, Integer(a), Integer(b)) => Boolean(a > b),
        (BinOp::GtEq, Integer(a), Integer(b)) => Boolean(a >= b),
        // Comparison — float
        (BinOp::Eq, Float(a), Float(b)) => Boolean(a == b),
        (BinOp::NotEq, Float(a), Float(b)) => Boolean(a != b),
        (BinOp::Lt, Float(a), Float(b)) => Boolean(a < b),
        (BinOp::LtEq, Float(a), Float(b)) => Boolean(a <= b),
        (BinOp::Gt, Float(a), Float(b)) => Boolean(a > b),
        (BinOp::GtEq, Float(a), Float(b)) => Boolean(a >= b),
        // Comparison — string
        (BinOp::Eq, String(a), String(b)) => Boolean(a.as_str() == b.as_str()),
        (BinOp::NotEq, String(a), String(b)) => Boolean(a.as_str() != b.as_str()),
        (BinOp::Lt, String(a), String(b)) => Boolean(a.as_str() < b.as_str()),
        (BinOp::LtEq, String(a), String(b)) => Boolean(a.as_str() <= b.as_str()),
        (BinOp::Gt, String(a), String(b)) => Boolean(a.as_str() > b.as_str()),
        (BinOp::GtEq, String(a), String(b)) => Boolean(a.as_str() >= b.as_str()),
        // Boolean logic
        (BinOp::And, Boolean(a), Boolean(b)) => Boolean(*a && *b),
        (BinOp::Or, Boolean(a), Boolean(b)) => Boolean(*a || *b),
        (BinOp::Eq, Boolean(a), Boolean(b)) => Boolean(a == b),
        (BinOp::NotEq, Boolean(a), Boolean(b)) => Boolean(a != b),
        // Nothing equality
        (BinOp::Eq, Nothing, Nothing) => Boolean(true),
        (BinOp::Eq, Nothing, _) => Boolean(false),
        (BinOp::Eq, _, Nothing) => Boolean(false),
        (BinOp::NotEq, Nothing, Nothing) => Boolean(false),
        (BinOp::NotEq, Nothing, _) => Boolean(true),
        (BinOp::NotEq, _, Nothing) => Boolean(true),
        // Bitwise
        (BinOp::BitAnd, Integer(a), Integer(b)) => Integer(a & b),
        (BinOp::BitOr, Integer(a), Integer(b)) => Integer(a | b),
        (BinOp::BitXor, Integer(a), Integer(b)) => Integer(a ^ b),
        (BinOp::Shl, Integer(a), Integer(b)) => Integer(a << (b & 63)),
        (BinOp::Shr, Integer(a), Integer(b)) => Integer(a >> (b & 63)),
        // Ranges produce a list of integers
        (BinOp::Range, Integer(a), Integer(b)) => {
            let mut list = FidanList::new();
            for n in *a..*b {
                list.append(Integer(n));
            }
            List(OwnedRef::new(list))
        }
        (BinOp::RangeInclusive, Integer(a), Integer(b)) => {
            let mut list = FidanList::new();
            for n in *a..=*b {
                list.append(Integer(n));
            }
            List(OwnedRef::new(list))
        }
        _ => {
            return Err(MirSignal::Panic(format!(
                "type error: `{:?}` on {} and {}",
                op,
                l.type_name(),
                r.type_name()
            )));
        }
    })
}

fn eval_unary(op: UnOp, v: FidanValue) -> Result<FidanValue, MirSignal> {
    use FidanValue::*;
    Ok(match (op, v) {
        (UnOp::Pos, v) => v,
        (UnOp::Neg, Integer(n)) => Integer(-n),
        (UnOp::Neg, Float(f)) => Float(-f),
        (UnOp::Not, Boolean(b)) => Boolean(!b),
        (op, v) => {
            return Err(MirSignal::Panic(format!(
                "type error: `{:?}` on {}",
                op,
                v.type_name()
            )));
        }
    })
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Run a `MirProgram` from its entry function.
pub fn run_mir(
    program: MirProgram,
    interner: Arc<SymbolInterner>,
    source_map: Arc<SourceMap>,
) -> Result<(), RunError> {
    let mut machine = MirMachine::new(Arc::new(program), interner, source_map);
    machine.run()
}
