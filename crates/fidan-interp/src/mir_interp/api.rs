// fidan-interp/src/mir_interp/api.rs
//
// Public API layer: MirReplState, the REPL-helper impl block, and the
// free public entry-point functions (run_mir, run_mir_repl_line, etc.).
//
// This file is a submodule of mir_interp.  It can access all private
// items from the parent module (MirMachine private fields, MirSignal,
// CallFrame, etc.) because Rust grants submodules full access to their
// parent module's private items.

use std::collections::HashSet;
use std::sync::Arc;

use crate::profiler::ProfileReport;
use fidan_lexer::SymbolInterner;
use fidan_mir::{BlockId, FunctionId, LocalId, MirProgram, Terminator};
use fidan_runtime::FidanValue;
use fidan_source::SourceMap;

use super::{
    CallFrame, MirMachine, MirSignal, RunError, TestResult, instruction_trace_span,
    route_signal_to_catch,
};

// ── Public API ────────────────────────────────────────────────────────────────

/// Persistent source, global values and stable slot names for REPL recompilation.
/// New initializer effects are selected using the previous source length;
/// declarations are rebuilt without replaying prior effects.
pub struct MirReplState {
    /// All source text entered so far (grows with each successful input).
    pub accumulated_source: String,
    /// Legacy explicit-offset cursor; unused by source-delta execution.
    pub ns_cursor: usize,
    /// Legacy explicit-offset cursor; unused by source-delta execution.
    pub body_cursor: usize,
    /// Global values after the last successful execution.
    /// Pre-filled into each new `MirMachine` before running the delta.
    pub globals_snapshot: Vec<FidanValue>,
    /// Ordered list of all global symbol names registered so far.
    /// Passed as `existing_globals` to `lower_program` on every recompilation
    /// to guarantee every symbol always gets the same `GlobalId` index.
    pub persistent_global_names: Vec<String>,
    /// Fast O(1) dedup guard for `persistent_global_names`.
    persistent_global_set: HashSet<String>,
}

impl MirReplState {
    pub fn new() -> Self {
        Self {
            accumulated_source: String::new(),
            ns_cursor: 0,
            body_cursor: 0,
            globals_snapshot: Vec::new(),
            persistent_global_names: Vec::new(),
            persistent_global_set: HashSet::new(),
        }
    }
}

impl Default for MirReplState {
    fn default() -> Self {
        Self::new()
    }
}

impl MirMachine {
    // ── REPL helpers ─────────────────────────────────────────────────────────

    /// Count **body** instructions in `blocks[0]` of the init function.
    /// Body instructions are those after the namespace-init section
    /// (`ns_instr_count` = `namespace_global_count * 2` pairs at the start).
    /// Returns `(ns_instr_count, body_instr_count)` for the REPL cursors.
    pub fn count_init_split_instrs(&self, ns_instr_count: usize) -> (usize, usize) {
        let total = self
            .program
            .function(FunctionId(0))
            .block(BlockId(0))
            .instructions
            .len();
        (ns_instr_count, total.saturating_sub(ns_instr_count))
    }

    /// Snapshot all global values (used to preserve state between REPL lines).
    pub fn snapshot_globals(&self) -> Vec<FidanValue> {
        self.globals.read().clone()
    }

    /// Pre-fill globals from `snapshot`.
    /// Slots that were added by new declarations (beyond the snapshot length)
    /// keep their default `Nothing` value.
    pub fn restore_globals(&mut self, snapshot: &[FidanValue]) {
        let mut g = self.globals.write();
        for (i, v) in snapshot.iter().enumerate() {
            if i < g.len() {
                g[i] = v.clone();
            }
        }
    }

