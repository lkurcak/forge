//! A live line per repository while forge works through them, like
//! `docker compose` shows its containers: `·` waiting, a spinner while in
//! flight, its mark once done. Results are printed above the live lines in
//! the repositories' order, as soon as every one before them is done, so the
//! live block shrinks from the top and what stays on screen is the same
//! report a pipe gets.
//!
//! Off a terminal there are no live lines, and results are just printed.

use std::io::{IsTerminal, stdout};
use std::time::Duration;

use indicatif::{MultiProgress, ProgressBar, ProgressDrawTarget, ProgressStyle};

pub struct Progress {
    /// `None` when stdout isn't a terminal.
    live: Option<Live>,
}

struct Live {
    multi: MultiProgress,
    lines: Vec<ProgressBar>,
}

impl Progress {
    pub fn new(names: &[&str]) -> Self {
        if !stdout().is_terminal() {
            return Self { live: None };
        }
        let multi = MultiProgress::with_draw_target(ProgressDrawTarget::stdout());
        let waiting = ProgressStyle::with_template("· {msg}").expect("valid template");
        let lines = names
            .iter()
            .map(|name| {
                multi.add(
                    ProgressBar::new_spinner()
                        .with_style(waiting.clone())
                        .with_message((*name).to_owned()),
                )
            })
            .collect();
        Self {
            live: Some(Live { multi, lines }),
        }
    }

    /// Item `i` is in flight.
    pub fn start(&self, i: usize) {
        if let Some(live) = &self.live {
            let line = &live.lines[i];
            line.set_style(
                ProgressStyle::with_template("{spinner} {msg}").expect("valid template"),
            );
            line.enable_steady_tick(Duration::from_millis(80));
        }
    }

    /// Item `i` is done, with `mark` for its result, though its report may
    /// still wait for those before it.
    pub fn finish(&self, i: usize, mark: &str) {
        if let Some(live) = &self.live {
            let line = &live.lines[i];
            line.set_style(
                ProgressStyle::with_template(&format!("{mark} {{msg}}")).expect("valid template"),
            );
            line.finish();
        }
    }

    /// Prints item `i`'s report in place of its live line.
    pub fn report(&self, i: usize, text: &str) {
        match &self.live {
            Some(live) => {
                live.lines[i].finish_and_clear();
                live.multi.remove(&live.lines[i]);
                // Only fails when the draw target is gone; nothing to do then.
                let _ = live.multi.println(text);
            }
            None => println!("{text}"),
        }
    }
}
