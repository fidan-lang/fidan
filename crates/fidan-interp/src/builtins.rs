use fidan_config::{BuiltinSemantic, builtin_semantic};
use fidan_diagnostics::DiagCode;
use fidan_runtime::display as runtime_display;
use fidan_runtime::{FidanString, FidanValue, SharedRef};

pub struct BuiltinError {
    pub code: DiagCode,
    pub message: String,
}

impl From<fidan_runtime::stdlib::StdlibRuntimeError> for BuiltinError {
    fn from(error: fidan_runtime::stdlib::StdlibRuntimeError) -> Self {
        Self {
            code: error.code,
            message: error.message,
        }
    }
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
            Ok(Some(fidan_runtime::builtins::read_input_line(
                &mut std::io::stdin().lock(),
            )?))
        }

        // ── Type conversion ───────────────────────────────────────────────────
        BuiltinSemantic::String => {
            let v = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            Ok(Some(FidanValue::String(FidanString::new(&display(&v)))))
        }
        BuiltinSemantic::Integer => {
            let v = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            Ok(Some(fidan_runtime::builtins::integer(&v)?))
        }
        BuiltinSemantic::Float => {
            let v = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            Ok(Some(fidan_runtime::builtins::float(&v)?))
        }
        BuiltinSemantic::Boolean => {
            let v = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            Ok(Some(FidanValue::Boolean(v.truthy())))
        }

        // ── Collections ───────────────────────────────────────────────────────
        BuiltinSemantic::Len => {
            let v = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            Ok(Some(FidanValue::Integer(fidan_runtime::builtins::len(&v)?)))
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
            let value = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            Ok(Some(fidan_runtime::builtins::hashset(value)?))
        }
        Some(BuiltinSemantic::SharedConstructor) => {
            let inner = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            Ok(Some(FidanValue::Shared(SharedRef::new(inner))))
        }
        Some(BuiltinSemantic::WeakSharedConstructor) => {
            let value = args.into_iter().next().unwrap_or(FidanValue::Nothing);
            Ok(Some(fidan_runtime::builtins::weak_shared(value)?))
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
