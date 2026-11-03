//! (OC-C3 CP-1) The trusted-begin transport routing suite: the
//! direct `SessionStore` source and the queued `WorkerMedium`
//! source — the begin admission and the installed-begin authority
//! forwarded through both, the exact-key deduplication (no second
//! independent publication), the pre-admission poison refusal (a
//! definite non-event), the admitted-reply-loss uncertain class and
//! its idempotent resolution, and the authority's truthful grade
//! transport. Platform-neutral: the SQLite provider (no platform
//! tier gate — the provider-specific tier behavior, the POSIX
//! reaffirmation, and the Windows refusal are covered in
//! engine-persist's own suites).

use std::path::PathBuf;

use engine_core::Encoder;
use engine_persist::checkpoint::{CheckpointMetadata, ReclaimedIndexEntry, SkipEntry};
use engine_persist::reclaim::{ReclaimUnit, ReclamationLedger};
use engine_persist::{
    AppendOutcome, AuthorityMode, CanonicalMedium, Hash, RevisionRecord, StoredObject, SyncTier,
    publish_reclamation_ledger,
};
use engine_slice::store::{MediumSource, SessionStore, StoreLocation};
use engine_slice::worker::{IoWorker, WorkerFault};

/// The routing fixture: founding → P1 → R1(shared+private) → SK →
/// MK(the meta) → the published audit — the evidence chain the
/// begin admission verifies. The begin itself is published BY the
/// routing under test.
fn begin_ready_sqlite(label: &str) -> (PathBuf, SessionStore, Hash, Hash) {
    let root = std::env::temp_dir().join(format!(
        "s7-cp1-route-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    let mut medium = SessionStore::open(&StoreLocation::Database(root.join("db.sqlite"))).unwrap();
    let shared = StoredObject::new({
        let mut enc = Encoder::new();
        enc.write_str("sektor7/save/1");
        enc.write_bytes(b"shared-payload");
        enc.into_bytes()
    })
    .unwrap();
    let private = StoredObject::new({
        let mut enc = Encoder::new();
        enc.write_str("sektor7/save/1");
        enc.write_bytes(b"private-payload");
        enc.into_bytes()
    })
    .unwrap();
    let founding = RevisionRecord::founding(AuthorityMode::Standalone, label, 0)
        .with_content_refs(vec![shared.hash()]);
    medium
        .append(&founding, std::slice::from_ref(&shared), None)
        .unwrap();
    let step = |medium: &mut SessionStore, objects: &[StoredObject], tick: u64| -> Hash {
        let head = medium.head().unwrap().unwrap();
        let epoch = medium.read_record(&head).unwrap().epoch;
        let refs: Vec<Hash> = objects.iter().map(|o| o.hash()).collect();
        let record = RevisionRecord::regular(head, epoch, tick).with_content_refs(refs);
        medium.append(&record, objects, Some(head)).unwrap();
        record.id()
    };
    let p1 = step(&mut medium, std::slice::from_ref(&shared), 1);
    let r1 = step(&mut medium, &[shared.clone(), private.clone()], 2);
    let sk = step(&mut medium, std::slice::from_ref(&shared), 3);
    let seed = engine_persist::checkpoint::compute_digest_seed(&founding.id());
    let r1_bytes = medium.read_record(&r1).unwrap().canonical_bytes();
    let range_digest = engine_persist::checkpoint::chain_range_digest(&seed, &[r1_bytes]);
    let meta = CheckpointMetadata::new(
        sk,
        r1,
        vec![SkipEntry::new(Hash::ZERO, p1)],
        vec![
            ReclaimedIndexEntry::new(None, r1, 2),
            ReclaimedIndexEntry::new(None, sk, 3),
        ],
        range_digest,
        seed,
    );
    let meta_object = StoredObject::new(meta.canonical_bytes()).unwrap();
    let mk = step(&mut medium, std::slice::from_ref(&meta_object), 4);
    let r1_refs = medium.read_record(&r1).unwrap().content_refs.clone();
    let sk_refs = medium.read_record(&sk).unwrap().content_refs.clone();
    let ledger = ReclamationLedger {
        mk,
        sk,
        range_digest: meta.range_digest,
        seed: meta.seed,
        records: 2,
        reclaimed_keys: 0,
        journal_files: Vec::new(),
        units: vec![
            ReclaimUnit {
                record: sk,
                objects: sk_refs,
            },
            ReclaimUnit {
                record: r1,
                objects: r1_refs,
            },
        ],
    };
    let (audit, outcome) = publish_reclamation_ledger(&mut medium, &ledger, 5).unwrap();
    assert!(matches!(outcome, AppendOutcome::Accepted { .. }));
    (root, medium, audit, mk)
}

#[test]
fn the_direct_session_store_begin_routing() {
    // The direct source: the begin admission and the installed-begin
    // authority through SessionStore — the provider's own accepted
    // implementations, unchanged; the exact-key deduplication on the
    // repeat (no second publication); the truthful grade.
    let (root, mut medium, audit, mk) = begin_ready_sqlite("direct");
    let outcome = medium.begin_reclamation(&audit, &mk, 160).unwrap();
    let installed = match outcome {
        engine_persist::reclaim::BeginPublicationOutcome::Accepted { id, tier } => {
            assert_eq!(tier, SyncTier::TransactionalWalFull);
            id
        }
        other => panic!("the direct routing accepts: {other:?}"),
    };
    // The repeat: the exact-key duplicate — never a second
    // independent publication.
    let outcome = medium.begin_reclamation(&audit, &mk, 161).unwrap();
    assert!(
        matches!(
            outcome,
            engine_persist::reclaim::BeginPublicationOutcome::DuplicateVerified { id } if id == installed
        ),
        "the repeat deduplicates on the exact key: {outcome:?}"
    );
    // The authority: the verified identity and the truthful grade.
    let authority = medium.verify_begin_authority(&audit, &mk).unwrap();
    assert_eq!(authority.id, installed);
    assert_eq!(authority.tier, SyncTier::TransactionalWalFull);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_worker_begin_routing_and_uncertainty() {
    // The queued source: both trusted begin methods through the
    // single FIFO queue; the pre-admission poison refusal (a
    // definite non-event); the admitted-reply-loss uncertain class
    // (never a definite absence, never a guessed success) and its
    // idempotent resolution — the re-submission finds the published
    // begin as the verified duplicate.
    let (root, _medium, audit, mk) = begin_ready_sqlite("worker");
    drop(_medium);
    let mut worker = IoWorker::spawn(&StoreLocation::Database(root.join("db.sqlite"))).unwrap();
    let source = worker.source().unwrap();
    let mut medium = source.open_medium().unwrap();
    // (a) The round-trips.
    let outcome = medium.begin_reclamation(&audit, &mk, 162).unwrap();
    let installed = match outcome {
        engine_persist::reclaim::BeginPublicationOutcome::Accepted { id, tier } => {
            assert_eq!(tier, SyncTier::TransactionalWalFull);
            id
        }
        other => panic!("the queued admission accepts: {other:?}"),
    };
    let authority = medium.verify_begin_authority(&audit, &mk).unwrap();
    assert_eq!(authority.id, installed);
    assert_eq!(authority.tier, SyncTier::TransactionalWalFull);
    // (b) The deduplication through the queue.
    let outcome = medium.begin_reclamation(&audit, &mk, 163).unwrap();
    assert!(
        matches!(
            outcome,
            engine_persist::reclaim::BeginPublicationOutcome::DuplicateVerified { id } if id == installed
        ),
        "the queued repeat deduplicates: {outcome:?}"
    );
    // (c) The admitted-reply-loss: the begin EXECUTES (it publishes)
    // but the reply is never sent — the typed uncertain class, never
    // a definite absence and never a guessed success.
    worker.fault_control().set(WorkerFault::LoseBeginReply);
    let error = medium.begin_reclamation(&audit, &mk, 164).unwrap_err();
    assert!(
        matches!(
            error,
            engine_persist::MediumError::Chain {
                rule: "begin-admission/worker-reply-uncertain"
            }
        ),
        "the lost reply preserves the uncertainty typed: {error:?}"
    );
    // The idempotent resolution: the re-submission finds the
    // published begin as the verified duplicate — no second
    // independent publication.
    let outcome = medium.begin_reclamation(&audit, &mk, 165).unwrap();
    assert!(
        matches!(
            outcome,
            engine_persist::reclaim::BeginPublicationOutcome::DuplicateVerified { id } if id == installed
        ),
        "the re-submission resolves idempotently: {outcome:?}"
    );
    // The authority still verifies after the uncertain window.
    let authority = medium.verify_begin_authority(&audit, &mk).unwrap();
    assert_eq!(authority.id, installed);
    // (d) The pre-admission poison refusal: a DEFINITE non-event.
    worker.poison();
    let error = medium.begin_reclamation(&audit, &mk, 166).unwrap_err();
    assert!(
        matches!(error, engine_persist::MediumError::WorkerUnavailable { .. }),
        "the poisoned worker refuses before admission (definite): {error:?}"
    );
    let error = medium.verify_begin_authority(&audit, &mk).unwrap_err();
    assert!(
        matches!(error, engine_persist::MediumError::WorkerUnavailable { .. }),
        "the poisoned worker refuses the authority request too: {error:?}"
    );
    drop(medium);
    drop(source);
    worker.stop();
    let _ = worker.join();
    let _ = std::fs::remove_dir_all(&root);
}
