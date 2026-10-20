//! Application timing policy for the integrated render loop (Stage 3
//! corrective finding A): the campaign simulation accrues **all**
//! elapsed monotonic time — rendering stalls and minimized periods
//! included — while camera movement is capped so a post-stall frame
//! cannot teleport the view. The policy is extracted from the demo
//! loop so the timing integration itself is unit-testable without a
//! GPU or window: the render loop routes every frame boundary through
//! [`FrameTiming::begin_frame`], on rendering and non-rendering
//! (minimized) frames alike.

use std::time::{Duration, Instant};

/// Camera-delta cap: frames longer than this move the camera as if the
/// frame were this long (post-stall view stability). The simulation is
/// NOT subject to this cap — simulation time is never silently
/// discarded.
pub const CAMERA_DT_CAP: Duration = Duration::from_millis(100);

/// One frame's timing decision, made by [`FrameTiming::begin_frame`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameSplit {
    /// Simulation advance for this frame: always the full raw elapsed
    /// interval.
    pub sim_advance: Duration,
    /// Camera movement delta: the interval capped at
    /// [`CAMERA_DT_CAP`], or `None` on frames that render nothing
    /// (minimized window) — no camera step without presentation.
    pub camera_dt: Option<Duration>,
}

/// Splits raw monotonic frame intervals into simulation and camera
/// deltas, and keeps the running total of advanced simulation time so
/// the no-discard invariant is observable in the run summary.
pub struct FrameTiming {
    camera_cap: Duration,
    last_frame_start: Instant,
    total_sim: Duration,
}

impl FrameTiming {
    pub fn new() -> Self {
        Self::new_at(CAMERA_DT_CAP, Instant::now())
    }

    /// Entry point with a controlled clock origin (tests).
    pub fn new_at(camera_cap: Duration, origin: Instant) -> Self {
        FrameTiming {
            camera_cap,
            last_frame_start: origin,
            total_sim: Duration::ZERO,
        }
    }

    /// Marks the frame boundary at `now`. `rendering` is false on
    /// frames where nothing will present (minimized window): the
    /// simulation still advances by the full interval — the campaign
    /// clock keeps running while the application runs; there is no
    /// silent pause policy.
    pub fn begin_frame(&mut self, now: Instant, rendering: bool) -> FrameSplit {
        let raw = now - self.last_frame_start;
        self.last_frame_start = now;
        self.total_sim += raw;
        FrameSplit {
            sim_advance: raw,
            camera_dt: rendering.then(|| raw.min(self.camera_cap)),
        }
    }

    /// Total simulation time handed to the driver since construction —
    /// every elapsed nanosecond the loop has seen.
    pub fn total_sim_advanced(&self) -> Duration {
        self.total_sim
    }
}

impl Default for FrameTiming {
    fn default() -> Self {
        Self::new()
    }
}
