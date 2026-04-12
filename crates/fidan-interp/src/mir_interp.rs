// fidan-interp/src/mir_interp.rs
//
// Phase 6: MIR interpreter.
//
// Executes a `MirProgram` by walking its SSA/CFG representation.
// All non-local control flow (exceptions) is handled via an explicit
// per-call-frame catch stack, mirroring the `PushCatch`/`PopCatch`
// instructions emitted by the MIR lowerer.

use std::collections::VecDeque;
use std::ffi::c_void;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

use fidan_ast::{BinOp, UnOp};
use fidan_config::{ReceiverBuiltinKind, infer_receiver_member};
use fidan_lexer::{Symbol, SymbolInterner};
use fidan_mir::{
    BlockId, Callee, FunctionId, GlobalId, Instr, LocalId, MirFunction, MirLit, MirObjectInfo,
    MirProgram, MirStringPart, MirTy, Operand, Rvalue, Terminator,
};
use fidan_runtime::{
    FidanClass, FidanDict, FidanList, FidanObject, FidanPending, FidanString, FidanValue, FieldDef,
    FunctionId as RuntimeFnId, OwnedRef, ParallelArgs, ParallelCapture, display as fidan_display,
};
use fidan_source::{SourceMap, Span};
use fidan_stdlib::{
    SandboxPolicy, SandboxViolation, StdlibResult, async_std, async_std::AsyncOp, module_exports,
    parallel::ParallelOp,
};
use rustc_hash::{FxHashMap, FxHashSet};

use crate::builtins;
use crate::externs;
use crate::profiler::{FnProfileEntry, FnProfileItem, ProfileReport};
use fidan_codegen_cranelift::{
    JitCompiler, JitFnEntry, JitRuntimeHooks, call_jit_fn, register_jit_runtime_hooks,
    with_jit_runtime_context,
};

// ── Public error types ────────────────────────────────────────────────────────

/// A single frame in a stack trace.
#[derive(Clone, Debug)]
pub struct TraceFrame {
    /// Function name with argument values: `inner(msg = "iteration 42")`
    pub label: String,
    /// Best-known source location for this frame.
    /// For the innermost frame this prefers the failing instruction span; for
    /// callers it remains the call site (`"file.fdn:2:5"`).
    pub location: Option<String>,
}

/// Error returned from [`MirMachine::run`] when an uncaught panic propagates to
/// the top level.
pub struct RunError {
    /// Diagnostic code used when rendering the error.
    pub code: fidan_diagnostics::DiagCode,
    /// Short description of the panic value shown in the error message.
    pub message: String,
    /// Call stack at the moment of the panic, **innermost frame first**.
    /// Empty when the panic originated outside any named function.
    pub trace: Vec<TraceFrame>,
}

fn canonical_receiver_method_name(
    receiver_kind: ReceiverBuiltinKind,
    method: &str,
) -> Option<&'static str> {
    infer_receiver_member(receiver_kind, method).map(|info| info.canonical_name)
}

fn catchable_signal_value(signal: MirSignal) -> Result<FidanValue, MirSignal> {
    match signal {
        MirSignal::Throw(value) => Ok(value),
        MirSignal::RuntimeError(code, message) => Ok(FidanValue::String(FidanString::new(
            &format!("error [{code}]: {message}"),
        ))),
        MirSignal::SandboxViolation(code, message) => Ok(FidanValue::String(FidanString::new(
            &format!("sandbox violation [{code}]: {message}"),
        ))),
        other => Err(other),
    }
}

fn route_signal_to_catch(
    frame: &mut CallFrame,
    signal: MirSignal,
) -> Result<Option<(BlockId, FidanValue)>, MirSignal> {
    let Some(catch_bb) = frame.catch_stack.pop() else {
        return Err(signal);
    };

    match catchable_signal_value(signal) {
        Ok(value) => Ok(Some((catch_bb, value))),
        Err(other) => {
            frame.catch_stack.push(catch_bb);
            Err(other)
        }
    }
}

fn instruction_trace_span(instr: &Instr) -> Option<Span> {
    match instr {
        Instr::Call { span, .. } => Some(*span),
        _ => None,
    }
}

// ── Object class table ────────────────────────────────────────────────────────

