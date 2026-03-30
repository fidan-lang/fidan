//! Formatter configuration.

use serde::Deserialize;
use std::error::Error;
use std::fmt::{Display, Formatter};
use std::path::{Path, PathBuf};

/// Options controlling how Fidan source code is formatted.
#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug)]
pub enum FormatConfigError {
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
}

impl Display for FormatConfigError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Read { path, source } => {
                write!(
                    f,
                    "could not read formatter config `{}`: {source}",
                    path.display()
                )
            }
            Self::Parse { path, source } => {
                write!(
                    f,
                    "could not parse formatter config `{}`: {source}",
                    path.display()
                )
            }
        }
    }
}

impl Error for FormatConfigError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Read { source, .. } => Some(source),
            Self::Parse { source, .. } => Some(source),
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
struct PartialFormatOptions {
    indent_width: Option<usize>,
    max_line_len: Option<usize>,
    trailing_comma: Option<bool>,
    blank_lines_between_items: Option<usize>,
}

#[derive(Debug, Clone, Default, Deserialize)]
struct FormatConfigFile {
    indent_width: Option<usize>,
    max_line_len: Option<usize>,
    trailing_comma: Option<bool>,
    blank_lines_between_items: Option<usize>,
    #[serde(default)]
    fmt: PartialFormatOptions,
}

impl FormatOptions {
    fn apply_partial(&mut self, partial: PartialFormatOptions) {
        if let Some(value) = partial.indent_width {
            self.indent_width = value;
        }
        if let Some(value) = partial.max_line_len {
            self.max_line_len = value;
        }
        if let Some(value) = partial.trailing_comma {
            self.trailing_comma = value;
        }
        if let Some(value) = partial.blank_lines_between_items {
            self.blank_lines_between_items = value;
        }
    }

    fn apply_cli_overrides(&mut self, indent_width: Option<usize>, max_line_len: Option<usize>) {
        if let Some(value) = indent_width {
            self.indent_width = value;
        }
        if let Some(value) = max_line_len {
            self.max_line_len = value;
        }
    }
}

fn config_from_file(config: FormatConfigFile) -> PartialFormatOptions {
    let mut merged = PartialFormatOptions {
        indent_width: config.indent_width,
        max_line_len: config.max_line_len,
        trailing_comma: config.trailing_comma,
        blank_lines_between_items: config.blank_lines_between_items,
    };
    if config.fmt.indent_width.is_some() {
        merged.indent_width = config.fmt.indent_width;
    }
    if config.fmt.max_line_len.is_some() {
        merged.max_line_len = config.fmt.max_line_len;
    }
    if config.fmt.trailing_comma.is_some() {
        merged.trailing_comma = config.fmt.trailing_comma;
    }
    if config.fmt.blank_lines_between_items.is_some() {
        merged.blank_lines_between_items = config.fmt.blank_lines_between_items;
    }
    merged
}

pub fn find_format_config(start: &Path) -> Option<PathBuf> {
    let mut dir = if start.is_dir() {
        Some(start)
    } else {
        start.parent()
    };
    while let Some(current) = dir {
        let candidate = current.join(".fidanfmt");
        if candidate.is_file() {
            return Some(candidate);
        }
        dir = current.parent();
    }
    None
}

pub fn load_format_options_for_path(
    path: Option<&Path>,
) -> Result<Option<FormatOptions>, FormatConfigError> {
    let Some(path) = path else {
        return Ok(None);
    };
    let Some(config_path) = find_format_config(path) else {
        return Ok(None);
    };
    let raw = std::fs::read_to_string(&config_path).map_err(|source| FormatConfigError::Read {
        path: config_path.clone(),
        source,
    })?;
    let parsed: FormatConfigFile =
        toml::from_str(&raw).map_err(|source| FormatConfigError::Parse {
            path: config_path.clone(),
            source,
        })?;
    let mut opts = FormatOptions::default();
    opts.apply_partial(config_from_file(parsed));
    Ok(Some(opts))
}

pub fn resolve_format_options_for_path(
    path: Option<&Path>,
    indent_width_override: Option<usize>,
    max_line_len_override: Option<usize>,
) -> Result<FormatOptions, FormatConfigError> {
    let mut opts = load_format_options_for_path(path)?.unwrap_or_default();
    opts.apply_cli_overrides(indent_width_override, max_line_len_override);
    Ok(opts)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_dir(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos();
        let dir =
            std::env::temp_dir().join(format!("fidan_fmt_{name}_{}_{}", std::process::id(), nonce));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn finds_nearest_fidanfmt() {
        let root = temp_dir("nearest");
        let nested = root.join("src").join("deep");
        std::fs::create_dir_all(&nested).expect("create nested temp dirs");
        let config = root.join(".fidanfmt");
        std::fs::write(&config, "indent_width = 2\n").expect("write config");
        let target = nested.join("demo.fdn");
        std::fs::write(&target, "var x=1\n").expect("write demo file");

        let found = find_format_config(&target).expect("find .fidanfmt");
        assert_eq!(found, config);

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn loads_flat_and_nested_fidanfmt() {
        let root = temp_dir("load");
        let target = root.join("demo.fdn");
        std::fs::write(
            root.join(".fidanfmt"),
            r#"
indent_width = 2
max_line_len = 88

[fmt]
blank_lines_between_items = 2
trailing_comma = false
"#,
        )
        .expect("write config");
        std::fs::write(&target, "var x=1\n").expect("write demo file");

        let opts = load_format_options_for_path(Some(&target))
            .expect("load config")
            .expect("config should exist");
        assert_eq!(
            opts,
            FormatOptions {
                indent_width: 2,
                max_line_len: 88,
                trailing_comma: false,
                blank_lines_between_items: 2,
            }
        );

        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn cli_overrides_fidanfmt_values() {
        let root = temp_dir("override");
        let target = root.join("demo.fdn");
        std::fs::write(
            root.join(".fidanfmt"),
            "indent_width = 2\nmax_line_len = 88\n",
        )
        .expect("write config");
        std::fs::write(&target, "var x=1\n").expect("write demo file");

        let opts = resolve_format_options_for_path(Some(&target), Some(6), Some(140))
            .expect("resolve config with overrides");
        assert_eq!(opts.indent_width, 6);
        assert_eq!(opts.max_line_len, 140);

        std::fs::remove_dir_all(&root).ok();
    }
}
