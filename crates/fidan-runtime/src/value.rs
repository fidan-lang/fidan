use std::sync::Arc;
use std::{fmt::Write as _, io, string::String};

use crate::parallel::FidanPending;
use crate::{
    FidanDict, FidanHashSet, FidanList, FidanObject, FidanString, OwnedRef, SharedRef,
    WeakSharedRef,
};

/// Opaque function identifier — same as fidan-mir's FunctionId but re-exported here
/// so fidan-runtime doesn't depend on fidan-mir (no circular dep).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FunctionId(pub u32);

/// Interpreter scheduler identity; IDs from another machine cannot address a
/// coincidentally equal local task ID in a parallel worker.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PendingTaskId {
    pub owner: u64,
    pub id: u64,
}

/// The universal boxed value type used by the interpreter and native runtime.
///
/// AOT uses native scalars where possible and boxed values at runtime boundaries.
///
/// ## Memory model
/// - Primitives (`Integer`, `Float`, `Boolean`, `Nothing`) are always **copied**.
/// - `String`, `List`, `Dict` are Copy-on-Write: cheap to clone, copy on mutation.
/// - `Object` is owned by an `OwnedRef<T>` (runtime Rc<RefCell<T>>).
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
    HashSet(OwnedRef<FidanHashSet>),
    Object(OwnedRef<FidanObject>),
    /// `Shared oftype T` — explicit ARC, cross-thread safe.
    Shared(SharedRef<FidanValue>),
    /// `WeakShared oftype T` — non-owning weak handle to a `Shared`.
    WeakShared(WeakSharedRef<FidanValue>),
    Function(FunctionId),
    /// Tuple: `(v1, v2, ...)`
    Tuple(Vec<FidanValue>),
    /// A value being computed asynchronously (`spawn` expression).
    Pending(FidanPending),
    /// Interpreter-only same-thread deferred task handle.
    PendingTask(PendingTaskId),
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
            FidanValue::HashSet(_) => "hashset",
            FidanValue::Object(_) => "object",
            FidanValue::Shared(_) => "Shared",
            FidanValue::WeakShared(_) => "WeakShared",
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
            FidanValue::HashSet(s) => !s.borrow().is_empty(),
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
            FidanValue::WeakShared(ws) => ws.is_alive(),
            _ => true,
        }
    }

    /// Snapshot ordinary mutable values into an independent thread-local graph.
    /// Aliases and cycles inside that graph are preserved. Only explicit Shared,
    /// pending handles and immutable Arc metadata retain cross-thread ownership.
    pub fn parallel_capture(&self) -> FidanValue {
        Self::capture_values(std::slice::from_ref(self))
            .pop()
            .unwrap()
    }

    /// Capture all worker roots together so aliases between arguments and globals survive.
    pub fn capture_values(values: &[FidanValue]) -> Vec<FidanValue> {
        let mut memo = std::collections::HashMap::new();
        let captured = values
            .iter()
            .map(|value| value.capture_local_graph(&mut memo))
            .collect();
        // Frozen structural keys retain insertion-time contents. Object identity keys
        // must instead refer to the copied object after the entire graph is built.
        let identities = memo
            .iter()
            .filter_map(|(source, value)| {
                if let FidanValue::Object(object) = value {
                    Some((*source, object.identity()))
                } else {
                    None
                }
            })
            .collect();
        for value in memo.values() {
            match value {
                FidanValue::Dict(dict) => dict.borrow_mut().remap_object_keys(&identities),
                FidanValue::HashSet(set) => set.borrow_mut().remap_object_keys(&identities),
                _ => {}
            }
        }
        captured
    }

    fn capture_local_graph(
        &self,
        memo: &mut std::collections::HashMap<usize, FidanValue>,
    ) -> FidanValue {
        let identity = match self {
            Self::List(value) => Some(value.identity()),
            Self::Dict(value) => Some(value.identity()),
            Self::HashSet(value) => Some(value.identity()),
            Self::Object(value) => Some(value.identity()),
            _ => None,
        };
        if let Some(previous) = identity.and_then(|identity| memo.get(&identity)) {
            return previous.clone();
        }
        match self {
            Self::List(source) => {
                let target = OwnedRef::new(FidanList::new());
                let value = Self::List(target.clone());
                memo.insert(source.identity(), value.clone());
                let entries = source
                    .borrow()
                    .iter()
                    .map(|item| item.capture_local_graph(memo))
                    .collect();
                *target.borrow_mut() = FidanList::from_vec(entries);
                value
            }
            Self::Dict(source) => {
                let target = OwnedRef::new(FidanDict::new());
                let value = Self::Dict(target.clone());
                memo.insert(source.identity(), value.clone());
                *target.borrow_mut() = source
                    .borrow()
                    .capture_values(|item| item.capture_local_graph(memo));
                value
            }
            Self::HashSet(source) => {
                let target = OwnedRef::new(FidanHashSet::new());
                let value = Self::HashSet(target.clone());
                memo.insert(source.identity(), value.clone());
                *target.borrow_mut() = source
                    .borrow()
                    .capture_values(|item| item.capture_local_graph(memo));
                value
            }
            Self::Object(source) => {
                let source = source.borrow();
                let target = OwnedRef::new(FidanObject {
                    class: Arc::clone(&source.class),
                    fields: Vec::new(),
                });
                let value = Self::Object(target.clone());
                memo.insert(identity.unwrap(), value.clone());
                target.borrow_mut().fields = source
                    .fields
                    .iter()
                    .map(|item| item.capture_local_graph(memo))
                    .collect();
                value
            }
            Self::Tuple(values) => Self::Tuple(
                values
                    .iter()
                    .map(|value| value.capture_local_graph(memo))
                    .collect(),
            ),
            Self::EnumVariant { tag, payload } => Self::EnumVariant {
                tag: Arc::clone(tag),
                payload: payload
                    .iter()
                    .map(|value| value.capture_local_graph(memo))
                    .collect(),
            },
            Self::Closure { fn_id, captured } => Self::Closure {
                fn_id: *fn_id,
                captured: captured
                    .iter()
                    .map(|value| value.capture_local_graph(memo))
                    .collect(),
            },
            // Keep this exhaustive: a new Rc-backed variant must choose an
            // explicit capture policy before it can cross an unsafe Send boundary.
            Self::Integer(_)
            | Self::Float(_)
            | Self::Boolean(_)
            | Self::Handle(_)
            | Self::Nothing
            | Self::String(_)
            | Self::Shared(_)
            | Self::WeakShared(_)
            | Self::Function(_)
            | Self::Pending(_)
            | Self::PendingTask(_)
            | Self::Namespace(_)
            | Self::StdlibFn(..)
            | Self::EnumType(_)
            | Self::ClassType(_)
            | Self::Range { .. } => self.clone(),
        }
    }
}

