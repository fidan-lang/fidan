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
    pub fn is_pass(&self) -> bool {
        matches!(self, Self::Passed)
    }

    pub fn symbol(&self) -> &'static str {
        match self {
            Self::Passed => "✅",
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
        Self {
            name: name.into(),
            result: None,
        }
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
    fidan_runtime::stdlib::test_runner::dispatch(name, args)
}

pub fn exported_names() -> &'static [&'static str] {
    fidan_runtime::stdlib::test_runner::exported_names()
}
