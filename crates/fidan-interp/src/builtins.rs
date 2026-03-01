use fidan_runtime::{FidanString, FidanValue, SharedRef};
use std::io::BufRead;

/// Try to handle a call to a built-in function.
///
/// Returns `Some(value)` if handled, `None` if the name is not a built-in.
/// Callers that dispatch **free-function** calls should first check
/// [`is_free_builtin`] and reject method-only names before calling this.
pub fn call_builtin(name: &str, args: Vec<FidanValue>) -> Option<FidanValue> {
    match name {
        // ── I/O ──────────────────────────────────────────────────────────────
        "print" => {
            let parts: Vec<String> = args.iter().map(display).collect();
            println!("{}", parts.join(" "));
            Some(FidanValue::Nothing)
        }
        "eprint" => {
            let parts: Vec<String> = args.iter().map(display).collect();
            eprintln!("{}", parts.join(" "));
            Some(FidanValue::Nothing)
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
            stdin.lock().read_line(&mut line).ok()?;
            // Strip trailing newline
            if line.ends_with('\n') {
                line.pop();
                if line.ends_with('\r') {
                    line.pop();
                }
            }
            Some(FidanValue::String(FidanString::new(&line)))
        }

        // ── Type conversion ───────────────────────────────────────────────────
        "string" => {
            let v = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            Some(FidanValue::String(FidanString::new(&display(&v))))
        }
        "integer" => {
            let v = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            Some(match &v {
                FidanValue::Integer(n) => FidanValue::Integer(*n),
                FidanValue::Float(f) => FidanValue::Integer(*f as i64),
                FidanValue::Boolean(b) => FidanValue::Integer(if *b { 1 } else { 0 }),
                FidanValue::String(s) => s
                    .as_str()
                    .parse::<i64>()
                    .map(FidanValue::Integer)
                    .unwrap_or(FidanValue::Nothing),
                _ => FidanValue::Nothing,
            })
        }
        "float" => {
            let v = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            Some(match &v {
                FidanValue::Float(f) => FidanValue::Float(*f),
                FidanValue::Integer(n) => FidanValue::Float(*n as f64),
                FidanValue::String(s) => s
                    .as_str()
                    .parse::<f64>()
                    .map(FidanValue::Float)
                    .unwrap_or(FidanValue::Nothing),
                _ => FidanValue::Nothing,
            })
        }
        "boolean" => {
            let v = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            Some(FidanValue::Boolean(v.truthy()))
        }

        // ── Collections ───────────────────────────────────────────────────────
        "len" => {
            let v = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            let n = match &v {
                FidanValue::String(s) => s.len() as i64,
                FidanValue::List(l) => l.borrow().len() as i64,
                FidanValue::Dict(d) => d.borrow().len() as i64,
                FidanValue::Tuple(t) => t.len() as i64,
                _ => return Some(FidanValue::Nothing),
            };
            Some(FidanValue::Integer(n))
        }
        "type" => {
            let v = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            Some(FidanValue::String(FidanString::new(v.type_name())))
        }

        // ── Math (always available as free functions) ─────────────────────────
        "abs" => {
            let v = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            Some(match v {
                FidanValue::Integer(n) => FidanValue::Integer(n.abs()),
                FidanValue::Float(f) => FidanValue::Float(f.abs()),
                other => other,
            })
        }
        "sqrt" => {
            let v = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            let f = match v {
                FidanValue::Integer(n) => n as f64,
                FidanValue::Float(f) => f,
                _ => return Some(FidanValue::Nothing),
            };
            Some(FidanValue::Float(f.sqrt()))
        }
        "floor" => {
            let v = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            Some(match v {
                FidanValue::Float(f) => FidanValue::Integer(f.floor() as i64),
                other => other,
            })
        }
        "ceil" => {
            let v = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            Some(match v {
                FidanValue::Float(f) => FidanValue::Integer(f.ceil() as i64),
                other => other,
            })
        }
        "round" => {
            let v = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            Some(match v {
                FidanValue::Float(f) => FidanValue::Integer(f.round() as i64),
                other => other,
            })
        }
        "max" => {
            let mut iter = args.into_iter();
            let a = iter.next().unwrap_or(FidanValue::Nothing);
            let b = iter.next().unwrap_or(FidanValue::Nothing);
            Some(match (&a, &b) {
                (FidanValue::Integer(x), FidanValue::Integer(y)) => {
                    if x >= y {
                        a
                    } else {
                        b
                    }
                }
                (FidanValue::Float(x), FidanValue::Float(y)) => {
                    if x >= y {
                        a
                    } else {
                        b
                    }
                }
                _ => a,
            })
        }
        "min" => {
            let mut iter = args.into_iter();
            let a = iter.next().unwrap_or(FidanValue::Nothing);
            let b = iter.next().unwrap_or(FidanValue::Nothing);
            Some(match (&a, &b) {
                (FidanValue::Integer(x), FidanValue::Integer(y)) => {
                    if x <= y {
                        a
                    } else {
                        b
                    }
                }
                (FidanValue::Float(x), FidanValue::Float(y)) => {
                    if x <= y {
                        a
                    } else {
                        b
                    }
                }
                _ => a,
            })
        }

        // ── Concurrency helpers ───────────────────────────────────────────────
        "wait" => {
            let ms = match args.into_iter().next().unwrap_or(FidanValue::Nothing) {
                FidanValue::Integer(n) => n.max(0) as u64,
                FidanValue::Float(f) => f.max(0.0) as u64,
                _ => 0,
            };
            std::thread::sleep(std::time::Duration::from_millis(ms));
            Some(FidanValue::Nothing)
        }

        _ => None,
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
pub fn display(val: &FidanValue) -> String {
    match val {
        FidanValue::Integer(n) => n.to_string(),
        FidanValue::Float(f) => {
            // Show "15.0" for whole floats, "3.14" for fractions.
            if f.fract() == 0.0 {
                format!("{:.1}", f)
            } else {
                f.to_string()
            }
        }
        FidanValue::Boolean(b) => b.to_string(),
        FidanValue::Nothing => "nothing".to_string(),
        FidanValue::String(s) => s.as_str().to_string(),
        FidanValue::List(l) => {
            let items: Vec<String> = l.borrow().iter().map(display).collect();
            format!("[{}]", items.join(", "))
        }
        FidanValue::Dict(d) => {
            let pairs: Vec<String> = d
                .borrow()
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
        FidanValue::Pending(_) => "<pending>".to_string(),
        FidanValue::Function(id) => format!("<action#{}>", id.0),
    }
}

/// Format an object with a resolved class name (used when the interner is available).
#[allow(dead_code)]
pub fn display_with_name(val: &FidanValue, class_name: &str) -> String {
    match val {
        FidanValue::Object(_) => format!("<{}>", class_name),
        other => display(other),
    }
}
