// fidan-interp/src/interp.rs
//
// Direct AST-walking interpreter.
//
// Phase 5 bootstrap: walks the AST produced by fidan-parser and evaluated by
// fidan-typeck.  The MIR pipeline (HIR → MIR → optimised interpreter) is a
// future phase; this gives us a working `fidan run` today.

use std::collections::HashMap;
use std::sync::Arc;

use fidan_ast::{
    Arg, AstArena, BinOp, Expr, ExprId, InterpPart, Item, Module, Param, Stmt, StmtId, UnOp,
};
use fidan_lexer::{Symbol, SymbolInterner};
use fidan_runtime::{
    FidanClass, FidanDict, FidanList, FidanObject, FidanString, FidanValue, FieldDef, OwnedRef,
};

use crate::builtins;
use crate::env::Env;
use crate::frame::{InterpResult, Signal};

// ── Internal registry types ───────────────────────────────────────────────────

/// A callable function/method definition derived from the AST.
#[derive(Clone)]
struct FuncDef {
    params: Vec<Param>,
    body: Vec<StmtId>,
}

/// Class definition: layout + method table.
struct ClassDef {
    /// Symbol name of the parent class (if any).
    parent_name: Option<Symbol>,
    /// Own field declarations (not including inherited).
    own_fields: Vec<(Symbol, bool)>, // (name, required)
    /// Methods defined inside `object { action ... }`.
    methods: HashMap<Symbol, FuncDef>,
}

// ── Public interpreter struct ─────────────────────────────────────────────────

pub struct Interpreter<'m> {
    arena: &'m AstArena,
    interner: Arc<SymbolInterner>,

    /// Top-level `action` declarations.
    functions: HashMap<Symbol, FuncDef>,

    /// Extension actions: outer key = class symbol, inner key = action name.
    ext_actions: HashMap<Symbol, HashMap<Symbol, FuncDef>>,

    /// Object class definitions.
    classes: HashMap<Symbol, ClassDef>,

    /// Variable environment.
    env: Env,

    // ── Cached common symbols ───────────────────────────────────────────────
    sym_initialize: Symbol,
}

impl<'m> Interpreter<'m> {
    // ── Construction ──────────────────────────────────────────────────────────

    fn new(module: &'m Module, interner: Arc<SymbolInterner>) -> Self {
        let sym_initialize = interner.intern("initialize");

        let mut interp = Interpreter {
            arena: &module.arena,
            interner: Arc::clone(&interner),
            functions: HashMap::new(),
            ext_actions: HashMap::new(),
            classes: HashMap::new(),
            env: Env::new(),
            sym_initialize,
        };

        interp.register_module(module);
        interp
    }

    // ── Registration pass ─────────────────────────────────────────────────────

    fn register_module(&mut self, module: &Module) {
        for &iid in &module.items {
            match self.arena.get_item(iid) {
                Item::ObjectDecl {
                    name,
                    parent,
                    fields,
                    methods,
                    ..
                } => {
                    let own_fields: Vec<(Symbol, bool)> =
                        fields.iter().map(|f| (f.name, f.required)).collect();

                    let mut mmap: HashMap<Symbol, FuncDef> = HashMap::new();
                    for &mid in methods {
                        match self.arena.get_item(mid) {
                            Item::ActionDecl {
                                name: mname,
                                params,
                                body,
                                ..
                            } => {
                                mmap.insert(
                                    *mname,
                                    FuncDef {
                                        params: params.clone(),
                                        body: body.clone(),
                                    },
                                );
                            }
                            _ => {}
                        }
                    }

                    self.classes.insert(
                        *name,
                        ClassDef {
                            parent_name: *parent,
                            own_fields,
                            methods: mmap,
                        },
                    );
                }

                Item::ActionDecl {
                    name, params, body, ..
                } => {
                    self.functions.insert(
                        *name,
                        FuncDef {
                            params: params.clone(),
                            body: body.clone(),
                        },
                    );
                }

                Item::ExtensionAction {
                    name,
                    extends,
                    params,
                    body,
                    ..
                } => {
                    self.ext_actions.entry(*extends).or_default().insert(
                        *name,
                        FuncDef {
                            params: params.clone(),
                            body: body.clone(),
                        },
                    );
                }

                _ => {}
            }
        }
    }

    // ── Module entry point ───────────────────────────────────────────────────

    fn run_module(&mut self, module: &Module) -> InterpResult<()> {
        self.run_module_repl(module).map(|_| ())
    }

    /// Like `run_module` but returns the value of the last `ExprStmt`, if any.
    /// Used by the REPL to auto-echo bare expression results.
    fn run_module_repl(&mut self, module: &Module) -> InterpResult<Option<FidanValue>> {
        let mut last_expr_val: Option<FidanValue> = None;
        for &iid in &module.items {
            match self.arena.get_item(iid) {
                Item::VarDecl { name, init, .. } => {
                    let val = match init {
                        Some(eid) => self.eval_expr(*eid)?,
                        None => FidanValue::Nothing,
                    };
                    self.env.define(*name, val);
                    last_expr_val = None; // declaration — suppress echo
                }
                Item::ExprStmt(eid) => {
                    let val = self.eval_expr(*eid)?;
                    last_expr_val = Some(val);
                }
                Item::Assign { target, value, .. } => {
                    let val = self.eval_expr(*value)?;
                    self.eval_assign(*target, val)?;
                    last_expr_val = None; // assignment — suppress echo
                }
                _ => {}
            }
        }
        Ok(last_expr_val)
    }

    // ── Expression evaluator ──────────────────────────────────────────────────

