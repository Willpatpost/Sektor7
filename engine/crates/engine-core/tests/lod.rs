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

#[test]
fn segment_crossing_activates_regions_both_endpoints_miss() {
    // Batch 1 review item A: a high-speed presence whose projected
    // movement segment passes through a region must activate it even
    // though both endpoints lie far outside every threshold.
    let controller = controller();
    let center = (0.0, 0.0);
    // Stationary at distance 120 — beyond event_exit (100): stays
    // Dormant.
    let stationary = [sample_at(-120.0, 0.0)];
    assert_eq!(
        controller.next_lod(LodLevel::Dormant, center, &stationary),
        LodLevel::Dormant
    );
    // Crossing at velocity 120 with lookahead 2: the bounded segment
    // runs from (-120, 0) to (120, 0) — straight through the center —
    // so the minimum segment distance is 0 and the region activates
    // fully, endpoint distances notwithstanding.
    let crossing = [sample_moving(-120.0, 0.0, 120.0, 0.0)];
    assert_eq!(
        controller.next_lod(LodLevel::Dormant, center, &crossing),
        LodLevel::Full
    );
}

#[test]
fn segment_near_miss_respects_thresholds() {
    // A projected path passing near — but outside every activation
    // threshold — must not activate; the same crossing pulled inside a
    // band reaches exactly the level the band warrants.
    let controller = controller();
    let center = (0.0, 0.0);
    let near_miss = [sample_moving(-120.0, 105.0, 120.0, 0.0)];
    // Segment at constant height 105 — beyond event_exit (100).
    assert_eq!(
        controller.next_lod(LodLevel::Dormant, center, &near_miss),
        LodLevel::Dormant
    );
    let grazing = [sample_moving(-120.0, 70.0, 120.0, 0.0)];
    // Segment at height 70 — inside event_enter (80) but outside
    // simplified_enter (30): a crossing presence reaches EventDriven.
    assert_eq!(
        controller.next_lod(LodLevel::Dormant, center, &grazing),
        LodLevel::EventDriven
    );
    let band = [sample_moving(-120.0, 90.0, 120.0, 0.0)];
    // Height 90 sits in the event hysteresis band (80..100): stable for
    // its adjacent levels via segment geometry, as via points.
    assert_eq!(
        controller.next_lod(LodLevel::EventDriven, center, &band),
        LodLevel::EventDriven
    );
    assert_eq!(
        controller.next_lod(LodLevel::Dormant, center, &band),
        LodLevel::Dormant
    );
}

#[test]
fn negative_or_oversized_thresholds_are_rejected() {
    // Batch 1 review item B: negative distances change meaning when
    // squared, and thresholds whose squares overflow are unusable on
    // the squared-distance decision path.
    let mut negative = config();
    negative.full_enter = -10.0;
    assert_eq!(
        LodController::new(negative).unwrap_err(),
        LodConfigError::InvalidValues
    );
    let mut oversized = config();
    oversized.event_exit = 1e200; // squared overflows to infinity
    assert_eq!(
        LodController::new(oversized).unwrap_err(),
        LodConfigError::InvalidValues
    );
}

#[test]
fn non_finite_presence_samples_do_not_participate() {
    // Batch 1 review item B policy: NaN/Infinity in a sample's position
    // or velocity never determines an authoritative transition — the
    // sample is ignored entirely. A lone non-finite sample behaves like
    // no presence at all (level unchanged); a finite sibling decides.
    let controller = controller();
    let center = (0.0, 0.0);
    let nan_position = [PresenceSample {
        position: (f64::NAN, 0.0),
        velocity: (0.0, 0.0),
    }];
    assert_eq!(
        controller.next_lod(LodLevel::Simplified, center, &nan_position),
        LodLevel::Simplified,
        "a lone non-finite sample is no presence"
    );
    let inf_velocity = [PresenceSample {
        position: (5.0, 0.0),
        velocity: (f64::INFINITY, 0.0),
    }];
    assert_eq!(
        controller.next_lod(LodLevel::Simplified, center, &inf_velocity),
        LodLevel::Simplified,
        "the whole sample is ignored, even with a finite near position"
    );
    let mixed = [
        PresenceSample {
            position: (f64::NAN, f64::NAN),
            velocity: (0.0, 0.0),
        },
        sample_at(5.0, 0.0),
    ];
    assert_eq!(
        controller.next_lod(LodLevel::Dormant, center, &mixed),
        LodLevel::Full,
        "finite samples decide when non-finite ones are ignored"
    );
}

#[test]
fn overflowed_projection_degenerates_to_stationary() {
    // A finite sample whose lookahead projection overflows to infinity
    // keeps a finite, deterministic distance by falling back to its
    // current position — never fabricating proximity or NaN.
    let controller = controller();
    let center = (0.0, 0.0);
    let overflowing = [PresenceSample {
        position: (1e150, 0.0),
        velocity: (1e308, 0.0),
    }];
    let distance = controller
        .effective_distance(center, &overflowing)
        .expect("finite fallback distance");
    assert!(distance.is_finite());
    assert_eq!(
        controller.next_lod(LodLevel::Dormant, center, &overflowing),
        LodLevel::Dormant,
        "an overflowing projection never fabricates proximity"
    );
}

#[test]
fn segment_distance_is_nan_free_under_huge_coordinates() {
    // Batch 2 priority E: finite-but-huge coordinates must not produce
    // NaN through squared-length/dot overflow. The naive formulation
    // computes inf/inf = NaN on the crossing case below; the robust
    // scaled projection keeps the crossing exact, resolves overflowing
    // distances to infinitely-far (which orders correctly), and never
    // returns NaN.
    let controller = controller();
    let center = (0.0, 0.0);
    // A colossal crossing: the segment spans ±1e200 through the center.
    // Naive: length_sq and dot both overflow to inf → t = NaN. Robust:
    // exact zero distance → Full.
    let huge_crossing = [sample_moving(-1e200, 0.0, 1e200, 0.0)];
    assert_eq!(
        controller.next_lod(LodLevel::Dormant, center, &huge_crossing),
        LodLevel::Full,
        "huge-coordinate crossings stay exact — never NaN-poisoned"
    );
    // A colossal near miss: the segment passes at height 1e200 — the
    // distance square overflows to inf, the honest far result.
    let huge_near_miss = [sample_moving(-1e200, 1e200, 1e200, 0.0)];
    let distance = controller
        .effective_distance(center, &huge_near_miss)
        .expect("a distance exists");
    assert!(!distance.is_nan(), "never NaN");
    assert_eq!(
        controller.next_lod(LodLevel::Dormant, center, &huge_near_miss),
        LodLevel::Dormant,
        "overflowing distances resolve to deterministically far"
    );
    // The pathological projection: a microscopic segment with a
    // colossally distant center — scaled terms overflow and must
    // resolve to infinitely far, never NaN.
    let microscopic = [sample_moving(1e-300, 0.0, 1e-300, 0.0)];
    let far_center = (1e308, 0.0);
    let distance = controller
        .effective_distance(far_center, &microscopic)
        .expect("a distance exists");
    assert!(
        !distance.is_nan(),
        "never NaN, even for overflowing projection terms"
    );
    assert_eq!(
        controller.next_lod(LodLevel::Simplified, far_center, &microscopic),
        LodLevel::Dormant,
        "deterministically far — demoted all the way, never NaN-frozen"
    );
}
