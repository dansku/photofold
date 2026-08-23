//! Progress reporting.
//!
//! A thin wrapper over `indicatif` so that the pipeline never has to ask whether
//! output is a terminal. When progress is disabled every bar is a hidden bar and
//! the calls become no-ops.

use std::time::Duration;

use indicatif::{MultiProgress, ProgressBar, ProgressStyle};

pub struct Progress {
    multi: MultiProgress,
    enabled: bool,
}

impl Progress {
    /// `enabled` should be false when stderr is not a terminal, so that piped
    /// output stays clean.
    pub fn new(enabled: bool) -> Self {
        Self {
            multi: MultiProgress::new(),
            enabled,
        }
    }

    /// An open-ended spinner for work with no known total, such as scanning.
    pub fn spinner(&self, message: &str) -> ProgressBar {
        if !self.enabled {
            return ProgressBar::hidden();
        }
        let bar = self.multi.add(ProgressBar::new_spinner());
        bar.set_style(
            ProgressStyle::with_template("{spinner:.cyan} {msg} {pos}")
                .unwrap_or_else(|_| ProgressStyle::default_spinner()),
        );
        bar.set_message(message.to_string());
        bar.enable_steady_tick(Duration::from_millis(120));
        bar
    }

    /// A percentage bar for work with a known number of items.
    pub fn bar(&self, len: u64, message: &str) -> ProgressBar {
        if !self.enabled {
            return ProgressBar::hidden();
        }
        let bar = self.multi.add(ProgressBar::new(len));
        bar.set_style(
            ProgressStyle::with_template(
                "{msg:<24} {bar:24.cyan/blue} {percent:>3}%  {pos}/{len} {eta_precise}",
            )
            .unwrap_or_else(|_| ProgressStyle::default_bar())
            .progress_chars("##-"),
        );
        bar.set_message(message.to_string());
        bar
    }

    /// Print a line above the bars without corrupting them.
    pub fn println(&self, line: impl AsRef<str>) {
        if self.enabled {
            let _ = self.multi.println(line.as_ref());
        } else {
            eprintln!("{}", line.as_ref());
        }
    }
}
