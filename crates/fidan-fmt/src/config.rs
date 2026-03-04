//! Formatter configuration.

/// Options controlling how Fidan source code is formatted.
///
/// Sensible defaults are provided via `Default`.  Override them from
/// `fidan.toml` (`[fmt]` section) or the `--config` flag.
#[derive(Debug, Clone)]
pub struct FormatOptions {
    /// Number of spaces per indent level (default: 4).
    pub indent_width: usize,
    /// Soft line-length limit used when deciding to break argument lists (default: 100).
    pub max_line_len: usize,
    /// Emit a trailing comma after the last item in multi-item parameter / argument
    /// lists (default: true).
    pub trailing_comma: bool,
    /// Number of blank lines inserted between top-level items (default: 1).
    pub blank_lines_between_items: usize,
}

impl Default for FormatOptions {
    fn default() -> Self {
        Self {
            indent_width: 4,
            max_line_len: 100,
            trailing_comma: true,
            blank_lines_between_items: 1,
        }
    }
}
