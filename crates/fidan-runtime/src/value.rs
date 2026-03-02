use std::sync::Arc;

use crate::{FidanDict, FidanList, FidanObject, FidanString, OwnedRef, SharedRef};
use crate::parallel::FidanPending;

/// Opaque function identifier — same as fidan-mir's FunctionId but re-exported here
/// so fidan-runtime doesn't depend on fidan-mir (no circular dep).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FunctionId(pub u32);

/// The universal Fidan value type used in the interpreter.
///
/// In AOT mode, this is replaced by typed native LLVM values.
///
/// ## Memory model
/// - Primitives (`Integer`, `Float`, `Boolean`, `Nothing`) are always **copied**.
/// - `String`, `List`, `Dict` are Copy-on-Write: cheap to clone, copy on mutation.
/// - `Object` is owned by an `OwnedRef<T>` (interpreter-internal Rc<RefCell<T>>).
/// - `Shared` is the only variant backed by `Arc<Mutex<T>>` — explicit opt-in.
#[derive(Debug, Clone)]
pub enum FidanValue {
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Nothing,
    String(FidanString),
    List(OwnedRef<FidanList>),
    Dict(OwnedRef<FidanDict>),
    Object(OwnedRef<FidanObject>),
    /// `Shared oftype T` — explicit ARC, cross-thread safe.
    Shared(SharedRef<FidanValue>),
    Function(FunctionId),
    /// Tuple: `(v1, v2, ...)`
    Tuple(Vec<FidanValue>),
    /// A value being computed on a background thread (`spawn` expression).
    Pending(FidanPending),
    /// A stdlib module namespace (e.g. `io`, `math`).
    /// Method calls on this value are routed to `fidan_stdlib::dispatch_stdlib`.
    Namespace(Arc<str>),
}

impl FidanValue {
    pub fn type_name(&self) -> &'static str {
        match self {
            FidanValue::Integer(_) => "integer",
            FidanValue::Float(_) => "float",
            FidanValue::Boolean(_) => "boolean",
            FidanValue::Nothing => "nothing",
            FidanValue::String(_) => "string",
            FidanValue::List(_) => "list",
            FidanValue::Dict(_) => "dict",
            FidanValue::Object(_) => "object",
            FidanValue::Shared(_) => "Shared",
            FidanValue::Function(_) => "action",
            FidanValue::Tuple(_) => "tuple",
            FidanValue::Pending(_) => "pending",
            FidanValue::Namespace(_) => "namespace",
        }
    }

    pub fn is_nothing(&self) -> bool {
        matches!(self, Self::Nothing)
    }

    pub fn truthy(&self) -> bool {
        match self {
            FidanValue::Boolean(b) => *b,
            FidanValue::Nothing => false,
            FidanValue::Integer(n) => *n != 0,
            FidanValue::Float(f) => *f != 0.0,
            FidanValue::String(s) => !s.is_empty(),
            _ => true,
        }
    }

    /// Create a version of this value safe to send into a parallel task.
    ///
    /// - **Primitives / String / Function** — cheap bit-copy or Arc bump.
    /// - **List / Dict** — new `Rc<RefCell<T>>` wrapping the *shared* inner
    ///   `Arc<Vec>` / `Arc<HashMap>`.  No data is copied until mutation (CoW).
    /// - **Object** — each field is recursively captured; `Arc<FidanClass>`
    ///   metadata is shared.
    /// - **Shared** — `Arc<Mutex<T>>` is intentionally shared across threads.
    /// - **Pending** — clones the `Arc<Mutex<JoinHandle>>` pointer.
    /// - **Tuple** — recurse per element.
    pub fn parallel_capture(&self) -> FidanValue {
        match self {
            FidanValue::Integer(n)   => FidanValue::Integer(*n),
            FidanValue::Float(f)     => FidanValue::Float(*f),
            FidanValue::Boolean(b)   => FidanValue::Boolean(*b),
            FidanValue::Nothing      => FidanValue::Nothing,
            FidanValue::Function(id) => FidanValue::Function(*id),

            // Arc<str> — single atomic refcount bump, no data copy.
            FidanValue::String(s) => FidanValue::String(s.clone()),

            // New Rc+RefCell wrapping the *same* inner Arc<Vec> (CoW preserved).
            FidanValue::List(r) => {
                let inner = r.borrow().clone(); // O(1): clones Arc<Vec>
                FidanValue::List(OwnedRef::new(inner))
            }

            // New Rc+RefCell wrapping the *same* inner Arc<HashMap> (CoW preserved).
            FidanValue::Dict(r) => {
                let inner = r.borrow().clone(); // O(1): clones Arc<HashMap>
                FidanValue::Dict(OwnedRef::new(inner))
            }

            // Field-by-field capture; Arc<FidanClass> is shared.
            FidanValue::Object(r) => {
                let obj = r.borrow();
                let fields: Vec<FidanValue> =
                    obj.fields.iter().map(|f| f.parallel_capture()).collect();
                FidanValue::Object(OwnedRef::new(FidanObject {
                    class: Arc::clone(&obj.class),
                    fields,
                }))
            }

            // Intentionally shared across threads.
            FidanValue::Shared(s) => FidanValue::Shared(s.clone()),

            // Share the Arc<Mutex<JoinHandle>>.
            FidanValue::Pending(p) => FidanValue::Pending(p.clone()),

            // Recurse per element.
            FidanValue::Tuple(elems) => {
                FidanValue::Tuple(elems.iter().map(|e| e.parallel_capture()).collect())
            }

            // Namespace is stateless — just clone the Arc<str>.
            FidanValue::Namespace(m) => FidanValue::Namespace(Arc::clone(m)),
        }
    }
}
