//! Logging + progress reporting.
//!
//! Rufus wires worker threads to the GUI through two global side-channels:
//! `uprintf()` (writes straight into the log edit control) and
//! `UpdateProgress()` (`SendMessage(hProgress, PBM_SETPOS, ...)`). That coupling
//! is exactly what makes the core un-portable. Here we invert it: the core only
//! knows about a [`Reporter`] trait, and the frontend supplies an implementation
//! (a CLI printer, or a GUI bridge that emits signals on the UI thread).

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Info,
    Warn,
    Error,
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Level::Debug => "DEBUG",
            Level::Info => "INFO",
            Level::Warn => "WARN",
            Level::Error => "ERROR",
        };
        f.write_str(s)
    }
}

/// Sink for human-facing log lines and operation progress.
///
/// Must be `Send + Sync` because long-running jobs run on worker threads.
pub trait Reporter: Send + Sync {
    /// A single log line.
    fn log(&self, level: Level, message: &str);

    /// Progress for the named operation, `fraction` in `0.0..=1.0`.
    fn progress(&self, operation: &str, fraction: f32);
}

/// A reporter that discards everything. Useful for tests and headless calls.
pub struct NullReporter;

impl Reporter for NullReporter {
    fn log(&self, _level: Level, _message: &str) {}
    fn progress(&self, _operation: &str, _fraction: f32) {}
}

/// Convenience helpers so call sites read like `rep.info("...")`.
pub trait ReporterExt: Reporter {
    fn debug(&self, msg: &str) {
        self.log(Level::Debug, msg);
    }
    fn info(&self, msg: &str) {
        self.log(Level::Info, msg);
    }
    fn warn(&self, msg: &str) {
        self.log(Level::Warn, msg);
    }
    fn error(&self, msg: &str) {
        self.log(Level::Error, msg);
    }
}

impl<T: Reporter + ?Sized> ReporterExt for T {}
