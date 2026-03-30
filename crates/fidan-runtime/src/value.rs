use std::sync::Arc;

use crate::parallel::FidanPending;
use crate::{FidanDict, FidanList, FidanObject, FidanString, OwnedRef, SharedRef};

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
    Handle(usize),
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
    /// A value being computed asynchronously (`spawn` expression).
    Pending(FidanPending),
    /// Interpreter-only same-thread deferred task handle.
    PendingTask(u64),
    /// A stdlib module namespace (e.g. `io`, `math`).
    /// Method calls on this value are routed to `fidan_stdlib::dispatch_stdlib`.
    Namespace(Arc<str>),
    /// A first-class reference to a stdlib function (e.g. `use std.io.{readFile}`).
    /// `StdlibFn(module, name)` — callable via `Callee::Dynamic` or directly displayed.
    StdlibFn(Arc<str>, Arc<str>),
    /// An enum type namespace (e.g. `Direction` itself — `Direction.North` is a field access).
    EnumType(Arc<str>),
    /// A concrete enum variant value (e.g. the result of `Direction.North`).
    /// `payload` is empty for unit variants and holds associated values for data variants.
    EnumVariant {
        tag: Arc<str>,
        payload: Vec<FidanValue>,
    },
    /// A first-class reference to a class type (e.g. `Animal` used as a value).
    ClassType(Arc<str>),
    /// A lazy integer range produced by `a..b` or `a...b`.
    /// Iteration and indexing are performed on-the-fly — no heap allocation
    /// until elements are actually materialised (e.g. via `collect` or `append`).
    Range {
        start: i64,
        end: i64,
        inclusive: bool,
    },
    /// A closure: a lambda with captured outer-scope values.
    /// `captured` holds a snapshot of each captured variable at the time the
    /// closure was created.  At call time the interpreter prepends these to
    /// the explicit arguments.
    Closure {
        fn_id: FunctionId,
        captured: Vec<FidanValue>,
    },
}

impl FidanValue {
    pub fn type_name(&self) -> &'static str {
        match self {
            FidanValue::Integer(_) => "integer",
            FidanValue::Float(_) => "float",
            FidanValue::Boolean(_) => "boolean",
            FidanValue::Handle(_) => "handle",
            FidanValue::Nothing => "nothing",
            FidanValue::String(_) => "string",
            FidanValue::List(_) => "list",
            FidanValue::Dict(_) => "dict",
            FidanValue::Object(_) => "object",
            FidanValue::Shared(_) => "Shared",
            FidanValue::Function(_) => "action",
            FidanValue::Closure { .. } => "action",
            FidanValue::Tuple(_) => "tuple",
            FidanValue::Pending(_) | FidanValue::PendingTask(_) => "pending",
            FidanValue::Namespace(_) => "namespace",
            FidanValue::StdlibFn(_, _) => "action",
            FidanValue::EnumType(_) => "enum-type",
            FidanValue::EnumVariant { .. } => "enum",
            FidanValue::ClassType(_) => "class-type",
            FidanValue::Range { .. } => "range",
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
            FidanValue::Handle(h) => *h != 0,
            FidanValue::String(s) => !s.is_empty(),
            // A Range is truthy when it contains at least one element.
            FidanValue::Range {
                start,
                end,
                inclusive,
            } => {
                if *inclusive {
                    start <= end
                } else {
                    start < end
                }
            }
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
            FidanValue::Integer(n) => FidanValue::Integer(*n),
            FidanValue::Float(f) => FidanValue::Float(*f),
            FidanValue::Boolean(b) => FidanValue::Boolean(*b),
            FidanValue::Handle(h) => FidanValue::Handle(*h),
            FidanValue::Nothing => FidanValue::Nothing,
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
            FidanValue::PendingTask(id) => FidanValue::PendingTask(*id),

            // Recurse per element.
            FidanValue::Tuple(elems) => {
                FidanValue::Tuple(elems.iter().map(|e| e.parallel_capture()).collect())
            }

            // Namespace is stateless — just clone the Arc<str>.
            FidanValue::Namespace(m) => FidanValue::Namespace(Arc::clone(m)),

            // StdlibFn is stateless — clone both Arc<str> pointers.
            FidanValue::StdlibFn(module, name) => {
                FidanValue::StdlibFn(Arc::clone(module), Arc::clone(name))
            }

            // EnumType and EnumVariant: clone Arc<str> and deep-clone payload.
            FidanValue::EnumType(s) => FidanValue::EnumType(Arc::clone(s)),
            FidanValue::EnumVariant { tag, payload } => FidanValue::EnumVariant {
                tag: Arc::clone(tag),
                payload: payload.iter().map(|v| v.parallel_capture()).collect(),
            },
            // ClassType is stateless — clone the Arc<str>.
            FidanValue::ClassType(s) => FidanValue::ClassType(Arc::clone(s)),

            // Range is plain data — copy start/end/inclusive.
            FidanValue::Range {
                start,
                end,
                inclusive,
            } => FidanValue::Range {
                start: *start,
                end: *end,
                inclusive: *inclusive,
            },

            // Recurse on each captured element.
            FidanValue::Closure { fn_id, captured } => FidanValue::Closure {
                fn_id: *fn_id,
                captured: captured.iter().map(|v| v.parallel_capture()).collect(),
            },
        }
    }
}

