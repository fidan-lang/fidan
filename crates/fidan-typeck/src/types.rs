use fidan_lexer::Symbol;
use std::fmt;

/// The compile-time type representation for Fidan values.
///
/// All types in Fidan are nullable — `Nothing` is assignable to any type.
/// Type errors arise when operating on a possibly-Nothing value without a guard.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum FidanType {
    // Primitives
    Integer,
    Float,
    Boolean,
    String,
    Nothing,
    Dynamic,
    // Composite
    List(Box<FidanType>),
    Dict(Box<FidanType>, Box<FidanType>),
    // User types
    Object(Symbol),
    // Concurrency wrappers
    Shared(Box<FidanType>),
    Pending(Box<FidanType>),
    // First-class action type (future)
    Function,
    // Inference placeholder — eliminated after type-checking a scope
    Unknown,
    // Propagated from parse errors — suppresses cascading diagnostics
    Error,
}

impl FidanType {
    /// `Nothing` is the Fidan null type.
    pub fn is_nothing(&self) -> bool { matches!(self, FidanType::Nothing) }

    /// `Dynamic` accepts any type (like TypeScript's `any`).
    pub fn is_dynamic(&self) -> bool { matches!(self, FidanType::Dynamic) }

    /// Error/Unknown suppress cascading diagnostics.
    pub fn is_error(&self) -> bool { matches!(self, FidanType::Error | FidanType::Unknown) }

    /// Returns true if a value of type `other` can be assigned to a slot of type `self`.
    ///
    /// Rules:
    /// - Same type: always OK.
    /// - `Nothing` → any type: OK (all types are nullable).
    /// - Any type → `Dynamic`: OK.
    /// - Integer ↔ Float: coercion allowed.
    /// - Error/Unknown in either position: suppressed (returns true) to avoid cascades.
    pub fn is_assignable_from(&self, other: &FidanType) -> bool {
        if self == other {
            return true;
        }
        if other.is_nothing() {
            return true; // Nothing is the universal null, assignable anywhere
        }
        if self.is_dynamic() || other.is_dynamic() {
            return true;
        }
        if self.is_error() || other.is_error() {
            return true; // suppress cascading errors
        }
        // Numeric coercions
        matches!(
            (self, other),
            (FidanType::Float, FidanType::Integer) | (FidanType::Integer, FidanType::Float)
        )
    }

    /// Human-readable name for use in diagnostic messages.
    ///
    /// Takes `&dyn Fn` to avoid monomorphisation recursion when nested types
    /// call back into the same function.
    pub fn display_name(&self, resolve: &dyn Fn(Symbol) -> std::string::String) -> std::string::String {
        match self {
            FidanType::Integer  => "integer".into(),
            FidanType::Float    => "float".into(),
            FidanType::Boolean  => "boolean".into(),
            FidanType::String   => "string".into(),
            FidanType::Nothing  => "nothing".into(),
            FidanType::Dynamic  => "dynamic".into(),
            FidanType::Function => "action".into(),
            FidanType::Unknown  => "?".into(),
            FidanType::Error    => "<error>".into(),
            FidanType::List(inner)    => format!("list oftype {}",                      inner.display_name(resolve)),
            FidanType::Dict(k, v)     => format!("dict oftype {} oftype {}", k.display_name(resolve), v.display_name(resolve)),
            FidanType::Shared(inner)  => format!("Shared oftype {}",                   inner.display_name(resolve)),
            FidanType::Pending(inner) => format!("Pending oftype {}",                  inner.display_name(resolve)),
            FidanType::Object(sym)    => resolve(*sym),
        }
    }
}

impl fmt::Display for FidanType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.display_name(&|sym| format!("object#{}", sym.0)))
    }
}
