//! `std.test` — Test block execution and result tracking.
//!
//! The test runner is used by `fidan test` to scan for `test { ... }` blocks
//! and run them, collecting pass/fail/error results.
//!
//! Also provides assertion functions callable from Fidan:
//!   `test.assert(cond)`, `test.assertEq(a, b)`, `test.assertNe(a, b)`,
//!   `test.assertSome(v)`, `test.fail(msg)`

use fidan_runtime::FidanValue;

/// Result of a single test case.
#[derive(Debug, Clone)]
pub enum TestResult {
    Passed,
    Failed(String),
    Errored(String),
}

impl TestResult {
    pub fn is_pass(&self) -> bool { matches!(self, Self::Passed) }

    pub fn symbol(&self) -> &'static str {
        match self {
            Self::Passed   => "✅",
            Self::Failed(_) => "❌",
            Self::Errored(_) => "💥",
        }
    }

    pub fn message(&self) -> Option<&str> {
        match self {
            Self::Passed => None,
            Self::Failed(m) | Self::Errored(m) => Some(m),
        }
    }
}

/// A single test case collected from the AST.
#[derive(Debug, Clone)]
pub struct TestCase {
    pub name: String,
    pub result: Option<TestResult>,
}

impl TestCase {
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into(), result: None }
    }

    pub fn passed(&mut self) {
        self.result = Some(TestResult::Passed);
    }

    pub fn failed(&mut self, msg: impl Into<String>) {
        self.result = Some(TestResult::Failed(msg.into()));
    }

    pub fn errored(&mut self, msg: impl Into<String>) {
        self.result = Some(TestResult::Errored(msg.into()));
    }
}

/// Dispatch a `test.<name>(args)` assertion call.
/// Returns `Err(String)` when an assertion fails (the caller should turn this
/// into a `Signal::Panic` so the test is marked as failed).
pub fn dispatch(name: &str, args: Vec<FidanValue>) -> Option<Result<FidanValue, String>> {
    match name {
        "assert" => {
            let cond = args.first().map(|v| v.truthy()).unwrap_or(false);
            if cond {
                Some(Ok(FidanValue::Nothing))
            } else {
                let msg = args.get(1).map(|v| format_val(v)).unwrap_or_else(|| "assertion failed".to_string());
                Some(Err(msg))
            }
        }
        "assertEq" | "assert_eq" => {
            let a = args.first().cloned().unwrap_or(FidanValue::Nothing);
            let b = args.get(1).cloned().unwrap_or(FidanValue::Nothing);
            if values_equal(&a, &b) {
                Some(Ok(FidanValue::Nothing))
            } else {
                Some(Err(format!("expected `{}` == `{}`", format_val(&a), format_val(&b))))
            }
        }
        "assertNe" | "assert_ne" => {
            let a = args.first().cloned().unwrap_or(FidanValue::Nothing);
            let b = args.get(1).cloned().unwrap_or(FidanValue::Nothing);
            if !values_equal(&a, &b) {
                Some(Ok(FidanValue::Nothing))
            } else {
                Some(Err(format!("expected `{}` != `{}`", format_val(&a), format_val(&b))))
            }
        }
        "assertGt" | "assert_gt" => {
            let ok = cmp_vals(args.first(), args.get(1)) == Some(std::cmp::Ordering::Greater);
            if ok { Some(Ok(FidanValue::Nothing)) }
            else {
                Some(Err(format!("expected `{}` > `{}`",
                    format_val(args.first().unwrap_or(&FidanValue::Nothing)),
                    format_val(args.get(1).unwrap_or(&FidanValue::Nothing)))))
            }
        }
        "assertLt" | "assert_lt" => {
            let ok = cmp_vals(args.first(), args.get(1)) == Some(std::cmp::Ordering::Less);
            if ok { Some(Ok(FidanValue::Nothing)) }
            else {
                Some(Err(format!("expected `{}` < `{}`",
                    format_val(args.first().unwrap_or(&FidanValue::Nothing)),
                    format_val(args.get(1).unwrap_or(&FidanValue::Nothing)))))
            }
        }
        "assertSome" | "assert_some" => {
            let v = args.first().cloned().unwrap_or(FidanValue::Nothing);
            if !v.is_nothing() {
                Some(Ok(FidanValue::Nothing))
            } else {
                Some(Err("expected a non-nothing value, got nothing".to_string()))
            }
        }
        "assertNothing" | "assert_nothing" => {
            let v = args.first().cloned().unwrap_or(FidanValue::Nothing);
            if v.is_nothing() {
                Some(Ok(FidanValue::Nothing))
            } else {
                Some(Err(format!("expected nothing, got `{}`", format_val(&v))))
            }
        }
        "assertType" | "assert_type" => {
            let val       = args.first().cloned().unwrap_or(FidanValue::Nothing);
            let expected  = match args.get(1) {
                Some(FidanValue::String(s)) => s.as_str().to_string(),
                _ => return Some(Ok(FidanValue::Nothing)),
            };
            let actual = val.type_name().to_string();
            if actual == expected {
                Some(Ok(FidanValue::Nothing))
            } else {
                Some(Err(format!("expected type `{}`, got `{}`", expected, actual)))
            }
        }
        "fail" => {
            let msg = args.first().map(|v| format_val(v)).unwrap_or_else(|| "test failed".to_string());
            Some(Err(msg))
        }
        "skip" => {
            // Marks the test as skipped — not an error, not a pass.
            // We just print the skip message and return Nothing.
            let msg = args.first().map(|v| format_val(v)).unwrap_or_else(|| "skipped".to_string());
            eprintln!("  ⏭  skip: {msg}");
            Some(Ok(FidanValue::Nothing))
        }
        _ => None,
    }
}

