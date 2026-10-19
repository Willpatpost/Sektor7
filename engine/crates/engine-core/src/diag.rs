//! Basic diagnostics: a severity-threshold record store with an optional
//! stderr echo.
//!
//! Wall-clock timestamps are deliberately excluded — diagnostics must never
//! make logical state non-deterministic. Records carry a level and a message
//! only. Richer metrics buses arrive with the subsystems that need them
//! (anti-abstraction rule, `docs/TECH-ARCHITECTURE.md` §8).

use std::io::Write;

/// Diagnostic severity. `Off` disables logging entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Off,
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Off => "off",
            Level::Trace => "trace",
            Level::Debug => "debug",
            Level::Info => "info",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }

    pub fn parse(text: &str) -> Option<Level> {
        match text.trim_ascii().to_ascii_lowercase().as_str() {
            "off" => Some(Level::Off),
            "trace" => Some(Level::Trace),
            "debug" => Some(Level::Debug),
            "info" => Some(Level::Info),
            "warn" | "warning" => Some(Level::Warn),
            "error" => Some(Level::Error),
            _ => None,
        }
    }
}

/// One diagnostic record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub level: Level,
    pub message: String,
}

/// Severity-threshold record store with an optional stderr echo.
///
/// Records accumulate unboundedly in Stage 0; a retention policy arrives
/// when a subsystem actually needs bounded buffers (anti-abstraction rule).
#[derive(Debug, Clone)]
pub struct Diagnostics {
    min_level: Level,
    records: Vec<Record>,
    echo_stderr: bool,
}

impl Diagnostics {
    pub fn new(min_level: Level) -> Self {
        Diagnostics {
            min_level,
            records: Vec::new(),
            echo_stderr: false,
        }
    }

    pub fn with_stderr_echo(min_level: Level) -> Self {
        Diagnostics {
            min_level,
            records: Vec::new(),
            echo_stderr: true,
        }
    }

    pub fn set_min_level(&mut self, level: Level) {
        self.min_level = level;
    }

    pub fn min_level(&self) -> Level {
        self.min_level
    }

    /// Records a message at `level` unless it falls below the threshold.
    /// More severe records always pass; `Off` suppresses everything.
    pub fn log(&mut self, level: Level, message: impl Into<String>) {
        if self.min_level == Level::Off || level < self.min_level {
            return;
        }
        let record = Record {
            level,
            message: message.into(),
        };
        if self.echo_stderr {
            let _ = writeln!(
                std::io::stderr(),
                "[{}] {}",
                record.level.as_str(),
                record.message
            );
        }
        self.records.push(record);
    }

    pub fn records(&self) -> &[Record] {
        &self.records
    }

    /// Number of records at or above `level`.
    pub fn count_at_or_above(&self, level: Level) -> usize {
        self.records.iter().filter(|r| r.level >= level).count()
    }

    pub fn clear(&mut self) {
        self.records.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_drops_less_severe_records() {
        let mut diag = Diagnostics::new(Level::Warn);
        diag.log(Level::Trace, "quiet");
        diag.log(Level::Info, "also quiet");
        diag.log(Level::Warn, "kept");
        diag.log(Level::Error, "kept too");
        assert_eq!(diag.records().len(), 2);
        assert_eq!(diag.count_at_or_above(Level::Error), 1);
    }

    #[test]
    fn off_suppresses_everything() {
        let mut diag = Diagnostics::new(Level::Off);
        diag.log(Level::Error, "suppressed");
        assert!(diag.records().is_empty());
    }

    #[test]
    fn level_parse_round_trips() {
        for level in [
            Level::Off,
            Level::Trace,
            Level::Debug,
            Level::Info,
            Level::Warn,
            Level::Error,
        ] {
            assert_eq!(Level::parse(level.as_str()), Some(level));
        }
        assert_eq!(Level::parse("nonsense"), None);
    }
}