/// Canonical string representation of any `FidanValue`.
///
/// This is the single source of truth — `fidan-interp::builtins::display` and
/// `fidan-stdlib::io::format_val` both delegate here so the output is consistent.
pub fn display(val: &FidanValue) -> String {
    let mut out = String::new();
    display_into(&mut out, val);
    out
}

/// Conventional payload field for a user-defined exception's human-readable text.
pub const EXCEPTION_MESSAGE_FIELD: &str = "message";

/// Render an uncaught exception without changing ordinary value display or its
/// catchable payload. The interpreter supplies its interned field identifier;
/// native objects use the existing branded, string-keyed representation.
pub fn display_exception(value: &FidanValue, message_field: Option<fidan_lexer::Symbol>) -> String {
    let message = match value {
        FidanValue::Object(object) => {
            message_field.and_then(|field| match object.borrow().get_field(field) {
                Some(FidanValue::String(message)) => Some(message.clone()),
                _ => None,
            })
        }
        FidanValue::Dict(dictionary) => {
            let dictionary = dictionary.borrow();
            if dictionary.aot_class_name().is_some() {
                dictionary
                    .iter()
                    .find_map(|(key, value)| match (key, value) {
                        (FidanValue::String(key), FidanValue::String(message))
                            if key.as_str() == EXCEPTION_MESSAGE_FIELD =>
                        {
                            Some(message.clone())
                        }
                        _ => None,
                    })
            } else {
                None
            }
        }
        _ => None,
    };
    match message {
        Some(message) => format!("{}: {}", display(value), message.as_str()),
        None => display(value),
    }
}

