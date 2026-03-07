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
//! | W5xxx  | Warnings — performance hints      |
//! | R1xxx  | Runtime — execution / control     |
//! | R2xxx  | Runtime — arithmetic / bounds     |
//! | R3xxx  | Runtime — I/O                     |
//! | R4xxx  | Runtime — sandbox / security      |
//! | R9xxx  | Runtime — parallel / concurrency  |
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
    DiagnosticCode {
        code: "E0104",
        title: "constant must have an initializer",
        category: "names",
    },
    DiagnosticCode {
        code: "E0105",
        title: "undefined type name",
        category: "names",
    },
    DiagnosticCode {
        code: "E0106",
        title: "module not found",
        category: "imports",
    },
    DiagnosticCode {
        code: "E0107",
        title: "object cannot extend itself",
        category: "names",
    },
    DiagnosticCode {
        code: "E0108",
        title: "unknown export from stdlib module",
        category: "imports",
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
    DiagnosticCode {
        code: "E0204",
        title: "unknown field or method on object",
        category: "types",
    },
    DiagnosticCode {
        code: "E0205",
        title: "nullable value used in non-nullable context",
        category: "null-safety",
    },
    // ── Argument / call ───────────────────────────────────────────────────────
    DiagnosticCode {
        code: "E0301",
        title: "missing required argument",
        category: "args",
    },
    DiagnosticCode {
        code: "E0302",
        title: "argument type mismatch",
        category: "args",
    },
    DiagnosticCode {
        code: "E0303",
        title: "decorator first parameter must have type `action`",
        category: "decorators",
    },
    DiagnosticCode {
        code: "E0304",
        title: "wrong number of extra arguments for decorator",
        category: "decorators",
    },
    DiagnosticCode {
        code: "E0305",
        title: "too many arguments provided to action",
        category: "args",
    },
    DiagnosticCode {
        code: "E0306",
        title: "`this` used outside object or extension-action context",
        category: "objects",
    },
    DiagnosticCode {
        code: "E0307",
        title: "`parent` used in a context with no parent type",
        category: "objects",
    },
    // ── Concurrency / safety ──────────────────────────────────────────────────
    DiagnosticCode {
        code: "E0401",
        title: "data race: module-level variable mutated in concurrent tasks",
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
    DiagnosticCode {
        code: "W1004",
        title: "spawned `Pending` value is never awaited",
        category: "concurrency",
    },
    DiagnosticCode {
        code: "W1005",
        title: "unused import",
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
    DiagnosticCode {
        code: "W2003",
        title: "bare action reference has no effect",
        category: "lint",
    },
    DiagnosticCode {
        code: "W2004",
        title: "unknown decorator",
        category: "lint",
    },
    DiagnosticCode {
        code: "W2005",
        title: "deprecated symbol",
        category: "lint",
    },
    DiagnosticCode {
        code: "W2006",
        title: "possibly-nothing value used in non-null context",
        category: "null-safety",
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
    // ── Runtime: sandbox / security ──────────────────────────────────────────
    DiagnosticCode {
        code: "R4001",
        title: "sandbox: file-system read denied",
        category: "sandbox",
    },
    DiagnosticCode {
        code: "R4002",
        title: "sandbox: file-system write denied",
        category: "sandbox",
    },
    DiagnosticCode {
        code: "R4003",
        title: "sandbox: environment access denied",
        category: "sandbox",
    },
    // ── Runtime: parallel / concurrency ──────────────────────────────────────
    DiagnosticCode {
        code: "R9001",
        title: "one or more tasks failed in a `parallel` block",
        category: "parallel",
    },
    // ── Performance hints ────────────────────────────────────────────────────
    DiagnosticCode {
        code: "W5001",
        title: "loop body uses `flexible` (dynamic) type — JIT cannot specialize",
        category: "performance",
    },
    DiagnosticCode {
        code: "W5002",
        title: "loop closure captures mutable outer variable — prevents hoisting",
        category: "performance",
    },
    DiagnosticCode {
        code: "W5003",
        title: "action called in hot loop path but lacks `@precompile`",
        category: "performance",
    },
    DiagnosticCode {
        code: "W5004",
        title: "`@precompile` has no effect in AOT build mode",
        category: "performance",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{diag_code, explanations::explain};

    // ── diag_code! macro validates at compile time ────────────────────────────
    // These `const` assertions fail to compile if the code is removed from CODES.

    const _R0001: DiagCode = diag_code!("R0001");
    const _R9001: DiagCode = diag_code!("R9001");
    const _E0000: DiagCode = diag_code!("E0000");
    const _E0401: DiagCode = diag_code!("E0401");
    const _W1004: DiagCode = diag_code!("W1004");

    // ── lookup() ─────────────────────────────────────────────────────────────

    #[test]
    fn lookup_known_codes() {
        assert!(lookup("E0000").is_some());
        assert!(lookup("E0401").is_some());
        assert!(lookup("W1004").is_some());
        assert!(lookup("R0001").is_some());
        assert!(lookup("R9001").is_some());
    }

    #[test]
    fn lookup_unknown_code_returns_none() {
        assert!(lookup("XXXXX").is_none());
        assert!(lookup("").is_none());
        assert!(lookup("E9999").is_none());
    }

    #[test]
    fn lookup_titles_are_nonempty() {
        for entry in CODES {
            assert!(
                !entry.title.is_empty(),
                "code {} has an empty title",
                entry.code
            );
        }
    }

    #[test]
    fn lookup_categories_are_nonempty() {
        for entry in CODES {
            assert!(
                !entry.category.is_empty(),
                "code {} has an empty category",
                entry.category
            );
        }
    }

    #[test]
    fn all_codes_are_five_characters() {
        for entry in CODES {
            assert_eq!(
                entry.code.len(),
                5,
                "code '{}' is not 5 characters",
                entry.code
            );
        }
    }

    // ── explain() ────────────────────────────────────────────────────────────

    #[test]
    fn explain_key_codes_have_text() {
        // These are the codes users are most likely to look up — they must
        // have long-form explanations registered in explanations.rs.
        let must_have = ["E0000", "E0401", "W1004", "R0001", "R9001"];
        for code_str in must_have {
            let code = DiagCode(code_str);
            assert!(
                explain(code).is_some(),
                "explain({code_str}) returned None — add an explanation in explanations.rs"
            );
        }
    }

    #[test]
    fn explain_returns_nonempty_text() {
        let must_have = ["E0000", "E0401", "W1004", "R0001", "R9001"];
        for code_str in must_have {
            let code = DiagCode(code_str);
            if let Some(text) = explain(code) {
                assert!(
                    !text.trim().is_empty(),
                    "explain({code_str}) returned empty text"
                );
            }
        }
    }

    // ── DiagCode Display ─────────────────────────────────────────────────────

    #[test]
    fn diag_code_display() {
        let c = diag_code!("E0401");
        assert_eq!(format!("{c}"), "E0401");
    }

    #[test]
    fn diag_code_equality() {
        let a = diag_code!("R9001");
        let b = DiagCode("R9001");
        assert_eq!(a, b);
    }
}
