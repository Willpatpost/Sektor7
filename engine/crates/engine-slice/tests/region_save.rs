//! Stage 5 Batch 4: the RegionDirector authoritative-state codec
//! (`engine-slice/region-director-save/1`) — the session snapshot's
//! director section. Byte-identical round-trips through real
//! transition scenarios (dormancy checkpoints, captured consequences,
//! applied credits, reconstruction labels), the restored pair's
//! continuation invisibility (director + spine + world restored
//! together, reactivations continuing identically), and the
//! fail-closed corruption corpus for the section's own gates.

use engine_core::{Decoder, Diagnostics, Encoder, Fingerprinter, Level, PresenceSample, Spine};
use engine_slice::npc::{NpcConfig, NpcWorld, REGION_COUNT};
use engine_slice::region::{RegionConfig, RegionDirector, stage4_state_fingerprint};
use engine_slice::sim::{SimConfig, SimDriver};

fn near() -> PresenceSample {
    PresenceSample {
        position: (100.0, 0.0),
        velocity: (0.0, 0.0),
    }
}

fn far() -> PresenceSample {
    PresenceSample {
        position: (2000.0, 2000.0),
        velocity: (0.0, 0.0),
    }
}

/// The scenario: active accrual (milestone crossings → pending
/// scratch), a full far-presence deactivation (checkpoints + ledger
/// captures), a near-presence reactivation (exactly-once application
/// → credits, reconstruction labels), a second accrual, and a second
/// deactivation — every director state populated, with BOTH applied
/// and pending captures live in the ledger at the end.
fn scenario_driver() -> SimDriver {
    let mut driver = SimDriver::with_regions(
        NpcConfig::default(),
        SimConfig::default(),
        RegionConfig::default(),
        &[near()],
    )
    .expect("the driver constructs");
    driver.run_headless(150);
    driver.set_presence(vec![far()]);
    driver.run_headless(5);
    driver.set_presence(vec![near()]);
    driver.run_headless(150);
    driver.set_presence(vec![far()]);
    driver.run_headless(5);
    driver
}

#[test]
fn round_trip_preserves_transitions_and_ledger() {
    let driver = scenario_driver();
    let director = driver.director().expect("director");
    // The scenario populated both ledger lanes and the checkpoints.
    assert!(
        director.ledger().applied_count() > 0,
        "the reactivation applied captured consequences"
    );
    assert!(
        director.ledger().pending_count() > 0,
        "the second deactivation captured fresh consequences"
    );
    let mut enc = Encoder::new();
    director.save(&mut enc);
    let bytes = enc.into_bytes();
    let mut dec = Decoder::new(&bytes);
    let restored = RegionDirector::restore(&mut dec).expect("the director restores");
    let mut re_enc = Encoder::new();
    restored.save(&mut re_enc);
    assert_eq!(re_enc.into_bytes(), bytes);
    // The region-state fingerprint — every level, episode,
    // checkpoint, reconstruction label, and ledger entry.
    let mut a = Fingerprinter::new();
    director.region_fingerprint(&mut a);
    let mut b = Fingerprinter::new();
    restored.region_fingerprint(&mut b);
    assert_eq!(a.finish(), b.finish());
    assert_eq!(
        restored.ledger().pending_count(),
        director.ledger().pending_count()
    );
    assert_eq!(
        restored.ledger().applied_count(),
        director.ledger().applied_count()
    );
    assert_eq!(
        restored.ledger().pending_total(),
        director.ledger().pending_total()
    );
}

