//! A counter for the operations that turned into hundreds of requests.
//!
//! Since the vendor's whole-listing endpoint went away, anything that has to know what
//! every job on the account was asked for costs one request per job. On a long-lived
//! account that is minutes of silence, and silence is indistinguishable from a hang -
//! particularly on the sweep, which runs immediately before a run offers to spend money.
//!
//! # Where it writes, and when it does not
//!
//! Always stderr, never stdout. `dbnget list` writes a table to stdout that people pipe
//! into other things, and a progress line interleaved into that would corrupt it.
//!
//! It draws only when stderr is a terminal. Redirected to a file or a pipe, the carriage
//! returns would accumulate as garbage rather than overwrite, so the meter disables
//! itself and the command produces exactly the bytes it always did. It also disables
//! itself when debug logging is on, because tracing writes to the same stream and the
//! two would overwrite each other into nonsense; someone running `-v` asked for the log,
//! which reports the same progress in a form that survives redirection.

use std::{
    io::{IsTerminal, Write},
    sync::{
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
};

use tracing::level_filters::LevelFilter;

/// Below this many items the work finishes about as fast as the meter could draw, and a
/// line that appears and vanishes is worse than no line.
const MIN_ITEMS: usize = 8;

/// A live count of finished work, or an inert stand-in.
#[derive(Debug)]
pub struct Progress {
    label: &'static str,
    total: usize,
    done: AtomicUsize,
    /// Serialises drawing. Workers finish concurrently, and two half-written lines
    /// interleaved on one row is precisely the mess this is meant to avoid.
    ///
    /// Held only across the synchronous write, never across an `await`.
    draw: Mutex<()>,
    enabled: bool,
}

impl Progress {
    /// Starts a meter for `total` items, or an inert one if nothing should be drawn.
    pub fn start(label: &'static str, total: usize) -> Self {
        let enabled = total >= MIN_ITEMS
            && std::io::stderr().is_terminal()
            && LevelFilter::current() < LevelFilter::DEBUG;
        let progress = Self {
            label,
            total,
            done: AtomicUsize::new(0),
            draw: Mutex::new(()),
            enabled,
        };
        progress.draw();
        progress
    }

    /// Records one finished item, redraws, and reports the new count.
    ///
    /// The count is returned so a caller that has been muted - stderr redirected, or
    /// debug logging holding the stream - can report the same progress through the log
    /// instead. Without that, turning on `-v` would make a long run QUIETER than leaving
    /// it off, which is the opposite of what the flag is for.
    pub fn advance(&self) -> usize {
        let done = self.done.fetch_add(1, Ordering::Relaxed) + 1;
        self.draw();
        done
    }

    /// Erases the line so whatever prints next starts on clean ground.
    ///
    /// Blanking it rather than leaving a completed counter behind: the meter is a
    /// progress signal, not a result, and every caller prints its own outcome.
    pub fn finish(&self) {
        if !self.enabled {
            return;
        }
        let Ok(_guard) = self.draw.lock() else {
            return;
        };
        let mut err = std::io::stderr();
        // Two spaces past the widest line this can draw, so no tail survives.
        let width = self.label.len() + 24;
        ignore(write!(err, "\r{:width$}\r", ""));
        ignore(err.flush());
    }

    fn draw(&self) {
        if !self.enabled {
            return;
        }
        // A poisoned lock means another thread panicked mid-draw. The meter is
        // cosmetic, so it goes quiet rather than propagating that into the command.
        let Ok(_guard) = self.draw.lock() else {
            return;
        };
        let done = self.done.load(Ordering::Relaxed);
        let mut err = std::io::stderr();
        ignore(write!(err, "\r{}: {done}/{} ", self.label, self.total));
        ignore(err.flush());
    }
}

/// Discards a write result on purpose.
///
/// Every write here is cosmetic, and a meter that could fail a command by being
/// undrawable would be worse than no meter at all. Named rather than a bare discard so
/// that intent is legible at each call.
fn ignore<T>(_result: std::io::Result<T>) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_small_job_draws_nothing() {
        // Under the threshold the meter must be inert even on a terminal, so a fast
        // command cannot flicker a line at someone.
        let progress = Progress::start("checking", MIN_ITEMS - 1);
        assert!(!progress.enabled);
    }

    #[test]
    fn a_redirected_stream_draws_nothing() {
        // The suite runs with stderr captured, which is exactly the redirected case:
        // whatever the item count, nothing may be written.
        let progress = Progress::start("checking", 1_000);
        assert!(!progress.enabled);
        // All three must be safe to call on an inert meter.
        progress.advance();
        progress.finish();
    }
}
