//! PP-B3 — the compacted walk and the membership universe (commit
//! 2's acceptance surface, over the REAL local canonical medium):
//! the simulated reclamation (commit 4's whole-revision deletes done
//! by hand on the scratch store — never a live campaign) leaves the
//! checkpointed chain gapped, and the cold walk must traverse the
//! gap under the skip table's self entry, report the MK and the
//! reclaimed totals honestly, and preserve BOTH historical identity
//! classes — the reclaimed keys (a reuse is rejected; the chain-wide
//! idempotency-key uniqueness never weakened, D-PP-4b honored) and
//! the reclaimed revision identities (a depends_on resolves via the
//! index with the strictly-earlier epoch, PP-B3's mandatory
//! condition 2). The fail-closed side: an uncovered absence — a
//! half-deleted revision set, or a hole in a chain that never
//! checkpointed — is hard corruption, never assumed reclamation. And
//! the honesty side: a present index is evidence for the FUTURE
//! reclamation, never counted as reclaimed state.

use std::path::PathBuf;

use engine_core::Encoder;
use engine_persist::{
    AppendOutcome, AuthorityMode, CanonicalMedium, CheckpointMetadata, Hash, LocalFileMedium,
    MediumError, ReclaimedIndexEntry, RevisionRecord, SkipEntry, StoredObject, chain_range_digest,
    compute_digest_seed, publish_checkpoint_mk,
};

// ------------------------------------------------------------- fixtures

/// A temporary campaign directory (the established suite pattern):
/// a unique child of the system temp dir, removed on drop.
struct TestDir(PathBuf);

impl TestDir {
    fn new(name: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "s7-pp-b3-{name}-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&path).unwrap();
        TestDir(path)
    }