#[test]
fn restored_pair_continues_invisibly() {
    // The session-composition preview: the spine (with the world
    // section) AND the director restored together continue exactly
    // like the never-saved driver — the full reactivation path
    // (validation, exactly-once application, reconstruction, resume)
    // included.
    let mut driver = SimDriver::with_regions(
        NpcConfig::default(),
        SimConfig::default(),
        RegionConfig::default(),
        &[near()],
    )
    .expect("the driver constructs");
    driver.run_headless(150);
    driver.set_presence(vec![far()]);
    driver.run_headless(5);
    let spine_bytes = driver.spine().save_bytes();
    let mut enc = Encoder::new();
    driver.director().expect("director").save(&mut enc);
    let director_bytes = enc.into_bytes();
    let mut spine = Spine::<NpcWorld>::restore(&spine_bytes, Diagnostics::new(Level::Off))
        .expect("the spine restores");
    let mut dec = Decoder::new(&director_bytes);
    let mut director = RegionDirector::restore(&mut dec).expect("the director restores");
    // Continue both identically: the restored pair through the
    // driver's own per-tick sequence (presence → director apply →
    // spine tick), the driver through its loop.
    let sample = near();
    for _ in 0..20 {
        director.apply(&mut spine, &[sample]);
        spine.run_tick();
    }
    driver.set_presence(vec![sample]);
    driver.run_headless(20);
    assert_eq!(
        stage4_state_fingerprint(&spine, &director),
        stage4_state_fingerprint(driver.spine(), driver.director().expect("director")),
        "the restored pair continues exactly like the never-saved driver"
    );
    // The reactivation really happened on both sides.
    assert!(
        spine.world().credits().iter().sum::<u64>() > 0,
        "the restored pair applied the captured consequences"
    );
}

#[test]
fn corruption_fails_closed() {
    // The section's own gates, on real bytes: the fixed region counts,
    // the finite configuration floats, the LOD discriminants — plus
    // strided truncation (a short input never yields a director).
    let driver = scenario_driver();
    let mut enc = Encoder::new();
    driver.director().expect("director").save(&mut enc);
    let section = enc.into_bytes();
    // The fixed-width prefix layout: the center count, 16 center
    // floats, 7 threshold floats, the adaptive threshold, the state
    // count, then the per-region states.
    const CENTERS_AT: usize = 8;
    const LOD_AT: usize = CENTERS_AT + REGION_COUNT * 2 * 8;
    const ADAPTIVE_AT: usize = LOD_AT + 7 * 8;
    const STATES_COUNT_AT: usize = ADAPTIVE_AT + 8;
    const FIRST_LEVEL_AT: usize = STATES_COUNT_AT + 8;
    assert!(section.len() > FIRST_LEVEL_AT);

    let accepts = |bytes: &[u8]| RegionDirector::restore(&mut Decoder::new(bytes)).is_ok();
    // The center-count gate: 9 regions is not this workload.
    let mut doctored = section.clone();
    doctored[0..8].copy_from_slice(&9u64.to_le_bytes());
    assert!(!accepts(&doctored), "center count 9 rejected");
    // The finite-float gate: a NaN center is corruption, never a
    // configuration.
    let mut doctored = section.clone();
    doctored[CENTERS_AT..CENTERS_AT + 8].copy_from_slice(&f64::NAN.to_bits().to_le_bytes());
    assert!(!accepts(&doctored), "NaN center rejected");
    // The state-count gate.
    let mut doctored = section.clone();
    doctored[STATES_COUNT_AT..STATES_COUNT_AT + 8].copy_from_slice(&9u64.to_le_bytes());
    assert!(!accepts(&doctored), "state count 9 rejected");
    // The LOD discriminant gate: the first state's level.
    let mut doctored = section.clone();
    doctored[FIRST_LEVEL_AT] = 9;
    assert!(!accepts(&doctored), "unknown LOD level rejected");
    // Truncation: strided cuts across the section and the final
    // sixteen bytes.
    let mut cut = 0;
    while cut < section.len() {
        assert!(!accepts(&section[..cut]), "cut at {cut} rejected");
        cut += 97;
    }
    for cut in section.len() - 16..section.len() {
        assert!(!accepts(&section[..cut]), "cut at {cut} rejected");
    }
}
