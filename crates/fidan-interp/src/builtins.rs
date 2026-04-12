use fidan_config::{BuiltinSemantic, builtin_semantic};
use fidan_diagnostics::{DiagCode, diag_code};
use fidan_runtime::display as runtime_display;
use fidan_runtime::{FidanHashSet, FidanString, FidanValue, OwnedRef, SharedRef};
use std::io::BufRead;

pub struct BuiltinError {
    pub code: DiagCode,
    pub message: String,
}

impl BuiltinError {
    fn runtime(message: String) -> Self {
        Self {
            code: diag_code!("R0001"),
            message,
        }
    }
}

fn invalid_conversion(target: &str, value: &FidanValue) -> BuiltinError {
    let rendered = match value {
        FidanValue::String(s) => format!("{:?}", s.as_str()),
        other => display(other),
    };
    BuiltinError::runtime(format!(
        "cannot convert {rendered} ({}) to {target}",
        value.type_name()
    ))
}

/// Try to handle a call to a core language built-in function.
///
/// These are **always** available without any `use` statement:
/// `print`, `eprint`, `input`, `string`, `integer`, `float`, `boolean`,
/// `len`, `type`, `Shared`.
///
/// All other functions (`abs`, `sqrt`, `floor`, `ceil`, `round`, `max`, `min`,
/// time utilities, etc.) require the appropriate `use std.*` import.
///
/// Returns `Ok(Some(value))` if handled, `Ok(None)` if the name is not a built-in,
/// or `Err(...)` if the built-in itself failed at runtime.
pub fn call_builtin(name: &str, args: Vec<FidanValue>) -> Result<Option<FidanValue>, BuiltinError> {
    let Some(semantic) = builtin_semantic(name) else {
        return Ok(None);
    };

    match semantic {
        // ── I/O ──────────────────────────────────────────────────────────────
        BuiltinSemantic::Print => {
            use std::io::Write as _;

            let mut stdout = std::io::stdout().lock();
            for (index, value) in args.iter().enumerate() {
                if index > 0 {
                    let _ = stdout.write_all(b" ");
                }
                let _ = fidan_runtime::write_display_io(&mut stdout, value);
            }
            let _ = stdout.write_all(b"\n");
            Ok(Some(FidanValue::Nothing))
        }
        BuiltinSemantic::Eprint => {
            use std::io::Write as _;

            let mut stderr = std::io::stderr().lock();
            for (index, value) in args.iter().enumerate() {
                if index > 0 {
                    let _ = stderr.write_all(b" ");
                }
                let _ = fidan_runtime::write_display_io(&mut stderr, value);
            }
            let _ = stderr.write_all(b"\n");
            Ok(Some(FidanValue::Nothing))
        }
        BuiltinSemantic::Input => {
            if let Some(prompt) = args.first() {
                use std::io::Write;
                let mut stdout = std::io::stdout().lock();
                let _ = fidan_runtime::write_display_io(&mut stdout, prompt);
                let _ = stdout.flush();
            }
            let stdin = std::io::stdin();
            let mut line = String::new();
            stdin
                .lock()
                .read_line(&mut line)
                .map_err(|err| BuiltinError::runtime(format!("failed to read input: {err}")))?;
            // Strip trailing newline
            if line.ends_with('\n') {
                line.pop();
                if line.ends_with('\r') {
                    line.pop();
                }
            }
            Ok(Some(FidanValue::String(FidanString::new(&line))))
        }

        // ── Type conversion ───────────────────────────────────────────────────
        BuiltinSemantic::String => {
            let v = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            Ok(Some(FidanValue::String(FidanString::new(&display(&v)))))
        }
        BuiltinSemantic::Integer => {
            let v = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            Ok(Some(match &v {
                FidanValue::Integer(n) => FidanValue::Integer(*n),
                FidanValue::Float(f) => FidanValue::Integer(*f as i64),
                FidanValue::Boolean(b) => FidanValue::Integer(if *b { 1 } else { 0 }),
                FidanValue::String(s) => s
                    .as_str()
                    .parse::<i64>()
                    .map(FidanValue::Integer)
                    .map_err(|_| invalid_conversion("integer", &v))?,
                _ => return Err(invalid_conversion("integer", &v)),
            }))
        }
        BuiltinSemantic::Float => {
            let v = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            Ok(Some(match &v {
                FidanValue::Float(f) => FidanValue::Float(*f),
                FidanValue::Integer(n) => FidanValue::Float(*n as f64),
                FidanValue::String(s) => s
                    .as_str()
                    .parse::<f64>()
                    .map(FidanValue::Float)
                    .map_err(|_| invalid_conversion("float", &v))?,
                _ => return Err(invalid_conversion("float", &v)),
            }))
        }
        BuiltinSemantic::Boolean => {
            let v = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            Ok(Some(FidanValue::Boolean(v.truthy())))
        }

        // ── Collections ───────────────────────────────────────────────────────
        BuiltinSemantic::Len => {
            let v = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            let n = match &v {
                FidanValue::String(s) => s.len() as i64,
                FidanValue::List(l) => l.borrow().len() as i64,
                FidanValue::Dict(d) => d.borrow().len() as i64,
                FidanValue::HashSet(s) => s.borrow().len() as i64,
                FidanValue::Tuple(t) => t.len() as i64,
                FidanValue::Range {
                    start,
                    end,
                    inclusive,
                } => {
                    if *inclusive {
                        (end - start + 1).max(0)
                    } else {
                        (end - start).max(0)
                    }
                }
                _ => {
                    return Err(BuiltinError::runtime(format!(
                        "len() is not supported for {}",
                        v.type_name()
                    )));
                }
            };
            Ok(Some(FidanValue::Integer(n)))
        }
        BuiltinSemantic::Type => {
            let v = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            Ok(Some(FidanValue::String(FidanString::new(v.type_name()))))
        }
        BuiltinSemantic::HashSetConstructor
        | BuiltinSemantic::SharedConstructor
        | BuiltinSemantic::WeakSharedConstructor
        | BuiltinSemantic::Assert
        | BuiltinSemantic::AssertEq
        | BuiltinSemantic::AssertNe => Ok(None),
    }
}