    fn store(&self) -> PathBuf {
        self.0.join("store")
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A well-formed domain-tagged content object (a non-checkpoint
/// domain — the walk's publication class).
fn publication_object(payload: &[u8]) -> StoredObject {
    let mut enc = Encoder::new();
    enc.write_str("test/publication");
    enc.write_bytes(payload);
    StoredObject::new(enc.into_bytes()).unwrap()
}

/// One keyed reclaimable publication (a regular revision carrying
/// one content object; the record's own epoch is the parent's + 1).
fn publication(
    parent: Hash,
    parent_epoch: u64,
    key: &[u8],
    payload: &[u8],
) -> (RevisionRecord, StoredObject) {
    let object = publication_object(payload);
    let record = RevisionRecord::regular(parent, parent_epoch, parent_epoch * 10 + 7)
        .with_idempotency_key(key.to_vec())
        .with_content_refs(vec![object.hash()]);
    (record, object)
}

fn accepted(
    medium: &mut LocalFileMedium,
    record: &RevisionRecord,
    objects: &[StoredObject],
    parent: Option<Hash>,
) -> Hash {
    match medium.append(record, objects, parent).unwrap() {
        AppendOutcome::Accepted { id, .. } => id,
        other => panic!("expected acceptance, got {other:?}"),
    }
}

/// The canonical checkpointed campaign: F(e0) ← S1(e1, key-a) ←
/// S2(e2, key-b) ← SK(e3, key-sk) ← MK(e4), with the honest metadata
/// — the self-retirement skip jumping to the founding (SK's parent
/// S2 is itself a range publication, so MK skips the whole reclaim
/// run), the index [S1, S2, SK] carrying both identity classes per
/// revision (SK's own retirement entry included — the mandatory
/// condition 2), and the founding-seeded digest over the range's
/// record bytes oldest first.
struct Checkpointed {
    store: PathBuf,
    founding: Hash,
    s1: Hash,
    s1_content: Hash,
    s2: Hash,
    s2_content: Hash,
    sk: Hash,
    sk_content: Hash,
    mk: Hash,
    s2_key: Vec<u8>,
}

fn checkpointed_campaign(dir: &TestDir) -> Checkpointed {
    let store = dir.store();
    let mut medium = LocalFileMedium::open(&store).unwrap();
    let founding_record = RevisionRecord::founding(AuthorityMode::Standalone, "membership", 0);
    let founding = accepted(&mut medium, &founding_record, &[], None);
    let (s1_record, s1_object) = publication(founding, 0, b"membership/key-a", b"s1-payload");
    let s1_content = s1_object.hash();
    let s1 = accepted(&mut medium, &s1_record, &[s1_object], Some(founding));
    let (s2_record, s2_object) = publication(s1, 1, b"membership/key-b", b"s2-payload");
    let s2_content = s2_object.hash();
    let s2 = accepted(&mut medium, &s2_record, &[s2_object], Some(s1));
    let (sk_record, sk_object) = publication(s2, 2, b"membership/key-sk", b"sk-payload");
    let sk_content = sk_object.hash();
    let sk = accepted(&mut medium, &sk_record, &[sk_object], Some(s2));
    // The honest metadata, computed exactly as the driver does.
    let seed = compute_digest_seed(&founding);
    let s1_bytes = medium.read_record(&s1).unwrap().canonical_bytes();
    let s2_bytes = medium.read_record(&s2).unwrap().canonical_bytes();
    let range_digest = chain_range_digest(&seed, &[s1_bytes, s2_bytes]);
    let index = vec![
        ReclaimedIndexEntry::new(Some(b"membership/key-a".to_vec()), s1, 1),
        ReclaimedIndexEntry::new(Some(b"membership/key-b".to_vec()), s2, 2),
        ReclaimedIndexEntry::new(Some(b"membership/key-sk".to_vec()), sk, 3),
    ];
    let metadata = CheckpointMetadata::new(
        sk,
        s2,
        vec![SkipEntry::self_skip(founding)],
        index,
        range_digest,
        seed,
    );
    let (mk, outcome) = publish_checkpoint_mk(&mut medium, sk, 3, &metadata, 40).unwrap();
    assert!(
        matches!(outcome, AppendOutcome::Accepted { .. }),
        "the MK accepts over SK"
    );
    drop(medium);
    Checkpointed {
        store,
        founding,
        s1,
        s1_content,
        s2,
        s2_content,
        sk,
        sk_content,
        mk,
        s2_key: b"membership/key-b".to_vec(),
    }
}

/// The simulated reclamation (commit 4's deletes done by hand on the
/// scratch store): the whole-revision unit — the record object AND
/// its content object together.
fn delete_revision(dir: &TestDir, revision: &Hash, content: &Hash) {
    let objects = dir.store().join("objects");
    std::fs::remove_file(objects.join(revision.hex())).unwrap();
    std::fs::remove_file(objects.join(content.hex())).unwrap();
}

// ------------------------------------------------------------- the matrix

#[test]
fn the_compacted_walk_validates_the_reclaimed_chain_and_preserves_membership() {
    // THE simulated reclamation: S1, S2, and SK — the whole-revision
    // units — are removed. The reopen's cold walk must traverse the
    // gap under the skip table's self entry (MK jumps to the
    // founding), report the MK and the reclaimed totals honestly,
    // and the membership universe must preserve the reclaimed keys
    // (a reuse rejected) and identities (a depends_on resolved via
    // the index with the strictly-earlier epoch).
    let dir = TestDir::new("compacted");
    let campaign = checkpointed_campaign(&dir);
    delete_revision(&dir, &campaign.s1, &campaign.s1_content);
    delete_revision(&dir, &campaign.s2, &campaign.s2_content);
    delete_revision(&dir, &campaign.sk, &campaign.sk_content);

    // THE compacted walk: the reopen itself validates the gapped
    // chain (the open's cold walk), and validate() reports the
    // honest summary.
    let mut medium = LocalFileMedium::open(&campaign.store).unwrap();
    let summary = medium.validate().unwrap();
    assert_eq!(summary.head, campaign.mk, "MK is the head");
    assert_eq!(summary.founding, campaign.founding);
    assert_eq!(
        summary.records, 2,
        "the walk covered MK and the founding only — the gap was skipped"
    );
    assert_eq!(summary.checkpoints, vec![campaign.mk]);
    assert_eq!(
        summary.reclaimed_records, 3,
        "S1, S2, and SK are genuinely absent"
    );
    assert_eq!(
        summary.reclaimed_keys, 3,
        "every reclaimed revision carried a key"
    );

    // THE membership universe — the historical KEYS: a reclaimed
    // key's reuse is rejected (the chain-wide uniqueness never
    // weakened, D-PP-4b honored).
    let (reuse_record, reuse_object) =
        publication(campaign.mk, 4, &campaign.s2_key, b"reuse-payload");
    let error = medium
        .append(&reuse_record, &[reuse_object], Some(campaign.mk))
        .unwrap_err();
    assert!(
        matches!(
            error,
            MediumError::Chain {
                rule: "duplicate-idempotency-key"
            }
        ),
        "the reclaimed key's reuse is rejected: {error:?}"
    );

    // THE membership universe — the historical IDENTITIES: a
    // depends_on naming the reclaimed S1 resolves via the index, its
    // indexed epoch (1) strictly below the referencing record's (5).
    let (dep_record, dep_object) =
        publication(campaign.mk, 4, b"membership/key-new", b"dep-payload");
    let dep_record = dep_record.with_depends_on(vec![campaign.s1]);
    let new_head = match medium
        .append(&dep_record, &[dep_object], Some(campaign.mk))
        .unwrap()
    {
        AppendOutcome::Accepted { id, .. } => id,
        other => panic!("the reclaim-indexed dependency must resolve: {other:?}"),
    };
    // And the chain still validates with the new record on top —
    // history unchanged (3 reclaimed, 1 MK, the founding).
    let summary = medium.validate().unwrap();
    assert_eq!(summary.head, new_head);
    assert_eq!(summary.records, 3);
    assert_eq!(
        summary.reclaimed_records, 3,
        "the append changed nothing about history"
    );
}

#[test]
fn a_half_deleted_revision_set_fails_closed() {
    // The partial reclamation (S2's revision removed while SK still
    // stands on it): SK's parent is absent, and no skip entry is
    // keyed on SK — SK's retirement is the SELF entry's business (a
    // skip standing on SK is dead by construction), so the table
    // offers no cover and the walk fails closed on the uncovered
    // absence: hard corruption, never assumed reclamation.
    let dir = TestDir::new("uncovered");
    let campaign = checkpointed_campaign(&dir);
    delete_revision(&dir, &campaign.s2, &campaign.s2_content);
    match LocalFileMedium::open(&campaign.store) {
        Err(MediumError::MissingObject { hash }) => assert_eq!(hash, campaign.s2),
        Err(other) => panic!("the uncovered absence is a missing object, got {other:?}"),
        Ok(_) => panic!("the uncovered absence must fail the open, never pass"),
    }
}

#[test]
fn an_absent_parent_without_any_checkpoint_fails_closed() {
    // A hole in a chain that never checkpointed: nothing covers the
    // absence — the walk fails closed, reclamation never assumed.
    let dir = TestDir::new("no-checkpoint");
    let store = dir.store();
    let mut medium = LocalFileMedium::open(&store).unwrap();
    let founding_record = RevisionRecord::founding(AuthorityMode::Standalone, "no-checkpoint", 0);
    let founding = accepted(&mut medium, &founding_record, &[], None);
    let (s1_record, s1_object) = publication(founding, 0, b"no-checkpoint/key-a", b"s1");
    let s1_content = s1_object.hash();
    let s1 = accepted(&mut medium, &s1_record, &[s1_object], Some(founding));
    let (s2_record, s2_object) = publication(s1, 1, b"no-checkpoint/key-b", b"s2");
    accepted(&mut medium, &s2_record, &[s2_object], Some(s1));
    drop(medium);
    delete_revision(&dir, &s1, &s1_content);
    match LocalFileMedium::open(&store) {
        Err(MediumError::MissingObject { hash }) => assert_eq!(hash, s1),
        Err(other) => panic!("the uncovered absence is a missing object, got {other:?}"),
        Ok(_) => panic!("the uncovered absence must fail the open, never pass"),
    }
}

#[test]
fn a_present_index_is_evidence_not_reclaimed_state() {
    // The honesty side: pre-reclamation the checkpoint's index names
    // records that are still present — the summary counts ZERO
    // reclaimed (the index is evidence for the FUTURE reclamation),
    // the whole chain walks normally, and the staged keys never
    // collide with the retained records' own keys.
    let dir = TestDir::new("present");
    let campaign = checkpointed_campaign(&dir);
    let mut medium = LocalFileMedium::open(&campaign.store).unwrap();
    let summary = medium.validate().unwrap();
    assert_eq!(summary.records, 5, "the whole chain walked");
    assert_eq!(summary.checkpoints, vec![campaign.mk]);
    assert_eq!(summary.reclaimed_records, 0);
    assert_eq!(summary.reclaimed_keys, 0);
    // And a key that exists ONLY as a retained record's key is still
    // rejected for reuse (the pre-reclamation universe is intact).
    let (reuse_record, reuse_object) =
        publication(campaign.mk, 4, &campaign.s2_key, b"reuse-payload");
    let error = medium
        .append(&reuse_record, &[reuse_object], Some(campaign.mk))
        .unwrap_err();
    assert!(
        matches!(
            error,
            MediumError::Chain {
                rule: "duplicate-idempotency-key"
            }
        ),
        "the retained key's reuse is rejected: {error:?}"
    );
}
