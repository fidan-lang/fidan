use anyhow::{Context, Result};
use std::io::{self, Write};

pub(crate) fn prompt_yes_no(prompt: &str, default: bool) -> Result<bool> {
    let suffix = if default { "[Y/n]" } else { "[y/N]" };
    eprint!("\x1b[1;33mconfirm\x1b[0m ");
    print!("\x1b[1m{prompt}\x1b[0m \x1b[36m{suffix}\x1b[0m ");
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