    fn eval_expr(&mut self, id: ExprId) -> InterpResult<FidanValue> {
        // Clone to release the arena borrow before making recursive calls.
        let expr = self.arena.get_expr(id).clone();

        match expr {
            // Literals –––––––––––––––––––––––––––––––––––––––––––––––––––––
            Expr::IntLit { value, .. } => Ok(FidanValue::Integer(value)),
            Expr::FloatLit { value, .. } => Ok(FidanValue::Float(value)),
            Expr::BoolLit { value, .. } => Ok(FidanValue::Boolean(value)),
            Expr::StrLit { value, .. } => Ok(FidanValue::String(FidanString::new(&value))),
            Expr::Nothing { .. } => Ok(FidanValue::Nothing),

            // Names –––––––––––––––––––––––––––––––––––––––––––––––––––––––
            Expr::Ident { name, .. } => {
                Ok(self.env.get(name).cloned().unwrap_or(FidanValue::Nothing))
            }
            Expr::This { .. } => Ok(self.env.this_val().cloned().unwrap_or(FidanValue::Nothing)),
            Expr::Parent { .. } => {
                // As a value, `parent` is the same object as `this`.
                // The distinction only matters for method dispatch (handled in eval_call).
                Ok(self.env.this_val().cloned().unwrap_or(FidanValue::Nothing))
            }

            // Operators ––––––––––––––––––––––––––––––––––––––––––––––––––
            Expr::Binary { op, lhs, rhs, .. } => self.eval_binary(op, lhs, rhs),
            Expr::Unary { op, operand, .. } => {
                let val = self.eval_expr(operand)?;
                Ok(self.eval_unary(op, val))
            }
            Expr::NullCoalesce { lhs, rhs, .. } => {
                let left = self.eval_expr(lhs)?;
                if left.is_nothing() {
                    self.eval_expr(rhs)
                } else {
                    Ok(left)
                }
            }
            Expr::Ternary {
                condition,
                then_val,
                else_val,
                ..
            } => {
                if self.eval_expr(condition)?.truthy() {
                    self.eval_expr(then_val)
                } else {
                    self.eval_expr(else_val)
                }
            }

            // Calls ––––––––––––––––––––––––––––––––––––––––––––––––––––––
            Expr::Call { callee, args, .. } => self.eval_call(callee, args),

            // Field access –––––––––––––––––––––––––––––––––––––––––––––––
            Expr::Field { object, field, .. } => {
                let obj = self.eval_expr(object)?;
                self.read_field(&obj, field)
            }

            // Index access –––––––––––––––––––––––––––––––––––––––––––––––
            Expr::Index { object, index, .. } => {
                let obj = self.eval_expr(object)?;
                let idx = self.eval_expr(index)?;
                self.eval_index(obj, idx)
            }

            // Assignment (expression form) –––––––––––––––––––––––––––––––
            Expr::Assign { target, value, .. } => {
                let val = self.eval_expr(value)?;
                self.eval_assign(target, val.clone())?;
                Ok(val)
            }
            Expr::CompoundAssign {
                op, target, value, ..
            } => {
                let rhs = self.eval_expr(value)?;
                let lhs = self.eval_expr(target)?;
                let new_val = self.apply_binop(op, lhs, rhs)?;
                self.eval_assign(target, new_val.clone())?;
                Ok(new_val)
            }

            // String interpolation –––––––––––––––––––––––––––––––––––––––
            Expr::StringInterp { parts, .. } => {
                let mut out = String::new();
                for part in parts {
                    match part {
                        InterpPart::Literal(s) => out.push_str(&s),
                        InterpPart::Expr(eid) => {
                            let v = self.eval_expr(eid)?;
                            out.push_str(&builtins::display(&v));
                        }
                    }
                }
                Ok(FidanValue::String(FidanString::new(&out)))
            }

            // Collection literals –––––––––––––––––––––––––––––––––––––––
            Expr::List { elements, .. } => {
                let mut list = FidanList::new();
                for eid in elements {
                    let v = self.eval_expr(eid)?;
                    list.append(v);
                }
                Ok(FidanValue::List(OwnedRef::new(list)))
            }
            Expr::Dict { entries, .. } => {
                let mut dict = FidanDict::new();
                for (k_id, v_id) in entries {
                    let key = self.eval_expr(k_id)?;
                    let val = self.eval_expr(v_id)?;
                    let key_str = match key {
                        FidanValue::String(s) => s,
                        other => FidanString::new(&builtins::display(&other)),
                    };
                    dict.insert(key_str, val);
                }
                Ok(FidanValue::Dict(OwnedRef::new(dict)))
            }

            // Check (inline match expression) ––––––––––––––––––––––––––
            Expr::Check {
                scrutinee, arms, ..
            } => {
                let val = self.eval_expr(scrutinee)?;
                for arm in arms {
                    // Wildcard `_` always matches.
                    let is_wildcard = {
                        let pexpr = self.arena.get_expr(arm.pattern).clone();
                        matches!(pexpr, Expr::Ident { name, .. } if {
                            let s = self.interner.resolve(name);
                            s.as_ref() == "_"
                        })
                    };
                    if is_wildcard || {
                        let pat = self.eval_expr(arm.pattern)?;
                        self.values_equal(&val, &pat)
                    } {
                        return self.exec_body(&arm.body);
                    }
                }
                Ok(FidanValue::Nothing)
            }

            // Concurrency (sequential fallback) –––––––––––––––––––––––—
            Expr::Spawn { expr, .. } | Expr::Await { expr, .. } => self.eval_expr(expr),

            Expr::Error { .. } => Ok(FidanValue::Nothing),
        }
    }

    // ── Call dispatch ─────────────────────────────────────────────────────────