/// Build `Arc<FidanClass>` instances from `MirObjectInfo` metadata.
/// Parent classes are resolved recursively; cycles are silently broken.
fn build_class_table(
    objects: &[MirObjectInfo],
    interner: &SymbolInterner,
) -> FxHashMap<fidan_lexer::Symbol, Arc<FidanClass>> {
    let drop_sym = interner.intern("drop");
    let mut table: FxHashMap<fidan_lexer::Symbol, Arc<FidanClass>> = FxHashMap::default();

    // Build in the order objects appear (HIR outputs parents before children).
    for obj in objects {
        // Collect inherited fields from parent chain first, then own fields.
        // Use a HashSet for O(1) dedup instead of Vec::contains (which is O(n) per check).
        let mut seen: FxHashSet<fidan_lexer::Symbol> = FxHashSet::default();
        let mut all_field_names: Vec<fidan_lexer::Symbol> = Vec::new();
        if let Some(parent_sym) = obj.parent
            && let Some(parent_class) = table.get(&parent_sym)
        {
            for fd in &parent_class.fields {
                if seen.insert(fd.name) {
                    all_field_names.push(fd.name);
                }
            }
        }
        for &f in &obj.field_names {
            if seen.insert(f) {
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
        let field_index: FxHashMap<fidan_lexer::Symbol, usize> =
            field_defs.iter().map(|fd| (fd.name, fd.index)).collect();
        let mut method_map = FxHashMap::default();
        for (&sym, &fid) in &obj.methods {
            method_map.insert(sym, RuntimeFnId(fid.0));
        }
        let parent = obj.parent.and_then(|p| table.get(&p).cloned());

        // `has_drop_action`: true if the class itself or any ancestor defines `drop`.
        let own_has_drop = obj.methods.keys().any(|&sym| sym == drop_sym);
        let parent_has_drop = parent.as_ref().map(|p| p.has_drop_action).unwrap_or(false);

        let name_str = interner.resolve(obj.name);
        let class = Arc::new(FidanClass {
            name: obj.name,
            name_str,
            parent,
            fields: field_defs,
            field_index,
            methods: method_map,
            has_drop_action: own_has_drop || parent_has_drop,
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
}

enum DeferredTask {
    Concurrent {
        task_fn: FunctionId,
        args: Vec<FidanValue>,
        task_name: String,
    },
    StaticSpawn {
        task_fn: FunctionId,
        args: Vec<FidanValue>,
    },
    DynamicSpawn {
        method: Option<Symbol>,
        args: Vec<FidanValue>,
    },
    Ready {
        value: FidanValue,
    },
    Gather {
        values: Vec<FidanValue>,
    },
    WaitAny {
        values: Vec<FidanValue>,
    },
    Timeout {
        handle: FidanValue,
        ms: u64,
    },
}

enum DeferredTaskError {
    Signal(MirSignal),
    ConcurrentTask {
        task_name: String,
        signal: MirSignal,
    },
}

enum PendingTaskState {
    Queued(DeferredTask),
    Running,
    Ready(Result<FidanValue, DeferredTaskError>),
}

impl CallFrame {
    fn new(local_count: u32) -> Self {
        Self {
            locals: vec![FidanValue::Nothing; local_count as usize],
            catch_stack: vec![],
            current_exception: None,
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
    classes: FxHashMap<Symbol, Arc<FidanClass>>,
    /// Source map for resolving spans to file/line/col in stack traces.
    source_map: Arc<SourceMap>,
    /// Names + call-site spans of all currently executing functions, outermost first.
    /// Each entry is `(full_label, call_site_span)` where the span is where *this*
    /// function was called from (i.e. the `Instr::Call` span in the caller).
    /// Each entry is `(fn_id, call_site_span, raw_args)`.  The label string is
    /// built lazily — only when a panic trace is actually needed (error path).
    call_stack: Vec<(FunctionId, Option<Span>, Vec<FidanValue>)>,
    /// Span of the `Instr::Call` currently being dispatched — consumed by the
    /// next `call_function` invocation to annotate its stack frame.
    pending_call_span: Option<Span>,
    /// Best-known source span of the first uncaught signal currently bubbling
    /// up the stack. Used to annotate the innermost frame with the precise
    /// failing call location instead of the function entry callsite.
    panic_site_span: Option<Span>,
    /// Call stack snapshot at the point of the first uncaught panic/throw,
    /// innermost first.  Populated once and never overwritten.
    panic_trace: Vec<TraceFrame>,
    /// Maps free-imported function names (e.g. `readFile`) to their stdlib module
    /// (e.g. `"io"`).  Populated from `use std.io.{readFile}` declarations.
    stdlib_free_fns: Arc<FxHashMap<Arc<str>, Arc<str>>>,
    /// Set of stdlib module names/aliases known in this program (e.g. `"io"`, `"math"`).
    /// O(1) lookup used by `dispatch_method` to distinguish stdlib vs user namespaces.
    stdlib_modules: Arc<FxHashSet<Arc<str>>>,
    /// Set of namespace aliases that were re-exported (`export use mod`) — used by
    /// `get_field` for O(1) chaining lookup (e.g. `lib.math.sqrt`).
    reexported_namespaces: Arc<FxHashSet<Arc<str>>>,
    /// Maps merged free-function names to their `FunctionId`.
    /// Used for `use mymod` / `test2.add(...)` user-module namespace dispatch.
    user_fn_map: Arc<FxHashMap<Symbol, FunctionId>>,
    /// Module-level global variables, shared across all threads.
    /// Init function writes (single-threaded); all other accesses are reads unless
    /// the user program explicitly mutates a global at runtime.  `RwLock` allows
    /// concurrent reads (parallel tasks) while still supporting write access.
    globals: Rc<parking_lot::RwLock<Vec<FidanValue>>>,
    /// Frozen snapshot of globals taken after the init function completes.
    /// When set, `LoadGlobal` reads from this lock-free `Arc<[FidanValue]>` slice
    /// instead of acquiring the `RwLock` on every access — a measurable speedup
    /// for read-heavy test suites and programs with many global constants.
    frozen_globals: Option<Box<[FidanValue]>>,
    /// Snapshot of all interned strings taken once after parsing.
    /// Allows O(1) symbol → &str resolution with a single `Arc::clone` and
    /// NO `RwLock` acquisition — critical for the hot method-dispatch path.
    str_table: Arc<[Arc<str>]>,
    /// Accumulated test results when running in `fidan test` mode.
    #[allow(dead_code)]
    pub test_results: Vec<TestResult>,
    /// Per-function call counters (indexed by `FunctionId`).
    /// Incremented atomically on every call; shared across threads.
    call_counters: Arc<Vec<AtomicU32>>,
    /// JIT-compiled function entries, one slot per function.
    /// `None` → not yet compiled; `Some(entry)` → ready to use.
    jit_fns: Arc<parking_lot::RwLock<Vec<Option<JitFnEntry>>>>,
    /// `true` per slot once the JIT has successfully compiled that function.
    /// Checked with a bare `Acquire` load — no lock needed for the fast path.
    jit_flags: Arc<Vec<AtomicBool>>,
    /// Number of calls after which the JIT kicks in (0 = disabled).
    jit_threshold: u32,
    /// Same-thread deferred tasks created by `spawn` / `concurrent`.
    pending_tasks: FxHashMap<u64, PendingTaskState>,
    pending_ready: VecDeque<u64>,
    next_pending_task_id: u64,
    /// Pre-interned symbol for the string `"drop"`.
    ///
    /// Stored here so `exec_drop_dispatch` can call `class.find_method(drop_sym)`
    /// in O(1) without re-interning the string on every drop site.
    drop_sym: Symbol,
    /// Per-function profiling counters (only populated in `fidan profile` mode).
    /// `None` in normal/JIT runs — zero overhead when not profiling.
    profile_data: Option<Arc<Vec<FnProfileEntry>>>,
    /// Lines read from stdin during this run (in call order).
    /// Populated only when `input()` is called while `replay_inputs` is empty.
    pub stdin_capture: Vec<String>,
    /// Pre-loaded stdin lines for a replay run.
    /// When non-empty, `input()` returns entries from this list in order
    /// instead of blocking on the real terminal.
    replay_inputs: Vec<String>,
    /// Next index into `replay_inputs` to return.
    replay_pos: usize,
    /// Zero-config sandbox policy (`None` = no sandboxing).
    sandbox: Option<Arc<SandboxPolicy>>,
}

/// A single test result recorded during `fidan test`.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct TestResult {
    pub name: String,
    pub passed: bool,
    pub message: Option<String>,
}

// SAFETY: `MirMachine` only crosses thread boundaries through `clone_for_thread`.
// That path rebuilds globals/frozen_globals as thread-local `parallel_capture()`
// copies before the machine is moved into the worker thread.
unsafe impl Send for MirMachine {}

unsafe fn jit_load_global_raw(ctx: *mut c_void, global_id: u32) -> i64 {
    let machine = unsafe { &mut *(ctx as *mut MirMachine) };
    let global = GlobalId(global_id);
    let ty = machine
        .program
        .globals
        .get(global_id as usize)
        .map(|g| &g.ty)
        .unwrap_or(&MirTy::Dynamic);
    let value = machine.global_value(global);
    machine.encode_jit_abi_value(&value, ty)
}

unsafe fn jit_store_global_raw(ctx: *mut c_void, global_id: u32, raw: i64) {
    let machine = unsafe { &mut *(ctx as *mut MirMachine) };
    let ty = machine
        .program
        .globals
        .get(global_id as usize)
        .map(|g| &g.ty)
        .unwrap_or(&MirTy::Dynamic)
        .clone();
    let value = machine.decode_jit_abi_value(raw, &ty);
    if let Some(slot) = machine.globals.write().get_mut(global_id as usize) {
        *slot = value;
    }
    machine.frozen_globals = None;
}

unsafe fn jit_call_fn_raw(ctx: *mut c_void, fn_id: u32, args_ptr: *const i64, arg_cnt: i64) -> i64 {
    let machine = unsafe { &mut *(ctx as *mut MirMachine) };
    let func_id = FunctionId(fn_id);
    let param_tys = machine
        .program
        .function(func_id)
        .params
        .iter()
        .map(|param| param.ty.clone())
        .collect::<Vec<_>>();
    let return_ty = machine.program.function(func_id).return_ty.clone();
    let raw_args = if arg_cnt <= 0 || args_ptr.is_null() {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(args_ptr, arg_cnt as usize) }
    };
    let args = raw_args
        .iter()
        .enumerate()
        .map(|(index, raw)| {
            let ty = param_tys.get(index).unwrap_or(&MirTy::Dynamic);
            machine.decode_jit_abi_value(*raw, ty)
        })
        .collect::<Vec<_>>();
    let result = machine
        .call_function(func_id, args)
        .unwrap_or(FidanValue::Nothing);
    machine.encode_jit_abi_value(&result, &return_ty)
}

impl MirMachine {
    fn register_jit_hooks() {
        register_jit_runtime_hooks(JitRuntimeHooks {
            load_global_raw: jit_load_global_raw,
            store_global_raw: jit_store_global_raw,
            call_fn_raw: jit_call_fn_raw,
        });
    }

    fn format_task_failure(task_name: &str, sig: MirSignal) -> String {
        match sig {
            MirSignal::Panic(m) => format!("task `{}` panicked: {}", task_name, m),
            MirSignal::Throw(v) => format!(
                "task `{}` threw an uncaught error: {}",
                task_name,
                crate::builtins::display(&v)
            ),
            MirSignal::ParallelFail(m) => format!("task `{}` failed: {}", task_name, m),
            MirSignal::RuntimeError(code, m) => {
                format!("task `{}` error [{code}]: {}", task_name, m)
            }
            MirSignal::SandboxViolation(code, m) => {
                format!("task `{}` sandbox violation [{code}]: {}", task_name, m)
            }
        }
    }

    pub fn new(
        program: Arc<MirProgram>,
        interner: Arc<SymbolInterner>,
        source_map: Arc<SourceMap>,
    ) -> Self {
        Self::register_jit_hooks();
        let classes = build_class_table(&program.objects, &interner);

        // Build the free-function import map from `use std.module.{fn}` declarations.
        let mut stdlib_free_fns: FxHashMap<Arc<str>, Arc<str>> = FxHashMap::default();
        // Build the stdlib module set: module names AND aliases, for O(1) is-stdlib lookup.
        let mut stdlib_modules: FxHashSet<Arc<str>> = FxHashSet::default();
        // Build the re-exported namespace set for O(1) get_field chaining lookup.
        let mut reexported_namespaces: FxHashSet<Arc<str>> = FxHashSet::default();
        for decl in &program.use_decls {
            // User-module re-export entries (`is_stdlib = false`) only exist to support
            // `get_field` chaining (e.g. `lib.math.sqrt`).  They must NOT be added to
            // `stdlib_modules` — doing so would mis-route dispatch through the stdlib
            // path instead of `user_fn_map`.
            if !decl.is_stdlib {
                continue;
            }
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

        // Populate reexported_namespaces: every use_decl with re_export=true
        // and no specific names becomes a re-exported namespace alias.
        for decl in &program.use_decls {
            if decl.re_export && decl.specific_names.is_none() {
                reexported_namespaces.insert(Arc::from(decl.alias.as_str()));
            }
        }

        let globals_count = program.globals.len();

        // Build user-module function name map: all merged free functions (non-init,
        // non-method). Methods have `this` as their first param; we detect that.
        let this_sym = interner.intern("this");
        let drop_sym = interner.intern("drop");
        let mut user_fn_map: FxHashMap<Symbol, FunctionId> = FxHashMap::default();
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

        // Snapshot all interned strings into an Arc-backed slice.
        // After parsing every symbol is already interned; indexing by sym.0
        // never contends on the RwLock inside SymbolInterner.
        let str_table: Arc<[Arc<str>]> = Arc::from(interner.snapshot().into_boxed_slice());

        // Build JIT counter infrastructure — must happen BEFORE `program` is moved.
        let fn_count = program.functions.len();
        let call_counters: Arc<Vec<AtomicU32>> = {
            let counters: Vec<AtomicU32> = (0..fn_count).map(|_| AtomicU32::new(0)).collect();
            // Pre-warm @precompile functions so they JIT on first call.
            for (i, f) in program.functions.iter().enumerate() {
                if f.precompile {
                    counters[i].store(499, Ordering::Relaxed);
                }
            }
            Arc::new(counters)
        };
        let jit_fns: Arc<parking_lot::RwLock<Vec<Option<JitFnEntry>>>> =
            Arc::new(parking_lot::RwLock::new(vec![None; fn_count]));
        let jit_flags: Arc<Vec<AtomicBool>> =
            Arc::new((0..fn_count).map(|_| AtomicBool::new(false)).collect());

        Self {
            program,
            interner,
            classes,
            source_map,
            call_stack: Vec::new(),
            pending_call_span: None,
            panic_site_span: None,
            panic_trace: Vec::new(),
            stdlib_free_fns: Arc::new(stdlib_free_fns),
            stdlib_modules: Arc::new(stdlib_modules),
            reexported_namespaces: Arc::new(reexported_namespaces),
            user_fn_map: Arc::new(user_fn_map),
            globals: Rc::new(parking_lot::RwLock::new(vec![
                FidanValue::Nothing;
                globals_count
            ])),
            frozen_globals: None,
            str_table,
            test_results: Vec::new(),
            call_counters,
            jit_fns,
            jit_flags,
            jit_threshold: 500,
            pending_tasks: FxHashMap::default(),
            pending_ready: VecDeque::new(),
            next_pending_task_id: 1,
            drop_sym,
            profile_data: None,
            stdin_capture: Vec::new(),
            replay_inputs: Vec::new(),
            replay_pos: 0,
            sandbox: None,
        }
    }

    fn global_value(&self, global: GlobalId) -> FidanValue {
        if let Some(ref frozen) = self.frozen_globals {
            frozen
                .get(global.0 as usize)
                .cloned()
                .unwrap_or(FidanValue::Nothing)
        } else {
            self.globals
                .read()
                .get(global.0 as usize)
                .cloned()
                .unwrap_or(FidanValue::Nothing)
        }
    }

    fn encode_jit_abi_value(&self, value: &FidanValue, ty: &MirTy) -> i64 {
        fidan_codegen_cranelift::encode_jit_abi_value(value, ty)
    }

    fn decode_jit_abi_value(&self, raw: i64, ty: &MirTy) -> FidanValue {
        fidan_codegen_cranelift::decode_jit_abi_value(raw, ty)
    }

    fn cleanup_frame_locals(&mut self, frame: &mut CallFrame) -> Result<(), MirSignal> {
        // Make function-exit lifetime semantics explicit so weak/shared handles
        // observe collection before control returns to the caller.
        for idx in (0..frame.locals.len()).rev() {
            let local = LocalId(idx as u32);
            self.exec_drop_dispatch(local, frame)?;
            if let Some(slot) = frame.locals.get_mut(idx) {
                *slot = FidanValue::Nothing;
            }
        }
        Ok(())
    }

    fn format_trace_location(&self, span: Option<Span>) -> Option<String> {
        span.map(|s| {
            let file = self.source_map.get(s.file);
            let (line, col) = file.line_col(s.start);
            format!("{}:{}:{}", file.name, line, col)
        })
    }

    fn prepare_call_args(
        &self,
        func: &MirFunction,
        args: &[FidanValue],
    ) -> Result<Vec<FidanValue>, MirSignal> {
        if args.len() > func.params.len() {
            let fn_name_s = self.sym_str(func.name).to_string();
            let expected = func.params.len();
            let got = args.len();
            return Err(MirSignal::Panic(format!(
                "too many arguments to `{fn_name_s}`: expected {expected}, got {got}"
            )));
        }

        let mut prepared = Vec::with_capacity(func.params.len());
        for (i, param) in func.params.iter().enumerate() {
            let val = args.get(i).cloned().unwrap_or(FidanValue::Nothing);
            let val = if matches!(val, FidanValue::Nothing) {
                if let Some(ref lit) = param.default {
                    mir_lit_to_value(lit)
                } else {
                    val
                }
            } else {
                val
            };
            if param.certain && matches!(val, FidanValue::Nothing) {
                let pname = self.sym_str(param.name);
                return Err(MirSignal::Panic(format!(
                    "certain parameter `{pname}` cannot be nothing"
                )));
            }
            prepared.push(val);
        }
        Ok(prepared)
    }

    /// Create a lightweight clone of this machine for use on a parallel thread.
    ///
    /// `Arc` fields (`program`, `interner`) are bumped by a single atomic
    /// refcount.  The `classes` map clones its `Arc<FidanClass>` pointers —
    /// O(n-classes), but class tables are small.
    fn clone_for_thread(&self) -> MirMachine {
        let thread_globals: Vec<FidanValue> = if let Some(ref frozen) = self.frozen_globals {
            frozen.iter().map(FidanValue::parallel_capture).collect()
        } else {
            self.globals
                .read()
                .iter()
                .map(FidanValue::parallel_capture)
                .collect()
        };
        MirMachine {
            program: Arc::clone(&self.program),
            interner: Arc::clone(&self.interner),
            classes: self.classes.clone(),
            source_map: Arc::clone(&self.source_map),
            call_stack: Vec::new(),
            pending_call_span: None,
            panic_site_span: None,
            panic_trace: Vec::new(),
            stdlib_free_fns: self.stdlib_free_fns.clone(),
            stdlib_modules: self.stdlib_modules.clone(),
            reexported_namespaces: self.reexported_namespaces.clone(),
            user_fn_map: self.user_fn_map.clone(),
            globals: Rc::new(parking_lot::RwLock::new(thread_globals.clone())),
            frozen_globals: Some(thread_globals.into_boxed_slice()),
            // One atomic bump on the outer Arc; all Arc<str> entries are shared.
            str_table: Arc::clone(&self.str_table),
            test_results: Vec::new(),
            call_counters: Arc::clone(&self.call_counters),
            jit_fns: Arc::clone(&self.jit_fns),
            jit_flags: Arc::clone(&self.jit_flags),
            jit_threshold: self.jit_threshold,
            pending_tasks: FxHashMap::default(),
            pending_ready: VecDeque::new(),
            next_pending_task_id: 1,
            drop_sym: self.drop_sym,
            profile_data: self.profile_data.as_ref().map(Arc::clone),
            // Parallel threads get a fresh capture buffer.
            // Replay inputs are inherited so forked subtasks can replay too,
            // but each thread starts from a fresh position.
            stdin_capture: Vec::new(),
            replay_inputs: self.replay_inputs.clone(),
            replay_pos: self.replay_pos,
            sandbox: self.sandbox.as_ref().map(Arc::clone),
        }
    }

    /// Activate sandbox policy on this machine.  Must be called before [`run`].
    pub fn set_sandbox(&mut self, policy: SandboxPolicy) {
        self.sandbox = Some(Arc::new(policy));
    }

    /// Enable per-function profiling.  Must be called before [`run`].
    ///
    /// Allocates one `FnProfileEntry` per function (atomic counters, zero cost
    /// when not profiling).  The JIT is automatically disabled when this is
    /// active so that every call passes through the interpreter timing hooks.
    pub fn enable_profiling(&mut self) {
        let fn_count = self.program.functions.len();
        let entries: Vec<FnProfileEntry> =
            (0..fn_count).map(|_| FnProfileEntry::default()).collect();
        self.profile_data = Some(Arc::new(entries));
    }

    /// Freeze the current global values into a lock-free boxed slice snapshot.
    ///
    /// Call this immediately after the init function (`run()`) returns so that
    /// all subsequent `LoadGlobal` instructions bypass the `RwLock` entirely.
    /// Child machines spawned via `clone_for_thread` inherit the snapshot
    /// automatically.
    pub fn freeze_globals(&mut self) {
        let snapshot: Box<[FidanValue]> = self.globals.read().clone().into_boxed_slice();
        self.frozen_globals = Some(snapshot);
    }

    /// Build a [`ProfileReport`] from the accumulated profiling counters.
    ///
    /// `fn_names` must be pre-collected before calling `run()` (one name per
    /// function, in `FunctionId` order).  `total_ns` is the wall time of the
    /// full program run.
    pub fn take_profile_report(
        &self,
        fn_names: &[String],
        program_name: &str,
        total_ns: u64,
    ) -> Option<ProfileReport> {
        let pd = self.profile_data.as_ref()?;

        let total_ms = total_ns as f64 / 1_000_000.0;
        let mut items: Vec<FnProfileItem> = pd
            .iter()
            .enumerate()
            .filter_map(|(i, entry)| {
                // Skip the init function (FunctionId 0) — it is not user-visible.
                if i == 0 {
                    return None;
                }
                let call_count = entry.call_count.load(Ordering::Relaxed);
                if call_count == 0 {
                    return None;
                }
                let fn_ns = entry.total_ns.load(Ordering::Relaxed);
                let fn_ms = fn_ns as f64 / 1_000_000.0;
                let avg_ms = fn_ms / call_count as f64;
                let pct = if total_ms > 0.0 {
                    fn_ms / total_ms * 100.0
                } else {
                    0.0
                };
                let name = fn_names
                    .get(i)
                    .cloned()
                    .unwrap_or_else(|| format!("<fn#{i}>"));
                Some(FnProfileItem {
                    name,
                    call_count,
                    total_ms: fn_ms,
                    avg_ms,
                    pct,
                })
            })
            .collect();

        // Sort by total time descending.
        items.sort_by(|a, b| {
            b.total_ms
                .partial_cmp(&a.total_ms)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        Some(ProfileReport {
            program_name: program_name.to_string(),
            total_ms,
            hot_paths: items,
        })
    }

    /// Resolve a `Symbol` to its string.
    ///
    /// Uses the pre-snapshotted `str_table` — O(1) array index + single
    /// `Arc::clone`, NO `RwLock` acquisition.  All symbols are stable by
    /// the time the interpreter runs (parsing is complete).
    #[inline]
    /// Override the JIT call-count threshold.
    /// Pass `0` to disable the JIT entirely.
    pub fn set_jit_threshold(&mut self, threshold: u32) {
        self.jit_threshold = threshold;
        // Re-pre-warm precompile functions based on the new threshold.
        if threshold > 0 {
            let pre_warm = threshold.saturating_sub(1);
            for (i, f) in self.program.functions.iter().enumerate() {
                if f.precompile {
                    self.call_counters[i].store(pre_warm, Ordering::Relaxed);
                }
            }
        }
    }

    fn queue_pending_task(&mut self, task: DeferredTask) -> FidanValue {
        let id = self.next_pending_task_id;
        self.next_pending_task_id += 1;
        self.pending_tasks
            .insert(id, PendingTaskState::Queued(task));
        self.pending_ready.push_back(id);
        FidanValue::PendingTask(id)
    }

    fn try_take_same_thread_pending_now(
        &mut self,
        id: u64,
    ) -> Option<Result<FidanValue, DeferredTaskError>> {
        match self.pending_tasks.remove(&id) {
            Some(PendingTaskState::Ready(result)) => Some(result),
            Some(state) => {
                self.pending_tasks.insert(id, state);
                None
            }
            None => Some(Ok(FidanValue::Nothing)),
        }
    }

    fn resolve_async_value(&mut self, value: FidanValue) -> Result<FidanValue, DeferredTaskError> {
        match value {
            FidanValue::PendingTask(id) => self.resolve_same_thread_pending(id),
            FidanValue::Pending(pending) => pending
                .try_join()
                .map_err(|message| DeferredTaskError::Signal(MirSignal::Panic(message))),
            other => Ok(other),
        }
    }

    fn try_take_async_value_now(
        &mut self,
        value: &FidanValue,
    ) -> Option<Result<FidanValue, DeferredTaskError>> {
        match value {
            FidanValue::PendingTask(id) => self.try_take_same_thread_pending_now(*id),
            FidanValue::Pending(pending) => pending.try_take_ready().map(|result| {
                result.map_err(|message| DeferredTaskError::Signal(MirSignal::Panic(message)))
            }),
            other => Some(Ok(other.clone())),
        }
    }

    fn wait_any_async(&mut self, values: Vec<FidanValue>) -> Result<FidanValue, DeferredTaskError> {
        if values.is_empty() {
            return Ok(async_std::wait_any_result(-1, FidanValue::Nothing));
        }
        loop {
            for (index, value) in values.iter().enumerate() {
                if let Some(result) = self.try_take_async_value_now(value) {
                    return result
                        .map(|resolved| async_std::wait_any_result(index as i64, resolved));
                }
            }
            if self.run_next_same_thread_task() {
                continue;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn timeout_async(
        &mut self,
        handle: FidanValue,
        ms: u64,
    ) -> Result<FidanValue, DeferredTaskError> {
        let deadline = Instant::now()
            .checked_add(Duration::from_millis(ms))
            .unwrap_or_else(Instant::now);
        loop {
            if let Some(result) = self.try_take_async_value_now(&handle) {
                return result.map(|resolved| async_std::timeout_result(true, resolved));
            }
            if Instant::now() >= deadline {
                return Ok(async_std::timeout_result(false, FidanValue::Nothing));
            }
            if self.run_next_same_thread_task() {
                continue;
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            std::thread::sleep(remaining.min(Duration::from_millis(1)));
        }
    }

    fn run_deferred_task(&mut self, task: DeferredTask) -> Result<FidanValue, DeferredTaskError> {
        match task {
            DeferredTask::Concurrent {
                task_fn,
                args,
                task_name,
            } => self
                .call_function(task_fn, args)
                .map_err(|signal| DeferredTaskError::ConcurrentTask { task_name, signal }),
            DeferredTask::StaticSpawn { task_fn, args } => self
                .call_function(task_fn, args)
                .map_err(DeferredTaskError::Signal),
            DeferredTask::DynamicSpawn { method, args } => {
                let mut vals = args;
                if vals.is_empty() {
                    return Ok(FidanValue::Nothing);
                }
                let first = vals.remove(0);
                match method {
                    Some(sym) => self
                        .dispatch_method_sym(first, sym, vals)
                        .map_err(DeferredTaskError::Signal),
                    None => match first {
                        FidanValue::Function(RuntimeFnId(id)) => self
                            .call_function(FunctionId(id), vals)
                            .map_err(DeferredTaskError::Signal),
                        FidanValue::Closure {
                            fn_id: RuntimeFnId(id),
                            captured,
                        } => {
                            let mut full_args = captured;
                            full_args.extend(vals);
                            self.call_function(FunctionId(id), full_args)
                                .map_err(DeferredTaskError::Signal)
                        }
                        _ => Ok(FidanValue::Nothing),
                    },
                }
            }
            DeferredTask::Ready { value } => Ok(value),
            DeferredTask::Gather { values } => {
                let mut out = FidanList::new();
                for value in values {
                    out.append(self.resolve_async_value(value)?);
                }
                Ok(FidanValue::List(OwnedRef::new(out)))
            }
            DeferredTask::WaitAny { values } => self.wait_any_async(values),
            DeferredTask::Timeout { handle, ms } => self.timeout_async(handle, ms),
        }
    }

    fn run_next_same_thread_task(&mut self) -> bool {
        while let Some(id) = self.pending_ready.pop_front() {
            let Some(state) = self.pending_tasks.remove(&id) else {
                continue;
            };
            match state {
                PendingTaskState::Queued(task) => {
                    self.pending_tasks.insert(id, PendingTaskState::Running);
                    let result = self.run_deferred_task(task);
                    self.pending_tasks
                        .insert(id, PendingTaskState::Ready(result));
                    return true;
                }
                PendingTaskState::Running => {
                    self.pending_tasks.insert(id, PendingTaskState::Running);
                }
                PendingTaskState::Ready(result) => {
                    self.pending_tasks
                        .insert(id, PendingTaskState::Ready(result));
                }
            }
        }
        false
    }

    fn resolve_same_thread_pending(&mut self, id: u64) -> Result<FidanValue, DeferredTaskError> {
        loop {
            match self.pending_tasks.get(&id) {
                Some(PendingTaskState::Queued(_)) | Some(PendingTaskState::Running) => {
                    if !self.run_next_same_thread_task() {
                        return Err(DeferredTaskError::Signal(MirSignal::Panic(
                            "same-thread task scheduler deadlocked while awaiting a running task"
                                .to_string(),
                        )));
                    }
                }
                Some(PendingTaskState::Ready(_)) => {
                    let Some(PendingTaskState::Ready(result)) = self.pending_tasks.remove(&id)
                    else {
                        unreachable!("pending task state changed while resolving");
                    };
                    return result;
                }
                None => return Ok(FidanValue::Nothing),
            }
        }
    }

    fn sym_str(&self, sym: Symbol) -> Arc<str> {
        Arc::clone(&self.str_table[sym.0 as usize])
    }

    /// Pre-load stdin lines for a replay run.  Call before `run()`.
    pub fn set_replay_inputs(&mut self, inputs: Vec<String>) {
        self.replay_inputs = inputs;
        self.replay_pos = 0;
    }

    /// Return all stdin lines captured during this run (in call order).
    pub fn get_stdin_capture(&self) -> &[String] {
        &self.stdin_capture
    }

    // ── Entry point ──────────────────────────────────────────────────────────

    /// Execute the main (top-level init) function.
    pub fn run(&mut self) -> Result<(), RunError> {
        self.call_stack.clear();
        self.panic_trace.clear();

        // ── Startup: fire custom decorator calls in declaration order ─────────
        // Collect into an owned Vec first to avoid holding a borrow on
        // `self.program` while calling `call_function` (which needs &mut self).
        let decorator_dispatch: Vec<(FunctionId, Vec<FidanValue>)> = {
            let mut entries = Vec::new();
            for func in self.program.functions.iter() {
                if func.custom_decorators.is_empty() {
                    continue;
                }
                // Pass the function itself as the first argument — just like Python's
                // `@decorator` protocol.  The decorator receives a callable
                // `FidanValue::Function` it can store, call, or wrap.
                let fn_val = FidanValue::Function(RuntimeFnId(func.id.0));
                for (dec_fn_id, extra_args) in &func.custom_decorators {
                    let mut args: Vec<FidanValue> = Vec::with_capacity(extra_args.len() + 1);
                    args.push(fn_val.clone());
                    args.extend(extra_args.iter().map(mir_lit_to_value));
                    entries.push((*dec_fn_id, args));
                }
            }
            entries
        };
        for (dec_fn_id, args) in decorator_dispatch {
            match self.call_function(dec_fn_id, args) {
                Ok(_) => {}
                Err(MirSignal::Throw(v)) => {
                    return Err(RunError {
                        code: fidan_diagnostics::diag_code!("R1002"),
                        message: format!(
                            "decorator: unhandled exception: {}",
                            builtins::display(&v)
                        ),
                        trace: std::mem::take(&mut self.panic_trace),
                    });
                }
                Err(MirSignal::Panic(msg)) => {
                    return Err(RunError {
                        code: fidan_diagnostics::diag_code!("R0001"),
                        message: format!("decorator panicked: {msg}"),
                        trace: std::mem::take(&mut self.panic_trace),
                    });
                }
                Err(MirSignal::ParallelFail(msg)) => {
                    return Err(RunError {
                        code: fidan_diagnostics::diag_code!("R9001"),
                        message: msg,
                        trace: std::mem::take(&mut self.panic_trace),
                    });
                }
                Err(MirSignal::RuntimeError(code, msg)) => {
                    return Err(RunError {
                        code,
                        message: msg,
                        trace: std::mem::take(&mut self.panic_trace),
                    });
                }
                Err(MirSignal::SandboxViolation(code, msg)) => {
                    return Err(RunError {
                        code,
                        message: msg,
                        trace: std::mem::take(&mut self.panic_trace),
                    });
                }
            }
        }

        let entry = FunctionId(0);
        match self.call_function(entry, vec![]) {
            Ok(_) => Ok(()),
            Err(MirSignal::Throw(v)) => Err(RunError {
                code: fidan_diagnostics::diag_code!("R1002"),
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
            Err(MirSignal::RuntimeError(code, msg)) => Err(RunError {
                code,
                message: msg,
                trace: std::mem::take(&mut self.panic_trace),
            }),
            Err(MirSignal::SandboxViolation(code, msg)) => Err(RunError {
                code,
                message: msg,
                trace: std::mem::take(&mut self.panic_trace),
            }),
        }
    }

    // ── Function call ─────────────────────────────────────────────────────────

    fn call_function(&mut self, fn_id: FunctionId, args: Vec<FidanValue>) -> MirResult {
        let func = self.program.function(fn_id);
        let args = self.prepare_call_args(func, &args)?;

        if func.extern_decl.is_some() {
            if self.sandbox.is_some() {
                return Err(MirSignal::SandboxViolation(
                    fidan_diagnostics::diag_code!("R4003"),
                    format!(
                        "foreign call to `{}` is denied under --sandbox",
                        self.interner.resolve(func.name)
                    ),
                ));
            }
            return externs::call_extern(func, args).map_err(|msg| {
                MirSignal::RuntimeError(fidan_diagnostics::diag_code!("R0001"), msg)
            });
        }

        // ── JIT hot-path check ────────────────────────────────────────────────
        // NOTE: Profiling disables JIT (jit_threshold is set to 0 by
        // `run_mir_with_profile`) so every call enters the interpreter path
        // and the timing hooks below are always reachable.
        if self.jit_threshold > 0 {
            let idx = fn_id.0 as usize;
            // Fast-path: single atomic load — no lock acquired when not compiled.
            if self.jit_flags[idx].load(Ordering::Acquire) {
                let entry = {
                    let guard = self.jit_fns.read();
                    guard[idx].clone()
                };
                if let Some(entry) = entry
                    && entry.is_native()
                {
                    let self_ptr = self as *mut MirMachine as *mut c_void;
                    return Ok(with_jit_runtime_context(self_ptr, || {
                        call_jit_fn(&entry, &args)
                    }));
                }
            } else {
                let prev = self.call_counters[idx].fetch_add(1, Ordering::Relaxed);
                if prev == self.jit_threshold.saturating_sub(1) {
                    // Threshold reached — attempt JIT compilation.
                    let mut compiler = JitCompiler::new();
                    let entry = compiler.compile_function(func, &self.program, &self.interner);
                    let is_native = entry.is_native();
                    self.jit_fns.write()[idx] = Some(entry);
                    let _ = Box::leak(Box::new(compiler));
                    // Publish the compiled flag AFTER the function is stored.
                    self.jit_flags[idx].store(true, Ordering::Release);
                    // Dispatch immediately — including this very (compilation-trigger) call.
                    if is_native {
                        let entry = {
                            let guard = self.jit_fns.read();
                            guard[idx].clone()
                        };
                        if let Some(entry) = entry {
                            let self_ptr = self as *mut MirMachine as *mut c_void;
                            return Ok(with_jit_runtime_context(self_ptr, || {
                                call_jit_fn(&entry, &args)
                            }));
                        }
                    }
                }
            }
        }

        // ── Profiling: record call and start inclusive timer ─────────────────
        let profile_start = if let Some(ref pd) = self.profile_data {
            let idx = fn_id.0 as usize;
            if idx < pd.len() {
                pd[idx].call_count.fetch_add(1, Ordering::Relaxed);
                Some(std::time::Instant::now())
            } else {
                None
            }
        } else {
            None
        };

        let local_count = func.local_count;

        let mut frame = CallFrame::new(local_count);

        // Bind parameters — defer all string formatting to the error path only.
        // On the happy path we do ZERO formatting/allocation per argument.
        // For `optional` params with a default: if the caller passed `nothing`
        // (or omitted the arg entirely), substitute the compile-time default.
        for (i, param) in func.params.iter().enumerate() {
            let val = args.get(i).cloned().unwrap_or(FidanValue::Nothing);
            frame.store(param.local, val);
        }

        // Consume the call-site span set by exec_instr just before calling us.
        // Push raw fn_id + args into the call stack — the label is built lazily
        // only when a panic/throw actually fires and we need the trace.
        let call_site_span = self.pending_call_span.take();
        self.call_stack.push((fn_id, call_site_span, args));

        // Block-level execution starting at entry (BlockId(0)).
        let result = self.run_function(fn_id, &mut frame);
        let cleanup_result = self.cleanup_frame_locals(&mut frame);

        // Capture the trace at the innermost frame (only once — don't overwrite).
        // Exclude the module-level entry function (FunctionId(0)) from the trace
        // since it is not a user-visible named function.
        // Labels are formatted HERE — only on the error path.
        if result.is_err() && self.panic_trace.is_empty() {
            let innermost_index = self.call_stack.len().saturating_sub(1);
            let panic_site_span = self.panic_site_span;
            self.panic_trace = self
                .call_stack
                .iter()
                .enumerate()
                .filter(|(i, _)| *i > 0) // skip entry function at index 0
                .rev()
                .map(|(i, (call_fn_id, span, call_args))| {
                    let func = self.program.function(*call_fn_id);
                    let fn_name = self.sym_str(func.name).to_string();
                    let arg_parts: Vec<String> = func
                        .params
                        .iter()
                        .zip(call_args.iter())
                        .map(|(p, v)| {
                            let pname = self.sym_str(p.name);
                            let vdisplay = match v {
                                fidan_runtime::FidanValue::String(_) => {
                                    format!("{:?}", builtins::display(v))
                                }
                                _ => builtins::display(v),
                            };
                            format!("{pname} = {vdisplay}")
                        })
                        .collect();
                    let label = if arg_parts.is_empty() {
                        format!("{fn_name}()")
                    } else {
                        format!("{fn_name}({})", arg_parts.join(", "))
                    };
                    let effective_span = if i == innermost_index {
                        panic_site_span.or(*span)
                    } else {
                        *span
                    };
                    let location = self.format_trace_location(effective_span);
                    TraceFrame { label, location }
                })
                .collect();
            self.panic_site_span = None;
        }

        self.call_stack.pop();

        // ── Profiling: accumulate inclusive elapsed time ──────────────────────
        if let Some(start) = profile_start
            && let Some(ref pd) = self.profile_data
        {
            let idx = fn_id.0 as usize;
            if idx < pd.len() {
                let ns = start.elapsed().as_nanos() as u64;
                pd[idx].total_ns.fetch_add(ns, Ordering::Relaxed);
            }
        }

        match (result, cleanup_result) {
            (Ok(v), Ok(())) => Ok(v.unwrap_or(FidanValue::Nothing)),
            (Ok(_), Err(e)) => Err(e),
            (Err(e), _) => Err(e),
        }
    }

    fn run_function(
        &mut self,
        fn_id: FunctionId,
        frame: &mut CallFrame,
    ) -> Result<Option<FidanValue>, MirSignal> {
        // Clone the Arc so we can hold &instr borrows while still calling
        // &mut self methods.  Arc::clone = one atomic increment, near-free.
        let program = Arc::clone(&self.program);
        let mut bb_id = BlockId(0);
        let mut prev_bb: Option<BlockId> = None;

        'outer: loop {
            // Evaluate phi-nodes using `prev_bb`.
            // Most blocks have no phi nodes — skip the Vec allocation entirely.
            {
                let bb = program.function(fn_id).block(bb_id);
                if !bb.phis.is_empty() {
                    let phis: Vec<(LocalId, FidanValue)> = bb
                        .phis
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
                        .collect();
                    for (dest, val) in phis {
                        frame.store(dest, val);
                    }
                }
            }

            // Execute instructions — borrow &Instr from the Arc-backed program.
            // This eliminates the Instr::clone() that was previously required to
            // satisfy the borrow checker, removing Vec<Operand> heap allocations
            // from the hot dispatch path (critical for parallel scaling).
            let instr_count = program.function(fn_id).block(bb_id).instructions.len();

            for i in 0..instr_count {
                let instr = &program.function(fn_id).block(bb_id).instructions[i];
                let signal_span = instruction_trace_span(instr);
                match self.exec_instr(instr, frame) {
                    Ok(Some(ret)) => return Ok(Some(ret)),
                    Ok(None) => {}
                    Err(signal) => {
                        if self.panic_site_span.is_none() {
                            self.panic_site_span = signal_span;
                        }
                        match route_signal_to_catch(frame, signal) {
                            Ok(Some((catch_bb, value))) => {
                                self.panic_site_span = None;
                                frame.current_exception = Some(value);
                                prev_bb = Some(bb_id);
                                bb_id = catch_bb;
                                continue 'outer;
                            }
                            Ok(None) => {}
                            Err(e) => return Err(e),
                        }
                    }
                }
            }

            // Handle terminator.
            // Terminator has no Vec fields — clone is a cheap stack copy.
            let term = program.function(fn_id).block(bb_id).terminator.clone();
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
    /// Takes `&Instr` — no clone of Vec<Operand> fields in Instr::Call etc.
    fn exec_instr(
        &mut self,
        instr: &Instr,
        frame: &mut CallFrame,
    ) -> Result<Option<FidanValue>, MirSignal> {
        match instr {
            Instr::Assign { dest, rhs, .. } => {
                let val = self.eval_rvalue(rhs, frame)?;
                frame.store(*dest, val);
            }
            Instr::Call {
                dest,
                callee,
                args,
                span,
                ..
            } => {
                // Record the call-site span so call_function can attach it to the frame.
                self.pending_call_span = Some(*span);
                // `args` is `&Vec<Operand>` — .iter() already used, no Vec clone.
                let arg_vals: Vec<FidanValue> =
                    args.iter().map(|a| self.eval_operand(a, frame)).collect();
                let result = self.dispatch_call(callee, arg_vals, frame)?;
                if let Some(d) = dest {
                    frame.store(*d, result);
                }
            }
            Instr::SetField {
                object,
                field,
                value,
            } => {
                let mut obj_val = self.eval_operand(object, frame);
                let val = self.eval_operand(value, frame);
                self.set_field(&mut obj_val, *field, val);
                // Write back — the object operand should be a local.
                if let Operand::Local(l) = object {
                    frame.store(*l, obj_val);
                }
            }
            Instr::GetField {
                dest,
                object,
                field,
            } => {
                let obj_val = self.eval_operand(object, frame);
                let val = self.get_field(&obj_val, *field);
                frame.store(*dest, val);
            }
            Instr::GetIndex {
                dest,
                object,
                index,
            } => {
                let obj_val = self.eval_operand(object, frame);
                let idx_val = self.eval_operand(index, frame);
                let val = self.index_get(obj_val, idx_val)?;
                frame.store(*dest, val);
            }
            Instr::SetIndex {
                object,
                index,
                value,
            } => {
                let obj_val = self.eval_operand(object, frame);
                let idx_val = self.eval_operand(index, frame);
                let val = self.eval_operand(value, frame);
                self.index_set(obj_val, idx_val, val)?;
            }
            Instr::Drop { local } => {
                // RAII: if the value being dropped is the last live reference to an
                // object whose class defines a `drop` action, call it now — before
                // the Rc refcount actually reaches zero via the frame slot overwrite.
                self.exec_drop_dispatch(*local, frame)?;
                frame.store(*local, FidanValue::Nothing);
            }
            Instr::CertainCheck { operand, name } => {
                if matches!(self.eval_operand(operand, frame), FidanValue::Nothing) {
                    let pname = self.sym_str(*name);
                    return Err(MirSignal::Panic(format!(
                        "certain parameter `{pname}` cannot be nothing"
                    )));
                }
            }
            Instr::Nop => {}
            Instr::PushCatch(catch_bb) => {
                frame.catch_stack.push(*catch_bb);
            }
            Instr::PopCatch => {
                frame.catch_stack.pop();
            }
            // ── Module-level globals ──────────────────────────────────────────
            Instr::LoadGlobal { dest, global } => {
                // Fast path: read directly from the frozen, lock-free snapshot
                // when it is available (after the init function completes).
                let val = if let Some(ref frozen) = self.frozen_globals {
                    frozen
                        .get(global.0 as usize)
                        .cloned()
                        .unwrap_or(FidanValue::Nothing)
                } else {
                    self.globals
                        .read()
                        .get(global.0 as usize)
                        .cloned()
                        .unwrap_or(FidanValue::Nothing)
                };
                frame.store(*dest, val);
            }
            Instr::StoreGlobal { global, value } => {
                let val = self.eval_operand(value, frame);
                if let Some(slot) = self.globals.write().get_mut(global.0 as usize) {
                    *slot = val;
                }
                self.frozen_globals = None;
            }
            // ── Concurrency / Parallelism ─────────────────────────────────────
            //
            // `concurrent { task ... }` uses structured same-thread tasks:
            // tasks are queued in the current frame and resolved at `JoinAll`
            // / `await` without crossing thread boundaries.
            //
            // `parallel { task ... }` remains OS-thread backed.
            //
            // `spawn expr` / `spawn dynamic` are same-thread deferred tasks:
            // they do not execute eagerly and instead run when first awaited.
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
            } => {
                let task_fn = *task_fn;
                let task_name = {
                    let func = self.program.function(task_fn);
                    self.sym_str(func.name).to_string()
                };
                let task_args: Vec<FidanValue> =
                    args.iter().map(|a| self.eval_operand(a, frame)).collect();
                frame.store(
                    *handle,
                    self.queue_pending_task(DeferredTask::Concurrent {
                        task_fn,
                        args: task_args,
                        task_name,
                    }),
                );
            }

            Instr::SpawnParallel {
                handle,
                task_fn,
                args,
            } => {
                // Capture the task name while `self` is still in scope so that
                // failure messages can include the source-level task name.
                let task_fn = *task_fn; // Copy — capture by value into the spawn closure
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
                        .map_err(|sig| MirMachine::format_task_failure(&task_name, sig))
                });
                frame.store(*handle, FidanValue::Pending(pending));
            }

            Instr::SpawnExpr {
                dest,
                task_fn,
                args,
            } => {
                let task_args: Vec<FidanValue> =
                    args.iter().map(|a| self.eval_operand(a, frame)).collect();
                frame.store(
                    *dest,
                    self.queue_pending_task(DeferredTask::StaticSpawn {
                        task_fn: *task_fn,
                        args: task_args,
                    }),
                );
            }

            Instr::SpawnDynamic { dest, method, args } => {
                let task_args: Vec<FidanValue> =
                    args.iter().map(|a| self.eval_operand(a, frame)).collect();
                frame.store(
                    *dest,
                    self.queue_pending_task(DeferredTask::DynamicSpawn {
                        method: *method,
                        args: task_args,
                    }),
                );
            }

            Instr::JoinAll { handles } => {
                // Wait for every handle in declaration order.  Results are
                // written back into the same local slots (Pending → resolved).
                // Task failures are collected and reported together as R9001.
                let mut failures: Vec<String> = Vec::new();
                let mut block_kind = "parallel";
                let resolved: Vec<(LocalId, FidanValue)> = handles
                    .iter()
                    .map(|&local| {
                        let val = frame.load(local);
                        let result = match &val {
                            FidanValue::PendingTask(id) => match self
                                .resolve_same_thread_pending(*id)
                            {
                                Ok(v) => v,
                                Err(DeferredTaskError::ConcurrentTask { task_name, signal }) => {
                                    block_kind = "concurrent";
                                    failures.push(Self::format_task_failure(&task_name, signal));
                                    FidanValue::Nothing
                                }
                                Err(DeferredTaskError::Signal(signal)) => {
                                    failures.push(Self::format_task_failure("<task>", signal));
                                    FidanValue::Nothing
                                }
                            },
                            FidanValue::Pending(p) => match p.try_join() {
                                Ok(v) => v,
                                Err(e) => {
                                    failures.push(e);
                                    FidanValue::Nothing
                                }
                            },
                            _ => val, // already resolved (sequential fallback)
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
                        "{} task{} failed in `{}` block:\n{}",
                        n, pl, block_kind, details
                    )));
                }
            }

            Instr::AwaitPending { dest, handle } => {
                let val = self.eval_operand(handle, frame);
                let resolved = match &val {
                    FidanValue::PendingTask(id) => match self.resolve_same_thread_pending(*id) {
                        Ok(v) => v,
                        Err(DeferredTaskError::Signal(signal))
                        | Err(DeferredTaskError::ConcurrentTask { signal, .. }) => {
                            return Err(signal);
                        }
                    },
                    FidanValue::Pending(p) => match p.try_join() {
                        Ok(v) => v,
                        Err(message) => return Err(MirSignal::Panic(message)),
                    },
                    _ => val,
                };
                frame.store(*dest, resolved);
            }

            Instr::ParallelIter {
                collection,
                body_fn,
                closure_args,
            } => {
                let body_fn = *body_fn; // Copy
                let coll = self.eval_operand(collection, frame);
                // Capture the shared "environment" args once; per-item bundles
                // below each include a capture-clone of these.
                let env_caps: Vec<ParallelCapture> = closure_args
                    .iter()
                    .map(|a| ParallelCapture(self.eval_operand(a, frame).parallel_capture()))
                    .collect();

                let items = self.iterable_items_snapshot(coll);

                if let Some(items) = items {
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
                                    // body_fn is Copy
                                    let msg = match sig {
                                        MirSignal::Panic(m) => m,
                                        MirSignal::Throw(v) => {
                                            format!(
                                                "uncaught throw in parallel iteration: {}",
                                                crate::builtins::display(&v)
                                            )
                                        }
                                        MirSignal::ParallelFail(m) => m,
                                        MirSignal::RuntimeError(code, m) => {
                                            format!("error [{code}]: {m}")
                                        }
                                        MirSignal::SandboxViolation(code, m) => {
                                            format!("sandbox violation [{code}]: {m}")
                                        }
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

    // ── Drop / RAII dispatch ──────────────────────────────────────────────────

    /// Called at every `Instr::Drop` site.
    ///
    /// If the local holds an `Object` value AND:
    ///   (a) the class (or any ancestor) has a user-defined `drop` action, AND
    ///   (b) the `Rc` inside `OwnedRef` has `strong_count == 1` (this is the
    ///       last live reference — the frame slot is about to be overwritten),
    ///
    /// …then the `drop` action is called with `this = the object`.
    ///
    /// This is pure RAII — no GC, no tracing, just deterministic destructor
    /// dispatch triggered at the point the compiler already inserted `Drop`.
    fn exec_drop_dispatch(
        &mut self,
        local: LocalId,
        frame: &mut CallFrame,
    ) -> Result<(), MirSignal> {
        let val = frame.load(local);
        if let FidanValue::Object(ref obj_ref) = val {
            // Fast path: most classes have no drop action — bail immediately.
            let (class_has_drop, drop_fn) = {
                let obj = obj_ref.borrow();
                let class = &obj.class;
                if !class.has_drop_action {
                    return Ok(());
                }
                // Resolve the `drop` method FunctionId from the class hierarchy.
                let fn_id = class.find_method(self.drop_sym);
                (true, fn_id)
            };

            if class_has_drop {
                // Check strong_count: only the last owner triggers the destructor.
                // Any clone/alias of this OwnedRef shares the same Rc — if count > 1
                // another live reference exists and drop will be called by whoever
                // holds the final reference.
                // NOTE: `obj_ref.0` is the `Rc<RefCell<FidanObject>>`.
                //
                // strong_count includes the val binding above (+1) and the frame slot
                // (+1 from frame.load clone), so a "sole owner" shows count == 2 here.
                // We use == 2 as the "last owner" threshold.
                let is_last_owner = std::rc::Rc::strong_count(&obj_ref.0) == 2;
                if is_last_owner && let Some(RuntimeFnId(id)) = drop_fn {
                    // Call `drop` with only `this` — no other arguments.
                    // We consume `val` here so the drop body can access `this`.
                    self.call_function(FunctionId(id), vec![val])?;
                }
            }
        }
        Ok(())
    }

    // ── Rvalue evaluation ─────────────────────────────────────────────────────

    fn eval_rvalue(
        &mut self,
        rhs: &Rvalue,
        frame: &mut CallFrame,
    ) -> Result<FidanValue, MirSignal> {
        match rhs {
            Rvalue::Use(op) => Ok(self.eval_operand(op, frame)),
            Rvalue::Literal(lit) => Ok(mir_lit_to_value(lit)),
            Rvalue::Binary { op, lhs, rhs } => {
                let l = self.eval_operand(lhs, frame);
                let r = self.eval_operand(rhs, frame);
                eval_binary(*op, l, r)
            }
            Rvalue::Unary { op, operand } => {
                let v = self.eval_operand(operand, frame);
                eval_unary(*op, v)
            }
            Rvalue::NullCoalesce { lhs, rhs } => {
                let l = self.eval_operand(lhs, frame);
                if l.is_nothing() {
                    Ok(self.eval_operand(rhs, frame))
                } else {
                    Ok(l)
                }
            }
            Rvalue::Call { callee, args } => {
                let arg_vals: Vec<FidanValue> =
                    args.iter().map(|a| self.eval_operand(a, frame)).collect();
                self.dispatch_call(callee, arg_vals, frame)
            }
            Rvalue::Construct { ty, fields } => {
                let field_vals: Vec<(Symbol, FidanValue)> = fields
                    .iter()
                    .map(|(sym, op)| (*sym, self.eval_operand(op, frame)))
                    .collect();
                self.construct_object(*ty, field_vals)
            }
            Rvalue::List(elems) => {
                let mut list = FidanList::new();
                for e in elems {
                    list.append(self.eval_operand(e, frame));
                }
                Ok(FidanValue::List(OwnedRef::new(list)))
            }
            Rvalue::Dict(pairs) => {
                let mut dict = FidanDict::new();
                for (k, v) in pairs {
                    let key = self.eval_operand(k, frame);
                    let val = self.eval_operand(v, frame);
                    let _ = dict.insert(key, val);
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
                for part in parts {
                    match part {
                        MirStringPart::Literal(lit) => s.push_str(lit),
                        MirStringPart::Operand(op) => {
                            let v = self.eval_operand(op, frame);
                            fidan_runtime::display_into(&mut s, &v);
                        }
                    }
                }
                Ok(FidanValue::String(FidanString::new(&s)))
            }
            Rvalue::CatchException => Ok(frame
                .current_exception
                .take()
                .unwrap_or(FidanValue::Nothing)),

            Rvalue::MakeClosure { fn_id, captures } => {
                let captured: Vec<FidanValue> = captures
                    .iter()
                    .map(|op| self.eval_operand(op, frame))
                    .collect();
                Ok(FidanValue::Closure {
                    fn_id: RuntimeFnId(*fn_id),
                    captured,
                })
            }

            Rvalue::Slice {
                target,
                start,
                end,
                inclusive,
                step,
            } => {
                let tgt = self.eval_operand(target, frame);
                let s = start.as_ref().map(|o| self.eval_operand(o, frame));
                let e = end.as_ref().map(|o| self.eval_operand(o, frame));
                let step = step.as_ref().map(|o| self.eval_operand(o, frame));
                self.eval_slice(tgt, s, e, *inclusive, step)
            }

            Rvalue::ConstructEnum { tag, payload } => {
                let tag_str = self.sym_str(*tag);
                let payload_vals: Vec<FidanValue> = payload
                    .iter()
                    .map(|op| self.eval_operand(op, frame))
                    .collect();
                Ok(FidanValue::EnumVariant {
                    tag: tag_str,
                    payload: payload_vals,
                })
            }

            Rvalue::EnumTagCheck {
                value,
                expected_tag,
            } => {
                let v = self.eval_operand(value, frame);
                let expected = self.sym_str(*expected_tag);
                let matches = matches!(&v,
                    FidanValue::EnumVariant { tag, .. } if tag.as_ref() == expected.as_ref());
                Ok(FidanValue::Boolean(matches))
            }

            Rvalue::EnumPayload { value, index } => {
                let v = self.eval_operand(value, frame);
                match v {
                    FidanValue::EnumVariant { payload, .. } => Ok(payload
                        .into_iter()
                        .nth(*index)
                        .unwrap_or(FidanValue::Nothing)),
                    _ => Ok(FidanValue::Nothing),
                }
            }
        }
    }

    // ── Operand evaluation ────────────────────────────────────────────────────

    fn eval_operand(&self, op: &Operand, frame: &CallFrame) -> FidanValue {
        match op {
            Operand::Local(l) => frame.load(*l),
            Operand::Const(lit) => mir_lit_to_value(lit),
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
                if let Some(extern_fn) = self
                    .program
                    .functions
                    .iter()
                    .find(|f| f.name == *sym && f.extern_decl.is_some())
                {
                    return self.call_function(extern_fn.id, args);
                }
                let name: Arc<str> = self.sym_str(*sym);
                // Check if this is a free-imported stdlib function (e.g. `use std.io.{readFile}`).
                if let Some(module) = self.stdlib_free_fns.get(&name).cloned() {
                    return self.dispatch_stdlib_call(&module, &name, args);
                }
                // ── Test assertion builtins ───────────────────────────────
                // These must be dispatched before `call_builtin` because they
                // need to return `Err(MirSignal::Panic)` on failure, which
                // the `Option`-returning `call_builtin` cannot express.
                match name.as_ref() {
                    "assert" => {
                        let cond = args.into_iter().next().unwrap_or(FidanValue::Nothing);
                        return if cond.truthy() {
                            Ok(FidanValue::Nothing)
                        } else {
                            Err(MirSignal::Panic("assertion failed".to_string()))
                        };
                    }
                    "assert_eq" => {
                        let mut it = args.into_iter();
                        let a = it.next().unwrap_or(FidanValue::Nothing);
                        let b = it.next().unwrap_or(FidanValue::Nothing);
                        let equal = fidan_values_equal(&a, &b);
                        return if equal {
                            Ok(FidanValue::Nothing)
                        } else {
                            let da = builtins::display(&a);
                            let db = builtins::display(&b);
                            Err(MirSignal::Panic(format!(
                                "assertion failed: expected {da} == {db}"
                            )))
                        };
                    }
                    "assert_ne" => {
                        let mut it = args.into_iter();
                        let a = it.next().unwrap_or(FidanValue::Nothing);
                        let b = it.next().unwrap_or(FidanValue::Nothing);
                        let equal = fidan_values_equal(&a, &b);
                        return if !equal {
                            Ok(FidanValue::Nothing)
                        } else {
                            let da = builtins::display(&a);
                            let db = builtins::display(&b);
                            Err(MirSignal::Panic(format!(
                                "assertion failed: expected {da} != {db}"
                            )))
                        };
                    }
                    "input" => {
                        // Replay mode: return the next pre-recorded line.
                        if self.replay_pos < self.replay_inputs.len() {
                            let line = self.replay_inputs[self.replay_pos].clone();
                            self.replay_pos += 1;
                            return Ok(FidanValue::String(FidanString::new(&line)));
                        }
                        // Normal mode: delegate to the builtin (reads stdin) and capture.
                        let v = builtins::call_builtin("input", args)
                            .map_err(|err| MirSignal::RuntimeError(err.code, err.message))?
                            .unwrap_or(FidanValue::Nothing);
                        if let FidanValue::String(ref s) = v {
                            self.stdin_capture.push(s.as_str().to_string());
                        }
                        return Ok(v);
                    }
                    _ => {}
                }
                // Constructor builtins (e.g. `Shared(val)`) take priority; then
                // true language builtins (print, input, len, type conversions, math).
                // String/list/dict receiver methods are NOT free functions and must
                // be invoked via `receiver.method()` — they live in call_bootstrap_method.
                if let Some(value) = builtins::call_builtin_constructor(&name, args.clone())
                    .map_err(|err| MirSignal::RuntimeError(err.code, err.message))?
                {
                    return Ok(value);
                }
                if let Some(value) = builtins::call_builtin(&name, args)
                    .map_err(|err| MirSignal::RuntimeError(err.code, err.message))?
                {
                    return Ok(value);
                }
                Err(MirSignal::Panic(format!("unknown builtin `{}`", name)))
            }
            Callee::Method { receiver, method } => {
                let recv = self.eval_operand(receiver, frame);
                self.dispatch_method_sym(recv, *method, args)
            }
            Callee::Dynamic(op) => {
                let v = self.eval_operand(op, frame);
                match v {
                    FidanValue::Function(RuntimeFnId(id)) => {
                        self.call_function(FunctionId(id), args)
                    }
                    FidanValue::Closure {
                        fn_id: RuntimeFnId(id),
                        captured,
                    } => {
                        let mut full_args = captured.clone();
                        full_args.extend(args);
                        self.call_function(FunctionId(id), full_args)
                    }
                    FidanValue::StdlibFn(ref module, ref name) => {
                        let m = Arc::clone(module);
                        let n = Arc::clone(name);
                        self.dispatch_stdlib_call(&m, &n, args)
                    }
                    FidanValue::ClassType(ref class_name) => {
                        self.instantiate_class_value(class_name.as_ref(), args)
                    }
                    _ => Err(MirSignal::Panic(format!(
                        "cannot call value of type `{}`",
                        v.type_name()
                    ))),
                }
            }
        }
    }

    /// Fast-path method dispatch using a pre-resolved `Symbol`, avoiding the
    /// `&str → Symbol` re-intern round-trip (and its RwLock acquisition) that
    /// `dispatch_method` requires.  Called from `Callee::Method` and
    /// `Instr::SpawnDynamic`, both of which already have the `Symbol` from MIR.
    fn dispatch_method_sym(
        &mut self,
        receiver: FidanValue,
        method: Symbol,
        args: Vec<FidanValue>,
    ) -> Result<FidanValue, MirSignal> {
        // Enum payload variant construction: `Result.Ok(x)` → EnumVariant { tag: "Ok", payload: [x] }.
        if let FidanValue::EnumType(_) = &receiver {
            let tag = self.sym_str(method);
            return Ok(FidanValue::EnumVariant { tag, payload: args });
        }
        // Fast path: stdlib namespace dispatch — Symbol lookup, no re-intern.
        if let FidanValue::Namespace(ref module) = receiver {
            if !self.stdlib_modules.contains(module.as_ref()) {
                if let Some(&fn_id) = self.user_fn_map.get(&method) {
                    return self.call_function(fn_id, args);
                }
                let method_name = self.sym_str(method);
                return Err(MirSignal::Panic(format!(
                    "no function `{}` in user module `{}`",
                    method_name, module
                )));
            }
            let method_name = self.sym_str(method);
            return self.dispatch_stdlib_call(module, &method_name, args);
        }
        // Shared<T> built-in methods (small fixed set — resolve string lazily).
        if let FidanValue::Shared(ref sr) = receiver {
            let method_name = self.sym_str(method);
            match canonical_receiver_method_name(ReceiverBuiltinKind::Shared, &method_name) {
                Some("get") => return Ok(sr.0.lock().unwrap().clone()),
                Some("set") => {
                    let val = args.into_iter().next().unwrap_or(FidanValue::Nothing);
                    *sr.0.lock().unwrap() = val;
                    return Ok(FidanValue::Nothing);
                }
                Some("weak") => return Ok(FidanValue::WeakShared(sr.downgrade())),
                _ => {}
            }
        }
        if let FidanValue::WeakShared(ref ws) = receiver {
            let method_name = self.sym_str(method);
            match canonical_receiver_method_name(ReceiverBuiltinKind::WeakShared, &method_name) {
                Some("upgrade") => {
                    return Ok(ws
                        .upgrade()
                        .map(FidanValue::Shared)
                        .unwrap_or(FidanValue::Nothing));
                }
                Some("isAlive") => {
                    return Ok(FidanValue::Boolean(ws.is_alive()));
                }
                _ => {}
            }
        }
        // Fast path: user-defined object methods — Symbol lookup, no re-intern.
        if let FidanValue::Object(ref obj_ref) = receiver {
            let class = obj_ref.borrow().class.clone();
            if let Some(RuntimeFnId(id)) = class.find_method(method) {
                let mut fn_args = vec![receiver];
                fn_args.extend(args);
                return self.call_function(FunctionId(id), fn_args);
            }
        }
        // For list callbacks and bootstrap, resolve the string lazily (O(1) Arc clone).
        let method_name = self.sym_str(method);
        if let FidanValue::List(ref list_ref) = receiver
            && let Some(result) = self.dispatch_list_callbacks(
                list_ref,
                canonical_receiver_method_name(ReceiverBuiltinKind::List, &method_name)
                    .unwrap_or(method_name.as_ref()),
                args.clone(),
            )?
        {
            return Ok(result);
        }
        crate::bootstrap::call_bootstrap_method(receiver, &method_name, args)
            .ok_or_else(|| MirSignal::Panic(format!("no method `{}` found", method_name)))
    }

    /// Dispatch list receiver methods that require a callback (access to `call_function`).
    /// Returns `Ok(Some(v))` when handled, `Ok(None)` to fall through to bootstrap.
    fn dispatch_list_callbacks(
        &mut self,
        list_ref: &OwnedRef<FidanList>,
        method: &str,
        args: Vec<FidanValue>,
    ) -> Result<Option<FidanValue>, MirSignal> {
        match method {
            "forEach" => {
                let callback = args.into_iter().next();
                let items: Vec<FidanValue> = list_ref.borrow().iter().cloned().collect();
                match callback {
                    Some(FidanValue::Function(RuntimeFnId(id))) => {
                        for item in items {
                            self.call_function(FunctionId(id), vec![item])?;
                        }
                    }
                    Some(FidanValue::Closure {
                        fn_id: RuntimeFnId(id),
                        captured,
                    }) => {
                        for item in items {
                            let mut call_args = captured.clone();
                            call_args.push(item);
                            self.call_function(FunctionId(id), call_args)?;
                        }
                    }
                    _ => {}
                }
                Ok(Some(FidanValue::Nothing))
            }
            "firstWhere" => {
                let callback = args.into_iter().next();
                let items: Vec<FidanValue> = list_ref.borrow().iter().cloned().collect();
                let result = match callback {
                    Some(FidanValue::Function(RuntimeFnId(id))) => {
                        let mut found = None;
                        for item in items {
                            let r = self.call_function(FunctionId(id), vec![item.clone()])?;
                            if r.truthy() {
                                found = Some(item);
                                break;
                            }
                        }
                        found
                    }
                    Some(FidanValue::Closure {
                        fn_id: RuntimeFnId(id),
                        captured,
                    }) => {
                        let mut found = None;
                        for item in items {
                            let mut call_args = captured.clone();
                            call_args.push(item.clone());
                            let r = self.call_function(FunctionId(id), call_args)?;
                            if r.truthy() {
                                found = Some(item);
                                break;
                            }
                        }
                        found
                    }
                    _ => None,
                };
                Ok(Some(result.unwrap_or(FidanValue::Nothing)))
            }
            _ => Ok(None),
        }
    }

    // ── Stdlib dispatch ───────────────────────────────────────────────────────

    fn dispatch_stdlib_call(
        &mut self,
        module: &str,
        name: &str,
        args: Vec<FidanValue>,
    ) -> Result<FidanValue, MirSignal> {
        if module == "__builtin__" {
            if let Some(value) = builtins::call_builtin_constructor(name, args.clone())
                .map_err(|err| MirSignal::Panic(err.message))?
            {
                return Ok(value);
            }
            if let Some(value) =
                builtins::call_builtin(name, args).map_err(|err| MirSignal::Panic(err.message))?
            {
                return Ok(value);
            }
            return Err(MirSignal::Panic(format!("unknown builtin `{name}`")));
        }
        // Sandbox check: guard all `io` module calls before execution.
        // Check sandbox first (Option branch) so non-sandbox runs skip the
        // module string comparison entirely on every stdlib call.
        if let Some(ref policy) = self.sandbox
            && module == "io"
        {
            let string_args: Vec<&str> = args
                .iter()
                .filter_map(|value| {
                    if let FidanValue::String(s) = value {
                        Some(s.as_str())
                    } else {
                        None
                    }
                })
                .collect();
            if let Err(violation) = policy.check_io_call(name, &string_args) {
                let (code, msg) = match &violation {
                    SandboxViolation::ReadDenied { .. } => {
                        (fidan_diagnostics::diag_code!("R4001"), violation.message())
                    }
                    SandboxViolation::WriteDenied { .. } => {
                        (fidan_diagnostics::diag_code!("R4002"), violation.message())
                    }
                    SandboxViolation::EnvDenied { .. } => {
                        (fidan_diagnostics::diag_code!("R4003"), violation.message())
                    }
                };
                return Err(MirSignal::SandboxViolation(code, msg));
            }
        }
        match fidan_stdlib::dispatch_stdlib(module, name, args) {
            Some(Ok(StdlibResult::Value(v))) => {
                // Check for test assertion failures encoded as `__test_fail__: msg`.
                if let FidanValue::String(ref s) = v {
                    let s_str = s.as_str();
                    if let Some(msg) = s_str.strip_prefix("__test_fail__: ") {
                        return Err(MirSignal::Panic(format!("assertion failed: {}", msg)));
                    }
                }
                Ok(v)
            }
            Some(Ok(StdlibResult::NeedsAsyncDispatch(op))) => self.exec_async_op(op),
            Some(Ok(StdlibResult::NeedsCallbackDispatch(op))) => self.exec_parallel_op(op),
            Some(Err(err)) => Err(MirSignal::RuntimeError(err.code, err.message)),
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

    fn exec_async_op(&mut self, op: AsyncOp) -> Result<FidanValue, MirSignal> {
        match op {
            AsyncOp::Sleep { ms } => Ok(FidanValue::Pending(FidanPending::sleep(ms))),
            AsyncOp::Ready { value } => Ok(self.queue_pending_task(DeferredTask::Ready { value })),
            AsyncOp::Gather { values } => {
                Ok(self.queue_pending_task(DeferredTask::Gather { values }))
            }
            AsyncOp::WaitAny { values } => {
                Ok(self.queue_pending_task(DeferredTask::WaitAny { values }))
            }
            AsyncOp::Timeout { handle, ms } => {
                Ok(self.queue_pending_task(DeferredTask::Timeout { handle, ms }))
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
            let field_index: FxHashMap<fidan_lexer::Symbol, usize> =
                field_defs.iter().map(|fd| (fd.name, fd.index)).collect();
            let name_str = self.interner.resolve(ty);
            let class = Arc::new(FidanClass {
                name: ty,
                name_str,
                parent: None,
                fields: field_defs,
                field_index,
                methods: Default::default(),
                has_drop_action: false,
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

    fn instantiate_class_value(
        &mut self,
        class_name: &str,
        args: Vec<FidanValue>,
    ) -> Result<FidanValue, MirSignal> {
        let Some(obj_info) = self
            .program
            .objects
            .iter()
            .find(|obj| self.interner.resolve(obj.name).as_ref() == class_name)
        else {
            return Err(MirSignal::Panic(format!("unknown class `{class_name}`")));
        };

        let class = self.classes.get(&obj_info.name).cloned().ok_or_else(|| {
            MirSignal::Panic(format!("runtime class metadata missing for `{class_name}`"))
        })?;

        let object = FidanValue::Object(OwnedRef::new(FidanObject::new(class)));

        if let Some(init_fn) = obj_info.init_fn {
            let mut init_args = Vec::with_capacity(args.len() + 1);
            init_args.push(object.clone());
            init_args.extend(args);
            let _ = self.call_function(init_fn, init_args)?;
        }

        Ok(object)
    }

    // ── Field access ──────────────────────────────────────────────────────────

    fn get_field(&self, val: &FidanValue, field: Symbol) -> FidanValue {
        match val {
            FidanValue::Object(obj_ref) => obj_ref
                .borrow()
                .get_field(field)
                .cloned()
                .unwrap_or(FidanValue::Nothing),
            // `.name` on a first-class action value returns its declared name.
            FidanValue::Function(RuntimeFnId(id)) => {
                let field_name = self.sym_str(field);
                if field_name.as_ref() == "name"
                    && let Some(func) = self.program.functions.get(*id as usize)
                {
                    let name = self.sym_str(func.name);
                    return FidanValue::String(FidanString::new(name.as_ref()));
                }
                FidanValue::Nothing
            }
            FidanValue::Namespace(module) if self.stdlib_modules.contains(module.as_ref()) => {
                let field_name = self.sym_str(field);
                if module_exports(module.as_ref()).contains(&field_name.as_ref()) {
                    FidanValue::StdlibFn(Arc::clone(module), field_name)
                } else {
                    FidanValue::Nothing
                }
            }
            // User namespace field access: e.g. `test2.math` where `test2` is a
            // user module namespace.  Resolve the field to a stdlib namespace value
            // only when the field name is a re-exported namespace (i.e. the imported
            // file contains `export use std.X`).  If it is NOT re-exported, returns
            // Nothing so the caller gets a proper "no method found" error.
            FidanValue::Namespace(module) if !self.stdlib_modules.contains(module.as_ref()) => {
                let field_name = self.sym_str(field);
                // O(1) lookup: was this namespace re-exported by the imported module?
                // `reexported_namespaces` is built once at MirMachine::new().
                if self.reexported_namespaces.contains(field_name.as_ref()) {
                    FidanValue::Namespace(field_name)
                } else {
                    FidanValue::Nothing
                }
            }
            // Enum type field access: `Direction.North` → EnumVariant { tag: "North" }.
            FidanValue::EnumType(_) => {
                let field_name = self.sym_str(field);
                FidanValue::EnumVariant {
                    tag: field_name,
                    payload: vec![],
                }
            }
            _ => FidanValue::Nothing,
        }
    }

    fn set_field(&self, val: &mut FidanValue, field: Symbol, new_val: FidanValue) {
        if let FidanValue::Object(obj_ref) = val {
            obj_ref.borrow_mut().set_field(field, new_val);
        }
    }

    // ── Indexing ──────────────────────────────────────────────────────────────

    fn eval_slice(
        &self,
        tgt: FidanValue,
        start: Option<FidanValue>,
        end: Option<FidanValue>,
        inclusive: bool,
        step: Option<FidanValue>,
    ) -> Result<FidanValue, MirSignal> {
        // Extract step (default 1; must not be 0).
        let step_i = match step {
            Some(FidanValue::Integer(n)) => {
                if n == 0 {
                    return Err(MirSignal::Panic("slice step cannot be zero".to_string()));
                }
                n
            }
            Some(other) => {
                return Err(MirSignal::Panic(format!(
                    "slice step must be an integer, got `{}`",
                    other.type_name()
                )));
            }
            None => 1,
        };

        // Helper: extract i64 from an optional index value.
        let to_i64 = |v: Option<FidanValue>| -> Result<Option<i64>, MirSignal> {
            match v {
                None => Ok(None),
                Some(FidanValue::Integer(n)) => Ok(Some(n)),
                Some(other) => Err(MirSignal::Panic(format!(
                    "slice index must be an integer, got `{}`",
                    other.type_name()
                ))),
            }
        };
        let start_raw = to_i64(start)?;
        let end_raw = to_i64(end)?;

        match tgt {
            FidanValue::List(r) => {
                let list = r.borrow();
                let len = list.len() as i64;
                let norm = |i: i64| if i < 0 { (len + i).max(0) } else { i.min(len) };
                let si = start_raw
                    .map(norm)
                    .unwrap_or(if step_i > 0 { 0 } else { len - 1 });
                let ei = end_raw
                    .map(|e| {
                        let n = norm(e);
                        if inclusive { n + 1 } else { n }
                    })
                    .unwrap_or(if step_i > 0 { len } else { -1 });

                let mut out = FidanList::new();
                let mut idx = si;
                while (step_i > 0 && idx < ei) || (step_i < 0 && idx > ei) {
                    if let Some(v) = list.get(idx as usize) {
                        out.append(v.clone());
                    }
                    idx += step_i;
                }
                Ok(FidanValue::List(OwnedRef::new(out)))
            }
            FidanValue::String(s) => {
                let str_ref = s.as_str();
                let len = str_ref.chars().count() as i64;
                let norm = |i: i64| if i < 0 { (len + i).max(0) } else { i.min(len) };
                let si = start_raw
                    .map(norm)
                    .unwrap_or(if step_i > 0 { 0 } else { len - 1 });
                let ei = end_raw
                    .map(|e| {
                        let n = norm(e);
                        if inclusive { n + 1 } else { n }
                    })
                    .unwrap_or(if step_i > 0 { len } else { -1 });

                // Fast path: contiguous forward slice — skip + take, no Vec.
                if step_i == 1 && si >= 0 && ei >= si {
                    let out: String = str_ref
                        .chars()
                        .skip(si as usize)
                        .take((ei - si) as usize)
                        .collect();
                    return Ok(FidanValue::String(FidanString::new(&out)));
                }

                // General path: arbitrary step \u2014 collect once then index.
                let chars: Vec<char> = str_ref.chars().collect();
                let mut out = String::new();
                let mut idx = si;
                while (step_i > 0 && idx < ei) || (step_i < 0 && idx > ei) {
                    if let Some(c) = chars.get(idx as usize) {
                        out.push(*c);
                    }
                    idx += step_i;
                }
                Ok(FidanValue::String(FidanString::new(&out)))
            }
            FidanValue::Range {
                start,
                end,
                inclusive: range_inclusive,
            } => {
                // Materialise a sub-slice of a lazy range into a List.
                let range_len = if range_inclusive {
                    (end - start + 1).max(0)
                } else {
                    (end - start).max(0)
                };
                let norm = |i: i64| {
                    if i < 0 {
                        (range_len + i).max(0)
                    } else {
                        i.min(range_len)
                    }
                };
                let si = start_raw
                    .map(norm)
                    .unwrap_or(if step_i > 0 { 0 } else { range_len - 1 });
                let ei = end_raw
                    .map(|e| {
                        let n = norm(e);
                        if inclusive { n + 1 } else { n }
                    })
                    .unwrap_or(if step_i > 0 { range_len } else { -1 });

                let mut out = FidanList::new();
                let mut idx = si;
                while (step_i > 0 && idx < ei) || (step_i < 0 && idx > ei) {
                    if idx >= 0 && idx < range_len {
                        out.append(FidanValue::Integer(start + idx));
                    }
                    idx += step_i;
                }
                Ok(FidanValue::List(OwnedRef::new(out)))
            }
            other => Err(MirSignal::Panic(format!(
                "cannot slice `{}`",
                other.type_name()
            ))),
        }
    }

    fn index_get(&self, obj: FidanValue, idx: FidanValue) -> Result<FidanValue, MirSignal> {
        match (obj, idx) {
            (FidanValue::List(r), FidanValue::Integer(i)) => {
                let list = r.borrow();
                let len = list.len() as i64;
                let norm = if i < 0 { len + i } else { i };
                list.get(norm as usize).cloned().ok_or_else(|| {
                    MirSignal::RuntimeError(
                        fidan_diagnostics::diag_code!("R2002"),
                        format!("list index {} out of range", i),
                    )
                })
            }
            (FidanValue::Dict(r), key) => Ok(r
                .borrow()
                .get(&key)
                .ok()
                .flatten()
                .cloned()
                .unwrap_or(FidanValue::Nothing)),
            (FidanValue::HashSet(r), FidanValue::Integer(i)) => {
                let set = r.borrow();
                set.value_at_sorted_index(i).ok_or_else(|| {
                    MirSignal::RuntimeError(
                        fidan_diagnostics::diag_code!("R2002"),
                        format!("hashset index {} out of range", i),
                    )
                })
            }
            (FidanValue::String(s), FidanValue::Integer(i)) => {
                // Avoid materialising a Vec<char> — walk with an iterator instead.
                let str_ref = s.as_str();
                let len = str_ref.chars().count() as i64;
                let norm = if i < 0 { len + i } else { i };
                if norm < 0 || norm >= len {
                    return Err(MirSignal::RuntimeError(
                        fidan_diagnostics::diag_code!("R2002"),
                        format!("string index {} out of range", i),
                    ));
                }
                let c = str_ref.chars().nth(norm as usize).unwrap();
                Ok(FidanValue::String(FidanString::new(&c.to_string())))
            }
            (
                FidanValue::Range {
                    start,
                    end,
                    inclusive,
                },
                FidanValue::Integer(i),
            ) => {
                // Index into a lazy range without materialising it.
                let len = if inclusive {
                    (end - start + 1).max(0)
                } else {
                    (end - start).max(0)
                };
                let norm = if i < 0 { len + i } else { i };
                if norm < 0 || norm >= len {
                    return Err(MirSignal::RuntimeError(
                        fidan_diagnostics::diag_code!("R2002"),
                        format!("range index {} out of range", i),
                    ));
                }
                Ok(FidanValue::Integer(start + norm))
            }
            (FidanValue::Tuple(items), FidanValue::Integer(i)) => {
                let len = items.len() as i64;
                let norm = if i < 0 { len + i } else { i };
                if norm < 0 || norm >= len {
                    return Err(MirSignal::RuntimeError(
                        fidan_diagnostics::diag_code!("R2002"),
                        format!("tuple index {} out of range", i),
                    ));
                }
                items.into_iter().nth(norm as usize).ok_or_else(|| {
                    MirSignal::RuntimeError(
                        fidan_diagnostics::diag_code!("R2002"),
                        format!("tuple index {} out of range", i),
                    )
                })
            }
            (obj, idx) => Err(MirSignal::Panic(format!(
                "cannot index `{}` with `{}`",
                obj.type_name(),
                idx.type_name()
            ))),
        }
    }

    fn iterable_items_snapshot(&self, collection: FidanValue) -> Option<Vec<FidanValue>> {
        match collection {
            FidanValue::List(list_ref) => Some(list_ref.borrow().iter().cloned().collect()),
            FidanValue::Tuple(items) => Some(items),
            FidanValue::HashSet(set_ref) => Some(set_ref.borrow().values_sorted()),
            FidanValue::Range {
                start,
                end,
                inclusive,
            } => {
                let mut items = Vec::new();
                if inclusive {
                    for n in start..=end {
                        items.push(FidanValue::Integer(n));
                    }
                } else {
                    for n in start..end {
                        items.push(FidanValue::Integer(n));
                    }
                }
                Some(items)
            }
            _ => None,
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
                    Err(MirSignal::RuntimeError(
                        fidan_diagnostics::diag_code!("R2002"),
                        format!("list index {} out of range", i),
                    ))
                }
            }
            (FidanValue::Dict(r), key) => {
                let _ = r.borrow_mut().insert(key, val);
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
    /// A typed runtime error with a known [`DiagCode`] (e.g. `R2001` division-by-zero,
    /// `R2002` out-of-bounds).  Carries a clean message with no embedded code prefix.
    RuntimeError(fidan_diagnostics::DiagCode, String),
    /// A sandbox policy violation — carries the exact [`DiagCode`] (`R4001`–`R4003`)
    /// and a clean human-readable message (no embedded code prefix).
    SandboxViolation(fidan_diagnostics::DiagCode, String),
}

type MirResult = Result<FidanValue, MirSignal>;

// ── Value equality helper (used by assert_eq / assert_ne) ────────────────────

/// Compare two `FidanValue`s for structural equality, mirroring `BinOp::Eq`.
///
/// Returns `true` when the values are considered equal by the Fidan runtime.
fn fidan_values_equal(a: &FidanValue, b: &FidanValue) -> bool {
    use FidanValue::*;
    match (a, b) {
        (Integer(x), Integer(y)) => x == y,
        (Float(x), Float(y)) => x == y,
        (Integer(x), Float(y)) => (*x as f64) == *y,
        (Float(x), Integer(y)) => *x == (*y as f64),
        (Boolean(x), Boolean(y)) => x == y,
        (String(x), String(y)) => x.as_str() == y.as_str(),
        (Nothing, Nothing) => true,
        (Nothing, _) | (_, Nothing) => false,
        (
            EnumVariant {
                tag: a,
                payload: pa,
            },
            EnumVariant {
                tag: b,
                payload: pb,
            },
        ) => {
            a == b
                && pa.len() == pb.len()
                && pa
                    .iter()
                    .zip(pb.iter())
                    .all(|(x, y)| fidan_values_equal(x, y))
        }
        (EnumType(a), EnumType(b)) => a == b,
        // Cross-type enum comparisons and object identity
        (EnumVariant { .. }, EnumType(_)) | (EnumType(_), EnumVariant { .. }) => false,
        (Object(a), Object(b)) => std::rc::Rc::ptr_eq(&a.0, &b.0),
        // ClassType identity
        (ClassType(a), ClassType(b)) => a == b,
        // Lazy range equality — two ranges are equal iff they represent the same sequence.
        (
            FidanValue::Range {
                start: as_,
                end: ae,
                inclusive: ai,
            },
            FidanValue::Range {
                start: bs,
                end: be,
                inclusive: bi,
            },
        ) => as_ == bs && ae == be && ai == bi,
        // Structural list equality
        (List(a), List(b)) => {
            let la = a.borrow();
            let lb = b.borrow();
            la.len() == lb.len()
                && la
                    .iter()
                    .zip(lb.iter())
                    .all(|(x, y)| fidan_values_equal(x, y))
        }
        // Structural tuple equality
        (Tuple(a), Tuple(b)) => {
            a.len() == b.len()
                && a.iter()
                    .zip(b.iter())
                    .all(|(x, y)| fidan_values_equal(x, y))
        }
        _ => false,
    }
}

// ── Arithmetic / logic helpers ────────────────────────────────────────────────

fn mir_lit_to_value(lit: &MirLit) -> FidanValue {
    match lit {
        MirLit::Int(n) => FidanValue::Integer(*n),
        MirLit::Float(f) => FidanValue::Float(*f),
        MirLit::Bool(b) => FidanValue::Boolean(*b),
        MirLit::Str(s) => FidanValue::String(FidanString::new(s)),
        MirLit::Nothing => FidanValue::Nothing,
        MirLit::FunctionRef(id) => FidanValue::Function(RuntimeFnId(*id)),
        MirLit::Namespace(m) => FidanValue::Namespace(Arc::from(m.as_str())),
        MirLit::StdlibFn { module, name } => {
            FidanValue::StdlibFn(Arc::from(module.as_str()), Arc::from(name.as_str()))
        }
        MirLit::EnumType(s) => FidanValue::EnumType(Arc::from(s.as_str())),
        MirLit::ClassType(s) => FidanValue::ClassType(Arc::from(s.as_str())),
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
                return Err(MirSignal::RuntimeError(
                    fidan_diagnostics::diag_code!("R2001"),
                    "division by zero".into(),
                ));
            }
            Integer(a / b)
        }
        (BinOp::Rem, Integer(a), Integer(b)) => {
            if *b == 0 {
                return Err(MirSignal::RuntimeError(
                    fidan_diagnostics::diag_code!("R2001"),
                    "modulo by zero".into(),
                ));
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
        // String concatenation — any value on either side coerces to string
        (BinOp::Add, String(a), String(b)) => {
            let mut s = std::string::String::with_capacity(a.len() + b.len());
            s.push_str(a.as_str());
            s.push_str(b.as_str());
            String(FidanString::new(&s))
        }
        (BinOp::Add, String(a), v) => {
            let s = format!("{}{}", a.as_str(), fidan_display(v));
            String(FidanString::new(&s))
        }
        (BinOp::Add, v, String(b)) => {
            let s = format!("{}{}", fidan_display(v), b.as_str());
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
        // Enum variant equality — delegates to fidan_values_equal so payloads are compared too.
        (BinOp::Eq, EnumVariant { .. }, EnumVariant { .. }) => Boolean(fidan_values_equal(&l, &r)),
        (BinOp::NotEq, EnumVariant { .. }, EnumVariant { .. }) => {
            Boolean(!fidan_values_equal(&l, &r))
        }
        // Enum type identity (two variables holding the same enum type are equal)
        (BinOp::Eq, EnumType(a), EnumType(b)) => Boolean(a == b),
        (BinOp::NotEq, EnumType(a), EnumType(b)) => Boolean(a != b),
        // EnumVariant vs EnumType (and vice-versa) — always false/true
        (BinOp::Eq, EnumVariant { .. }, EnumType(_))
        | (BinOp::Eq, EnumType(_), EnumVariant { .. }) => Boolean(false),
        (BinOp::NotEq, EnumVariant { .. }, EnumType(_))
        | (BinOp::NotEq, EnumType(_), EnumVariant { .. }) => Boolean(true),
        // Object identity: two references to the same instance are equal; distinct instances are not
        (BinOp::Eq, Object(a), Object(b)) => Boolean(std::rc::Rc::ptr_eq(&a.0, &b.0)),
        (BinOp::NotEq, Object(a), Object(b)) => Boolean(!std::rc::Rc::ptr_eq(&a.0, &b.0)),
        // ClassType identity
        (BinOp::Eq, ClassType(a), ClassType(b)) => Boolean(a == b),
        (BinOp::NotEq, ClassType(a), ClassType(b)) => Boolean(a != b),
        // Bitwise
        (BinOp::BitAnd, Integer(a), Integer(b)) => Integer(a & b),
        (BinOp::BitOr, Integer(a), Integer(b)) => Integer(a | b),
        (BinOp::BitXor, Integer(a), Integer(b)) => Integer(a ^ b),
        (BinOp::Shl, Integer(a), Integer(b)) => Integer(a << (b & 63)),
        (BinOp::Shr, Integer(a), Integer(b)) => Integer(a >> (b & 63)),
        // Ranges produce a lazy sentinel — no heap allocation until elements
        // are actually needed (e.g. materialised via append/collect).
        (BinOp::Range, Integer(a), Integer(b)) => FidanValue::Range {
            start: *a,
            end: *b,
            inclusive: false,
        },
        (BinOp::RangeInclusive, Integer(a), Integer(b)) => FidanValue::Range {
            start: *a,
            end: *b,
            inclusive: true,
        },
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

mod api;
pub use api::{
    MirReplState, run_mir, run_mir_repl_line, run_mir_with_jit, run_mir_with_profile,
    run_mir_with_replay, run_tests,
};
