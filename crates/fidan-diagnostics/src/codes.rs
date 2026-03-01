//! Fidan diagnostic code registry.
//!
//! Every compiler and runtime message carries a stable 5-character code.
//!
//! **Naming convention**
//!
//! | Prefix | Category                          |
//! |--------|-----------------------------------|
//! | E0xxx  | Syntax / parse errors             |
//! | E1xxx  | Name-resolution errors (reserved) |
//! | E2xxx  | Type errors (reserved)            |
//! | E3xxx  | Argument / call errors (reserved) |
//! | E4xxx  | Concurrency / safety errors       |
//! | W1xxx  | Warnings — lifecycle              |
//! | W2xxx  | Warnings — style / lint           |
//! | R1xxx  | Runtime — execution / control     |
//! | R2xxx  | Runtime — arithmetic / bounds     |
//! | R3xxx  | Runtime — I/O                     |
//!
//! Use `fidan explain <code>` (Phase 10) for the full HTML/text description of
//! any code.

/// Metadata for one Fidan diagnostic code.
#[derive(Debug, Clone, Copy)]
pub struct DiagnosticCode {
    /// Stable 5-character identifier, e.g. `"E0101"`.
    pub code: &'static str,
    /// Short title shown by `fidan explain` and in IDE tooltips.
    pub title: &'static str,
    /// Broad category tag, e.g. `"names"`, `"types"`, `"io"`.
    pub category: &'static str,
}

/// All known Fidan diagnostic codes.
pub static CODES: &[DiagnosticCode] = &[
    // ── Syntax / parse ────────────────────────────────────────────────────────
    DiagnosticCode {
        code: "E0000",
        title: "unexpected token or syntax error",
        category: "syntax",
    },
    DiagnosticCode {
        code: "E0001",
        title: "unterminated string literal",
        category: "syntax",
    },
    // ── Name resolution ───────────────────────────────────────────────────────
    DiagnosticCode {
        code: "E0100",
        title: "undefined object in `extends` clause",
        category: "names",
    },
    DiagnosticCode {
        code: "E0101",
        title: "undefined name",
        category: "names",
    },
    DiagnosticCode {
        code: "E0102",
        title: "variable already declared in this scope",
        category: "names",
    },
    DiagnosticCode {
        code: "E0103",
        title: "cannot assign to constant",
        category: "names",
    },
    // ── Type system ───────────────────────────────────────────────────────────
    DiagnosticCode {
        code: "E0201",
        title: "type mismatch in assignment or initialiser",
        category: "types",
    },
    DiagnosticCode {
        code: "E0202",
        title: "return type mismatch",
        category: "types",
    },
    DiagnosticCode {
        code: "E0203",
        title: "unsupported operand types for operator",
        category: "types",
    },
    // ── Argument / call ───────────────────────────────────────────────────────
    DiagnosticCode {
        code: "E0301",
        title: "missing required argument",
        category: "args",
    },
    // ── Concurrency / safety ──────────────────────────────────────────────────
    DiagnosticCode {
        code: "E0401",
        title: "non-`Shared` value crossed a thread boundary",
        category: "concurrency",
    },
    DiagnosticCode {
        code: "E0402",
        title: "unawaited `Pending` value dropped",
        category: "concurrency",
    },
    // ── Warnings: lifecycle ───────────────────────────────────────────────────
    DiagnosticCode {
        code: "W1001",
        title: "variable declared without a value",
        category: "init",
    },
    DiagnosticCode {
        code: "W1002",
        title: "variable declared but never used",
        category: "unused",
    },
    DiagnosticCode {
        code: "W1003",
        title: "action parameter never used",
        category: "unused",
    },
    // ── Warnings: style ───────────────────────────────────────────────────────
    DiagnosticCode {
        code: "W2001",
        title: "file does not have the `.fdn` extension",
        category: "style",
    },
    DiagnosticCode {
        code: "W2002",
        title: "bare literal has no effect",
        category: "lint",
    },
    // ── Runtime: control flow ─────────────────────────────────────────────────
    DiagnosticCode {
        code: "R0001",
        title: "unhandled interpreter error",
        category: "runtime",
    },
    DiagnosticCode {
        code: "R1001",
        title: "stack overflow",
        category: "runtime",
    },
    DiagnosticCode {
        code: "R1002",
        title: "user-thrown panic",
        category: "runtime",
    },
    // ── Runtime: arithmetic / bounds ──────────────────────────────────────────
    DiagnosticCode {
        code: "R2001",
        title: "division by zero",
        category: "arithmetic",
    },
    DiagnosticCode {
        code: "R2002",
        title: "index out of bounds",
        category: "bounds",
    },
    DiagnosticCode {
        code: "R2003",
        title: "arithmetic overflow",
        category: "arithmetic",
    },
    // ── Runtime: I/O ──────────────────────────────────────────────────────────
    DiagnosticCode {
        code: "R3001",
        title: "failed to open file",
        category: "io",
    },
    DiagnosticCode {
        code: "R3002",
        title: "failed to read file",
        category: "io",
    },
    DiagnosticCode {
        code: "R3003",
        title: "failed to write file",
        category: "io",
    },
    DiagnosticCode {
        code: "R3004",
        title: "permission denied",
        category: "io",
    },
];

/// A validated diagnostic code.
///
/// Can only be constructed via [`DiagCode::new`], which is `const` and panics
/// at **compile time** (when called from a `const` context) if the code is not
/// present in [`CODES`].  Use the [`diag_code!`] macro to get this guarantee
/// automatically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiagCode(pub &'static str);

impl DiagCode {
    /// Validate `code` against [`CODES`] and return a [`DiagCode`].
    ///
    /// Calling this in a `const` context (e.g. via `diag_code!`) causes a
    /// **compile-time error** for unknown codes.
    pub const fn new(code: &'static str) -> Self {
        DiagCode(assert_code(code))
    }
}

impl std::fmt::Display for DiagCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

/// Assert that `code` is a registered diagnostic code, returning it unchanged.
/// Panics at compile time when evaluated in a `const` context.
pub const fn assert_code(code: &'static str) -> &'static str {
    let code_bytes = code.as_bytes();
    let mut i = 0;
    while i < CODES.len() {
        let candidate = CODES[i].code.as_bytes();
        if candidate.len() == code_bytes.len() {
            let mut j = 0;
            let mut matched = true;
            while j < candidate.len() {
                if candidate[j] != code_bytes[j] {
                    matched = false;
                    break;
                }
                j += 1;
            }
            if matched {
                return code;
            }
        }
        i += 1;
    }
    panic!("unknown diagnostic code — add it to `CODES` in fidan-diagnostics/src/codes.rs first")
}

/// Look up a code's metadata.  Returns `None` for unknown codes.
pub fn lookup(code: &str) -> Option<&'static DiagnosticCode> {
    CODES.iter().find(|c| c.code == code)
}

/// Short human-readable title for a code, or `""` if unknown.
pub fn title(code: &str) -> &'static str {
    lookup(code).map(|c| c.title).unwrap_or("")
}