fn values_equal(a: &FidanValue, b: &FidanValue) -> bool {
    match (a, b) {
        (FidanValue::Integer(x), FidanValue::Integer(y)) => x == y,
        (FidanValue::Float(x),   FidanValue::Float(y))   => (x - y).abs() < 1e-12,
        (FidanValue::Boolean(x), FidanValue::Boolean(y)) => x == y,
        (FidanValue::String(x),  FidanValue::String(y))  => x.as_str() == y.as_str(),
        (FidanValue::Nothing,    FidanValue::Nothing)     => true,
        // Float/int cross comparison
        (FidanValue::Integer(x), FidanValue::Float(y))   => (*x as f64 - y).abs() < 1e-12,
        (FidanValue::Float(x),   FidanValue::Integer(y)) => (x - *y as f64).abs() < 1e-12,
        _ => false,
    }
}

fn cmp_vals(a: Option<&FidanValue>, b: Option<&FidanValue>) -> Option<std::cmp::Ordering> {
    match (a, b) {
        (Some(FidanValue::Integer(x)), Some(FidanValue::Integer(y))) => Some(x.cmp(y)),
        (Some(FidanValue::Float(x)),   Some(FidanValue::Float(y)))   => x.partial_cmp(y),
        (Some(FidanValue::Integer(x)), Some(FidanValue::Float(y)))   => (*x as f64).partial_cmp(y),
        (Some(FidanValue::Float(x)),   Some(FidanValue::Integer(y))) => x.partial_cmp(&(*y as f64)),
        _ => None,
    }
}

fn format_val(v: &FidanValue) -> String {
    match v {
        FidanValue::String(s)  => s.as_str().to_string(),
        FidanValue::Integer(n) => n.to_string(),
        FidanValue::Float(f)   => f.to_string(),
        FidanValue::Boolean(b) => b.to_string(),
        FidanValue::Nothing    => "nothing".to_string(),
        FidanValue::List(_)    => "[list]".to_string(),
        FidanValue::Dict(_)    => "{dict}".to_string(),
        FidanValue::Object(_)  => "[object]".to_string(),
        _ => "[value]".to_string(),
    }
}

pub fn exported_names() -> &'static [&'static str] {
    &[
        "assert", "assertEq", "assert_eq", "assertNe", "assert_ne",
        "assertGt", "assert_gt", "assertLt", "assert_lt",
        "assertSome", "assert_some", "assertNothing", "assert_nothing",
        "assertType", "assert_type", "fail", "skip",
    ]
}