    /// REPL split-execution: skips already-executed namespace init instructions
    /// (`ns_cursor`), runs new namespace inits up to `ns_instr_count`, skips
    /// already-executed body instructions (`body_cursor`), then runs the new
    /// body delta.  Nothing is ever re-executed.
    pub fn run_init_split(
        &mut self,
        ns_cursor: usize,
        ns_instr_count: usize,
        body_cursor: usize,
    ) -> Result<(), RunError> {
        self.call_stack.clear();
        self.panic_trace.clear();
        let entry = FunctionId(0);
        let func = self.program.function(entry);
        let local_count = func.local_count;
        let mut frame = CallFrame::new(local_count);
        self.call_stack.push((entry, None, vec![]));
        let result = self.run_init_split_inner(&mut frame, ns_cursor, ns_instr_count, body_cursor);
        self.call_stack.pop();
        match result {
            Ok(_) => Ok(()),
            Err(MirSignal::Throw(v)) => Err(RunError {
                code: fidan_diagnostics::diag_code!("R1002"),
                message: format!("unhandled exception: {}", self.display_exception(&v)),
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

    /// Run the init function (FunctionId 0) starting at instruction
    /// `start_offset` within `blocks[0]`, skipping all earlier instructions.
    ///
    /// This is the core of the MIR REPL delta-execution: recompile the full
    /// accumulated source, pre-fill globals, then only *execute* the new part.
    pub fn run_init_delta(&mut self, start_offset: usize) -> Result<(), RunError> {
        self.call_stack.clear();
        self.panic_trace.clear();
        let entry = FunctionId(0);
        let func = self.program.function(entry);
        let local_count = func.local_count;
        let mut frame = CallFrame::new(local_count);
        self.call_stack.push((entry, None, vec![]));
        let result = self.run_init_from_offset(&mut frame, start_offset);
        self.call_stack.pop();
        match result {
            Ok(_) => Ok(()),
            Err(MirSignal::Throw(v)) => Err(RunError {
                code: fidan_diagnostics::diag_code!("R1002"),
                message: format!("unhandled exception: {}", self.display_exception(&v)),
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

    /// Execute the init function starting at instruction `start_offset` in
    /// `blocks[0]`.  All subsequent blocks execute normally (no offset).
    fn run_init_from_offset(
        &mut self,
        frame: &mut CallFrame,
        start_offset: usize,
    ) -> Result<Option<FidanValue>, MirSignal> {
        let fn_id = FunctionId(0);
        let program = Arc::clone(&self.program);
        let mut bb_id = BlockId(0);
        let mut prev_bb: Option<BlockId> = None;
        let mut is_first_block = true;

        'outer: loop {
            // Phi-node resolution (same as in run_function).
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

            let instr_count = program.function(fn_id).block(bb_id).instructions.len();
            // On the entry block only, skip the already-executed prefix.
            let instr_start = if is_first_block {
                is_first_block = false;
                start_offset.min(instr_count)
            } else {
                0
            };

            for i in instr_start..instr_count {
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
                                is_first_block = false;
                                continue 'outer;
                            }
                            Ok(None) => {}
                            Err(e) => return Err(e),
                        }
                    }
                }
            }

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
                    return Err(MirSignal::Panic(
                        "reached unreachable block in init function".into(),
                    ));
                }
            }
        }
    }

    /// Inner implementation for split-cursor REPL execution.
    ///
    /// Runs bb0 instructions in three phases:
    /// 1. `[0 .. ns_instr_count]`  — namespace init (always re-run, idempotent)
    /// 2. skip `body_cursor` instructions
    /// 3. run the rest (new body delta)
    fn run_init_split_inner(
        &mut self,
        frame: &mut CallFrame,
        ns_cursor: usize,
        ns_instr_count: usize,
        body_cursor: usize,
    ) -> Result<Option<FidanValue>, MirSignal> {
        let fn_id = FunctionId(0);
        let program = Arc::clone(&self.program);
        let mut bb_id = BlockId(0);
        let mut prev_bb: Option<BlockId> = None;
        let mut entered_bb0 = false;

        'outer: loop {
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

            let instr_count = program.function(fn_id).block(bb_id).instructions.len();
            // On the entry block: skip old ns inits, run new ns inits, skip old
            // body, run new body delta — in one contiguous pass with two skips.
            let instr_start = if !entered_bb0 && bb_id == BlockId(0) {
                entered_bb0 = true;
                // Run new namespace inits [ns_cursor..ns_instr_count].
                let ns_new_end = ns_instr_count.min(instr_count);
                for i in ns_cursor..ns_new_end {
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
                // Body starts at ns_instr_count; skip already-executed body instrs.
                (ns_instr_count + body_cursor).min(instr_count)
            } else {
                0
            };

            for i in instr_start..instr_count {
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
                    return Err(MirSignal::Panic(
                        "reached unreachable block in init function".into(),
                    ));
                }
            }
        }
    }

