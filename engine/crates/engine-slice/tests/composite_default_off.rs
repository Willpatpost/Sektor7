//! The R14 production-default activation-refusal proof — this test
//! file is its OWN binary (its own process); no test in this file
//! calls the activation facility, so the process-global flag starts
//! at its default. The file compiles in every build (no
//! required-features) — the default production configuration.

use engine_persist::StoredObject;
use engine_persist::composite_manifest::{self, CompositeManifest, ManifestEntry, SectionKind};
use engine_persist::local::LocalFileMedium;
use engine_persist::medium::{CanonicalMedium, MediumError};
use engine_persist::record::{AuthorityMode, RevisionRecord};

fn temp_root() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "s7-k2c1-r14-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ))
}

#[test]
fn the_production_default_activation_refuses_composite_publication() {
    // R14: in THIS process (its own test binary), no test has called
    // the activation facility — the process-global flag is at its
    // default. With the composite-test-fixture feature enabled (the
    // engine-slice dev-dep), the facility EXISTS and the flag
    // starts FALSE; the append_composite on a real provider REFUSES
    // with the typed ActivationBarrier. The compile-time proof (the
    // function absent from a production build) is the feature gate
    // itself — verified by the delivery evidence (nm shows 0 symbols
    // in the release binary; the function is #[cfg(feature)]-gated).
    assert!(
        !engine_persist::composite_append::composite_publication_enabled(),
        "the process-global flag starts at its default (false) in this isolated binary"
    );

    // A real provider refuses the composite publication.
    let root = temp_root();
    std::fs::create_dir_all(&root).unwrap();
    let mut medium = LocalFileMedium::open(root.join("store")).unwrap();

    // A minimal valid composite fixture.
    let mut entries = Vec::new();
    let mut chunks = Vec::new();
    for (seed, tag) in [
        (0u8, composite_manifest::TAG_HEADER),
        (1, composite_manifest::TAG_NEAR),
        (2, composite_manifest::TAG_FAR),
        (3, composite_manifest::TAG_BLOCKED),
        (4, composite_manifest::TAG_HELD),
        (5, composite_manifest::TAG_LEDGER_META),
    ] {
        let payload = vec![seed; 10 + seed as usize];
        let object = composite_manifest::encode_chunk_envelope(tag, &payload);
        entries.push(ManifestEntry {
            tag,
            identity: object.hash(),
            payload_length: payload.len() as u64,
            kind: SectionKind::Singleton,
        });
        chunks.push(object);
    }
    let sealed = 4u64 * 31;
    for seed in [6u8, 7] {
        let payload = vec![seed; sealed as usize];
        let object = composite_manifest::encode_chunk_envelope(
            composite_manifest::TAG_LEDGER_RECORDS,
            &payload,
        );
        entries.push(ManifestEntry {
            tag: composite_manifest::TAG_LEDGER_RECORDS,
            identity: object.hash(),
            payload_length: sealed,
            kind: SectionKind::LedgerSealed,
        });
        chunks.push(object);
    }
    let tail = 2u64 * 31;
    let payload = vec![8u8; tail as usize];
    let object =
        composite_manifest::encode_chunk_envelope(composite_manifest::TAG_LEDGER_RECORDS, &payload);
    entries.push(ManifestEntry {
        tag: composite_manifest::TAG_LEDGER_RECORDS,
        identity: object.hash(),
        payload_length: tail,
        kind: SectionKind::LedgerTail,
    });
    chunks.push(object);
    for (seed, tag) in [
        (9u8, composite_manifest::TAG_WORLD),
        (10, composite_manifest::TAG_DIRECTOR),
    ] {
        let payload = vec![seed; 10 + seed as usize];
        let object = composite_manifest::encode_chunk_envelope(tag, &payload);
        entries.push(ManifestEntry {
            tag,
            identity: object.hash(),
            payload_length: payload.len() as u64,
            kind: SectionKind::Singleton,
        });
        chunks.push(object);
    }
    let manifest = CompositeManifest {
        history_kind: composite_manifest::HistoryKind::Full,
        ledger_boundary: 4,
        composed_length: entries.iter().map(|e| e.payload_length).sum(),
        entries,
    };
    let composite_root = StoredObject::new(composite_manifest::encode(&manifest)).unwrap();
    let record = RevisionRecord::founding(AuthorityMode::Standalone, "r14", 0)
        .with_content_refs(vec![composite_root.hash()]);
    let request = engine_persist::composite_append::CompositeAppend {
        record: &record,
        root: &composite_root,
        new_chunks: &chunks,
        reused_chunks: &[],
    };
    let refusal = medium
        .append_composite(&request, None)
        .expect_err("the default production refuses composite publication");
    assert!(
        matches!(
            refusal,
            MediumError::CompositeAdmission { ref detail }
                if detail.contains("not activated")
        ),
        "the typed ActivationBarrier refusal: {refusal:?}"
    );
    // The chain is UNCHANGED (nothing was published).
    assert!(medium.head().unwrap().is_none());
    let _ = std::fs::remove_dir_all(&root);
}
