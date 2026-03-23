use fidan_diagnostics::{DiagCode, diag_code};
use fidan_runtime::display as runtime_display;
use fidan_runtime::{FidanString, FidanValue, SharedRef};
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
    match name {
        // ── I/O ──────────────────────────────────────────────────────────────
        "print" => {
            let parts: Vec<String> = args.iter().map(display).collect();
            println!("{}", parts.join(" "));
            Ok(Some(FidanValue::Nothing))
        }
        "eprint" => {
            let parts: Vec<String> = args.iter().map(display).collect();
            eprintln!("{}", parts.join(" "));
            Ok(Some(FidanValue::Nothing))
        }
        "input" => {
            let prompt = args.first().map(display).unwrap_or_default();
            if !prompt.is_empty() {
                use std::io::Write;
                print!("{}", prompt);
                let _ = std::io::stdout().flush();
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
        "string" => {
            let v = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            Ok(Some(FidanValue::String(FidanString::new(&display(&v)))))
        }
        "integer" => {
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
        "float" => {
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
        "boolean" => {
            let v = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            Ok(Some(FidanValue::Boolean(v.truthy())))
        }

        // ── Collections ───────────────────────────────────────────────────────
        "len" => {
            let v = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            let n = match &v {
                FidanValue::String(s) => s.len() as i64,
                FidanValue::List(l) => l.borrow().len() as i64,
                FidanValue::Dict(d) => d.borrow().len() as i64,
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
        "type" => {
            let v = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            Ok(Some(FidanValue::String(FidanString::new(v.type_name()))))
        }

        _ => Ok(None),
    }
}

/// Try to handle a call to a builtin type constructor (e.g. `Shared(val)`).
pub fn call_builtin_constructor(name: &str, args: Vec<FidanValue>) -> Option<FidanValue> {
    match name {
        "Shared" => {
            let inner = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            Some(FidanValue::Shared(SharedRef::new(inner)))
        }
        _ => None,
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