pub fn write_display_io<W: io::Write>(out: &mut W, val: &FidanValue) -> io::Result<()> {
    match val {
        FidanValue::Integer(n) => {
            let mut buffer = itoa::Buffer::new();
            out.write_all(buffer.format(*n).as_bytes())
        }
        FidanValue::Float(f) => {
            if f.fract() == 0.0 {
                write!(out, "{f:.1}")
            } else {
                write!(out, "{f}")
            }
        }
        FidanValue::Boolean(true) => out.write_all(b"true"),
        FidanValue::Boolean(false) => out.write_all(b"false"),
        FidanValue::Handle(h) => write!(out, "handle({h:#x})"),
        FidanValue::Nothing => out.write_all(b"nothing"),
        FidanValue::String(s) => out.write_all(s.as_str().as_bytes()),
        FidanValue::List(l) => {
            out.write_all(b"[")?;
            for (index, item) in l.borrow().iter().enumerate() {
                if index > 0 {
                    out.write_all(b", ")?;
                }
                write_display_io(out, item)?;
            }
            out.write_all(b"]")
        }
        FidanValue::Dict(d) => {
            let borrowed = d.borrow();
            if let Some(cn) = borrowed.aot_class_name() {
                out.write_all(b"<")?;
                out.write_all(cn.as_bytes())?;
                return out.write_all(b">");
            }
            out.write_all(b"{")?;
            for (index, (key, value)) in borrowed.entries_sorted_refs().into_iter().enumerate() {
                if index > 0 {
                    out.write_all(b", ")?;
                }
                write_display_io(out, key)?;
                out.write_all(b": ")?;
                write_display_io(out, value)?;
            }
            out.write_all(b"}")
        }
        FidanValue::HashSet(set) => {
            out.write_all(b"hashset({")?;
            for (index, value) in set.borrow().values_sorted_refs().into_iter().enumerate() {
                if index > 0 {
                    out.write_all(b", ")?;
                }
                write_display_io(out, value)?;
            }
            out.write_all(b"})")
        }
        FidanValue::Tuple(items) => {
            out.write_all(b"(")?;
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.write_all(b", ")?;
                }
                write_display_io(out, item)?;
            }
            out.write_all(b")")
        }
        FidanValue::Object(o) => {
            let name = o.borrow().class.name_str.clone();
            out.write_all(b"<")?;
            out.write_all(name.as_bytes())?;
            out.write_all(b">")
        }
        FidanValue::Shared(s) => {
            let Ok(inner) = s.lock() else {
                return out.write_all(b"Shared(<locked>)");
            };
            out.write_all(b"Shared(")?;
            write_display_io(out, &inner)?;
            out.write_all(b")")
        }
        FidanValue::WeakShared(ws) => {
            if let Some(shared) = ws.upgrade() {
                let Ok(inner) = shared.lock() else {
                    return out.write_all(b"WeakShared(<locked>)");
                };
                out.write_all(b"WeakShared(")?;
                write_display_io(out, &inner)?;
                out.write_all(b")")
            } else {
                out.write_all(b"WeakShared(<collected>)")
            }
        }
        FidanValue::Pending(_) | FidanValue::PendingTask(_) => out.write_all(b"<pending>"),
        FidanValue::Function(id) => write!(out, "<action#{}>", id.0),
        FidanValue::Closure { fn_id, .. } => write!(out, "<action#{}>", fn_id.0),
        FidanValue::Namespace(m) => write!(out, "<module:{m}>"),
        FidanValue::StdlibFn(module, name) => write!(out, "<action:{module}.{name}>"),
        FidanValue::EnumType(s) => write!(out, "<enum:{s}>"),
        FidanValue::EnumVariant { tag, payload } => {
            out.write_all(tag.as_bytes())?;
            if payload.is_empty() {
                return Ok(());
            }
            out.write_all(b"(")?;
            for (index, item) in payload.iter().enumerate() {
                if index > 0 {
                    out.write_all(b", ")?;
                }
                write_display_io(out, item)?;
            }
            out.write_all(b")")
        }
        FidanValue::ClassType(s) => write!(out, "<class:{s}>"),
        FidanValue::Range {
            start,
            end,
            inclusive,
        } => {
            let mut start_buffer = itoa::Buffer::new();
            let mut end_buffer = itoa::Buffer::new();
            out.write_all(start_buffer.format(*start).as_bytes())?;
            if *inclusive {
                out.write_all(b"...")?;
            } else {
                out.write_all(b"..")?;
            }
            out.write_all(end_buffer.format(*end).as_bytes())
        }
    }
}