    fn eval_call(&mut self, callee_id: ExprId, args: Vec<Arg>) -> InterpResult<FidanValue> {
        let callee = self.arena.get_expr(callee_id).clone();

        match callee {
            // ── Ident call: free function, constructor, or builtin ───────
            Expr::Ident { name, .. } => {
                let name_str = self.interner.resolve(name).to_string();

                // 1. Built-in function
                let raw_args = self.eval_args_raw(&args)?;
                let vals: Vec<FidanValue> = raw_args.iter().map(|(_, v)| v.clone()).collect();
                if let Some(result) = builtins::call_builtin(&name_str, vals) {
                    return Ok(result);
                }

                // 2. Class constructor
                if self.classes.contains_key(&name) {
                    return self.construct_object(name, raw_args);
                }

                // 3. Top-level function
                if self.functions.contains_key(&name) {
                    return self.call_named_function(name, raw_args, None);
                }

                // 4. Extension action called as a free function (no receiver)
                for (_class_sym, action_map) in &self.ext_actions {
                    if action_map.contains_key(&name) {
                        // clone to release borrow
                        if let Some(fdef) = action_map.get(&name).cloned() {
                            let locals = self.bind_args(&fdef.params.clone(), raw_args)?;
                            let body = fdef.body.clone();
                            self.env.push_frame(Some(name_str.clone()), None);
                            for (sym, val) in locals {
                                self.env.define(sym, val);
                            }
                            let result = self.exec_body(&body);
                            self.env.pop_frame();
                            return match result {
                                Ok(v) | Err(Signal::Return(v)) => Ok(v),
                                Err(e) => Err(e),
                            };
                        }
                    }
                }

                // Undefined — silently return Nothing
                Ok(FidanValue::Nothing)
            }

            // ── Field call: method dispatch ──────────────────────────────
            Expr::Field {
                object: obj_id,
                field,
                ..
            } => {
                let obj_expr = self.arena.get_expr(obj_id).clone();
                let is_parent_call = matches!(obj_expr, Expr::Parent { .. });

                // Get the receiver object.
                let obj_val = if is_parent_call {
                    self.env.this_val().cloned().unwrap_or(FidanValue::Nothing)
                } else {
                    self.eval_expr(obj_id)?
                };

                let raw_args = self.eval_args_raw(&args)?;

                match &obj_val.clone() {
                    FidanValue::Object(obj_ref) => {
                        let this_class = obj_ref.borrow().class.name;
                        let dispatch_class = if is_parent_call {
                            self.classes
                                .get(&this_class)
                                .and_then(|cd| cd.parent_name)
                                .unwrap_or(this_class)
                        } else {
                            this_class
                        };
                        self.call_method_on(obj_val, dispatch_class, field, raw_args)
                    }
                    // Built-in methods on collections and strings
                    other => {
                        let vals: Vec<FidanValue> = raw_args.into_iter().map(|(_, v)| v).collect();
                        Ok(self
                            .call_builtin_method(other, field, vals)
                            .unwrap_or(FidanValue::Nothing))
                    }
                }
            }

            // Direct expression call (unusual in Fidan, skip for now)
            _ => Ok(FidanValue::Nothing),
        }
    }

    // ── Method dispatch ───────────────────────────────────────────────────────

    /// Call method `method_name` on `obj_val` dispatching from `class_name`.
    /// Searches up the class hierarchy if the method is not found locally.
    fn call_method_on(
        &mut self,
        obj_val: FidanValue,
        class_name: Symbol,
        method_name: Symbol,
        raw_args: Vec<(Option<Symbol>, FidanValue)>,
    ) -> InterpResult<FidanValue> {
        // 1. Built-in method on the object?
        let method_str = self.interner.resolve(method_name).to_string();
        let vals: Vec<FidanValue> = raw_args.iter().map(|(_, v)| v.clone()).collect();
        if let Some(r) = self.call_builtin_method(&obj_val, method_name, vals) {
            return Ok(r);
        }

        // 2. Defined method in this class?
        if let Some(fdef) = self
            .classes
            .get(&class_name)
            .and_then(|cd| cd.methods.get(&method_name))
            .cloned()
        {
            return self.exec_callable(&method_str, fdef, raw_args, Some(obj_val));
        }

        // 3. Extension action targeting this class?
        if let Some(fdef) = self
            .ext_actions
            .get(&class_name)
            .and_then(|m| m.get(&method_name))
            .cloned()
        {
            return self.exec_callable(&method_str, fdef, raw_args, Some(obj_val));
        }

        // 4. Walk up the hierarchy.
        if let Some(parent_name) = self.classes.get(&class_name).and_then(|cd| cd.parent_name) {
            return self.call_method_on(obj_val, parent_name, method_name, raw_args);
        }

        // Method not found — try as a builtin by string name
        let vals2: Vec<FidanValue> = raw_args.into_iter().map(|(_, v)| v).collect();
        if let Some(r) = builtins::call_builtin(&method_str, vals2) {
            return Ok(r);
        }

        Ok(FidanValue::Nothing)
    }

    // ── Object construction ───────────────────────────────────────────────────

    fn construct_object(
        &mut self,
        class_name: Symbol,
        raw_args: Vec<(Option<Symbol>, FidanValue)>,
    ) -> InterpResult<FidanValue> {
        let class_arc = self.make_fidan_class(class_name);
        let obj_ref = OwnedRef::new(FidanObject::new(class_arc));
        let obj_val = FidanValue::Object(obj_ref);

        // Call `initialize` if defined anywhere in the hierarchy.
        let sym_init = self.sym_initialize;
        let has_init = self.find_method(class_name, sym_init).is_some();

        if has_init {
            match self.call_method_on(obj_val.clone(), class_name, sym_init, raw_args) {
                Ok(_) | Err(Signal::Return(_)) => {}
                Err(e) => return Err(e),
            }
        }

        Ok(obj_val)
    }

    /// Build a `FidanClass` Arc for a given class name, incorporating inherited fields.
    fn make_fidan_class(&self, class_name: Symbol) -> Arc<FidanClass> {
        let mut all_fields: Vec<FieldDef> = Vec::new();

        // Recursively gather parent fields first.
        if let Some(parent_name) = self.classes.get(&class_name).and_then(|cd| cd.parent_name) {
            let parent_cls = self.make_fidan_class(parent_name);
            all_fields = parent_cls.fields.clone();
        }

        // Append own fields.
        let start = all_fields.len();
        if let Some(cd) = self.classes.get(&class_name) {
            for (i, &(field_sym, _)) in cd.own_fields.iter().enumerate() {
                all_fields.push(FieldDef {
                    name: field_sym,
                    index: start + i,
                });
            }
        }

        Arc::new(FidanClass {
            name: class_name,
            parent: None, // not used by the AST interpreter
            fields: all_fields,
            methods: HashMap::new(), // not used by the AST interpreter
        })
    }

    // ── Function invocation helpers ───────────────────────────────────────────

