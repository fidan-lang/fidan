use anyhow::{Context, Result};
use std::io::{self, Write};

fn render_yes_no_prompt(prompt: &str, default: bool, color: bool) -> (String, String) {
    let suffix = if default { "[Y/n]" } else { "[y/N]" };
    let label = crate::terminal::paint(color, "\x1b[1;33m", "confirm");
    let prompt_text = crate::terminal::paint(color, "\x1b[1m", prompt);
    let suffix_text = crate::terminal::paint(color, "\x1b[36m", suffix);
    (format!("{label} "), format!("{prompt_text} {suffix_text} "))
}

pub(crate) fn prompt_yes_no(prompt: &str, default: bool) -> Result<bool> {
    let color =
        crate::terminal::stderr_supports_color() && crate::terminal::stdout_supports_color();
    let (stderr_prefix, stdout_prompt) = render_yes_no_prompt(prompt, default, color);
    eprint!("{stderr_prefix}");
    print!("{stdout_prompt}");
    io::stdout().flush().ok();
    let mut line = String::new();
    io::stdin()
        .read_line(&mut line)
        .context("failed to read response")?;
    let trimmed = line.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        return Ok(default);
    }
    Ok(matches!(trimmed.as_str(), "y" | "yes"))
}

#[cfg(test)]
mod tests {
    use super::render_yes_no_prompt;

    #[test]
    fn yes_no_prompt_is_plain_without_color() {
        let (stderr_prefix, stdout_prompt) =
            render_yes_no_prompt("remove this install?", false, false);
        assert_eq!(stderr_prefix, "confirm ");
        assert_eq!(stdout_prompt, "remove this install? [y/N] ");
        assert!(!stderr_prefix.contains('\x1b'));
        assert!(!stdout_prompt.contains('\x1b'));
    }
}
