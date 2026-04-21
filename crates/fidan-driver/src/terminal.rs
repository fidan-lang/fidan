use std::io::IsTerminal;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stream {
    Stdin,
    Stdout,
    Stderr,
}

fn env_allows_color(term: Option<&str>, no_color_present: bool) -> bool {
    if no_color_present {
        return false;
    }

    if let Some(term) = term
        && term.eq_ignore_ascii_case("dumb")
    {
        return false;
    }

    true
}

pub fn is_terminal(stream: Stream) -> bool {
    match stream {
        Stream::Stdin => std::io::stdin().is_terminal(),
        Stream::Stdout => std::io::stdout().is_terminal(),
        Stream::Stderr => std::io::stderr().is_terminal(),
    }
}

pub fn supports_color(stream: Stream) -> bool {
    is_terminal(stream)
        && env_allows_color(
            std::env::var("TERM").ok().as_deref(),
            std::env::var_os("NO_COLOR").is_some(),
        )
}

pub fn stdin_is_terminal() -> bool {
    is_terminal(Stream::Stdin)
}

pub fn stdout_is_terminal() -> bool {
    is_terminal(Stream::Stdout)
}

pub fn stderr_is_terminal() -> bool {
    is_terminal(Stream::Stderr)
}

pub fn stdout_supports_color() -> bool {
    supports_color(Stream::Stdout)
}

pub fn stderr_supports_color() -> bool {
    supports_color(Stream::Stderr)
}

#[cfg(test)]
mod tests {
    use super::env_allows_color;

    #[test]
    fn env_allows_color_by_default() {
        assert!(env_allows_color(Some("xterm-256color"), false));
        assert!(env_allows_color(None, false));
    }

    #[test]
    fn env_disables_color_for_no_color() {
        assert!(!env_allows_color(Some("xterm-256color"), true));
    }

    #[test]
    fn env_disables_color_for_dumb_term() {
        assert!(!env_allows_color(Some("dumb"), false));
        assert!(!env_allows_color(Some("DUMB"), false));
    }
}