    /// Run a sequence of pre-lowered test functions and return per-test outcomes.
    ///
    /// Called by [`run_tests`] after the init function has already been run.
    /// Each test is called fresh (call stack cleared between tests).
    pub fn run_test_suite(&mut self, test_fns: &[(String, FunctionId)]) -> Vec<TestResult> {
        let mut results = Vec::with_capacity(test_fns.len());
        for (name, fn_id) in test_fns {
            self.call_stack.clear();
            self.panic_trace.clear();
            let outcome = self.call_function(*fn_id, vec![]);
            let result = match outcome {
                Ok(_) => TestResult {
                    name: name.clone(),
                    passed: true,
                    message: None,
                },
                Err(MirSignal::Panic(msg)) => TestResult {
                    name: name.clone(),
                    passed: false,
                    message: Some(msg),
                },
                Err(MirSignal::Throw(v)) => TestResult {
                    name: name.clone(),
                    passed: false,
                    message: Some(format!(
                        "unhandled exception: {}",
                        self.display_exception(&v)
                    )),
                },
                Err(MirSignal::ParallelFail(msg)) => TestResult {
                    name: name.clone(),
                    passed: false,
                    message: Some(msg),
                },
                Err(MirSignal::RuntimeError(code, msg)) => TestResult {
                    name: name.clone(),
                    passed: false,
                    message: Some(format!("[{code}] {msg}")),
                },
                Err(MirSignal::SandboxViolation(code, msg)) => TestResult {
                    name: name.clone(),
                    passed: false,
                    message: Some(format!("[{code}] {msg}")),
                },
            };
            results.push(result);
        }
        results
    }
}

/// Execute one REPL line using the MIR interpreter.
///
/// The caller typechecks the full accumulated source, then lowers it using
/// `fidan_mir::lower_repl_program` and the prior source length. All declarations
/// remain available, but initialization contains only the new input's effects.
/// Globals are restored before execution and committed after successful execution.
///
/// If `echo_sym` is `Some(sym)`, the program is expected to contain a global
/// with that name (the last declaration with that name, if redeclared multiple
/// times in REPL mode).  Its value after execution is returned as
/// `Ok(Some(FidanValue))` so the caller can display it.  `Nothing` values are
/// returned as-is; the caller decides whether to suppress them.
pub fn run_mir_repl_line(
    state: &mut MirReplState,
    program: MirProgram,
    interner: Arc<SymbolInterner>,
    source_map: Arc<SourceMap>,
    jit_threshold: u32,
    echo_sym: Option<fidan_lexer::Symbol>,
) -> Result<Option<FidanValue>, RunError> {
    let mut machine = MirMachine::new(Arc::new(program), interner, source_map);
    machine.set_jit_threshold(jit_threshold);
    machine.restore_globals(&state.globals_snapshot);
    machine.run()?;
    // Commit metadata only after the new source suffix executes successfully.
    for global in &machine.program.globals {
        let name = machine.interner.resolve(global.name);
        if state.persistent_global_set.insert(name.to_string()) {
            state.persistent_global_names.push(name.to_string());
        }
    }
    state.globals_snapshot = machine.snapshot_globals();

    // If the caller requested an echo, find the global with `echo_sym` (the last
    // declaration wins — in REPL mode redeclarations produce multiple GlobalId slots
    // and `global_map` points to the latest one) and return its value.
    if let Some(sym) = echo_sym {
        // Iterate in reverse to find the LAST GlobalId registered for this symbol.
        let echo_gid = machine
            .program
            .globals
            .iter()
            .enumerate()
            .rev()
            .find(|(_, g)| g.name == sym)
            .map(|(i, _)| i);
        if let Some(idx) = echo_gid {
            let snap = &state.globals_snapshot;
            if idx < snap.len() {
                return Ok(Some(snap[idx].clone()));
            }
        }
    }

    Ok(None)
}

/// Run a `MirProgram` from its entry function with a configurable JIT threshold.
///
/// `jit_threshold = 0`   → JIT disabled.
/// `jit_threshold > 0`   → compile a function after this many interpreter calls.
pub fn run_mir_with_jit(
    program: MirProgram,
    interner: Arc<SymbolInterner>,
    source_map: Arc<SourceMap>,
    jit_threshold: u32,
) -> Result<(), RunError> {
    let mut machine = MirMachine::new(Arc::new(program), interner, source_map);
    machine.set_jit_threshold(jit_threshold);
    machine.run()
}