    /// Call a top-level named function.
    fn call_named_function(
        &mut self,
        name: Symbol,
        raw_args: Vec<(Option<Symbol>, FidanValue)>,
        this: Option<FidanValue>,
    ) -> InterpResult<FidanValue> {
        let name_str = self.interner.resolve(name).to_string();
        let fdef = match self.functions.get(&name).cloned() {
            Some(f) => f,
            None => return Ok(FidanValue::Nothing),
        };
        self.exec_callable(&name_str, fdef, raw_args, this)
    }

    /// Execute a callable (bind args, push frame, run body, pop frame).
    ///
    /// `name` is pushed onto the interpreter call stack and shown in
    /// stack traces on uncaught panics.
    fn exec_callable(
        &mut self,
        name: &str,
        fdef: FuncDef,
        raw_args: Vec<(Option<Symbol>, FidanValue)>,
        this: Option<FidanValue>,
    ) -> InterpResult<FidanValue> {
        let mut locals = self.bind_args(&fdef.params, raw_args)?;
        // Second pass: evaluate default expressions for params that were not
        // supplied (bind_args leaves them as Nothing when a default exists).
        for (i, param) in fdef.params.iter().enumerate() {
            if let Some(default_id) = param.default {
                if matches!(locals[i].1, FidanValue::Nothing) {
                    locals[i].1 = self.eval_expr(default_id)?;
                }
            }
        }
        self.env.push_frame(Some(name.to_string()), this);
        for (sym, val) in locals {
            self.env.define(sym, val);
        }
        let result = self.exec_body(&fdef.body);
        self.env.pop_frame();
        match result {
            Ok(v) | Err(Signal::Return(v)) => Ok(v),
            Err(e) => Err(e),
        }
    }

    // ── Argument evaluation & binding ─────────────────────────────────────────

    /// Evaluate all argument expressions, preserving their names.
    fn eval_args_raw(&mut self, args: &[Arg]) -> InterpResult<Vec<(Option<Symbol>, FidanValue)>> {
        let mut out = Vec::with_capacity(args.len());
        for a in args {
            let v = self.eval_expr(a.value)?;
            out.push((a.name, v));
        }
        Ok(out)
    }

    /// Bind evaluated args to param declarations, producing a list of
    /// (Symbol, FidanValue) ready to be `define`d in the new frame.
    fn bind_args(
        &self,
        params: &[Param],
        args: Vec<(Option<Symbol>, FidanValue)>,
    ) -> InterpResult<Vec<(Symbol, FidanValue)>> {
        let mut result: Vec<(Symbol, FidanValue)> = Vec::new();
        let mut positional_idx = 0usize;

        for param in params {
            // Try to match by name first.
            let named_match = args.iter().find(|(n, _)| *n == Some(param.name));
            if let Some((_, v)) = named_match {
                result.push((param.name, v.clone()));
                continue;
            }

            // Try positional.
            // Skip over args that were consumed as named.
            let named_symbols: Vec<Symbol> = args.iter().filter_map(|(n, _)| *n).collect();

            // Find the next positional arg (one without a name, in order).
            let positional_args: Vec<&FidanValue> = args
                .iter()
                .filter(|(n, _)| n.is_none())
                .map(|(_, v)| v)
                .collect();

            if positional_idx < positional_args.len() {
                result.push((param.name, positional_args[positional_idx].clone()));
                positional_idx += 1;
                continue;
            }

            // Not provided: use default if any, else Nothing.
            if let Some(_default_id) = param.default {
                // Defaults are constant expressions — eval them in the current scope.
                // We can't call &mut self here because bind_args takes &self.
                // Workaround: store the default ExprId, return it separately.
                // For now, use Nothing; defaults are handled in a second pass.
                result.push((param.name, FidanValue::Nothing));
            } else {
                result.push((param.name, FidanValue::Nothing));
            }

            let _ = named_symbols; // silence unused warning
        }

        Ok(result)
    }

    // ── Statement executor ────────────────────────────────────────────────────

