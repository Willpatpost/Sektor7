//! Four-level simulation-LOD targeting — pure, reusable mechanism
//! (Stage 4 authorization §4, `docs/TECH-ARCHITECTURE.md` §2.5): a
//! presence-driven LOD state machine with per-boundary **hysteresis**
//! (promotion and demotion cross different thresholds, so distance noise
//! at a boundary cannot oscillate the level) and **velocity-aware
//! lookahead** (an approaching presence counts as closer than it is,
//! activating regions before arrival).
//!
//! Mechanism only: thresholds, hysteresis bands, and the lookahead scale
//! are game configuration supplied through [`LodConfig`]; what counts as
//! a presence source (players, cameras) is the application's, and the
//! mapping between event classes and spatial regions is likewise
//! application policy — nothing here mentions classes or regions.
//! Decisions are pure functions of the explicit inputs (current level,
//! region center, presence samples); wall-clock time never enters, and
//! with no presence the level is returned unchanged (what an empty world
//! means is the caller's policy).
//!
//! Arithmetic is IEEE-754 `f64` (`+`/`*`/comparisons — correctly rounded
//! and cross-platform deterministic for these operations); the decision
//! path compares **squared** distances against squared thresholds, so no
//! `sqrt` is needed to choose a level.

/// Simulation level of detail, ordered from most to least detailed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LodLevel {
    /// Highest-detail resident simulation — regions occupied or
    /// directly observed by presence.
    Full,
    /// Reduced-detail representation: coarser, less frequent updates.
    Simplified,
    /// Records plus scheduled wake-ups only: cost follows actual
    /// events, not per-entity per-tick work.
    EventDriven,
    /// No ordinary processing — obligations are suspended elsewhere
    /// (the spine's held set); a dormant region costs storage, not CPU.
    Dormant,
}

impl LodLevel {
    /// One step toward more detail, or `None` at [`LodLevel::Full`].
    pub fn more_detailed(self) -> Option<Self> {
        match self {
            LodLevel::Full => None,
            LodLevel::Simplified => Some(LodLevel::Full),
            LodLevel::EventDriven => Some(LodLevel::Simplified),
            LodLevel::Dormant => Some(LodLevel::EventDriven),
        }
    }

    /// One step toward less detail, or `None` at [`LodLevel::Dormant`].
    pub fn less_detailed(self) -> Option<Self> {
        match self {
            LodLevel::Full => Some(LodLevel::Simplified),
            LodLevel::Simplified => Some(LodLevel::EventDriven),
            LodLevel::EventDriven => Some(LodLevel::Dormant),
            LodLevel::Dormant => None,
        }
    }
}

/// One presence sample: position plus velocity in the application's
/// world units (velocity per unit of the lookahead scale — together
/// they project where the presence will be).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PresenceSample {
    pub position: (f64, f64),
    pub velocity: (f64, f64),
}

/// LOD targeting configuration — all thresholds are distances from a
/// region center in the application's world units.
///
/// Hysteresis: each boundary between adjacent levels has an *enter*
/// threshold (distance must fall **below** it to promote) and an *exit*
/// threshold (distance must rise **above** it to demote), with
/// `enter < exit` — the band between them is stable for both adjacent
/// levels, so a distance oscillating inside a band cannot thrash the
/// level. Adjacent boundaries must not overlap (`exit ≤ next enter`) so
/// at most one promotion/demotion condition can hold per boundary.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LodConfig {
    /// Promote to Full below this distance.
    pub full_enter: f64,
    /// Demote from Full above this distance (`> full_enter`).
    pub full_exit: f64,
    /// Promote to Simplified below this distance.
    pub simplified_enter: f64,
    /// Demote from Simplified above this distance (`> simplified_enter`).
    pub simplified_exit: f64,
    /// Promote to EventDriven below this distance.
    pub event_enter: f64,
    /// Demote from EventDriven above this distance (`> event_enter`).
    pub event_exit: f64,
    /// Lookahead scale multiplied against presence velocity when
    /// projecting an approaching sample's position (≥ 0).
    pub lookahead: f64,
}

impl LodConfig {
    /// Validates finiteness, non-negative lookahead, and threshold
    /// ordering (`full_enter < full_exit ≤ simplified_enter <
    /// simplified_exit ≤ event_enter < event_exit`).
    pub fn validated(self) -> Result<Self, LodConfigError> {
        let values = [
            self.full_enter,
            self.full_exit,
            self.simplified_enter,
            self.simplified_exit,
            self.event_enter,
            self.event_exit,
            self.lookahead,
        ];
        if values.iter().any(|value| !value.is_finite()) || self.lookahead < 0.0 {
            return Err(LodConfigError::InvalidValues);
        }
        let ordered = self.full_enter < self.full_exit
            && self.full_exit <= self.simplified_enter
            && self.simplified_enter < self.simplified_exit
            && self.simplified_exit <= self.event_enter
            && self.event_enter < self.event_exit;
        if !ordered {
            return Err(LodConfigError::ThresholdOrder);
        }
        Ok(self)
    }
}

