//! Stage 4 LOD controller contracts (authorization §4, §9 B): pure
//! presence-driven targeting, per-boundary hysteresis (no boundary
//! thrashing), velocity-aware lookahead, multi-presence resolution,
//! configuration validation, and decision determinism.

use engine_core::lod::{LodConfig, LodConfigError, LodController, LodLevel, PresenceSample};

fn config() -> LodConfig {
    LodConfig {
        full_enter: 10.0,
        full_exit: 14.0,
        simplified_enter: 30.0,
        simplified_exit: 40.0,
        event_enter: 80.0,
        event_exit: 100.0,
        lookahead: 2.0,
    }
}

fn controller() -> LodController {
    LodController::new(config()).expect("valid config")
}

fn sample_at(x: f64, y: f64) -> PresenceSample {
    PresenceSample {
        position: (x, y),
        velocity: (0.0, 0.0),
    }
}

fn sample_moving(x: f64, y: f64, vx: f64, vy: f64) -> PresenceSample {
    PresenceSample {
        position: (x, y),
        velocity: (vx, vy),
    }
}

#[test]
fn config_validation_rejects_invalid_thresholds() {
    let mut inverted = config();
    inverted.full_enter = 20.0; // > full_exit
    assert_eq!(
        LodController::new(inverted).unwrap_err(),
        LodConfigError::ThresholdOrder
    );
    let mut overlapping = config();
    overlapping.full_exit = 31.0; // > simplified_enter — cross-boundary overlap
    assert_eq!(
        LodController::new(overlapping).unwrap_err(),
        LodConfigError::ThresholdOrder
    );
    let mut negative_lookahead = config();
    negative_lookahead.lookahead = -1.0;
    assert_eq!(
        LodController::new(negative_lookahead).unwrap_err(),
        LodConfigError::InvalidValues
    );
    assert!(LodController::new(config()).is_ok());
}

#[test]
fn distance_bands_target_levels_with_hysteresis_stability() {
    let controller = controller();
    let center = (0.0, 0.0);
    // Deep inside the full band → Full from any level (multi-level jump).
    assert_eq!(
        controller.next_lod(LodLevel::Dormant, center, &[sample_at(5.0, 0.0)]),
        LodLevel::Full
    );
    // Far beyond every band → Dormant from any level.
    assert_eq!(
        controller.next_lod(LodLevel::Full, center, &[sample_at(500.0, 0.0)]),
        LodLevel::Dormant
    );
    // Just inside the enter threshold promotes; just outside the exit
    // threshold demotes.
    assert_eq!(
        controller.next_lod(LodLevel::Simplified, center, &[sample_at(9.9, 0.0)]),
        LodLevel::Full
    );
    assert_eq!(
        controller.next_lod(LodLevel::Full, center, &[sample_at(14.1, 0.0)]),
        LodLevel::Simplified
    );
    // Inside the full hysteresis band (10..14): both adjacent levels are
    // stable — the anti-thrash property.
    let band = [sample_at(12.0, 0.0)];
    assert_eq!(
        controller.next_lod(LodLevel::Full, center, &band),
        LodLevel::Full
    );
    assert_eq!(
        controller.next_lod(LodLevel::Simplified, center, &band),
        LodLevel::Simplified
    );
    // The event band behaves the same way for EventDriven/Dormant.
    let event_band = [sample_at(90.0, 0.0)];
    assert_eq!(
        controller.next_lod(LodLevel::EventDriven, center, &event_band),
        LodLevel::EventDriven
    );
    assert_eq!(
        controller.next_lod(LodLevel::Dormant, center, &event_band),
        LodLevel::Dormant
    );
}

#[test]
fn hysteresis_prevents_boundary_oscillation() {
    let controller = controller();
    let center = (0.0, 0.0);
    let mut level = LodLevel::Full;
    for distance in [11.0, 13.0, 10.5, 13.9, 11.5] {
        level = controller.next_lod(level, center, &[sample_at(distance, 0.0)]);
        assert_eq!(level, LodLevel::Full, "band oscillation must not demote");
    }
    let mut level = LodLevel::Simplified;
    for distance in [11.0, 13.0, 10.5, 13.9, 11.5] {
        level = controller.next_lod(level, center, &[sample_at(distance, 0.0)]);
        assert_eq!(
            level,
            LodLevel::Simplified,
            "band oscillation must not promote"
        );
    }
}

#[test]
fn velocity_lookahead_activates_approaching_regions_earlier() {
    let controller = controller();
    let center = (100.0, 0.0);
    // Stationary at distance 25 (inside the simplified band): stable.
    let stationary = [sample_moving(75.0, 0.0, 0.0, 0.0)];
    assert_eq!(
        controller.next_lod(LodLevel::Simplified, center, &stationary),
        LodLevel::Simplified
    );
    // Approaching at 8 per lookahead unit: projected distance 9 < 10 →
    // the region activates to Full before arrival.
    let approaching = [sample_moving(75.0, 0.0, 8.0, 0.0)];
    assert_eq!(
        controller.next_lod(LodLevel::Simplified, center, &approaching),
        LodLevel::Full,
        "lookahead projects the approaching presence"
    );
    // Receding velocity never counts against: projected is farther, the
    // current distance still decides.
    let receding = [sample_moving(75.0, 0.0, -8.0, 0.0)];
    assert_eq!(
        controller.next_lod(LodLevel::Simplified, center, &receding),
        LodLevel::Simplified
    );
}

#[test]
fn multiple_presence_inputs_use_the_nearest() {
    let controller = controller();
    let center = (0.0, 0.0);
    // One far, one near → the near sample decides.
    let two = [sample_at(70.0, 0.0), sample_at(5.0, 0.0)];
    assert_eq!(
        controller.next_lod(LodLevel::Dormant, center, &two),
        LodLevel::Full
    );
    // Only the far sample → EventDriven (70 < 80 enter, no further).
    assert_eq!(
        controller.next_lod(LodLevel::Dormant, center, &[sample_at(70.0, 0.0)]),
        LodLevel::EventDriven
    );
    // No presence at all: the level is unchanged (what an empty world
    // means is application policy, decided above this mechanism).
    assert_eq!(
        controller.next_lod(LodLevel::Simplified, center, &[]),
        LodLevel::Simplified
    );
}

#[test]
fn targeting_is_pure_and_deterministic() {
    let controller = controller();
    let center = (33.0, 44.0);
    let presence = [sample_moving(10.0, 20.0, 1.5, -0.5), sample_at(60.0, 0.0)];
    let first = controller.next_lod(LodLevel::EventDriven, center, &presence);
    let second = controller.next_lod(LodLevel::EventDriven, center, &presence);
    assert_eq!(first, second);
}