/// Try to handle a call to a builtin type constructor (e.g. `Shared(val)`).
pub fn call_builtin_constructor(
    name: &str,
    args: Vec<FidanValue>,
) -> Result<Option<FidanValue>, BuiltinError> {
    match builtin_semantic(name) {
        Some(BuiltinSemantic::HashSetConstructor) => {
            let source = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            let set = match source {
                FidanValue::Nothing => FidanHashSet::new(),
                FidanValue::List(list) => FidanHashSet::from_values(list.borrow().iter().cloned())
                    .map_err(|err| BuiltinError::runtime(err.to_string()))?,
                FidanValue::HashSet(existing) => existing.borrow().clone(),
                other => {
                    return Err(BuiltinError::runtime(format!(
                        "hashset(items) expects a list or hashset, got {}",
                        other.type_name()
                    )));
                }
            };
            Ok(Some(FidanValue::HashSet(OwnedRef::new(set))))
        }
        Some(BuiltinSemantic::SharedConstructor) => {
            let inner = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            Ok(Some(FidanValue::Shared(SharedRef::new(inner))))
        }
        Some(BuiltinSemantic::WeakSharedConstructor) => {
            let inner = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            match inner {
                FidanValue::Shared(shared) => Ok(Some(FidanValue::WeakShared(shared.downgrade()))),
                FidanValue::WeakShared(weak) => Ok(Some(FidanValue::WeakShared(weak))),
                other => Err(BuiltinError::runtime(format!(
                    "WeakShared(shared) expects a Shared value, got {}",
                    other.type_name()
                ))),
            }
        }
        _ => Ok(None),
    }
}

/// Format a `FidanValue` as a human-readable string (used by `print` and
/// string interpolation).
///
/// Delegates to `fidan_runtime::display` — the single source of truth.
/// Other crates (`fidan-stdlib`) import `fidan_runtime::display` directly.
pub fn display(val: &FidanValue) -> String {
    runtime_display(val)
}

/// Format an object with a resolved class name (used when the interner is available).
#[allow(dead_code)]
pub fn display_with_name(val: &FidanValue, class_name: &str) -> String {
    match val {
        FidanValue::Object(_) => format!("<{}>", class_name),
        other => display(other),
    }
}
