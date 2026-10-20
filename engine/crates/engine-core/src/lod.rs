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
//! `sqrt` is needed to choose a level. Sample geometry policy (Batch 1
//! review item B): a presence sample with any non-finite position or
//! velocity coordinate never participates — NaN/Infinity cannot
//! determine an authoritative transition — and a lookahead projection
//! that overflows to non-finite degenerates to the stationary position.

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
    /// Validates finiteness, non-negative thresholds and lookahead, the
    /// finiteness of every **squared** threshold (the decision path
    /// compares squared distances, so a threshold whose square
    /// overflows would silently change meaning — Batch 1 review item
    /// B), and threshold ordering (`full_enter < full_exit ≤
    /// simplified_enter < simplified_exit ≤ event_enter < event_exit`).
    pub fn validated(self) -> Result<Self, LodConfigError> {
        let thresholds = [
            self.full_enter,
            self.full_exit,
            self.simplified_enter,
            self.simplified_exit,
            self.event_enter,
            self.event_exit,
        ];
        let values_ok = thresholds
            .iter()
            .all(|value| value.is_finite() && *value >= 0.0 && (*value * *value).is_finite())
            && self.lookahead.is_finite()
            && self.lookahead >= 0.0;
        if !values_ok {
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
    /// A threshold is non-finite or negative, its square is not finite,
    /// or the lookahead is negative or non-finite.
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
    /// sample contributes the minimum distance from the center to its
    /// bounded movement segment — approaching regions activate before
    /// arrival, a crossing path activates even with distant endpoints,
    /// and receding velocity never counts beyond the current position.
    /// Non-finite samples do not participate (see
    /// [`Self::next_lod`]'s policy note).
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
    /// sample, the minimum squared distance from the center to the
    /// **bounded movement segment** `[position, position + velocity ×
    /// lookahead]` — a projected path that crosses a region activates
    /// it even when both endpoints lie outside every threshold (Batch 1
    /// review item A), and receding movement never counts beyond the
    /// current position (the segment includes it).
    ///
    /// Non-finite policy (Batch 1 review item B): a sample with any
    /// non-finite position or velocity coordinate does not participate
    /// — NaN/Infinity never determines an authoritative transition. A
    /// projection that overflows to non-finite degenerates to the
    /// stationary position, keeping the result finite and deterministic.
    fn effective_distance_sq(
        &self,
        center: (f64, f64),
        presence: &[PresenceSample],
    ) -> Option<f64> {
        presence
            .iter()
            .filter_map(|sample| {
                let (px, py) = sample.position;
                let (vx, vy) = sample.velocity;
                if !px.is_finite() || !py.is_finite() || !vx.is_finite() || !vy.is_finite() {
                    return None;
                }
                let projected = (
                    px + vx * self.config.lookahead,
                    py + vy * self.config.lookahead,
                );
                let end = if projected.0.is_finite() && projected.1.is_finite() {
                    projected
                } else {
                    sample.position
                };
                Some(segment_dist_sq(sample.position, end, center))
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

/// Minimum squared distance from `center` to the bounded segment
/// `[start, end]`, endpoints included (Batch 1 review item A): a
/// projected path passing near — or through — a region counts even when
/// both endpoints are distant. Zero-length segments (stationary
/// presence) fall back to the point distance.
///
/// Numerically robust projection (Batch 2 priority E): the direction is
/// scaled by its largest component before any division, so finite but
/// huge coordinates cannot yield `inf/inf` = NaN. The scaled
/// denominator is finite by construction (each scaled component is ≤ 1
/// in magnitude); a non-finite scaled projection means the center is
/// colossally far relative to the segment — deterministically
/// infinitely distant, never NaN. The residual differences are bounded
/// between the finite endpoint offsets, so only the final squaring can
/// overflow — to `inf`, which orders correctly (farther than
/// everything) instead of poisoning comparisons.
fn segment_dist_sq(start: (f64, f64), end: (f64, f64), center: (f64, f64)) -> f64 {
    let dx = end.0 - start.0;
    let dy = end.1 - start.1;
    if dx == 0.0 && dy == 0.0 {
        return dist_sq(start, center);
    }
    let scale = dx.abs().max(dy.abs());
    let ux = dx / scale;
    let uy = dy / scale;
    let denominator = ux * ux + uy * uy;
    let wx = center.0 - start.0;
    let wy = center.1 - start.1;
    let projection = (wx / scale) * ux + (wy / scale) * uy;
    if !projection.is_finite() {
        return f64::INFINITY;
    }
    let t = (projection / denominator).clamp(0.0, 1.0);
    let ex = wx - t * dx;
    let ey = wy - t * dy;
    ex * ex + ey * ey
}
