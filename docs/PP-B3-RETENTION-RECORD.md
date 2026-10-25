# PP-B3 — Production Persistence Batch 3: Canonical Retention, Checkpoints, Journal Rotation, and Verified Reclamation — IMPLEMENTATION RECORD

> **Status: DELIVERED — STOPPED FOR THE INDEPENDENT PASS/HOLD REVIEW.**
> The implementation GO's full checklist is delivered on
> `production-persistence-b3` (created from the frozen PP-B2 closure
> `00109b0`). This record never declares the batch CLOSED-PASS — that
> verdict belongs to the independent reviewer and the owner alone.
> PP-B4, Stage 6, and everything beyond PP-B3 remain unauthorized.

## §1 — The delivery chain and the scope

The branch `production-persistence-b3` was created from the exact
frozen PP-B2 closure `00109b0a0dd4710a4d550df022b3a4aaacc64aab` (the
accepted implementation `c987efd`; the worktree verified clean; no
merges; every protected branch frozen: `main`, `stage5`,
`macos-port`, `production-persistence-design`,
`production-persistence-b1`, `production-persistence-b2`).

The delivery — five commits, each gated (fmt, clippy `-D warnings`,
the full workspace suite, every existing regression green):

| Commit | The deliverable | The workspace |
|---|---|---|
| `093395e` | The `checkpoint-metadata/1` codec + the SK/MK publication driver | 567 passed + 1 ignored |
| `7b581db` | The compacted walk + the membership universe (both providers) | 572 passed + 1 ignored |
| `934841a` | The durable T1 journal rotation (§4.4) | 578 passed + 1 ignored |
| `f93a101` | The audited explicit two-phase reclamation | 587 passed + 1 ignored |
| (this commit) | The multi-checkpoint example + the benchmark §8 + this record | 588 passed + 1 ignored |

From the PP-B2 baseline of 550 tests: **38 new tests**, every one of
them a PP-B3 behavior; no existing test was modified except where a
delivered behavior legitimately extends an asserted surface (the
checkpoint composition test's index expectations — SK's own
retirement entry, below).

The scope — the GO's eleven deliverables, each mapped to its
implementation and regression:

1. **The two-revision SK/MK checkpoint publication** —
   `SessionCrossing::publish_checkpoint` (crossing.rs): phase 1 the
   compaction (the F1 preflight discipline), phase 2 SK through the
   existing `publish_snapshot` path, phase 3 the range walk + the
   skip table + the index + the digest + the empty-range refusal,
   phase 4 MK via `publish_checkpoint_mk` under the crossing's
   Unknown discipline. Regression:
   `the_checkpoint_publication_composes_sk_then_mk_over_the_real_machinery`.
2. **The versioned checkpoint-metadata object** —
   `engine-persist/checkpoint.rs`, domain
   `engine-persist/checkpoint-metadata/1`, strict decode.
   Regressions: the 13-test codec matrix.
3. **The permanent historical validation metadata** — the MK records
   and their metadata objects are never reclaim candidates (the
   driver's eligibility never names them; the medium's reclaim
   trusts the audited index). Regression: the example's
   MK1/MK2-permanent assertions.
4. **The skip-table compacted walk** — the gap-aware cold walks in
   both providers' `validate()` and (the delivery's mid-work
   corrective) the pipeline's `walk_chain`. Regressions:
   `pp_b3_membership` (4) + the sqlite parity + the recovery tests
   over reclaimed chains.
5. **The reclaimed-record index preserving historical
   identities+keys** — `ReclaimedIndexEntry` (both identity classes:
   the idempotency key where present, the hash+epoch). Regressions:
   the key-reuse rejections + the `depends_on` resolutions.
6. **The coherent membership universe** — the mediums' cursors gain
   the reclaim index (identity→epoch) merged over genuinely-absent
   entries; the append fast paths in both providers + the walk
   suffixes + the SQLite transaction checks consult it.
   Regressions: `pp_b3_membership` + the parity.