    fn exec_stmt(&mut self, id: StmtId) -> InterpResult<FidanValue> {
        let stmt = self.arena.get_stmt(id).clone();

        match stmt {
            Stmt::VarDecl { name, init, .. } => {
                let val = match init {
                    Some(eid) => self.eval_expr(eid)?,
                    None => FidanValue::Nothing,
                };
                self.env.define(name, val);
                Ok(FidanValue::Nothing)
            }

            Stmt::Assign { target, value, .. } => {
                let val = self.eval_expr(value)?;
                self.eval_assign(target, val)?;
                Ok(FidanValue::Nothing)
            }

            Stmt::Expr { expr, .. } => {
                self.eval_expr(expr)?;
                Ok(FidanValue::Nothing)
            }

            Stmt::Return { value, .. } => {
                let v = match value {
                    Some(eid) => self.eval_expr(eid)?,
                    None => FidanValue::Nothing,
                };
                Err(Signal::Return(v))
            }

            Stmt::Break { .. } => Err(Signal::Break),
            Stmt::Continue { .. } => Err(Signal::Continue),

            Stmt::If {
                condition,
                then_body,
                else_ifs,
                else_body,
                ..
            } => {
                if self.eval_expr(condition)?.truthy() {
                    return self.exec_body(&then_body);
                }
                for elif in &else_ifs {
                    if self.eval_expr(elif.condition)?.truthy() {
                        return self.exec_body(&elif.body);
                    }
                }
                if let Some(eb) = else_body {
                    return self.exec_body(&eb);
                }
                Ok(FidanValue::Nothing)
            }

            Stmt::While {
                condition, body, ..
            } => {
                loop {
                    if !self.eval_expr(condition)?.truthy() {
                        break;
                    }
                    match self.exec_body(&body) {
                        Ok(_) => {}
                        Err(Signal::Break) => break,
                        Err(Signal::Continue) => continue,
                        Err(e) => return Err(e),
                    }
                }
                Ok(FidanValue::Nothing)
            }

            Stmt::For {
                binding,
                iterable,
                body,
                ..
            } => {
                let iter_val = self.eval_expr(iterable)?;
                let items = self.collect_iterable(iter_val)?;
                'for_loop: for item in items {
                    self.env.define(binding, item);
                    match self.exec_body(&body) {
                        Ok(_) => {}
                        Err(Signal::Break) => break 'for_loop,
                        Err(Signal::Continue) => continue 'for_loop,
                        Err(e) => return Err(e),
                    }
                }
                Ok(FidanValue::Nothing)
            }

            Stmt::ParallelFor {
                binding,
                iterable,
                body,
                ..
            } => {
                // Sequential fallback — Phase 5.5 adds real parallelism.
                let iter_val = self.eval_expr(iterable)?;
                let items = self.collect_iterable(iter_val)?;
                for item in items {
                    self.env.define(binding, item);
                    match self.exec_body(&body) {
                        Ok(_) => {}
                        Err(Signal::Break) => break,
                        Err(Signal::Continue) => continue,
                        Err(e) => return Err(e),
                    }
                }
                Ok(FidanValue::Nothing)
            }

            Stmt::ConcurrentBlock { tasks, .. } => {
                // Sequential fallback.
                for task in tasks {
                    self.exec_body(&task.body)?;
                }
                Ok(FidanValue::Nothing)
            }

            Stmt::Attempt {
                body,
                catches,
                otherwise,
                finally,
                ..
            } => {
                let body_result = self.exec_body(&body);

                // Determine the outcome of body + catch handling.
                let post_catch = match body_result {
                    Ok(_) => Ok(true), // success — run `otherwise` before `finally`
                    Err(Signal::Panic { value: err_val, trace: orig_trace }) => {
                        let mut caught = false;
                        let mut catch_outcome = Ok(false);
                        for catch in &catches {
                            caught = true;
                            let binding = catch.binding;
                            let catch_body = catch.body.clone();
                            self.env.push_frame(None, None);
                            if let Some(b) = binding {
                                self.env.define(b, err_val.clone());
                            }
                            let r = self.exec_body(&catch_body);
                            self.env.pop_frame();
                            catch_outcome = match r {
                                Ok(_) => Ok(false), // caught and handled
                                Err(e) => Err(e),   // catch body re-threw
                            };
                            break;
                        }
                        if caught {
                            catch_outcome
                        } else {
                            Err(Signal::Panic { value: err_val, trace: orig_trace }) // uncaught
                        }
                    }
                    Err(e) => Err(e),
                };

                // `otherwise` runs only when the body succeeded with no panic.
                let post_otherwise = match &post_catch {
                    Ok(true) => {
                        if let Some(ob) = otherwise {
                            self.exec_body(&ob).map(|_| ())
                        } else {
                            Ok(())
                        }
                    }
                    _ => Ok(()),
                };

                // `finally` ALWAYS runs — even on re-panic or uncaught panic.
                let finally_result = if let Some(fb) = finally {
                    self.exec_body(&fb).map(|_| ())
                } else {
                    Ok(())
                };

                // Propagate signals: post_catch → post_otherwise → finally (in priority order).
                // `finally` errors take lowest priority.
                match post_catch {
                    Ok(_) => {
                        post_otherwise?;
                        finally_result?;
                        Ok(FidanValue::Nothing)
                    }
                    Err(e) => {
                        // Run `finally` (already done above) then re-raise.
                        let _ = finally_result; // any finally error is swallowed on re-panic
                        Err(e)
                    }
                }
            }

            Stmt::Panic { value, .. } => {
                let v = self.eval_expr(value)?;
                let trace = self.env.stack_trace();
                Err(Signal::Panic { value: v, trace })
            }

            Stmt::Check {
                scrutinee, arms, ..
            } => {
                let val = self.eval_expr(scrutinee)?;
                for arm in &arms {
                    let is_wildcard = {
                        let pexpr = self.arena.get_expr(arm.pattern).clone();
                        matches!(pexpr, Expr::Ident { name, .. } if {
                            let s = self.interner.resolve(name);
                            s.as_ref() == "_"
                        })
                    };
                    if is_wildcard || {
                        let pat = self.eval_expr(arm.pattern)?;
                        self.values_equal(&val, &pat)
                    } {
                        let body = arm.body.clone();
                        return self.exec_body(&body);
                    }
                }
                Ok(FidanValue::Nothing)
            }

            Stmt::Error { .. } => Ok(FidanValue::Nothing),
        }
    }

    /// Execute a block of statements (no extra scope push — callers manage that).
    fn exec_body(&mut self, stmts: &[StmtId]) -> InterpResult<FidanValue> {
        let mut last = FidanValue::Nothing;
        for &sid in stmts {
            last = self.exec_stmt(sid)?;
        }
        Ok(last)
    }

    // ── Assignment to an lvalue ───────────────────────────────────────────────

    fn eval_assign(&mut self, target_id: ExprId, value: FidanValue) -> InterpResult<()> {
        let target = self.arena.get_expr(target_id).clone();
        match target {
            Expr::Ident { name, .. } => {
                if !self.env.assign(name, value.clone()) {
                    self.env.define(name, value);
                }
                Ok(())
            }
            Expr::Field { object, field, .. } => {
                let obj_val = self.eval_expr(object)?;
                match obj_val {
                    FidanValue::Object(obj_ref) => {
                        obj_ref.borrow_mut().set_field(field, value);
                    }
                    _ => {} // silently ignore field-set on non-object
                }
                Ok(())
            }
            Expr::This { .. } => {
                self.env.set_this(value);
                Ok(())
            }
            Expr::Index { object, index, .. } => {
                let obj_val = self.eval_expr(object)?;
                let idx_val = self.eval_expr(index)?;
                match obj_val {
                    FidanValue::List(list_ref) => {
                        if let FidanValue::Integer(i) = idx_val {
                            if i >= 0 {
                                list_ref.borrow_mut().set_at(i as usize, value);
                            }
                        }
                    }
                    FidanValue::Dict(dict_ref) => {
                        let key = match idx_val {
                            FidanValue::String(s) => s,
                            other => FidanString::new(&builtins::display(&other)),
                        };
                        dict_ref.borrow_mut().insert(key, value);
                    }
                    _ => {}
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }

    // ── Binary operator evaluation ────────────────────────────────────────────

    fn eval_binary(
        &mut self,
        op: BinOp,
        lhs_id: ExprId,
        rhs_id: ExprId,
    ) -> InterpResult<FidanValue> {
        // Short-circuit logical operators first.
        match op {
            BinOp::And => {
                let l = self.eval_expr(lhs_id)?;
                if !l.truthy() {
                    return Ok(FidanValue::Boolean(false));
                }
                let r = self.eval_expr(rhs_id)?;
                return Ok(FidanValue::Boolean(r.truthy()));
            }
            BinOp::Or => {
                let l = self.eval_expr(lhs_id)?;
                if l.truthy() {
                    return Ok(FidanValue::Boolean(true));
                }
                let r = self.eval_expr(rhs_id)?;
                return Ok(FidanValue::Boolean(r.truthy()));
            }
            _ => {}
        }

        let lhs = self.eval_expr(lhs_id)?;
        let rhs = self.eval_expr(rhs_id)?;
        self.apply_binop(op, lhs, rhs)
    }

    fn apply_binop(&self, op: BinOp, lhs: FidanValue, rhs: FidanValue) -> InterpResult<FidanValue> {
        use BinOp::*;
        use FidanValue::*;

        Ok(match (op, lhs, rhs) {
            // ── Arithmetic ───────────────────────────────────────────────
            (Add, Integer(a), Integer(b)) => Integer(a.wrapping_add(b)),
            (Add, Float(a), Float(b)) => Float(a + b),
            (Add, Integer(a), Float(b)) => Float(a as f64 + b),
            (Add, Float(a), Integer(b)) => Float(a + b as f64),
            (Add, String(a), String(b)) => String(a.append(&b)),
            (Add, String(a), other) => {
                String(a.append(&FidanString::new(&builtins::display(&other))))
            }

            (Sub, Integer(a), Integer(b)) => Integer(a.wrapping_sub(b)),
            (Sub, Float(a), Float(b)) => Float(a - b),
            (Sub, Integer(a), Float(b)) => Float(a as f64 - b),
            (Sub, Float(a), Integer(b)) => Float(a - b as f64),

            (Mul, Integer(a), Integer(b)) => Integer(a.wrapping_mul(b)),
            (Mul, Float(a), Float(b)) => Float(a * b),
            (Mul, Integer(a), Float(b)) => Float(a as f64 * b),
            (Mul, Float(a), Integer(b)) => Float(a * b as f64),

            (Div, Integer(a), Integer(b)) => {
                if b == 0 {
                    let trace = self.env.stack_trace();
                    return Err(Signal::Panic {
                        value: FidanValue::String(FidanString::new("division by zero")),
                        trace,
                    });
                }
                Integer(a / b)
            }
            (Div, Float(a), Float(b)) => Float(a / b),
            (Div, Integer(a), Float(b)) => Float(a as f64 / b),
            (Div, Float(a), Integer(b)) => Float(a / b as f64),

            (Rem, Integer(a), Integer(b)) => {
                if b == 0 {
                    let trace = self.env.stack_trace();
                    return Err(Signal::Panic {
                        value: FidanValue::String(FidanString::new("division by zero (remainder)")),
                        trace,
                    });
                }
                Integer(a % b)
            }
            (Rem, Float(a), Float(b)) => Float(a % b),

            (Pow, Integer(a), Integer(b)) => Integer(i64::pow(a, b.max(0) as u32)),
            (Pow, Float(a), Float(b)) => Float(a.powf(b)),
            (Pow, Integer(a), Float(b)) => Float((a as f64).powf(b)),
            (Pow, Float(a), Integer(b)) => Float(a.powf(b as f64)),

            // ── Bitwise ──────────────────────────────────────────────────
            (BitAnd, Integer(a), Integer(b)) => Integer(a & b),
            (BitOr, Integer(a), Integer(b)) => Integer(a | b),
            (BitXor, Integer(a), Integer(b)) => Integer(a ^ b),
            (Shl, Integer(a), Integer(b)) => Integer(a << (b & 63)),
            (Shr, Integer(a), Integer(b)) => Integer(a >> (b & 63)),

            // ── Comparison ───────────────────────────────────────────────
            (Eq, a, b) => Boolean(self.values_equal(&a, &b)),
            (NotEq, a, b) => Boolean(!self.values_equal(&a, &b)),

            (Lt, Integer(a), Integer(b)) => Boolean(a < b),
            (Lt, Float(a), Float(b)) => Boolean(a < b),
            (Lt, Integer(a), Float(b)) => Boolean((a as f64) < b),
            (Lt, Float(a), Integer(b)) => Boolean(a < b as f64),

            (LtEq, Integer(a), Integer(b)) => Boolean(a <= b),
            (LtEq, Float(a), Float(b)) => Boolean(a <= b),
            (LtEq, Integer(a), Float(b)) => Boolean((a as f64) <= b),
            (LtEq, Float(a), Integer(b)) => Boolean(a <= b as f64),

            (Gt, Integer(a), Integer(b)) => Boolean(a > b),
            (Gt, Float(a), Float(b)) => Boolean(a > b),
            (Gt, Integer(a), Float(b)) => Boolean((a as f64) > b),
            (Gt, Float(a), Integer(b)) => Boolean(a > b as f64),

            (GtEq, Integer(a), Integer(b)) => Boolean(a >= b),
            (GtEq, Float(a), Float(b)) => Boolean(a >= b),
            (GtEq, Integer(a), Float(b)) => Boolean((a as f64) >= b),
            (GtEq, Float(a), Integer(b)) => Boolean(a >= b as f64),

            // ── Range ─────────────────────────────────────────────────────
            (Range, Integer(start), Integer(end)) => {
                let mut l = FidanList::new();
                for i in start..end {
                    l.append(Integer(i));
                }
                List(OwnedRef::new(l))
            }
            (RangeInclusive, Integer(start), Integer(end)) => {
                let mut l = FidanList::new();
                for i in start..=end {
                    l.append(Integer(i));
                }
                List(OwnedRef::new(l))
            }

            // Fallback: unsupported operand combination — runtime type error
            (op, lhs, rhs) => {
                let op_s = match op {
                    Add => "+",
                    Sub => "-",
                    Mul => "*",
                    Div => "/",
                    Rem => "%",
                    Pow => "**",
                    BitAnd => "&",
                    BitOr => "|",
                    BitXor => "^",
                    Shl => "<<",
                    Shr => ">>",
                    _ => "(op)",
                };
                let msg = format!(
                    "operator `{op_s}` cannot be applied to `{}` and `{}`",
                    lhs.type_name(),
                    rhs.type_name()
                );
                let trace = self.env.stack_trace();
                return Err(Signal::Panic {
                    value: FidanValue::String(FidanString::new(&msg)),
                    trace,
                });
            }
        })
    }

    fn eval_unary(&self, op: UnOp, val: FidanValue) -> FidanValue {
        match (op, val) {
            (UnOp::Neg, FidanValue::Integer(n)) => FidanValue::Integer(-n),
            (UnOp::Neg, FidanValue::Float(f)) => FidanValue::Float(-f),
            (UnOp::Not, v) => FidanValue::Boolean(!v.truthy()),
            (_, v) => v,
        }
    }

    // ── Indexing ──────────────────────────────────────────────────────────────

    fn eval_index(&self, obj: FidanValue, idx: FidanValue) -> InterpResult<FidanValue> {
        match (obj, idx) {
            (FidanValue::List(l), FidanValue::Integer(i)) => Ok(l
                .borrow()
                .get(i as usize)
                .cloned()
                .unwrap_or(FidanValue::Nothing)),
            (FidanValue::Dict(d), FidanValue::String(k)) => {
                Ok(d.borrow().get(&k).cloned().unwrap_or(FidanValue::Nothing))
            }
            (FidanValue::String(s), FidanValue::Integer(i)) => {
                let ch = s.as_str().chars().nth(i as usize);
                Ok(ch
                    .map(|c| FidanValue::String(FidanString::new(&c.to_string())))
                    .unwrap_or(FidanValue::Nothing))
            }
            _ => Ok(FidanValue::Nothing),
        }
    }

    // ── Field access ──────────────────────────────────────────────────────────

    fn read_field(&self, val: &FidanValue, field: Symbol) -> InterpResult<FidanValue> {
        match val {
            FidanValue::Object(obj_ref) => Ok(obj_ref
                .borrow()
                .get_field(field)
                .cloned()
                .unwrap_or(FidanValue::Nothing)),
            FidanValue::List(l) => {
                // `.len` as a property
                let field_str = self.interner.resolve(field);
                if field_str.as_ref() == "len" || field_str.as_ref() == "length" {
                    Ok(FidanValue::Integer(l.borrow().len() as i64))
                } else {
                    Ok(FidanValue::Nothing)
                }
            }
            FidanValue::Dict(d) => {
                let field_str = self.interner.resolve(field);
                if field_str.as_ref() == "len" || field_str.as_ref() == "length" {
                    Ok(FidanValue::Integer(d.borrow().len() as i64))
                } else {
                    // Try as a string key.
                    let key = FidanString::new(field_str.as_ref());
                    Ok(d.borrow().get(&key).cloned().unwrap_or(FidanValue::Nothing))
                }
            }
            FidanValue::String(s) => {
                let field_str = self.interner.resolve(field);
                if field_str.as_ref() == "len" || field_str.as_ref() == "length" {
                    Ok(FidanValue::Integer(s.len() as i64))
                } else {
                    Ok(FidanValue::Nothing)
                }
            }
            _ => Ok(FidanValue::Nothing),
        }
    }

    // ── Built-in methods on primitive/collection types ────────────────────────

    fn call_builtin_method(
        &self,
        obj: &FidanValue,
        method: Symbol,
        args: Vec<FidanValue>,
    ) -> Option<FidanValue> {
        let name = self.interner.resolve(method);
        let name = name.as_ref();

        match obj {
            FidanValue::List(l) => match name {
                "append" | "add" | "push" => {
                    for arg in args {
                        l.borrow_mut().append(arg);
                    }
                    Some(FidanValue::Nothing)
                }
                "len" | "length" => Some(FidanValue::Integer(l.borrow().len() as i64)),
                "get" => {
                    if let Some(FidanValue::Integer(i)) = args.first() {
                        Some(
                            l.borrow()
                                .get(*i as usize)
                                .cloned()
                                .unwrap_or(FidanValue::Nothing),
                        )
                    } else {
                        Some(FidanValue::Nothing)
                    }
                }
                _ => None,
            },
            FidanValue::Dict(d) => match name {
                "get" => {
                    if let Some(FidanValue::String(k)) = args.first() {
                        Some(d.borrow().get(k).cloned().unwrap_or(FidanValue::Nothing))
                    } else {
                        Some(FidanValue::Nothing)
                    }
                }
                "set" | "insert" => {
                    if let (Some(FidanValue::String(k)), Some(v)) = (args.first(), args.get(1)) {
                        d.borrow_mut().insert(k.clone(), v.clone());
                        Some(FidanValue::Nothing)
                    } else {
                        Some(FidanValue::Nothing)
                    }
                }
                "len" | "length" => Some(FidanValue::Integer(d.borrow().len() as i64)),
                _ => None,
            },
            FidanValue::String(s) => match name {
                "len" | "length" => Some(FidanValue::Integer(s.len() as i64)),
                "upper" | "to_upper" => Some(FidanValue::String(FidanString::new(
                    &s.as_str().to_uppercase(),
                ))),
                "lower" | "to_lower" => Some(FidanValue::String(FidanString::new(
                    &s.as_str().to_lowercase(),
                ))),
                "trim" => Some(FidanValue::String(FidanString::new(s.as_str().trim()))),
                "contains" => {
                    if let Some(FidanValue::String(needle)) = args.first() {
                        Some(FidanValue::Boolean(s.as_str().contains(needle.as_str())))
                    } else {
                        Some(FidanValue::Boolean(false))
                    }
                }
                "starts_with" => {
                    if let Some(FidanValue::String(prefix)) = args.first() {
                        Some(FidanValue::Boolean(s.as_str().starts_with(prefix.as_str())))
                    } else {
                        Some(FidanValue::Boolean(false))
                    }
                }
                "ends_with" => {
                    if let Some(FidanValue::String(suffix)) = args.first() {
                        Some(FidanValue::Boolean(s.as_str().ends_with(suffix.as_str())))
                    } else {
                        Some(FidanValue::Boolean(false))
                    }
                }
                _ => None,
            },
            _ => None,
        }
    }

    // ── Iteration ─────────────────────────────────────────────────────────────

    fn collect_iterable(&self, val: FidanValue) -> InterpResult<Vec<FidanValue>> {
        match val {
            FidanValue::List(l) => Ok(l.borrow().iter().cloned().collect()),
            FidanValue::String(s) => Ok(s
                .as_str()
                .chars()
                .map(|c| FidanValue::String(FidanString::new(&c.to_string())))
                .collect()),
            _ => Ok(vec![]),
        }
    }

    // ── Pattern matching ──────────────────────────────────────────────────────

    fn values_equal(&self, a: &FidanValue, b: &FidanValue) -> bool {
        use FidanValue::*;
        match (a, b) {
            (Integer(x), Integer(y)) => x == y,
            (Float(x), Float(y)) => x == y,
            (Integer(x), Float(y)) => (*x as f64) == *y,
            (Float(x), Integer(y)) => *x == (*y as f64),
            (Boolean(x), Boolean(y)) => x == y,
            (Nothing, Nothing) => true,
            (String(x), String(y)) => x.as_str() == y.as_str(),
            (Nothing, _) | (_, Nothing) => false,
            _ => false,
        }
    }

    #[allow(dead_code)]
    fn matches_pattern(&self, val: &FidanValue, pattern: &FidanValue) -> bool {
        self.values_equal(val, pattern)
    }

    // ── Method lookup helper ──────────────────────────────────────────────────

    /// Find the class that provides `method_name` in the hierarchy of `class_name`.
    fn find_method(&self, class_name: Symbol, method_name: Symbol) -> Option<()> {
        let cd = self.classes.get(&class_name)?;
        if cd.methods.contains_key(&method_name) {
            return Some(());
        }
        if self
            .ext_actions
            .get(&class_name)
            .map(|m| m.contains_key(&method_name))
            .unwrap_or(false)
        {
            return Some(());
        }
        if let Some(parent) = cd.parent_name {
            return self.find_method(parent, method_name);
        }
        None
    }
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Run a parsed, type-checked Fidan module.
///
/// Prints to stdout via built-in `print`, returns `Ok(())` on success, or
/// `Err(message)` if an uncaught `panic` terminates execution.
/// Error returned from [`run`] when an uncaught panic propagates to the top level.
pub struct RunError {
    /// Short description of the panic value shown in the error message.
    pub message: String,
    /// Call stack at the moment of the panic, **innermost frame first**.
    /// Empty when the panic originated outside any named function.
    pub trace: Vec<String>,
}

pub fn run(module: &Module, interner: Arc<SymbolInterner>) -> Result<(), RunError> {
    let mut interp = Interpreter::new(module, interner);
    match interp.run_module(module) {
        Ok(()) | Err(Signal::Return(_)) => Ok(()),
        Err(Signal::Panic { value, trace }) => Err(RunError {
            message: format!("runtime panic: {}", builtins::display(&value)),
            trace,
        }),
        Err(Signal::Break) => Err(RunError {
            message: "unexpected `break` outside a loop".to_string(),
            trace: vec![],
        }),
        Err(Signal::Continue) => Err(RunError {
            message: "unexpected `continue` outside a loop".to_string(),
            trace: vec![],
        }),
    }
}

// ── Persistent REPL state ─────────────────────────────────────────────────────

/// Interpreter state that persists across REPL lines.
///
/// Variable bindings, action definitions, and object declarations accumulate
/// here so that `var x = 1` on line 1 is still visible on line 2.
pub struct ReplState {
    interner: Arc<SymbolInterner>,
    env: Env,
    functions: HashMap<Symbol, FuncDef>,
    ext_actions: HashMap<Symbol, HashMap<Symbol, FuncDef>>,
    classes: HashMap<Symbol, ClassDef>,
    sym_initialize: Symbol,
}

/// Create a fresh [`ReplState`] for a new REPL session.
pub fn new_repl_state(interner: Arc<SymbolInterner>) -> ReplState {
    let sym_initialize = interner.intern("initialize");
    ReplState {
        interner,
        env: Env::new(),
        functions: HashMap::new(),
        ext_actions: HashMap::new(),
        classes: HashMap::new(),
        sym_initialize,
    }
}

/// Execute one REPL line against the persistent [`ReplState`].
///
/// Declarations and variable bindings from previous calls remain visible.
/// State is written back even if the line produces a runtime error.
///
/// Returns `Ok(Some(display_string))` when the last item was a bare expression
/// with a non-`Nothing` result — the caller should print that string.
/// Returns `Ok(None)` for declarations, assignments, or `Nothing` results.
pub fn run_repl_line(state: &mut ReplState, module: &Module) -> Result<Option<String>, String> {
    // Swap persistent state into a temporary interpreter bound to this module.
    let mut interp = Interpreter {
        arena: &module.arena,
        interner: Arc::clone(&state.interner),
        functions: std::mem::take(&mut state.functions),
        ext_actions: std::mem::take(&mut state.ext_actions),
        classes: std::mem::take(&mut state.classes),
        env: std::mem::take(&mut state.env),
        sym_initialize: state.sym_initialize,
    };
    // Register new top-level declarations from this line.
    interp.register_module(module);
    let result = interp.run_module_repl(module);
    // Write back persistent state regardless of success/failure.
    state.functions = interp.functions;
    state.ext_actions = interp.ext_actions;
    state.classes = interp.classes;
    state.env = interp.env;
    match result {
        Ok(maybe_val) => {
            let echo = match maybe_val {
                Some(FidanValue::Nothing) | None => None,
                Some(v) => Some(builtins::display(&v)),
            };
            Ok(echo)
        }
        Err(Signal::Return(_)) => Ok(None),
        Err(Signal::Panic { value, .. }) => Err(format!("runtime panic: {}", builtins::display(&value))),
        Err(Signal::Break) => Err("unexpected `break` outside a loop".to_string()),
        Err(Signal::Continue) => Err("unexpected `continue` outside a loop".to_string()),
    }
}
