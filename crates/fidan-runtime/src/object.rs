use crate::{FidanValue};
use fidan_lexer::Symbol;
use std::collections::HashMap;
use std::sync::Arc;

#[derive(Debug, Clone)]
pub struct FieldDef {
    pub name:  Symbol,
    pub index: usize,
}

#[derive(Debug)]
pub struct FidanClass {
    pub name:    Symbol,
    pub parent:  Option<Arc<FidanClass>>,
    pub fields:  Vec<FieldDef>,
    pub methods: HashMap<Symbol, crate::FunctionId>,
}

impl FidanClass {
    pub fn find_method(&self, name: Symbol) -> Option<crate::FunctionId> {
        if let Some(&id) = self.methods.get(&name) { return Some(id); }
        self.parent.as_ref()?.find_method(name)
    }
}

#[derive(Debug, Clone)]
pub struct FidanObject {
    pub class:  Arc<FidanClass>,
    pub fields: Vec<FidanValue>,
}

impl FidanObject {
    pub fn new(class: Arc<FidanClass>) -> Self {
        let len = class.fields.len();
        FidanObject { class, fields: vec![FidanValue::Nothing; len] }
    }
    pub fn get_field(&self, name: Symbol) -> Option<&FidanValue> {
        let idx = self.class.fields.iter().find(|f| f.name == name)?.index;
        self.fields.get(idx)
    }
    pub fn set_field(&mut self, name: Symbol, value: FidanValue) {
        if let Some(f) = self.class.fields.iter().find(|f| f.name == name) {
            self.fields[f.index] = value;
        }
    }
}