7. **Safe snapshot discovery + retired-checkpoint handling** — the
   discovery remains content-domain decode (never hash ordering);
   the newest snapshot is the recovery anchor and never a reclaim
   candidate (the retention window); a crashed SK (MK never
   committed) stands as an ordinary snapshot. Regressions: the
   recovery tests + the crash regression (§2.3).
8. **The durable T1 journal rotation under the §4.4 survival
   rules** — `rotation.rs` + `SessionCrossing::rotate_journal` + the
   pointer flip + the recovery's pointer resolution. Regressions:
   `pp_b3_rotation` (2) + the rotation unit matrix (4).
9. **The verify-before-reclaim eligibility** — Phase A: validate,
   the window, the binding, the digest RECOMPUTED against the
   still-present bytes, the pre-scan. Regressions: `pp_b3_reclaim`
   (5, incl. the window refusal + the wrong-deletion fail-closed).
10. **The audited explicit restartable two-phase reclamation** —
    Phase A's `reclamation-ledger/1` audit revision (the
    deterministic MK-derived key, read back verified) before any
    delete; Phase B's idempotent whole-revision deletes. Regressions:
    the lifecycle + the idempotent re-run + the example.
11. **The full failure/process-kill/compat/performance evidence** —
    the crash regression (§2.3), the compatibility (every existing
    suite green through the pointer-aware recovery and the
    gap-aware walks), the benchmark §8 (§10 below).

## §2 — The three mandatory PP-B3 conditions

### §2.1 — The non-circular self-retirement-skip encoding

**Delivered.** No field of the metadata object derives from MK's own
bytes: the encoding carries the known-SK identity (`sk` — MK's
parent, a PRIOR record computable before MK exists) and the reserved
`Hash::ZERO` self marker (a skip entry whose `retained` field names
"the MK revision referencing THIS metadata object" — at most one,
decode-enforced). Strict contextual validation binds the metadata to
its referencing record: the decode's conditional self-ancestor rule
(the target equals `sk_parent` while that parent is retained; a
retained target otherwise), the skip-on-sk refusal (a skip standing
on SK is dead by construction — nothing retained references a
retired SK but MK, whose traversal IS the self entry), and the
skip-target-reclaimed refusal (no jump target names an indexed
revision). Regressions: the codec matrix's self-entry rules
(`the_self_entry_ancestor_must_equal_sk_parent`,
`a_reclaimed_sk_parent_redirects_the_self_target_to_retained`,
`a_skip_standing_on_sk_is_refused`) and the driver composition test's
exact skip-table assertion.

### §2.2 — The reclaimed SK revisions in the historical membership evidence

**Delivered.** The checkpoint's index carries SK's OWN retirement
entry — the retired snapshot's identity AND its idempotency key (a
driver amendment in commit 2: the index is everything the
checkpoint's reclamation may delete, the range publications plus SK
itself). The membership universe merges the index entries that are
genuinely absent: their keys join the cursor's seen-keys (a retired
key's reuse is REJECTED — the chain-wide uniqueness never weakened,
D-PP-4b honored) and their identities resolve `depends_on`
references with their indexed epochs under the strictly-earlier rule.
Regressions: the key-reuse rejections in `pp_b3_membership`, the
`pp_b3_reclaim` lifecycle, and the example; the `depends_on`
resolutions in `pp_b3_membership` and the example (the GO's
`C3.depends_on[hash(S2′)]` case).

### §2.3 — The SK-committed/MK-uncommitted process-termination regression

