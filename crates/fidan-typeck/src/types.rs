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
    Handle,
    Nothing,
    Dynamic,
    // Composite
    List(Box<FidanType>),
    Dict(Box<FidanType>, Box<FidanType>),
    /// Tuple: `(T1, T2, ...)`.  Empty vec = untyped/flexible tuple.
    Tuple(Vec<FidanType>),
    // User types
    Object(Symbol),
    /// Simple enumeration type declared with `enum Name { Variant, ... }`.
    Enum(Symbol),
    /// First-class reference to a class type itself (not an instance).
    /// `var b = Animal` gives `b` type `ClassType("Animal")`.
    ClassType(Symbol),
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
    pub fn is_nothing(&self) -> bool {
        matches!(self, FidanType::Nothing)
    }

    /// `Dynamic` accepts any type (like TypeScript's `any`).
    pub fn is_dynamic(&self) -> bool {
        matches!(self, FidanType::Dynamic)
    }

    /// Error/Unknown suppress cascading diagnostics.
    pub fn is_error(&self) -> bool {
        matches!(self, FidanType::Error | FidanType::Unknown)
    }

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
        if matches!(
            (self, other),
            (FidanType::Float, FidanType::Integer) | (FidanType::Integer, FidanType::Float)
        ) {
            return true;
        }
        // Parameterized types — covariant (List<Dynamic> accepts List<T>, etc.)
        match (self, other) {
            (FidanType::List(s), FidanType::List(o)) => s.is_assignable_from(o),
            (FidanType::Dict(sk, sv), FidanType::Dict(ok, ov)) => {
                sk.is_assignable_from(ok) && sv.is_assignable_from(ov)
            }
            (FidanType::Shared(s), FidanType::Shared(o)) => s.is_assignable_from(o),
            (FidanType::Pending(s), FidanType::Pending(o)) => s.is_assignable_from(o),
            (FidanType::Tuple(st), FidanType::Tuple(ot)) => {
                // Empty tuple on either side = untyped — allow assignment.
                if st.is_empty() || ot.is_empty() {
                    return true;
                }
                st.len() == ot.len()
                    && st
                        .iter()
                        .zip(ot.iter())
                        .all(|(a, b)| a.is_assignable_from(b))
            }
            _ => false,
        }
    }

    /// Human-readable name for use in diagnostic messages.
    ///
    /// Takes `&dyn Fn` to avoid monomorphisation recursion when nested types
    /// call back into the same function.
    pub fn display_name(
        &self,
        resolve: &dyn Fn(Symbol) -> std::string::String,
    ) -> std::string::String {
        match self {
            FidanType::Integer => "integer".into(),
            FidanType::Float => "float".into(),
            FidanType::Boolean => "boolean".into(),
            FidanType::String => "string".into(),
            FidanType::Handle => "handle".into(),
            FidanType::Nothing => "nothing".into(),
            FidanType::Dynamic => "dynamic".into(),
            FidanType::Function => "action".into(),
            FidanType::Unknown => "?".into(),
            FidanType::Error => "<error>".into(),
            FidanType::List(inner) => format!("list oftype {}", inner.display_name(resolve)),
            FidanType::Dict(k, v) => format!(
                "dict oftype {} oftype {}",
                k.display_name(resolve),
                v.display_name(resolve)
            ),
            FidanType::Tuple(elems) => {
                if elems.is_empty() {
                    "tuple".into()
                } else {
                    let inner: Vec<String> =
                        elems.iter().map(|t| t.display_name(resolve)).collect();
                    format!("({})", inner.join(", "))
                }
            }
            FidanType::Shared(inner) => format!("Shared oftype {}", inner.display_name(resolve)),
            FidanType::Pending(inner) => format!("Pending oftype {}", inner.display_name(resolve)),
            FidanType::Object(sym) => resolve(*sym),
            FidanType::Enum(sym) => resolve(*sym),
            FidanType::ClassType(sym) => format!("class<{}>", resolve(*sym)),
        }
    }
}

impl fmt::Display for FidanType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}",
            self.display_name(&|sym| format!("object#{}", sym.0))
        )
    }
}