/// Canonical string representation of any `FidanValue`.
///
/// This is the single source of truth — `fidan-interp::builtins::display` and
/// `fidan-stdlib::io::format_val` both delegate here so the output is consistent.
pub fn display(val: &FidanValue) -> String {
    match val {
        FidanValue::Integer(n) => n.to_string(),
        FidanValue::Float(f) => {
            if f.fract() == 0.0 {
                format!("{:.1}", f)
            } else {
                f.to_string()
            }
        }
        FidanValue::Boolean(b) => b.to_string(),
        FidanValue::Handle(h) => format!("handle({h:#x})"),
        FidanValue::Nothing => "nothing".to_string(),
        FidanValue::String(s) => s.as_str().to_string(),
        FidanValue::List(l) => {
            let items: Vec<String> = l.borrow().iter().map(display).collect();
            format!("[{}]", items.join(", "))
        }
        FidanValue::Dict(d) => {
            let borrowed = d.borrow();
            // If the dict has a "__class__" entry it's an AOT object — display as <ClassName>.
            let class_key = FidanString::new("__class__");
            if let Some(FidanValue::String(cn)) = borrowed.get(&class_key) {
                return format!("<{}>", cn.as_str());
            }
            let pairs: Vec<String> = borrowed
                .iter()
                .map(|(k, v)| format!("{}: {}", k.as_str(), display(v)))
                .collect();
            format!("{{{}}}", pairs.join(", "))
        }
        FidanValue::Tuple(items) => {
            let parts: Vec<String> = items.iter().map(display).collect();
            format!("({})", parts.join(", "))
        }
        FidanValue::Object(o) => {
            let name = o.borrow().class.name_str.clone();
            format!("<{}>", name)
        }
        FidanValue::Shared(s) => {
            let inner = s.0.lock().unwrap();
            format!("Shared({})", display(&inner))
        }
        FidanValue::Pending(_) | FidanValue::PendingTask(_) => "<pending>".to_string(),
        FidanValue::Function(id) => format!("<action#{}>", id.0),
        FidanValue::Closure { fn_id, .. } => format!("<action#{}>", fn_id.0),
        FidanValue::Namespace(m) => format!("<module:{}>", m),
        FidanValue::StdlibFn(module, name) => format!("<action:{}.{}>", module, name),
        FidanValue::EnumType(s) => format!("<enum:{}>", s),
        FidanValue::EnumVariant { tag, payload } => {
            if payload.is_empty() {
                tag.as_ref().to_string()
            } else {
                let args: Vec<String> = payload.iter().map(display).collect();
                format!("{}({})", tag, args.join(", "))
            }
        }
        FidanValue::ClassType(s) => format!("<class:{}>", s),
        FidanValue::Range {
            start,
            end,
            inclusive,
        } => {
            if *inclusive {
                format!("{}...{}", start, end)
            } else {
                format!("{}..{}", start, end)
            }
        }
    }
}
