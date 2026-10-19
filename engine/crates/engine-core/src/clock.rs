//! Canonical campaign clock seed (one clock per campaign — M-27 in miniature).

use std::fmt;

/// A logical campaign tick.
///
/// The spine's tick loop is the only component that advances campaign time;
/// wall-clock/OS time never enters simulation logic
/// (`docs/TECH-ARCHITECTURE.md` §3.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Tick(pub u64);

impl Tick {
    pub const ZERO: Tick = Tick(0);

    pub fn as_u64(self) -> u64 {
        self.0
    }

    pub fn saturating_add(self, ticks: u64) -> Tick {
        Tick(self.0.saturating_add(ticks))
    }
}

impl fmt::Display for Tick {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
