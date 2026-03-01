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
    FidanClass, FidanDict, FidanList, FidanObject, FidanString, FidanValue, FieldDef,
    FunctionId as RuntimeFnId, OwnedRef,
};

use crate::builtins;
use crate::interp::RunError;

// ── Object class table ────────────────────────────────────────────────────────

/// Build `Arc<FidanClass>` instances from `MirObjectInfo` metadata.
/// Parent classes are resolved recursively; cycles are silently broken.
fn build_class_table(
    objects: &[MirObjectInfo],
) -> std::collections::HashMap<fidan_lexer::Symbol, Arc<FidanClass>> {
    use std::collections::HashMap;
    let mut table: HashMap<fidan_lexer::Symbol, Arc<FidanClass>> = HashMap::new();

    // Build in the order objects appear (HIR outputs parents before children).
    for obj in objects {
        let field_defs: Vec<FieldDef> = obj
            .field_names
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
        let class = Arc::new(FidanClass {
            name: obj.name,
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
}

impl MirMachine {
    pub fn new(program: Arc<MirProgram>, interner: Arc<SymbolInterner>) -> Self {
        let classes = build_class_table(&program.objects);
        Self {
            program,
            interner,
            classes,
        }
    }

    /// Resolve a `Symbol` to its string.
    fn sym_str(&self, sym: Symbol) -> Arc<str> {
        self.interner.resolve(sym)
    }

    // ── Entry point ──────────────────────────────────────────────────────────

    /// Execute the main (top-level init) function.
    pub fn run(&mut self) -> Result<(), RunError> {
        let entry = FunctionId(0);
        match self.call_function(entry, vec![]) {
            Ok(_) => Ok(()),
            Err(MirSignal::Throw(v)) => Err(RunError {
                message: format!("unhandled exception: {}", builtins::display(&v)),
                trace: vec![],
            }),
            Err(MirSignal::Panic(msg)) => Err(RunError {
                message: msg,
                trace: vec![],
            }),
        }
    }

    // ── Function call ─────────────────────────────────────────────────────────

    fn call_function(&mut self, fn_id: FunctionId, args: Vec<FidanValue>) -> MirResult {
        let func = self.program.function(fn_id);
        let fn_name = self.sym_str(func.name).to_string();
        let local_count = func.local_count;

        let mut frame = CallFrame::new(local_count, fn_name);

        // Bind parameters.
        for (i, param) in func.params.iter().enumerate() {
            let val = args.get(i).cloned().unwrap_or(FidanValue::Nothing);
            frame.store(param.local, val);
        }

        // Block-level execution starting at entry (BlockId(0)).
        let return_val = self.run_function(fn_id, &mut frame)?;
        Ok(return_val.unwrap_or(FidanValue::Nothing))
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
                dest, callee, args, ..
            } => {
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
            // Concurrency: sequential stubs.
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
                let arg_vals: Vec<FidanValue> =
                    args.iter().map(|a| self.eval_operand(a, frame)).collect();
                let result = self.call_function(task_fn, arg_vals)?;
                frame.store(handle, result);
            }
            Instr::SpawnExpr {
                dest,
                task_fn,
                args,
            } => {
                let arg_vals: Vec<FidanValue> =
                    args.iter().map(|a| self.eval_operand(a, frame)).collect();
                let result = self.call_function(task_fn, arg_vals)?;
                frame.store(dest, result);
            }
            Instr::JoinAll { .. } => {
                // All handles are already completed (sequential stub).
            }
            Instr::AwaitPending { dest, handle } => {
                // In sequential mode, the handle IS the value.
                let v = self.eval_operand(&handle, frame);
                frame.store(dest, v);
            }
            Instr::ParallelIter {
                collection,
                body_fn,
                closure_args,
            } => {
                let coll = self.eval_operand(&collection, frame);
                let extra: Vec<FidanValue> = closure_args
                    .iter()
                    .map(|a| self.eval_operand(a, frame))
                    .collect();
                if let FidanValue::List(list_ref) = coll {
                    let items: Vec<FidanValue> = list_ref.borrow().iter().cloned().collect();
                    for item in items {
                        let mut fn_args = vec![item];
                        fn_args.extend(extra.clone());
                        self.call_function(body_fn, fn_args)?;
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
        // Fall through: treat method as a global builtin with receiver as first arg.
        let mut all_args = vec![receiver];
        all_args.extend(args);
        builtins::call_builtin(method, all_args)
            .ok_or_else(|| MirSignal::Panic(format!("no method `{}` found", method)))
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
            let class = Arc::new(FidanClass {
                name: ty,
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
pub fn run_mir(program: MirProgram, interner: Arc<SymbolInterner>) -> Result<(), RunError> {
    let mut machine = MirMachine::new(Arc::new(program), interner);
    machine.run()
}
