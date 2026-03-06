//! Per-function profiling data types and report rendering.
//!
//! Used by `fidan profile` (feature 22.4).  At runtime the `MirMachine` holds
//! an `Option<Arc<Vec<FnProfileEntry>>>` — `None` in normal/JIT runs (zero
//! overhead); `Some(…)` only when the user runs `fidan profile`.

use std::sync::atomic::AtomicU64;

// ── Per-function counters ─────────────────────────────────────────────────────

/// Atomic call-count and inclusive-time accumulator per function.
///
/// "Inclusive" means the timer runs for the full duration of the function
/// including all callee time — the same as wall-clock time per call frame.
pub struct FnProfileEntry {
    pub call_count: AtomicU64,
    /// Accumulated wall time in nanoseconds (inclusive of callees).
    pub total_ns: AtomicU64,
}

impl Default for FnProfileEntry {
    fn default() -> Self {
        Self {
            call_count: AtomicU64::new(0),
            total_ns: AtomicU64::new(0),
        }
    }
}

// ── Report types ──────────────────────────────────────────────────────────────

/// One row in the profile report.
#[derive(Debug, Clone)]
pub struct FnProfileItem {
    pub name: String,
    pub call_count: u64,
    /// Inclusive total time in milliseconds.
    pub total_ms: f64,
    /// Average time per call in milliseconds.
    pub avg_ms: f64,
    /// Percentage of the overall program wall time.
    pub pct: f64,
}

/// Full profile report, ready for rendering.
#[derive(Debug, Clone)]
pub struct ProfileReport {
    pub program_name: String,
    /// Total wall time of the program in milliseconds.
    pub total_ms: f64,
    /// Functions sorted by total time descending.
    pub hot_paths: Vec<FnProfileItem>,
}

impl ProfileReport {
    /// Render a human-readable terminal table.
    pub fn render(&self) -> String {
        use std::fmt::Write as _;

        let mut out = String::new();
        let total_str = format_ms(self.total_ms);
        let _ = writeln!(
            out,
            "\x1b[2m[% profile]: {}  ({} total)\x1b[0m\n",
            self.program_name, total_str
        );

        if self.hot_paths.is_empty() {
            let _ = writeln!(out, "  (no profiling data — all functions compiled by JIT)");
            return out;
        }

        let _ = writeln!(out, "  hot paths");

        // Column widths.
        let name_w = self
            .hot_paths
            .iter()
            .map(|r| r.name.len())
            .max()
            .unwrap_or(6)
            .max(6);
        let count_w = self
            .hot_paths
            .iter()
            .map(|r| format_count(r.call_count).len())
            .max()
            .unwrap_or(1);

        for item in &self.hot_paths {
            let name_col = format!("{:<width$}", item.name, width = name_w);
            let count_col = format!(
                "{:>width$}×",
                format_count(item.call_count),
                width = count_w
            );
            let avg_col = format!("{:>12}", format_ms(item.avg_ms));
            let total_col = format!("{:>12}", format_ms(item.total_ms));
            let pct_col = format!("{:>5.1}%", item.pct);
            let _ = writeln!(
                out,
                "    {}  called {}  avg {}  total {}  {}",
                name_col, count_col, avg_col, total_col, pct_col
            );
        }

        // Suggestion block.
        if let Some(top) = self.hot_paths.first() {
            if top.pct > 80.0 && top.call_count > 0 {
                let _ = writeln!(out);
                let _ = writeln!(out, "  suggestion");
                let _ = writeln!(
                    out,
                    "    action {} is >{:.0}% of runtime",
                    top.name,
                    top.pct.floor()
                );
                let _ = writeln!(out, "    consider annotating with @precompile");
            }
        }

        out
    }

    /// Render a JSON representation for `--profile-out`.
    pub fn render_json(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        let _ = writeln!(out, "{{");
        let _ = writeln!(
            out,
            "  \"program\": \"{}\",",
            escape_json(&self.program_name)
        );
        let _ = writeln!(out, "  \"total_ms\": {:.4},", self.total_ms);
        let _ = writeln!(out, "  \"hot_paths\": [");
        for (i, item) in self.hot_paths.iter().enumerate() {
            let comma = if i + 1 < self.hot_paths.len() {
                ","
            } else {
                ""
            };
            let _ = writeln!(out, "    {{");
            let _ = writeln!(out, "      \"name\": \"{}\",", escape_json(&item.name));
            let _ = writeln!(out, "      \"call_count\": {},", item.call_count);
            let _ = writeln!(out, "      \"total_ms\": {:.4},", item.total_ms);
            let _ = writeln!(out, "      \"avg_ms\": {:.6},", item.avg_ms);
            let _ = writeln!(out, "      \"pct\": {:.2}", item.pct);
            let _ = writeln!(out, "    }}{}", comma);
        }
        let _ = writeln!(out, "  ]");
        let _ = write!(out, "}}");
        out
    }
}

// ── Formatting helpers ────────────────────────────────────────────────────────

fn format_ms(ms: f64) -> String {
    if ms >= 1_000.0 {
        format!("{:.2} s  ", ms / 1_000.0)
    } else if ms >= 1.0 {
        format!("{:.3} ms ", ms)
    } else if ms >= 0.001 {
        format!("{:.3} ms ", ms)
    } else {
        format!("{:.1} μs ", ms * 1_000.0)
    }
}

fn format_count(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

fn escape_json(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}