/// Rejection reasons for an invalid [`LodConfig`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LodConfigError {
    /// A threshold is non-finite, or the lookahead is negative.
    InvalidValues,
    /// Thresholds violate the required enter/exit and cross-boundary
    /// ordering.
    ThresholdOrder,
}

/// Pure LOD targeting: presence distance → level under hysteresis.
#[derive(Debug)]
pub struct LodController {
    config: LodConfig,
    // Squared thresholds — the decision path never takes a sqrt.
    full_enter_sq: f64,
    full_exit_sq: f64,
    simplified_enter_sq: f64,
    simplified_exit_sq: f64,
    event_enter_sq: f64,
    event_exit_sq: f64,
}

impl LodController {
    pub fn new(config: LodConfig) -> Result<Self, LodConfigError> {
        config.validated()?;
        Ok(LodController {
            full_enter_sq: config.full_enter * config.full_enter,
            full_exit_sq: config.full_exit * config.full_exit,
            simplified_enter_sq: config.simplified_enter * config.simplified_enter,
            simplified_exit_sq: config.simplified_exit * config.simplified_exit,
            event_enter_sq: config.event_enter * config.event_enter,
            event_exit_sq: config.event_exit * config.event_exit,
            config,
        })
    }

    pub fn config(&self) -> LodConfig {
        self.config
    }

    /// Effective distance from the presence set to a region center
    /// (diagnostic form, with sqrt). The nearest sample wins, and each
    /// sample counts the nearer of its current and lookahead-projected
    /// position — approaching regions activate before arrival, receding
    /// velocity never counts against.
    pub fn effective_distance(
        &self,
        center: (f64, f64),
        presence: &[PresenceSample],
    ) -> Option<f64> {
        self.effective_distance_sq(center, presence).map(f64::sqrt)
    }

    /// Next LOD for a region given its current level, center, and the
    /// presence set. Demotion applies while the current level's exit
    /// threshold is exceeded; promotion while the next-more-detailed
    /// level's enter threshold is met — so a presence jump can traverse
    /// multiple levels in one decision, while a distance inside a
    /// hysteresis band never flips the level. With no presence, the
    /// current level is returned unchanged.
    pub fn next_lod(
        &self,
        current: LodLevel,
        center: (f64, f64),
        presence: &[PresenceSample],
    ) -> LodLevel {
        let Some(distance_sq) = self.effective_distance_sq(center, presence) else {
            return current;
        };
        let mut lod = current;
        while let Some(lower) = lod.less_detailed() {
            if distance_sq > self.exit_sq(lod) {
                lod = lower;
            } else {
                break;
            }
        }
        while let Some(higher) = lod.more_detailed() {
            if distance_sq < self.enter_sq(higher) {
                lod = higher;
            } else {
                break;
            }
        }
        lod
    }

    /// Nearest effective squared distance over the presence set: per
    /// sample, the smaller of the current and projected distances.
    fn effective_distance_sq(
        &self,
        center: (f64, f64),
        presence: &[PresenceSample],
    ) -> Option<f64> {
        presence
            .iter()
            .map(|sample| {
                let projected = (
                    sample.position.0 + sample.velocity.0 * self.config.lookahead,
                    sample.position.1 + sample.velocity.1 * self.config.lookahead,
                );
                dist_sq(sample.position, center).min(dist_sq(projected, center))
            })
            .reduce(f64::min)
    }

    /// Enter threshold of the boundary whose more-detailed side is
    /// `promoted_to`.
    fn enter_sq(&self, promoted_to: LodLevel) -> f64 {
        match promoted_to {
            LodLevel::Full => self.full_enter_sq,
            LodLevel::Simplified => self.simplified_enter_sq,
            // `EventDriven`'s enter threshold governs Dormant →
            // EventDriven; `Dormant` is never a promotion target.
            LodLevel::EventDriven | LodLevel::Dormant => self.event_enter_sq,
        }
    }

    /// Exit threshold of the boundary whose less-detailed side is below
    /// `demoted_from`.
    fn exit_sq(&self, demoted_from: LodLevel) -> f64 {
        match demoted_from {
            LodLevel::Full => self.full_exit_sq,
            LodLevel::Simplified => self.simplified_exit_sq,
            LodLevel::EventDriven => self.event_exit_sq,
            // Dormant cannot demote further; the value is never used.
            LodLevel::Dormant => f64::INFINITY,
        }
    }
}

fn dist_sq(a: (f64, f64), b: (f64, f64)) -> f64 {
    let dx = a.0 - b.0;
    let dy = a.1 - b.1;
    dx * dx + dy * dy
}
