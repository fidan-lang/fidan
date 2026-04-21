pub(crate) fn paint(enabled: bool, ansi_prefix: &str, text: &str) -> String {
    if enabled {
        format!("{ansi_prefix}{text}\x1b[0m")
    } else {
        text.to_string()
    }
}

pub(crate) fn stdout_supports_color() -> bool {
    fidan_driver::terminal::stdout_supports_color()
}

pub(crate) fn stderr_supports_color() -> bool {
    fidan_driver::terminal::stderr_supports_color()
}

#[cfg(test)]
mod tests {
    use super::paint;

    #[test]
    fn paint_is_plain_when_disabled() {
        assert_eq!(paint(false, "\x1b[31m", "hello"), "hello");
    }

    #[test]
    fn paint_wraps_with_ansi_when_enabled() {
        assert_eq!(paint(true, "\x1b[31m", "hello"), "\x1b[31mhello\x1b[0m");
    }
}
