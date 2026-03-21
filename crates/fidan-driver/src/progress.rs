use indicatif::{ProgressBar, ProgressStyle};
use std::io::{IsTerminal, stderr};
use std::time::Duration;

pub struct ProgressReporter {
    bar: ProgressBar,
    hidden: bool,
}

impl ProgressReporter {
    pub fn bytes(prefix: &str, message: impl Into<String>, total: Option<u64>) -> Self {
        let hidden = !stderr().is_terminal();
        let bar = if hidden {
            ProgressBar::hidden()
        } else if let Some(total) = total {
            let bar = ProgressBar::new(total);
            bar.set_style(
                ProgressStyle::with_template(
                    "{prefix:>10.magenta} {spinner} {msg} [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})",
                )
                .expect("valid progress bar template")
                .progress_chars("=> ")
                .tick_strings(&["/", "-", "\\", "|"]),
            );
            bar.enable_steady_tick(Duration::from_millis(100));
            bar
        } else {
            let bar = ProgressBar::new_spinner();
            bar.set_style(
                ProgressStyle::with_template("{prefix:>10.magenta} {spinner} {msg}")
                    .expect("valid spinner template")
                    .tick_strings(&["/", "-", "\\", "|"]),
            );
            bar.enable_steady_tick(Duration::from_millis(100));
            bar
        };

        bar.set_prefix(prefix.to_string());
        bar.set_message(message.into());

        Self { bar, hidden }
    }

    pub fn spinner(prefix: &str, message: impl Into<String>) -> Self {
        Self::bytes(prefix, message, None)
    }

    pub fn inc(&self, delta: u64) {
        self.bar.inc(delta);
    }

    pub fn set_message(&self, message: impl Into<String>) {
        self.bar.set_message(message.into());
    }

    pub fn finish_and_clear(&self) {
        if !self.hidden {
            self.bar.finish_and_clear();
        }
    }
}

impl Drop for ProgressReporter {
    fn drop(&mut self) {
        self.finish_and_clear();
    }
}