/// Run a `MirProgram`, optionally replaying pre-recorded stdin inputs.
///
/// Returns the run result alongside every stdin line that was actually read
/// (i.e. lines from `input()` calls that consumed real stdin, not replay lines).
/// The capture is populated even when the run fails, so the caller can decide
/// whether to persist a replay bundle.
///
/// * `replay_inputs` — pass a non-empty `Vec` to replay; pass `vec![]` for a
///   normal run that only captures stdin for potential later replay.
pub fn run_mir_with_replay(
    program: MirProgram,
    interner: Arc<SymbolInterner>,
    source_map: Arc<SourceMap>,
    jit_threshold: u32,
    replay_inputs: Vec<String>,
    sandbox: Option<fidan_stdlib::SandboxPolicy>,
) -> (Result<(), RunError>, Vec<String>) {
    let mut machine = MirMachine::new(Arc::new(program), interner, source_map);
    machine.set_jit_threshold(jit_threshold);
    if !replay_inputs.is_empty() {
        machine.set_replay_inputs(replay_inputs);
    }
    if let Some(policy) = sandbox {
        machine.set_sandbox(policy);
    }
    let result = machine.run();
    let captured = machine.get_stdin_capture().to_vec();
    (result, captured)
}

/// Run a `MirProgram` from its entry function (default JIT threshold: 500).
pub fn run_mir(
    program: MirProgram,
    interner: Arc<SymbolInterner>,
    source_map: Arc<SourceMap>,
) -> Result<(), RunError> {
    run_mir_with_jit(program, interner, source_map, 500)
}

/// Run a `MirProgram` under the profiler and return a [`ProfileReport`].
///
/// The JIT is disabled so that every function call passes through the
/// interpreter timing hooks.  The report is `None` only if `enable_profiling`
/// failed internally (should never happen in practice).
///
/// `program_name` is used as the title line in the rendered report — pass
/// the source file name (e.g. `"app.fdn"`).
pub fn run_mir_with_profile(
    program: MirProgram,
    interner: Arc<SymbolInterner>,
    source_map: Arc<SourceMap>,
    program_name: &str,
) -> (Result<(), RunError>, Option<ProfileReport>) {
    // Collect function names before moving `program` into the machine.
    let fn_names: Vec<String> = program
        .functions
        .iter()
        .map(|f| interner.resolve(f.name).to_string())
        .collect();

    let mut machine = MirMachine::new(Arc::new(program), Arc::clone(&interner), source_map);
    machine.set_jit_threshold(0); // disable JIT — all calls must hit the interpreter hooks
    machine.enable_profiling();

    let wall_start = std::time::Instant::now();
    let result = machine.run();
    let total_ns = wall_start.elapsed().as_nanos() as u64;

    let report = machine.take_profile_report(&fn_names, program_name, total_ns);
    (result, report)
}

/// Run all `test { … }` blocks in a `MirProgram` and return per-test results.
///
/// 1. The init function (id 0) is run first so globals and imports are set up.
/// 2. Each named test function is called in declaration order.
/// 3. A test passes if the function returns normally; it fails if it panics
///    (typically via `assert`/`assert_eq`).
pub fn run_tests(
    program: MirProgram,
    interner: Arc<SymbolInterner>,
    source_map: Arc<SourceMap>,
) -> (Result<(), RunError>, Vec<TestResult>) {
    // Collect the test list before moving `program` into the machine.
    let test_fns: Vec<(String, FunctionId)> = program.test_functions.clone();

    let mut machine = MirMachine::new(Arc::new(program), interner, source_map);
    machine.set_jit_threshold(0); // keep JIT off for reproducible test runs

    // Run module init (sets up globals, runs top-level statements).
    if let Err(e) = machine.run() {
        return (Err(e), vec![]);
    }

    // Freeze globals after init: all subsequent LoadGlobal instructions will
    // use the lock-free snapshot, skipping the RwLock on every read.
    machine.freeze_globals();

    let results = machine.run_test_suite(&test_fns);
    (Ok(()), results)
}
