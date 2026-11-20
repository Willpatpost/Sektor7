//! Round-local output of the encounter transition. Consumers receive complete
//! volleys in simulation order. Neither consumption nor loss changes the fight.

use std::collections::VecDeque;

use engine_core::WeaponId;

use crate::encounter_demo::EncounterOutcome;

pub const EVENT_CAPACITY: usize = 32;
/// A stalled consumer should resume with the current fight, without replaying
/// an old burst of sound or briefly displaying expired geometry.
pub const MAX_EVENT_AGE_TICKS: u64 = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RayImpact {
    Adversary {
        target: u32,
        damaged: bool,
        defeated: bool,
    },
    Dormant {
        target: u32,
    },
    Cover,
    Miss,
    OutOfRange,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RayFact {
    /// The direction supplied to the resolver (spread directions need not be unit length).
    pub direction: [f32; 3],
    pub endpoint: [f32; 3],
    pub impact: RayImpact,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Volley {
    /// Round-local ordinal of this accepted trigger pull.
    pub id: u32,
    pub weapon: WeaponId,
    /// World-space eye and facing sampled on the logical tick that resolved the shot.
    pub origin: [f32; 3],
    pub aim: [f32; 3],
    /// Valid resolved rays in the weapon's deterministic fan order.
    pub rays: Vec<RayFact>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum PresentationEvent {
    Fired(Volley),
    Switched(WeaponId),
    PlayerDamaged,
    Activated,
    RoundStarted,
    Terminal(EncounterOutcome),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct StreamStats {
    pub queued: usize,
    /// Whole oldest events evicted to make room.
    pub dropped: u64,
    pub stale: u64,
    /// Refused rays or endpoints that cannot be represented with finite coordinates.
    pub invalid_rays: u64,
}

#[derive(Debug, Clone, Default)]
pub struct PresentationStream {
    pending: VecDeque<(u64, PresentationEvent)>,
    /// The normal single-consumer loop reuses one ray buffer on every fire.
    recycled_rays: Vec<Vec<RayFact>>,
    dropped: u64,
    stale: u64,
    invalid_rays: u64,
}

impl PresentationStream {
    pub fn stats(&self) -> StreamStats {
        StreamStats {
            queued: self.pending.len(),
            dropped: self.dropped,
            stale: self.stale,
            invalid_rays: self.invalid_rays,
        }
    }

    pub(crate) fn ray_buffer(&mut self, count: usize) -> Vec<RayFact> {
        debug_assert!(count <= engine_core::MAX_SPREAD_RAYS as usize);
        let mut buffer = self.recycled_rays.pop().unwrap_or_default();
        buffer.reserve_exact(count);
        buffer
    }

    pub(crate) fn invalid_ray(&mut self) {
        self.invalid_rays = self.invalid_rays.saturating_add(1);
    }

    fn recycle(&mut self, event: PresentationEvent) {
        if let PresentationEvent::Fired(mut volley) = event
            && self.recycled_rays.len() < EVENT_CAPACITY
        {
            volley.rays.clear();
            self.recycled_rays.push(volley.rays);
        }
    }

    pub(crate) fn push(&mut self, tick: u64, event: PresentationEvent) {
        if self.pending.len() == EVENT_CAPACITY {
            let (_, oldest) = self.pending.pop_front().expect("capacity is positive");
            self.recycle(oldest);
            self.dropped = self.dropped.saturating_add(1);
        }
        self.pending.push_back((tick, event));
    }

    /// Delivers once to a caller that fans out to all render and audio consumers.
    /// `now` is the current round's logical tick. Each complete event is kept for
    /// at most 16 ticks, and insertion order breaks ties within one tick.
    pub fn drain(&mut self, now: u64, mut consume: impl FnMut(u64, &PresentationEvent)) {
        while let Some((tick, event)) = self.pending.pop_front() {
            if now.saturating_sub(tick) < MAX_EVENT_AGE_TICKS {
                consume(tick, &event);
            } else {
                self.stale = self.stale.saturating_add(1);
            }
            self.recycle(event);
        }
    }
}