pub fn display_into(out: &mut String, val: &FidanValue) {
    match val {
        FidanValue::Integer(n) => {
            let mut buffer = itoa::Buffer::new();
            out.push_str(buffer.format(*n));
        }
        FidanValue::Float(f) => {
            if f.fract() == 0.0 {
                let _ = write!(out, "{f:.1}");
            } else {
                let _ = write!(out, "{f}");
            }
        }
        FidanValue::Boolean(true) => out.push_str("true"),
        FidanValue::Boolean(false) => out.push_str("false"),
        FidanValue::Handle(h) => {
            let _ = write!(out, "handle({h:#x})");
        }
        FidanValue::Nothing => out.push_str("nothing"),
        FidanValue::String(s) => out.push_str(s.as_str()),
        FidanValue::List(l) => {
            out.push('[');
            for (index, item) in l.borrow().iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                display_into(out, item);
            }
            out.push(']');
        }
        FidanValue::Dict(d) => {
            let borrowed = d.borrow();
            // Only the runtime constructor can brand a Dict-backed native object.
            if let Some(cn) = borrowed.aot_class_name() {
                out.push('<');
                out.push_str(cn);
                out.push('>');
                return;
            }
            out.push('{');
            for (index, (key, value)) in borrowed.entries_sorted_refs().into_iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                display_into(out, key);
                out.push_str(": ");
                display_into(out, value);
            }
            out.push('}');
        }
        FidanValue::HashSet(set) => {
            out.push_str("hashset({");
            for (index, value) in set.borrow().values_sorted_refs().into_iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                display_into(out, value);
            }
            out.push_str("})");
        }
        FidanValue::Tuple(items) => {
            out.push('(');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                display_into(out, item);
            }
            out.push(')');
        }
        FidanValue::Object(o) => {
            let name = o.borrow().class.name_str.clone();
            out.push('<');
            out.push_str(&name);
            out.push('>');
        }
        FidanValue::Shared(s) => {
            let Ok(inner) = s.lock() else {
                out.push_str("Shared(<locked>)");
                return;
            };
            out.push_str("Shared(");
            display_into(out, &inner);
            out.push(')');
        }
        FidanValue::WeakShared(ws) => {
            if let Some(shared) = ws.upgrade() {
                let Ok(inner) = shared.lock() else {
                    out.push_str("WeakShared(<locked>)");
                    return;
                };
                out.push_str("WeakShared(");
                display_into(out, &inner);
                out.push(')');
            } else {
                out.push_str("WeakShared(<collected>)");
            }
        }
        FidanValue::Pending(_) | FidanValue::PendingTask(_) => out.push_str("<pending>"),
        FidanValue::Function(id) => {
            let _ = write!(out, "<action#{}>", id.0);
        }
        FidanValue::Closure { fn_id, .. } => {
            let _ = write!(out, "<action#{}>", fn_id.0);
        }
        FidanValue::Namespace(m) => {
            let _ = write!(out, "<module:{m}>");
        }
        FidanValue::StdlibFn(module, name) => {
            let _ = write!(out, "<action:{module}.{name}>");
        }
        FidanValue::EnumType(s) => {
            let _ = write!(out, "<enum:{s}>");
        }
        FidanValue::EnumVariant { tag, payload } => {
            if payload.is_empty() {
                out.push_str(tag);
            } else {
                out.push_str(tag);
                out.push('(');
                for (index, item) in payload.iter().enumerate() {
                    if index > 0 {
                        out.push_str(", ");
                    }
                    display_into(out, item);
                }
                out.push(')');
            }
        }
        FidanValue::ClassType(s) => {
            let _ = write!(out, "<class:{s}>");
        }
        FidanValue::Range {
            start,
            end,
            inclusive,
        } => {
            let mut start_buffer = itoa::Buffer::new();
            let mut end_buffer = itoa::Buffer::new();
            out.push_str(start_buffer.format(*start));
            if *inclusive {
                out.push_str("...");
            } else {
                out.push_str("..");
            }
            out.push_str(end_buffer.format(*end));
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn exception_display_only_reads_a_branded_objects_string_message() {
        let mut dictionary = FidanDict::new();
        let key = FidanValue::String(FidanString::new(EXCEPTION_MESSAGE_FIELD));
        dictionary
            .insert(key.clone(), FidanValue::String(FidanString::new("details")))
            .unwrap();
        let ordinary = FidanValue::Dict(OwnedRef::new(dictionary.clone()));
        assert_eq!(display_exception(&ordinary, None), display(&ordinary));
        dictionary.mark_aot_object(FidanString::new("Problem"));
        let object = FidanValue::Dict(OwnedRef::new(dictionary));
        assert_eq!(display(&object), "<Problem>");
        assert_eq!(display_exception(&object, None), "<Problem>: details");
        let FidanValue::Dict(fields) = &object else {
            unreachable!()
        };
        fields
            .borrow_mut()
            .insert(key, FidanValue::Integer(42))
            .unwrap();
        assert_eq!(display_exception(&object, None), "<Problem>");
        let string = FidanValue::String(FidanString::new("plain error"));
        assert_eq!(display_exception(&string, None), "plain error");
    }

    #[test]
    fn parallel_capture_detaches_nested_mutable_values() {
        let child = OwnedRef::new(FidanList::from_vec(vec![FidanValue::Integer(1)]));
        let source = FidanValue::List(OwnedRef::new(FidanList::from_vec(vec![FidanValue::List(
            child.clone(),
        )])));
        let FidanValue::List(snapshot) = source.parallel_capture() else {
            panic!("expected list")
        };
        let FidanValue::List(captured_child) = snapshot.borrow().get(0).unwrap().clone() else {
            panic!("expected nested list")
        };
        assert_ne!(
            child.identity(),
            captured_child.identity(),
            "parallel tasks must never share an Rc allocation"
        );
        captured_child.borrow_mut().append(FidanValue::Integer(2));
        assert_eq!(
            child.borrow().len(),
            1,
            "captured mutations must not change the source thread"
        );
    }

    #[test]
    fn parallel_capture_preserves_aliases_and_cycles_across_roots() {
        let list = OwnedRef::new(FidanList::new());
        list.borrow_mut().append(FidanValue::List(list.clone()));
        let values = FidanValue::capture_values(&[
            FidanValue::List(list.clone()),
            FidanValue::List(list.clone()),
        ]);
        let [FidanValue::List(first), FidanValue::List(second)] = values.as_slice() else {
            panic!("lists")
        };
        assert_eq!(first.identity(), second.identity());
        assert_ne!(first.identity(), list.identity());
        let FidanValue::List(cycle) = first.borrow().get(0).unwrap().clone() else {
            panic!("cycle")
        };
        assert_eq!(first.identity(), cycle.identity());
        // Break the deliberate cycles so the regression itself does not leak Rc graphs.
        first.borrow_mut().pop();
        list.borrow_mut().pop();
    }

    #[test]
    fn parallel_capture_preserves_frozen_collection_keys() {
        let key = OwnedRef::new(FidanList::from_vec(vec![FidanValue::Integer(1)]));
        let mut dictionary = crate::FidanDict::new();
        dictionary
            .insert(FidanValue::List(key.clone()), FidanValue::Integer(7))
            .unwrap();
        key.borrow_mut().append(FidanValue::Integer(2));
        let source = FidanValue::Dict(OwnedRef::new(dictionary));
        let FidanValue::Dict(captured) = source.parallel_capture() else {
            panic!("dict")
        };
        let frozen_key = FidanValue::List(OwnedRef::new(FidanList::from_vec(vec![
            FidanValue::Integer(1),
        ])));
        assert!(matches!(
            captured.borrow().get(&frozen_key).unwrap(),
            Some(FidanValue::Integer(7))
        ));
        assert!(
            captured
                .borrow()
                .get(&FidanValue::List(key))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn parallel_capture_retargets_object_identity_keys() {
        let class = std::sync::Arc::new(crate::FidanClass {
            name: fidan_lexer::Symbol(0),
            name_str: "Box".into(),
            parent: None,
            fields: vec![],
            field_index: Default::default(),
            methods: Default::default(),
            has_drop_action: false,
        });
        let object = FidanValue::Object(OwnedRef::new(crate::FidanObject::new(class)));
        let mut dictionary = crate::FidanDict::new();
        dictionary
            .insert(object.clone(), FidanValue::Integer(42))
            .unwrap();
        let set = crate::FidanHashSet::from_values([object.clone()]).unwrap();
        let captured = FidanValue::capture_values(&[
            object.clone(),
            FidanValue::Dict(OwnedRef::new(dictionary)),
            FidanValue::HashSet(OwnedRef::new(set)),
        ]);
        let [copied, FidanValue::Dict(dict), FidanValue::HashSet(set)] = captured.as_slice() else {
            panic!("payload")
        };
        assert!(matches!(
            dict.borrow().get(copied).unwrap(),
            Some(FidanValue::Integer(42))
        ));
        assert!(dict.borrow().get(&object).unwrap().is_none());
        assert!(set.borrow().contains(copied).unwrap());
        assert!(!set.borrow().contains(&object).unwrap());
    }

    use super::{
        EXCEPTION_MESSAGE_FIELD, FidanValue, display, display_exception, write_display_io,
    };
    use crate::{FidanDict, FidanHashSet, FidanList, FidanString, OwnedRef};

    #[test]
    fn io_display_matches_string_display_for_nested_values() {
        let mut list = FidanList::new();
        list.append(FidanValue::Integer(7));
        list.append(FidanValue::String(FidanString::new("eight")));
        let set = FidanHashSet::from_values([
            FidanValue::Integer(3),
            FidanValue::Integer(1),
            FidanValue::Integer(2),
        ])
        .expect("hashset");
        let value = FidanValue::Tuple(vec![
            FidanValue::List(OwnedRef::new(list)),
            FidanValue::HashSet(OwnedRef::new(set)),
            FidanValue::Range {
                start: 4,
                end: 6,
                inclusive: true,
            },
            FidanValue::EnumVariant {
                tag: "Ok".into(),
                payload: vec![FidanValue::Boolean(true)],
            },
        ]);

        let mut buf = Vec::new();
        write_display_io(&mut buf, &value).expect("write display");
        let io_rendered = String::from_utf8(buf).expect("utf8");
        assert_eq!(io_rendered, display(&value));
    }
}
