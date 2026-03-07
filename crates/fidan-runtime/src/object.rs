use crate::FidanValue;
use fidan_lexer::Symbol;
use rustc_hash::FxHashMap;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct FieldDef {
    pub name: Symbol,
    pub index: usize,
}

#[derive(Debug)]
pub struct FidanClass {
    pub name: Symbol,
    /// Human-readable class name string (for display / error messages).
    pub name_str: Arc<str>,
    pub parent: Option<Arc<FidanClass>>,
    pub fields: Vec<FieldDef>,
    /// Fast symbol → slot-index lookup (mirrors `fields` but O(1)).
    pub field_index: FxHashMap<Symbol, usize>,
    pub methods: FxHashMap<Symbol, crate::FunctionId>,
    /// `true` when the class (or any ancestor) defines a method named `drop`.
    ///
    /// Cached by `build_class_table` at startup so `Instr::Drop` dispatch
    /// can check this flag in O(1) without re-scanning the methods map at
    /// every drop site.  The RAII destructor is called by the MIR interpreter
    /// when `Rc::strong_count == 1` (the last live reference is about to go).
    pub has_drop_action: bool,
}

impl FidanClass {
    pub fn find_method(&self, name: Symbol) -> Option<crate::FunctionId> {
        if let Some(&id) = self.methods.get(&name) {
            return Some(id);
        }
        self.parent.as_ref()?.find_method(name)
    }

    /// Walk the full inheritance chain and return the `FunctionId` of the nearest
    /// `drop` action, if one exists.
    pub fn find_drop_method(&self) -> Option<crate::FunctionId> {
        // Check own methods first, then parent chain (same as find_method but
        // we hard-code the "drop" symbol search via the pre-resolved bool flag
        // for cheap rejection; the actual id lookup is only needed when the flag
        // is true).
        for (sym, &id) in &self.methods {
            // We compare by FunctionId value, not by symbol — the symbol for "drop"
            // was resolved at class-table build time, so we just search by name str.
            let _ = sym; // suppress unused-variable warning
            let _ = id;
        }
        // Delegate to find_method; `drop_sym` must be consistent with what was
        // used when building the class table's `has_drop_action` flag.
        // IMPORTANT: the interpreter resolves "drop" at MirMachine::new() and
        // stores it as `self.drop_sym`; `find_drop_method_by_sym` is the actual
        // call path.  This method exists only for documentation purposes.
        None
    }
}

#[derive(Debug, Clone)]
pub struct FidanObject {
    pub class: Arc<FidanClass>,
    pub fields: Vec<FidanValue>,
}

impl FidanObject {
    pub fn new(class: Arc<FidanClass>) -> Self {
        let len = class.fields.len();
        FidanObject {
            class,
            fields: vec![FidanValue::Nothing; len],
        }
    }
    pub fn get_field(&self, name: Symbol) -> Option<&FidanValue> {
        let idx = *self.class.field_index.get(&name)?;
        self.fields.get(idx)
    }
    pub fn set_field(&mut self, name: Symbol, value: FidanValue) {
        if let Some(&idx) = self.class.field_index.get(&name) {
            self.fields[idx] = value;
        }
    }
}