**Delivered — REAL process termination.** The genuine in-driver seam
`ARM_AFTER_SK_EXIT` (`#[doc(hidden)]`, process-local, exit 8,
distinct from the mid-install convention's 7) fires inside
`publish_checkpoint` immediately after SK's commit and before MK. The
child test (`pp_b3_child_crash_after_sk`, spawned via the established
`current_exe --exact` harness with the absent-env no-op guard) arms
the seam and dies. The parent verifies: the head is the crashed SK
whose object domain is the session-snapshot family (NEVER checkpoint
metadata); the whole chain validates; the EXISTING safe recovery
path reconstructs the session from SK as an ordinary snapshot (the
recovered spine's history kind Compacted — no checkpoint status
fabricated, no reclaim eligibility); and the retry over the recovered
session completes safely, the crashed SK an ordinary indexed
publication inside the new checkpoint's range.

## §3 — The hard rules honored

- **D-PP-4a (REJECTED — absolute):** no confirmed consequence record
  or batch object is ever a reclaim candidate. The reclamation's
  eligible set is exactly the chosen checkpoint's index — the range
  publications plus SK — never the confirming anchors; the driver
  never names them and the medium's delete trusts the audit.
- **D-PP-4b (REJECTED — absolute):** the chain-wide idempotency-key
  uniqueness is never weakened. The reclaimed keys join the
  membership universe; every append path in both providers rejects a
  retired key's reuse.
- **Never rewrite `revision-record/1` bytes or content identities:**
  the reclamation only deletes whole revisions (the record object
  and its content objects together); no byte is ever rewritten.
- **The founding root never reclaimed:** the founding is a walk
  boundary in every walk and never an index member.
- **MK records+objects permanent, never-GC:** the driver's
  eligibility never names them; the example asserts their presence
  post-reclamation.
- **The discovery by content-domain decode, never hash ordering:**
  every walk classifies records by their first content object's
  domain tag (`peek_object_domain` / `peek_domain`).
- **A missing required object on a retained record is hard
  corruption:** every walk fails closed on an uncovered absence
  (`MissingObject`), never assuming reclamation. Regression:
  `a_hand_deleted_range_record_without_an_audit_fails_closed` and
  the membership matrix's half-deleted and no-checkpoint cases.
- **Reclamation explicit+logged, never automatic:** the reclamation
  is an operator method whose every action is preceded by the
  on-chain audit revision; nothing in the batch reclaims on a
  threshold.
- **Destructive tests on scratch stores only:** every test builds
  its campaign under the system temp dir, removed on drop.
- **The backup accounts for the active journal and the permanent
  checkpoint metadata:** `backup_campaign` resolves the rotation
  pointer before locking and copying (the manifest's v3
  source-journal provenance binds the ACTIVE journal); the MK
  metadata objects are chain-referenced content captured by the
  whole-chain `VACUUM INTO`. Regression:
  `the_backup_captures_the_active_journal_after_a_rotation`.
- **NO PP-B4 async:** nothing in the batch spawns workers; every
  operation is synchronous under the single-writer discipline.
- **The provisional thresholds never silent runtime policy:** the
  retention window (`generations`) is an explicit operator
  parameter; the publication interval is the operator's choice;
  nothing fires automatically.

## §4 — Commit 1: the checkpoint-metadata codec + the SK/MK driver

`engine-persist/src/checkpoint.rs` (new): the versioned
`checkpoint-metadata/1` object — `CheckpointMetadata` (the known-SK
binding, the skip table sorted-unique by `retained` with the single
self entry, the index sorted-unique by `hash` carrying both
identity classes per entry, the range digest, the founding-derived
seed, the range count); `SkipEntry` with the `Hash::ZERO` self
marker; `ReclaimedIndexEntry`; the digest chain
(`compute_digest_seed` — SHA-256 over the domain tag and the
founding identity, the same seed for every checkpoint of one
campaign; `chain_range_digest` — the running SHA-256 over the
range's record canonical bytes oldest-first); the deterministic MK
key; `publish_checkpoint_mk` (the metadata-binding check, the
head-must-be-SK check, the candidate returned for the caller's
Unknown reconciliation). The strict decode: the domain and version
gates, truncation and trailing bytes, the sorted-unique orders, the
zero-identity rule, the duplicate-self rule, skip-on-sk,
skip-on-reclaimed, skip-target-reclaimed, the conditional
self-ancestor rule, the range-count coherence.

The driver (crossing.rs): `CheckpointReport` +
`SessionCrossing::publish_checkpoint` — the four phases with the
empty-range refusal (`checkpoint-empty-range`) and the fail-closed
MK outcomes (`checkpoint-mk-unresolved` / `checkpoint-mk-unverifiable`,
never a guessed outcome). The walk classifies by content domain:
the session-snapshot family (reclaimable publications), the
consequence-batch domain (retained anchors), a prior checkpoint
metadata (the permanent boundary returning its carried seed — no
re-walk below it), the founding (the fresh seed), anything else
(refused). The crash seam `ARM_AFTER_SK_EXIT` (§2.3).

**The mid-work corrective (commit 1, caught by the empty-range and
crash tests):** the skip computation originally emitted a skip keyed
on SK itself when SK's parent was a publication (dead by
construction — the decode's skip-on-sk correctly refused it) and the
self entry blindly targeted `sk_parent` even when that parent was
itself reclaimable. Fixed both sides: never an SK entry; the self
target is the NEAREST RETAINED ancestor (`sk_parent` while
retained, the bottom of the reclaim run otherwise); the decode's
conditional rule and the new skip-target-reclaimed refusal.

## §5 — Commit 2: the compacted walk + the membership universe

Both providers' trusted cursors gain the reclaim index
(identity→epoch) accumulated from every on-chain MK's metadata
during the cold walk — merged only over GENUINELY ABSENT entries
(pre-reclamation the still-present range records are retained and
already accounted; their index entries are evidence for the future
reclamation, never double-counted; a cross-index identity is the
typed `checkpoint-index-incoherent`; a key colliding with a retained
record is the `duplicate-idempotency-key` refusal). The compacted
walk: an absent parent is a gap — legitimate only under the most
recent on-chain MK's skip table (a skip for exactly the record whose
parent is absent, or that MK's own self entry), jumping to the
nearest retained ancestor under the gap-aware epoch linkage (the
target strictly below the pre-gap record); an uncovered absence is
hard corruption. `ChainSummary` gains the additive
`checkpoints`/`reclaimed_records`/`reclaimed_keys` fields (zero/empty
before any reclamation — the provider parity preserved). The
condition-2 driver amendment: SK's own retirement entry in the index.

**The mid-work corrective (commit 2, caught twice by the sqlite
parity test):** the SQLite gap-detection query and the test's
simulated-reclamation DELETEs originally bound the HEX string
identity — the tables are keyed by the RAW 32-byte identity blobs, so
the hex bindings silently matched nothing. Both fixed to the
raw-bytes form.

## §6 — Commit 3: the journal rotation

`engine-persist/src/rotation.rs` (new): `rotate_journal` writes the
surviving transactions and cancellation records to a fresh journal
(`journal-<nanos>.t1`) RE-ANCHORED under the new file's own hash
chain — the documented choice: verbatim frames would break the
record chain over the dropped span, while the transaction identities
are content-derived, so duplicate recognition, cancellation
suppression, and recovery replay all survive the re-encoding; the
new file is fully scan-verified BEFORE the flip; the pointer
`<legacy>.active` flips atomically (tmp+fsync+rename+dir-sync); the
legacy journal is retained byte-untouched. `active_journal_path`
resolves the pointer (a corrupt pointer fails closed) or falls back
to the legacy itself.

The classifier (`SessionCrossing::rotate_journal`): each committed
transaction is classified against the CHAIN-VERIFIED anchor — the
newest session snapshot's lane-summary boundary (never the caller's
unpublished in-memory state) — and the pipeline's chain-derived
confirmed set. A transaction retires only when canceled or when
every consequential identity is canonically confirmed AND subsumed
below the verified boundary (§4.4's retirement rule); everything
else survives — the outstanding unconfirmed work, the plans, the
confirmed-but-unmaterialized captures. The crossing's pipeline
reopens over the new journal (the confirmed set re-derived from the
chain). The recovery resolves the pointer first — a pointer-resolved
journal that does not exist fails closed; only the absent-legacy
case keeps the accepted B3 coherence semantics. The backup binds the
active journal.

## §7 — Commit 4: the two-phase reclamation

`engine-persist/src/reclaim.rs` (new): the `reclamation-ledger/1`
audit object — the event's bindings (the reclaimed MK and SK, the
pre-verified range digest and seed, the counts, the journal files
removed alongside) binding to the checkpoint's permanent metadata,
never duplicating it; the deterministic MK-derived idempotency key
(one audit per checkpoint ever — the restart's recognition).

The medium reclaim (`CanonicalMedium::reclaim`, both providers + the
seam): the whole-revision units, idempotent, never rewriting bytes.

The driver (`SessionCrossing::reclaim_checkpoint`): Phase A — no
destructive action before the audit is durably established: the
compacted walk validates; the retention window (the `generations`
newest checkpoints must remain — the recovery works from the newest
SK); the SK/MK binding and metadata decode; the pre-scan for an
installed audit (the bounded descent head→target-MK, never crossing
a gap — the restart never re-requires the deleted bytes); the digest
RECOMPUTED against the still-present range bytes (the publications
oldest-first by epoch; SK's own entry an index member but never a
digest member); the journal survival evidence; the audit revision
appended and read back verified. Phase B — the idempotent deletes +
the old journal file only when the rotation's verified active
journal stands. The post-conditions: the compacted walk validates
over the gapped chain; the newest snapshot remains discoverable.

**The mid-work corrective (commit 4, a real recovery-breaking gap
caught by the driver tests):** the PIPELINE's `walk_chain` — the
confirmed-evidence collection at `CommitPipeline::open` that the
recovery's anchor synthesis depends on — was gap-unaware: the
recovery over a reclaimed chain failed `MissingObject`. Fixed with
the compacted-mode treatment (the checkpoint-metadata recognition
during the descent + the skip-table consultation on the absent
parent) — the evidence collection now reaches the batches below
reclaimed ranges through the permanent skips.

## §8 — The acceptance matrices

The GO §11 matrices, each delivered as a named suite:

- **A — the checkpoint publication** (`tests/pp_b3_checkpoint.rs`,
  4): the composition over the real machinery (MK.parent == SK
  exactly, the metadata bindings, the range, the skips, the index,
  the digest, the deterministic key, the validated head); the
  empty-range refusal (the refused SK stands as an ordinary
  publication); the mandatory crash regression (§2.3); the child
  entry.
- **B — the compacted walk + the membership universe**
  (`tests/pp_b3_membership.rs`, 4 + the sqlite parity, 1): the walk
  over hand-deleted whole-revision units on scratch stores; the
  half-deleted and no-checkpoint fail-closed cases; the
  present-index honesty; the provider parity.
- **C — the rotation** (`rotation.rs` unit tests, 4 +
  `tests/pp_b3_rotation.rs`, 2): the mechanism; the
  dedup-preservation proof; the pointer matrix; the full lifecycle;
  the backup accounting.
- **D — the reclamation** (`reclaim.rs` unit tests, 5 +
  `tests/pp_b3_reclaim.rs`, 5): the codec matrix; the lifecycle;
  the window; the wrong-deletion; the full stack; the
  multi-checkpoint example (§9).
- **E — the failure/process-kill evidence:** the real child kill at
  the checkpoint's crash boundary (§2.3 — the batch's new crash
  window); the reclamation's crash-restart semantics exercised via
  the idempotent re-run (the pre-scan recognizing the installed
  audit — the identical code path a mid-Phase-B crash takes; the
  audit-first ordering means a crash before the audit leaves
  nothing deleted); the compatibility evidence — every existing
  recovery, backup, B4, and B5 suite green unchanged through the
  pointer-aware recovery and the gap-aware walks.

## §9 — The multi-checkpoint example (the GO's mandatory material)

`the_multi_checkpoint_example_the_mandatory_retirement_chain`
(tests/pp_b3_reclaim.rs): the verbatim chain
F ← S1 ← C1 ← S2 ← S2′ ← C2 ← SK1 ← MK1 ← S3 ← C3 ← S4 ← C4 ←
SK2 ← MK2, with S3.parent = MK1 asserted against the chain itself,
built over the real crossing (four confirmation cycles — each far
boundary, canonical confirmation, reactivation, and accrual run).
The reclamation of MK1's range under the generations=1 window, with
every mandated case:

- **SK1 retired whole** — its revision absent post-reclamation (the
  record object and its content object removed together).
- **SK2 the newest discoverable** — the recovery works from it.
- **MK1/MK2 permanent** — both records and their metadata objects
  readable post-reclamation.
- **The self-skip past SK1** — the compacted walk validates through
  MK1's self entry and the anchors' skips past the reclaimed
  publications (11 retained records: the founding, C1, C2, MK1, S3,
  C3, S4, C4, SK2, MK2, and the audit).
- **The range-1 key reuse rejected** — S2's key, preserved by MK1's
  permanent index, rejected at the append.
- **C3.depends_on[hash(S2′)] resolved via MK1's index with the
  strictly-earlier epoch** — a depends_on naming the reclaimed S2′
  accepted at the append (the indexed epoch strictly below the new
  record's).
- **The wrong-deletion fail-closed** —
  `a_hand_deleted_range_record_without_an_audit_fails_closed` (the
  same suite).

## §10 — The measurements (the benchmark §8)

`--benchmark-persistence` (pod, dev profile — NOT authoritative
physical-rig evidence, D-PP-7):

```
8. PP-B3 retention (the checkpoint + the reclamation + the rotation)
  150 ticks: 1021074 B → 412990 B after the reclamation (−59.6%, 6 objects removed)
  600 ticks: 1655458 B → 417702 B after the reclamation (−74.8%, 6 objects removed)
  1200 ticks: 2493357 B → 411471 B after the reclamation (−83.5%, 6 objects removed)
  2400 ticks: 4184159 B → 412990 B after the reclamation (−90.1%, 6 objects removed)
  2 publications per range: 1655458 B → 417702 B (6 objects removed)
  8 publications per range: 6534373 B → 1604254 B (18 objects removed)
  the checkpoint publication (compact + SK + MK): min 284.124 / median 284.942 / p95 285.812 / max 285.812 ms (n=5)
  the journal rotation (the survivor re-anchor + the flip): min 333.421 / median 333.759 / p95 334.032 / max 334.032 ms (n=5)
  the reclamation (the audit + the deletes): min 228.282 / median 228.617 / p95 230.017 / max 230.017 ms (n=5)
```

**The reading:** the store's bytes after one audited reclamation
stay FLAT at ~413 KB across the 150→2400-tick sweep while the
un-reclaimed store grows linearly — the canonical chain's bounded
steady state, measured (the reduction −59.6% at 150 ticks growing to
−90.1% at 2400). The interval sensitivity: the reclaimed bytes scale
with the range size (2 publications per range → 6 objects; 8 → 18)
while the retained anchors and metadata stay flat. The operation
costs (dev pod): the checkpoint ~285 ms, the rotation ~334 ms, the
reclamation ~229 ms — all well above any frame budget, consistent
with PP-B2's compaction costs: these are explicit operator
operations, never gameplay-frame operations (the measured limitation
accepted as such — no inference that automatic or in-frame retention
is safe without further scheduling evidence). The release-mode
physical-rig verification remains the owner's (D-PP-7).

## §11 — The CI evidence

The branch pushed to `origin/production-persistence-b3`; the draft
PR #3 (`production-persistence-b3` → `stage5`, the CI vehicle only —
NEVER MERGED; PRs #1 and #2 unchanged, open/draft/never-merged).
The CI run over the code head: **[THE RUN ID AND THE PER-PLATFORM
VERDICTS ARE FILLED AT THE DELIVERY VERIFICATION — the
delivery-verification note below is amended by the docs-only
verification commit once the run is verified firsthand job-level;
the docs-only record commit triggers no further run (the
pull_request path filter is `engine/**`)]**

## §12 — The rig instructions (the owner's physical verification)

The release-mode verification on the Windows rig (D-PP-7 — the
authoritative measurements; every number above is pod/dev and
non-authoritative):

```
cd engine
cargo build --release
cargo test --release --workspace
cargo test --release -p engine-slice --test pp_b3_checkpoint -- --nocapture
cargo test --release -p engine-slice --test pp_b3_membership -- --nocapture
cargo test --release -p engine-slice --test pp_b3_rotation -- --nocapture
cargo test --release -p engine-slice --test pp_b3_reclaim -- --nocapture
cargo run --release -p engine-slice -- --benchmark-persistence
cargo test --release --workspace  # the double selftest: the frozen fingerprint pins
```

The expected surfaces: the workspace green (the pod count 588 + 1
ignored may differ by platform for the SDL soak, always stated);
the four PP-B3 suites green (the crash regression's child dying at
exit 8); the benchmark §8's numbers at release speed (the flat
reclaimed steady state is the structural claim — the absolute
milliseconds are the rig's).

## §13 — The honest limitations

- **The thresholds are provisional** (D-PP-4's ratification): the
  retention window, the publication interval, and the generation
  count are explicit operator parameters with engineering-default
  values, NOT empirically validated policy. The benchmark's §8b is
  the sensitivity evidence; the acceptance of any specific cadence
  is a separate owner decision.
- **The operation costs are frame-budget concerns** (as PP-B2's
  compaction costs were): ~230-335 ms dev-pod medians for the
  checkpoint/rotation/reclamation — explicit operator operations,
  never gameplay-frame operations.
- **The §11E process-kill evidence covers the checkpoint's crash
  boundary (the batch's new window) with a real child kill; the
  reclamation's mid-Phase-B crash is exercised via the idempotent
  re-run** (the pre-scan → the installed audit → the idempotent
  deletes — the identical code path a crash-restart takes, with the
  audit-first ordering guaranteeing a pre-audit crash deletes
  nothing). A real child kill at the audit/delete boundary is an
  identified follow-up, not a claim.
- **The re-anchoring choice** (the rotation): the surviving frames
  are re-encoded under the new journal's hash chain rather than
  copied verbatim — verbatim frames would break the record chain
  over the dropped span. The transaction identities are
  content-derived, so dedup/cancellation/replay semantics are
  preserved; the digests and sequences honestly differ. This is a
  documented design choice, not an equivalence claim.
- **The reclaim's storage floor:** the reclaimed steady state
  (~413 KB in the sweep) is bounded by the retained anchors, the
  permanent metadata, the current window's snapshots, and the
  founding — the chain never shrinks below the working set. That is
  the design's intent (evidence-preserving retention), not a defect.
- **Pod vs rig:** every measurement above is pod/dev — D-PP-7's
  boundary stands.

## §14 — The exclusions honored

Nothing outside the PP-B3 scope was touched: no async workers (the
single-serialization-point discipline is synchronous throughout), no
PP-B4, no Stage 6, no networking, no provider changes beyond the
trait's reclaim, no save-authority changes, no merges (PRs #1/#2/#3
stay open/draft), no protected-branch modifications. The pipeline-
free path is untouched. The B4 consequence-lane machinery is
consumed, never modified.

## §15 — The STOP

**PP-B3 IS DELIVERED AND STOPPED FOR THE INDEPENDENT PASS/HOLD
REVIEW.** The full chain: `093395e` → `7b581db` → `934841a` →
`f93a101` → this commit. The exact HEAD and the tested SHA are the
delivery-verification note's (§11). The next entry point: the
independent reviewer's PASS/HOLD verdict or the owner's rig
evidence (§12). This record never declares the batch CLOSED — that
verdict belongs to the review and the owner alone. PP-B4, Stage 6,
and all further implementation remain unauthorized until separate
explicit owner GOs.
