#![allow(dead_code)]
use crate::types::FidanType;
use fidan_lexer::Symbol;
use fidan_source::Span;
use rustc_hash::FxHashMap;

#[derive(Debug, Clone, PartialEq)]
pub enum ConstValue {
    Bool(bool),
    Int(i64),
    Float(f64),
    String(String),
    Nothing,
}

/// The kind of scope currently active.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScopeKind {
    Module,
    Object,
    Action,
    Block,
}

/// Initialization state of a variable (for definite-assignment analysis).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Initialized {
    /// Definitely assigned at this point.
    Yes,
    /// Declared but never assigned (holds `nothing`).
    No,
    /// Assigned on some control-flow paths only.
    Maybe,
}

/// What namespace a symbol inhabits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SymbolKind {
    Var,
    Action,
    Object,
    Param,
    Field,
    BuiltinAction,
}

/// Everything the type checker knows about a single named symbol.
#[derive(Debug, Clone)]
pub struct SymbolInfo {
    pub kind: SymbolKind,
    pub ty: FidanType,
    pub span: Span,
    pub is_mutable: bool,
    pub initialized: Initialized,
    pub const_value: Option<ConstValue>,
}

/// One lexical scope level.
#[derive(Debug)]
pub struct Scope {
    pub symbols: FxHashMap<Symbol, SymbolInfo>,
    pub kind: ScopeKind,
}

impl Scope {
    pub fn new(kind: ScopeKind) -> Self {
        Self {
            symbols: FxHashMap::default(),
            kind,
        }
    }
}

/// Lexical scope stack.  The last element is the innermost (current) scope.
#[derive(Debug)]
pub struct SymbolTable {
    scopes: Vec<Scope>,
}

impl SymbolTable {
    pub fn new() -> Self {
        Self {
            scopes: vec![Scope::new(ScopeKind::Module)],
        }
    }

    pub fn push_scope(&mut self, kind: ScopeKind) {
        self.scopes.push(Scope::new(kind));
    }

    pub fn pop_scope(&mut self) {
        if self.scopes.len() > 1 {
            self.scopes.pop();
        }
    }

    /// Check if `name` is already declared in the current (innermost) scope only.
    pub fn lookup_current_scope(&self, name: Symbol) -> Option<&SymbolInfo> {
        self.scopes.last().and_then(|s| s.symbols.get(&name))
    }

    /// Define a symbol in the current (innermost) scope.
    pub fn define(&mut self, name: Symbol, info: SymbolInfo) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.symbols.insert(name, info);
        }
    }

    /// Remove a symbol from the current (innermost) scope, returning it if found.
    /// Used to temporarily hide a variable while evaluating its own initializer,
    /// so that self-referential declarations like `var x = x + 1` are caught.
    pub fn remove_from_current_scope(&mut self, name: Symbol) -> Option<SymbolInfo> {
        self.scopes.last_mut()?.symbols.remove(&name)
    }

    /// Look up a symbol, walking from innermost to outermost scope.
    pub fn lookup(&self, name: Symbol) -> Option<&SymbolInfo> {
        for scope in self.scopes.iter().rev() {
            if let Some(info) = scope.symbols.get(&name) {
                return Some(info);
            }
        }
        None
    }

    /// Mark an existing symbol as definitely initialized (used after assignment).
    pub fn mark_initialized(&mut self, name: Symbol) {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(info) = scope.symbols.get_mut(&name) {
                info.initialized = Initialized::Yes;
                return;
            }
        }
    }

    pub fn current_kind(&self) -> ScopeKind {
        self.scopes
            .last()
            .map(|s| s.kind)
            .unwrap_or(ScopeKind::Module)
    }

    /// Iterate over every `Symbol` key defined in any scope level.
    ///
    /// Used by the fix engine to find "did you mean" candidates when a name
    /// cannot be resolved.
    pub fn all_names(&self) -> impl Iterator<Item = Symbol> + '_ {
        self.scopes.iter().flat_map(|s| s.symbols.keys().copied())
    }
}
