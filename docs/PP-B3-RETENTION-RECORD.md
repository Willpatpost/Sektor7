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
| `88e7d81` | The multi-checkpoint example + the benchmark §8 + this record | 588 passed + 1 ignored |
| `b3377fd` | The review corrective — the HOLD's four findings F1–F4 (§16) | 597 passed + 1 ignored |
| `d721620` | The renewed-review corrective — R1, the whole-manifest completion (§17) | 601 passed + 1 ignored |
| `1927e2b` | The renewed-review correctives — R2 and R3 (§18) | 605 passed + 1 ignored |
| `8fc9632` | The renewed-review corrective — R4, the safe publication ordering (§19) | 614 passed + 1 ignored |
| `965aa5b` | The renewed-review corrective — R5, the pointer-durability distinction (§20) | 617 passed + 1 ignored |

From the PP-B2 baseline of 550 tests: **38 new tests**, every one of
them a PP-B3 behavior; no existing test was modified except where a
delivered behavior legitimately extends an asserted surface (the
checkpoint composition test's index expectations — SK's own
retirement entry, below).

**The independent review of the first delivery returned HOLD with
four bounded corrective findings (F1–F4)**; the corrective
(`b3377fd`, §16) adds **9 further tests** (the workspace 597
passed + 1 ignored) and updates the three existing idempotent
re-run assertions to the corrected F3 semantics — a completed
reclamation is now distinguished from restartable work by the typed
`reclaim/nothing-eligible` refusal, never a repeated no-op.
**The renewed review of the corrected branch returned HOLD with
one remaining bounded finding (R1)**; the corrective (`d721620`,
§17) adds **4 further tests** (the workspace 601 passed + 1
ignored). **The review of the R1-corrected branch closed R1 and
returned HOLD with two bounded findings (R2 and R3)**; the
correctives (`1927e2b`, §18) add **4 further tests** (the
workspace 605 passed + 1 ignored). **The review of the
R2/R3-corrected branch closed R2 and R3 and returned HOLD with
one bounded finding (R4)**; the corrective (`8fc9632`, §19) adds
**6 further test functions** (the workspace 614 passed + 1
ignored — the 611 distinct tests plus the three child processes'
own nested single-test reports in the harness output). **The
review of the R4-corrected branch found R4 substantially closed
and returned HOLD with one bounded durability finding (R5)**;
the corrective (`965aa5b`, §20) adds **2 further test functions**
(the workspace 617 passed + 1 ignored — the 613 distinct tests
plus the four child processes' own nested single-test reports).

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
NEVER MERGED; PRs #1 and #2 unchanged, open/draft/never-merged):
https://github.com/Willpatpost/Sektor7/pull/3. The CI run over the
code head: **40217295238 — GREEN on all three platforms, verified
firsthand job-level** (ubuntu-latest 3m46s / windows-latest 2m34s /
macos-15 ARM64 3m28s, over the code head `88e7d81` via the PR #3
pull_request event; the record's docs-only verification commit
triggers no further run — the pull_request path filter is
`engine/**`). The run's annotations (the Node 20 deprecation notice
and the macOS arm64 capacity note) are runner-infrastructure
messages, not test or build findings.

## §12 — The rig instructions (the owner's physical verification)

The release-mode verification on the Windows rig (D-PP-7 — the
authoritative measurements; every number above is pod/dev and
non-authoritative):

```
cd engine
cargo build --release
cargo test --release --workspace
cargo test --release -p engine-slice --test pp_b3_checkpoint -- --nocapture
cargo test --release -p engine-persist --test pp_b3_membership -- --nocapture
cargo test --release -p engine-slice --test pp_b3_rotation -- --nocapture
cargo test --release -p engine-slice --test pp_b3_reclaim -- --nocapture
cargo run --release -p engine-slice -- --benchmark-persistence
cargo test --release --workspace  # the double selftest: the frozen fingerprint pins
```

**The acceptance-script correction (the owner's physical run,
§21):** the membership suite's target lives in `engine-persist`,
not `engine-slice` — the initial acceptance script's first
invocation (`-p engine-slice --test pp_b3_membership`) failed
with EXIT 101 because that target does not exist in that
package. The owner corrected the command to `-p engine-persist`
(the corrected form above), resumed the same acceptance
transcript, and obtained EXIT 0 with all four membership tests
passing. This was an acceptance-script error, not an engine
failure; the first invocation is recorded as the failed command
it was, never misrepresented as passing.

The expected surfaces: the workspace green (the pod count 617 + 1
ignored may differ by platform for the SDL soak, always stated;
the count includes the four R4/R5 child processes' own nested
single-test reports — §19–§20);
the four PP-B3 suites green (the crash regression's child dying at
exit 8; the reclamation's children dying at exits 9 and 10 — §16;
the R1 windows' children dying at exits 11 and 12 — §17); the benchmark §8's numbers at release speed (the flat
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
- **The §11E process-kill evidence, completed by the corrective
  (§16, F4)**: the first delivery covered the checkpoint's crash
  boundary with a real child kill and the reclamation's mid-Phase-B
  restart only via the idempotent re-run; the corrective adds two
  deterministic real-child kills at the reclamation's own Phase-B
  boundaries (exit 9 — the audit durably on the chain, Phase B never
  run; exit 10 — the first unit's record deleted, its objects not)
  and the audited deletion manifest that makes the interrupted
  deletes converge onto the cleanly-reclaimed twin's exact storage
  state. These are process-termination kills (exit-code seams): they
  establish real-death restartability of the audited protocol, NOT
  platform-wide power-loss durability — that boundary belongs to the
  owner's rig (D-PP-7) and is never claimed here.
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
`f93a101` → `88e7d81` (the code) → the docs-only verification
commit `129c9bd`. The tested code SHA `88e7d81` (the CI 40217295238
green ×3 over it; the workspace 588 passed + 1 intentionally ignored
at it). The next entry point: the independent reviewer's PASS/HOLD
verdict or the owner's rig evidence (§12). This record never declares
the batch CLOSED — that verdict belongs to the review and the owner
alone. PP-B4, Stage 6, and all further implementation remain
unauthorized until separate explicit owner GOs.

**SUPERSEDED — the verdict arrived: HOLD with four bounded
corrective findings (F1–F4). The corrective is delivered at
`b3377fd` and the RENEWED STOP stands in §16.**

## §16 — The independent review's HOLD: the corrective delivery (F1–F4)

The verdict on the first delivery: **HOLD — four bounded corrective
findings (F1–F4)**, with the authorization to correct autonomously
on `production-persistence-b3` (correct, add tests, commit, push,
rerun the verification) and STOP again for the renewed review. The
corrective code commit **`b3377fd`** (this docs-only verification
commit amends the record around it; the chain `093395e` →
`7b581db` → `934841a` → `f93a101` → `88e7d81` → `129c9bd` →
`b3377fd`).

**F1 — the repeated-rotation active-path root.** The finding: the
second rotation published its pointer beside the ROTATED file while
the recovery resolved only the pointer beside the original campaign
journal — two potential authorities. The fix: `SessionCrossing`
carries `journal_root` (set at `open_at` from the opened journal,
overridden at `recover_at` by the caller's journal path — the
campaign root, never the rotated file), every rotation publishes
`<root>.active` there, and `reclaimable_journal_files` enumerates
ALL stale generations beside the root (the root file and every
`journal-*.t1` except the active one, each retired with its `.lock`
sidecar under the audited ledger — the prior generations never
accumulate). Regressions:
`three_successive_rotations_keep_one_authoritative_active_path`
(three rotations with outstanding work between them — the survivors
accumulate and re-anchor whole each time; the pointer at the root
after each; the restart's recovery following it to the THIRD
generation with the outstanding plans replayed; the fourth rotation
through the recovered crossing re-landing the pointer at the root;
the backup capturing the fourth generation's bytes and verifying)
and
`an_interrupted_later_flip_selects_the_previous_authoritative_journal`
(the hand-reconstructed pre-flip durable state — the root pointer
still naming the first generation — recovering via the previous
authoritative superset journal with the plans replayed, the
orphaned second generation inert, a subsequent rotation re-pointing
the root).

**F2 — the zero-generation window and the post-delete check.** The
finding: `generations=0` names no protected checkpoint — it could
eventually retire the last recovery anchor, and the anchor check
ran too late (after the destructive operation). The fix: the window
validation FIRST (`generations < 1` → the typed
`reclaim/invalid-window` refusal before any audit revision or
destructive operation) and the pre-deletion anchor invariant (the
newest checkpoint's SK read and its first content object verified
to BE a session-snapshot object — a verified recoverable anchor
remains after the deletion; prerequisites are never post-delete
checks). Regression:
`unsafe_retention_windows_are_refused_before_any_audit_or_deletion`
(generations=0 refused with the canonical and journal state
unchanged: no audit appended — 8 records — nothing deleted, the
range record still present, the journal file intact).

**F3 — the reclamation never progressing past the oldest audited
checkpoint.** The finding: MK records are permanent, so re-selecting
the same completed MK was a no-op repeat — the reclamation never
progressed to the next generation. The fix: the target selection
iterates the checkpoints beyond the window OLDEST-FIRST and
distinguishes a FULLY reclaimed range (every index member absent —
skipped, never repeated) from an incomplete one (an interrupted
Phase B — resumed) from an eligible one (the oldest with any
present member); a completed re-run now refuses with the typed
`reclaim/nothing-eligible` (the three existing idempotent re-run
assertions — the lifecycle, the full-stack, the example — updated
to these semantics). Regression:
`four_checkpoints_reclaim_sequentially_across_generations` (the
19-record chain F ← S1 ← C1 ← S2 ← C2 ← SK1 ← MK1 ← S3 ← C3 ← SK2 ←
MK2 ← S4 ← C4 ← SK3 ← MK3 ← S5 ← C5 ← SK4 ← MK4; the
two-generation window reclaims MK1's range — eligible 3, removed 6;
the minimal window then PROGRESSES to MK2's and MK3's — eligible 2
and removed 4 each, MK1 skipped and never repeated; the fourth call
refuses nothing-eligible; the store measurably shrinks across the
generations — no accumulation of retired publication snapshots; all
four MKs and their metadata objects permanent; the walk validating
through the chained skip tables at 15 records with the reclaimed
accounting 7; a key from MK1's range still rejected and a
depends_on naming MK2's reclaimed member still resolved after three
generations; the recovery working from the protected window's
anchor).

**F4 — the missing real mid-reclamation termination evidence.** The
finding: a death between a record-file deletion and its
referenced-object deletions orphans content a re-run could not
identify from the absent record — and no real-kill evidence existed
at these boundaries. The fix: `ReclaimUnit {record, objects}` — the
complete per-unit deletion manifest collected during Phase A's
digest verification while the range bytes are still present, carried
in the ledger (the unit-count coherence rule, bounded counts), and
Phase B deletes from the manifest in BOTH providers (the file
medium: the record file, the objects, AND the quarantined copies
the next open's orphan sweep moved — so interrupted deletions
converge; SQLite: the manifest's rows in ONE transaction,
all-or-nothing — the two atomicity models verified separately); the
resume arm verifies the installed audit's bindings AND that its
manifest covers exactly the permanent index's identities. Two
deterministic real-child-process seams: `ARM_RECLAIM_AFTER_AUDIT`
(exit 9 — the audit durably on the chain, Phase B never run) and
`ARM_RECLAIM_MID_UNIT` (exit 10 — the first unit's record deleted,
its objects not). Regressions:
`a_real_death_after_the_audit_resumes_and_completes_to_convergence`
(a genuine child death at exit 9; the durable state validating at 9
records with nothing reclaimed; the recovery plus the same retry
recognizing the installed audit by its deterministic key, deleting
from the manifest — removed 4 — and converging with NO second audit
at 7 records; the re-run refusing),
`a_real_death_mid_unit_converges_from_the_audited_manifest` (a
genuine child death at exit 10 between the record deletion and its
object deletions; the post-death walk validating — the
still-present SK1 walked, only S1 a gap; the same retry resuming
from the audited manifest — never re-reading the removed record —
and converging onto the cleanly-reclaimed TWIN's exact storage
state: identical object sets, byte-identical audit head, empty
quarantine — the intended storage state, not merely quarantined
orphans; the re-run refusing), and
`the_sqlite_provider_reclaims_with_transactional_atomicity` (the
same two-checkpoint reclamation over `StoreLocation::Database`:
eligible 2 / removed 4 in one transaction, the validate at
reclaimed 2 / records 7, the recovery, the nothing-eligible re-run).

**The verification at `b3377fd`:** fmt and `clippy --workspace
--all-targets -- -D warnings` green; the workspace **597 passed +
1 intentionally ignored, 0 failed** (the delivery's 588 + the 9
corrective tests); the two corrected suites green (`pp_b3_reclaim`
12 — the 5 prior plus 7 new; `pp_b3_rotation` 4 — the 2 prior plus
2 new); the double release-mode selftest green both runs (597 / 45
suites, identical results modulo timings — the frozen Stage-0
fingerprint pins hold, `engine-core` untouched); `cargo build
--workspace` green. **The CI: run 40222061708 GREEN on all three
platforms, verified firsthand job-level over the corrected head
`b3377fd`** (ubuntu-latest 10m41s — the Linux-only Vulkan offscreen
smoke is the long step; windows-latest 3m39s; macos-15 ARM64
2m59s; via the PR #3 pull_request event — the CI vehicle, never
merged; the annotations are the known runner-infrastructure notes).
The protected branches verified unchanged (main=5dbb377,
stage5=047faf9, macos-port=ba395d8, design=a47ef10, pp-b1=bfe60b7,
pp-b2=00109b0); PRs #1/#2 unchanged, open/draft.

**The honest boundaries of the corrective:**
- The two reclamation seams are PROCESS-TERMINATION kills
  (deterministic exit-code deaths at the exact durable boundaries):
  they establish real-death restartability of the audited protocol
  and the manifest-driven convergence — NOT platform-wide
  power-loss durability (the OS page cache and the device layers
  remain the rig's domain, D-PP-7).
- The "additional journal appends" after the three-rotation restart
  are realized as the fourth rotation's re-anchoring (real journal
  content written by the restarted session, the pointer re-landing
  at the root): post-recovery boundary deactivations are
  empty-scratch BY THE ACCEPTED B2 DESIGN (a post-crash boundary
  re-derives the LOD against the snapshot; the region's captured
  content is already durably protected in the outstanding
  transactions) — verified empirically in both the confirmed-first
  and the all-unconfirmed arcs, and asserted as such in the test
  rather than worked around.
- The benchmark §8 was not re-extended: the F3 driver test is the
  sequential multi-generation non-accumulation evidence (the store
  measurably shrinking across three reclamation generations), and
  §8a's flat-steady-state claim is unchanged by the corrective.

**The preserved invariants:** D-PP-4's architecture; the D-PP-4a/4b
rejections (the eligible set is exactly the audited index; the
confirmations never named); the two-revision SK/MK structure; the
immutable revision-record bytes; the permanent MK metadata and
confirming-evidence guarantees; PP-B1/B2 behavior; the fingerprints
and recovery contracts.

**THE RENEWED STOP.** PP-B3's corrective is delivered and stopped
for the RENEWED independent PASS/HOLD review. The tested code SHA
`b3377fd`; the CI 40222061708 green ×3 over it; the workspace 597
passed + 1 intentionally ignored at it. The next entry point: the
renewed verdict or the owner's rig evidence (§12). This record
never declares the batch CLOSED. PP-B4, Stage 6, and all further
implementation remain unauthorized until separate explicit owner
GOs.

**SUPERSEDED — the renewed verdict arrived: HOLD with one
remaining bounded finding (R1). The corrective is delivered at
`d721620` and the FINAL RENEWED STOP stands in §17.**

## §17 — The renewed review's HOLD: the R1 corrective (the whole-manifest completion)

The renewed verdict on `b3377fd`/`c794b01`: **HOLD — one remaining
bounded corrective finding (R1)**, with the authorization to make
the bounded correction exclusively on `production-persistence-b3`
and STOP again for the renewed review. The corrective code commit
**`d721620`** (this docs-only verification commit amends the record
around it; the chain `129c9bd` → `b3377fd` → `c794b01` →
`d721620`).

**THE FINDING (R1) — incomplete Phase B classified as fully
reclaimed.** The target selection considered a checkpoint fully
reclaimed when none of its INDEXED REVISION RECORDS remained —
insufficient against the durable audit's manifest, which also
requires the deletion of the referenced content objects, the
possible quarantined copies, and the audited stale journal
generations with their sidecar locks. Two interruption windows:
**(A) the last-unit window** — a death after the LAST indexed
record's deletion and before its referenced objects leaves every
indexed record absent while the manifest is incomplete; the next
call skipped the checkpoint. **(B) the post-medium/pre-journal
window** — a death after the canonical medium's manifest deletes
and before the audited stale-journal retirement leaves the journal
cleanup permanently incomplete. The existing exit-10 test killed
after the FIRST unit's record deletion while another indexed
record remained, so neither window was covered.

**THE CORRECTIVE.** (1) The THREE-STATE SELECTION
(`reclaim_checkpoint`): per candidate oldest-first beyond the
window, the audit is discovered per candidate
(`find_installed_reclamation`); an UNAUDITED range with present
indexed records selects the FRESH path; an AUDITED range gets the
coherence verification (fail closed on invalid bindings — the
resume standard) and then THE WHOLE-MANIFEST COMPLETION CHECK —
the medium's `reclaim_pending(units)` probe OR the driver-side
journal check (any audited journal name or its `.lock` still a
file) — pending selects the RESUME (carrying the discovered
ledger: no duplicate audit, no re-derivation, no re-read of
already-deleted range bytes); only a fully satisfied manifest is
completed and SKIPPED. A subsequent call reports
`reclaim/nothing-eligible` only AFTER the cleanup truly completes.
(2) `CanonicalMedium::reclaim_pending` (both providers + the
SessionStore seam): the local medium probes each unit's record
file, its content objects, AND the quarantined copies (the sweep's
moved bytes with their `.reason` files); SQLite probes the record
and object rows (SELECT EXISTS over the raw 32-byte identity
blobs — the one-transaction atomicity means a remaining row is an
uncommitted Phase B). (3) The GAP-AWARE AUDIT DISCOVERY: the
per-candidate descent crosses legitimately reclaimed ranges via
the most recent on-chain MK's skip table (the established
compacted-walk treatment — an uncovered absence still fails
closed), so an audit sitting below a later-reclaimed range is
discovered without a legitimate gap being reinterpreted as
corruption.

**THE REAL-KILL EVIDENCE (two deterministic child seams, both
windows).** `ARM_RECLAIM_LAST_UNIT` (local.rs, exit 11 — after the
LAST unit's record deletion, before its objects) and
`ARM_RECLAIM_AFTER_MEDIUM` (crossing.rs, exit 12 — after
`medium.reclaim`, before the journal retirement). The regressions:
`a_real_death_at_the_last_unit_resumes_and_converges` (window A:
the durable post-death walk already skips both index members — the
misclassification's exact precondition, asserted; the recovery
plus the same retry RESUMES (never nothing-eligible), completes
the last unit's objects including the sweep-quarantined copies,
appends NO second audit (the head unchanged), converges onto the
cleanly-reclaimed twin's identical object sets with an empty
quarantine; the reclaimed key's reuse still rejected and a
depends_on naming the reclaimed member still resolved via the
permanent index; the subsequent call refuses nothing-eligible) and
`a_real_death_before_the_journal_retirement_resumes_and_completes`
(window B, over the ROTATED two-checkpoint campaign: the stale
root journal AND its sidecar lock still on disk post-death — the
incomplete retirement visible; the recovery plus the same retry
removes zero medium bytes (idempotent) and retires the audited
stale journal with its lock, converging onto the twin's identical
object sets and byte-identical audit head; the subsequent call
refuses nothing-eligible). Both children rebuild their fixtures at
environment-provided paths (the `rotated_two_checkpoint_campaign_at`
path-based core).

**The verification at `d721620`:** fmt and `clippy --workspace
--all-targets -- -D warnings` green (the selection's target tuple
factored into the `ReclaimTarget` type); the workspace **601
passed + 1 intentionally ignored, 0 failed** (the F1–F4
corrective's 597 + the 4 R1 tests); the `pp_b3_reclaim` matrix 16
passed 0 failed (the 12 prior plus the 4 new); the checkpoint,
rotation, and membership matrices green unchanged; the double
release-mode selftest green both runs (601 / 45 suites, identical
results modulo timings — the frozen Stage-0 fingerprint pins hold,
`engine-core` untouched); `cargo build --workspace` green. **The
CI: run 40227543159 GREEN on all three platforms, verified
firsthand job-level over the corrected head `d721620`**
(ubuntu-latest 7m33s — the Linux-only Vulkan offscreen smoke is
the long step; windows-latest 3m48s; macos-15 ARM64 3m42s; via the
PR #3 pull_request event — the CI vehicle, never merged; the
annotations are the known runner-infrastructure notes). The
protected branches verified unchanged (main=5dbb377,
stage5=047faf9, macos-port=ba395d8, design=a47ef10, pp-b1=bfe60b7,
pp-b2=00109b0); PRs #1/#2 unchanged, open/draft.

**The honest boundaries of the R1 corrective:**
- The two new seams are PROCESS-TERMINATION kills (deterministic
  exit-code deaths at the exact durable boundaries): they
  establish real-death restartability of the audited protocol and
  the whole-manifest completion, NOT platform-wide power-loss
  durability (D-PP-7's rig domain, never claimed).
- The SQLite provider's transactional semantics make window A
  structurally impossible on that provider (the Phase-B delete is
  one all-or-nothing transaction — there is no intermediate
  records-gone-objects-present state); its `reclaim_pending` row
  probe is exercised by the parity test's completed re-run, and
  window B's driver-side journal handling is provider-independent
  (covered by the local real-kill test over the same driver path).
- A disclosed latent gap, PRE-EXISTING and unexercised by any test
  (an identified follow-up, never silently skipped): the
  checkpoint RANGE WALK classifies the session-snapshot family,
  the consequence-batch domain, the checkpoint-metadata boundary,
  and the founding — it does NOT classify the
  reclamation-ledger/1 domain, so a checkpoint published AFTER a
  reclamation would refuse at the range walk. Every existing test
  publishes its checkpoints before any reclamation (the audited
  cadence of the delivered machinery); the R1 gap-aware descent
  handles the audit-discovery side of the interleaved ordering
  defensively, but the range-walk classification is outside R1's
  bounded scope and remains for a future authorized corrective.
- The fail-closed wrong-deletion case is unchanged: a hand-deleted
  range record with SOME present selects the fresh path and fails
  at the digest walk.

**The preserved invariants:** D-PP-4's architecture; the
D-PP-4a/4b rejections; the two-revision SK/MK structure; the
immutable revision-record bytes; the permanent MK metadata and
confirming-evidence guarantees; PP-B1/B2 behavior; the fingerprints
and recovery contracts; no automatic reclamation, no new policy
defaults, no PP-B4 orchestration.

**THE FINAL RENEWED STOP.** PP-B3's R1 corrective is delivered and
stopped for the renewed independent PASS/HOLD review. The tested
code SHA `d721620`; the CI 40227543159 green ×3 over it; the
workspace 601 passed + 1 intentionally ignored at it. The next
entry point: the renewed verdict or the owner's rig evidence
(§12). This record never declares the batch CLOSED. The physical
Windows acceptance is NOT yet requested. PP-B4, Stage 6, and all
further implementation remain unauthorized until separate explicit
owner GOs.

**SUPERSEDED — the renewed verdict arrived: R1 CLOSED (do not
reopen without new contradictory evidence); HOLD with two bounded
findings (R2 and R3). The correctives are delivered at `1927e2b`
and the FINAL RENEWED STOP stands in §18.**

## §18 — The renewed review's HOLD: the R2 and R3 correctives

The renewed verdict on `d721620`/`7462899`: **R1 CLOSED** (the
`reclaim_pending` implementation, the audited three-state
selection, and the exit-11/exit-12 regressions accepted; the CI
40227543159 verified) — **HOLD with two bounded corrective
findings (R2 and R3)**, authorized as bounded PP-B3 correctness
corrections under the existing owner GO. The corrective code
commit **`1927e2b`** (this docs-only verification commit amends
the record around it; the chain `7462899` → `1927e2b`).

**R2 — the checkpoint-after-reclamation lifecycle.** The finding
(disclosed in §17's honest boundaries, confirmed by the review's
source read): the checkpoint RANGE WALK classified only the
session-snapshot family (Publication), the consequence-batch
domain (Anchor), the checkpoint-metadata boundary, and the
founding — a canonical reclamation AUDIT sitting between
checkpoints hit the `checkpoint-walk/unclassifiable` refusal, so a
checkpoint published after a reclamation could never complete.
THE FIX (`walk_checkpoint_range`): the `reclamation-ledger/1`
domain classifies as RETAINED EVIDENCE — a `RangeKind::Anchor`
exactly like a confirming revision, never a reclaimable snapshot
publication — with the STRICT DECODE as the fail-closed gate (an
invalid, missing, or incoherent ledger object refuses the
checkpoint, never merely skipped), and the audit's retained
position participating in the ancestry and the skip tables
exactly like a confirming anchor (an anchor standing on a
reclaimable publication jumps to the audit — the nearest retained
record — under the same skip computation). No existing revision
format changed; the historical fingerprints hold. THE
REGRESSIONS: `checkpoints_continue_across_reclamations_and_recover`
(the nine-step lifecycle over LocalFileMedium — checkpoint,
checkpoint, RECLAIM, the continuation with a publication standing
ON the audit (parent asserted) and a full confirmation cycle,
CHECKPOINT 3 over the audit (the previously impossible step),
recover and validate (the walk 11 records with the audit retained
in the ancestry), RECLAIM the next range (progressing past the
completed MK1), continue, CHECKPOINT 4 over the second audit,
reopen and recover — the exactly-once behavior, the historical
key-reuse rejection and the reclaimed-dependency resolution, the
permanent MK metadata and both audits readable, the final walk 14
records with both reclaimed ranges skipped, and the wider-window
nothing-eligible refusal);
`the_sqlite_lifecycle_continues_across_reclamations` (the same
essential cadence over `StoreLocation::Database` — the provider
parity);
`a_missing_retained_audit_object_fails_the_checkpoint_closed`
(the audit's ledger object removed by hand — the store's
validation fails AND the next checkpoint's range walk refuses
with the typed missing-object error, never skipping the audit).

**R3 — the journal-generation ownership.** The finding:
`reclaimable_journal_files` selected stale generations by the
directory-wide `journal-*.t1` filename predicate excluding the
active — NOT proof of campaign ownership (in a shared directory
a foreign campaign's root or generation matches the predicate,
and the reclamation could delete it or its lock). THE FIX: (1)
THE GENERATIONS MANIFEST (rotation.rs — the versioned domain
`engine-persist/journal-generations/1`, strict decode: the
domain/version gates, bounded counts, the plain-file-name rule,
the duplicate rejection, no trailing bytes): the campaign's own
recorded lineage — the generation names THIS campaign's rotations
created or rotated away from — maintained by the rotation itself
(the pre-flip active captured BEFORE the flip, the new
generation's name, deduplicated, written atomically
tmp+fsync+rename+dir-sync after the pointer flip; a crash between
the flip and the record's write leaves the new generation
unrecorded — conservatively retained, the next rotation records
it as its prior active); `read_journal_generations` (a missing
record means NO generation is eligible — ownership conservatively
unproven: a pre-rotation campaign has no stale generations) and
`retire_journal_generations` (the reclamation's Phase B removes
the retired names). (2) `reclaimable_journal_files` rewritten: the
candidates are the MANIFEST's names minus the active — never a
directory scan. (3) THE CAMPAIGN-ROOT BINDING (`reclaim_checkpoint`):
the journal path supplied must equal the crossing's own
`journal_root` (the path it opened or recovered with) — a foreign
root fails closed `reclaim/journal-root` before anything. The
pre-rotation compatibility preserved (no pointer means no
rotation — the root IS the active, never touched); the lock
handling unchanged (each retired name's `.lock` removed with it,
never another file). THE REGRESSION:
`shared_directory_campaigns_never_cross_journal_ownership` (two
independent campaigns — different roots, different stores — in
ONE shared directory, both carrying pending actionable T1
evidence, both rotated (A twice); A reclaims its eligible history
and B's journals, lock, pointer, and canonical state are
byte-identical and unchanged; A's own root and first generation
retired while its active journal stands and its manifest names
only the active; an UNOWNED `journal-*.t1` (real journal bytes,
no manifest record) is retained — no ownership evidence, no
deletion; B recovers independently with its pending plan replayed
exactly once; A recovers with its cleanup correct and the
completed ranges refusing; a reclamation handed B's root fails
closed on the campaign-root binding; B's INTERRUPTED rotation (the
pointer hand-rewritten to its root — the pre-flip state) recovers
through the previous authoritative journal and a subsequent
rotation re-lands its pointer with A untouched throughout).

**The verification at `1927e2b`:** fmt and `clippy --workspace
--all-targets -- -D warnings` green; the workspace **605 passed +
1 intentionally ignored, 0 failed** (the R1 corrective's 601 +
the 4 R2/R3 tests); the `pp_b3_reclaim` matrix 20 passed 0 failed
(the 16 prior plus the 4 new); the checkpoint, rotation, and
membership matrices green unchanged; the double release-mode
selftest green both runs (605 / 45 suites, identical results
modulo timings — the frozen Stage-0 fingerprint pins hold,
`engine-core` untouched); `cargo build --workspace` green. **The
CI: run 40232169084 GREEN on all three platforms, verified
firsthand job-level over the corrected head `1927e2b`**
(ubuntu-latest 3m38s; windows-latest 4m10s; macos-15 ARM64
2m40s; via the PR #3 pull_request event — the CI vehicle, never
merged; the annotations are the known runner-infrastructure
notes). The protected branches verified unchanged (main=5dbb377,
stage5=047faf9, macos-port=ba395d8, design=a47ef10, pp-b1=bfe60b7,
pp-b2=00109b0); PRs #1/#2 unchanged, open/draft.

**The honest boundaries of the R2/R3 correctives:**
- The generations manifest is the ROTATION-MAINTAINED lineage
  under the single-writer discipline — an explicit ownership
  record, not a cryptographic proof: a hand-corrupted manifest
  naming foreign files would be honored by the retirement (the
  strict decode validates the record's form, not the provenance
  of the names it carries). The single-writer discipline plus the
  campaign-root binding bound that surface; a stronger
  content-bound ownership proof would be a new design, outside
  these bounded corrections.
- The manifest's crash window (between the pointer flip and the
  record's write) leaves the new generation unrecorded —
  conservatively retained forever unless a later rotation records
  it as its prior active. Retention is the safe direction.
- No new process-kill seams were required by R2/R3 (the
  lifecycle and ownership regressions are deterministic
  in-process constructions; the interrupted-rotation case is the
  hand-reconstructed pre-flip durable state, the F1-established
  pattern).

**The preserved invariants:** D-PP-4's architecture; the
D-PP-4a/4b rejections; the SK/MK structure and the digest-chain
computation; the immutable revision-record bytes and the
byte-identical existing formats; the permanent MK metadata and
confirming-evidence guarantees; the fully corrected F1–F4 and R1
behaviors; PP-B1/B2 behavior; the fingerprints and recovery
contracts; no PP-B4 threading or background GC; no persistence
redesign.

**THE FINAL RENEWED STOP.** PP-B3's R2/R3 correctives are
delivered and stopped for the renewed independent PASS/HOLD
review. The tested code SHA `1927e2b`; the CI 40232169084 green
×3 over it; the workspace 605 passed + 1 intentionally ignored at
it. The next entry point: the renewed verdict or the owner's rig
evidence (§12). This record never declares the batch CLOSED. The
physical Windows acceptance is NOT yet requested. PP-B4, Stage 6,
and all further implementation remain unauthorized until separate
explicit owner GOs.

**SUPERSEDED — the renewed verdict arrived: R2 and R3 CLOSED (do
not reopen without new contradictory evidence); HOLD with one
bounded finding (R4). The corrective is delivered at `8fc9632`
and the FINAL RENEWED STOP stands in §19.**

## §19 — The renewed review's HOLD: the R4 corrective (the safe publication ordering)

The renewed verdict on `1927e2b`/`6bd4b27`: **R2 CLOSED**
(checkpoint-after-reclamation continuity, provider parity, and
audit retention) and **R3 CLOSED for the authorized
normal-operation ownership model** (the rotation-maintained
campaign generation manifest, shared-directory isolation, and
campaign-root binding) — **HOLD with one bounded corrective
finding (R4)**, authorized as a bounded PP-B3 correctness
correction under the existing owner GO. The corrective code
commit **`8fc9632`** (this docs-only verification commit amends
the record around it; the chain `6bd4b27` → `8fc9632`).

**THE FINDING (R4) — the failure after the authoritative journal
pointer publication.** The counterexample: journal B is written
and scan-verified while the live crossing holds journal A; the
root `.active` pointer is atomically switched to B; the R3
ownership-manifest read/write/sync FAILS; `rotate_journal`
returns `Err` via `?`; the crossing never performs the pipeline
swap; the live crossing keeps writing journal A while crash
recovery resolves B — additional T1 effects written to A are not
present in B, violating the single-authoritative-journal and
recoverability guarantees. The issue is the ORDINARY ERROR-RETURN
PATH after the authority pointer may have changed (not the
already accepted process-crash retention behavior).

**THE CORRECTIVE — three parts.** (1) **THE PUBLICATION
ORDERING** (rotation.rs): the ownership record (the R3
generations manifest) is maintained BEFORE the pointer flip —
every manifest failure is a PRE-FLIP failure: the error returns
with the authority unchanged, the caller stays safely on the
current journal (a manifest I/O failure can never hand control
back to an operational stale-journal writer, because the pointer
has not moved). A crash between the manifest write and the flip
leaves the new generation recorded but inert — the pointer still
names the prior journal (the authority and a superset); a
recorded-but-never-active generation is harmless. (2) **THE
AUTHORITY BOUNDARY** (rotation.rs): the pointer rename is the
LAST fallible step that may return an error — after it, the live
pointer names the new journal, so no later failure may return an
error that leaves the caller operational on the stale journal.
The post-rename directory sync is a DURABILITY step, not an
authority step: its failure means the flip may not survive a
power loss — the live authority is still the new journal — so it
is carried as the report's new `durability_warning` field
(`Option<String>` on `JournalRotation`), never an error return.
(3) **THE POST-FLIP POISON** (crossing.rs): the driver's
post-flip pipeline reconstruction (the audited fallible
post-flip steps: the journal reopen, the medium open, the
pipeline rebuild) is extracted into `rebuild_pipeline_after_rotation`;
a failure sets `journal_authority_lost` and EVERY further
authoritative operation — `publish_snapshot`, `deactivate` (the
shared six-phase transition driver), `activate`,
`commit_canonical`, `compact_session`, `publish_checkpoint`,
`rotate_journal`, `reclaim_checkpoint` — refuses with the typed
`rotation/authority-lost` until a fresh recovery re-derives the
crossing from the pointer (the medium-level `checkpoint` boundary
stays usable — it never writes through the journal). The
conservative retention for orphaned/unrecorded generations, R3's
strict campaign isolation, and the backup/recovery pointer
semantics are all preserved.

**THE FAULT INJECTION (deterministic, real returned I/O errors —
never natural disk-full).** `GENERATIONS_IO_FAULT` (the ownership
record's read/write returns the injected `JournalError::Io` — the
pre-flip failure path), `POINTER_DIR_SYNC_FAULT` (the post-flip
pointer directory sync skipped and reported as the durability
warning), and `ROTATION_SWAP_FAULT` (the post-flip pipeline
rebuild fails with the injected `MediumError::Io`). The statics
are process-local and the harness runs the file's tests in
parallel, so each fault-armed scenario runs in its OWN spawned
child process (the child/parent pattern the exit-8-through-12
seams established) — the parent verifies the durable outcomes
firsthand. **THE REGRESSIONS (3 child/parent pairs,
pp_b3_rotation.rs):**
`a_manifest_io_failure_before_the_flip_leaves_the_authority_unchanged`
(the child: the refused rotation with the typed Io error, the
authority-unchanged asserts, and the post-failure boundary
landing in the still-authoritative journal; the parent: the
pointer unchanged, the journal carrying the child's post-failure
T1 evidence, the recovery resolving the same journal with the
plans replayed exactly once, and the retry rotation succeeding
with no durability warning);
`a_pointer_dir_sync_failure_after_the_flip_still_rebinds_the_authority`
(the child: the rotation completing WITH the warning and the
rebind — the pointer and the in-memory writer agree — plus the
post-rotation boundary into the new generation; the parent: the
durable verification — the new generation carries the
post-rotation T1 evidence while the stale journal receives
nothing new — and the recovery resolving the new authority with
the plan replayed exactly once);
`a_pipeline_rebuild_failure_after_the_flip_poisons_the_crossing`
(the child: the refused swap with the typed Io error, the
authority-changed assert, and the poison refusing publication,
commit, reclamation, and deactivation; the parent: the recovery
resolving the NEW authoritative journal with the outstanding plan
replayed exactly once, the repeat rotation succeeding, and the
lineage coherent — the manifest carrying the root and both
generations).

**The verification at `8fc9632`:** fmt and `clippy --workspace
--all-targets -- -D warnings` green; `cargo build --workspace`
green; the workspace **614 passed + 1 intentionally ignored, 0
failed** (the R2/R3 correctives' 605 plus the 6 new test
functions plus the 3 child processes' own nested single-test
reports in the harness output — 611 distinct tests); the
`pp_b3_rotation` matrix 10 passed 0 failed (the 4 prior tests
plus the 3 parents and the 3 children); the reclaim, checkpoint,
and membership matrices green unchanged; the double release-mode
selftest green both runs (identical results modulo timings — the
frozen Stage-0 fingerprint pins hold, `engine-core` untouched).
**The CI: run 40241079631 GREEN on all three platforms, verified
firsthand job-level over the corrected head `8fc9632`**
(macos-15 ARM64 2m35s; ubuntu-latest 3m42s; windows-latest
3m48s; via the PR #3 pull_request event — the CI vehicle, never
merged; the annotations are the known runner-infrastructure
notes). The protected branches verified unchanged (main=5dbb377,
stage5=047faf9, macos-port=ba395d8, design=a47ef10, pp-b1=bfe60b7,
pp-b2=00109b0); PRs #1/#2 unchanged, open/draft.

**The surviving limitations (stated honestly):**
- The durability warning's semantics: a failed post-flip pointer
  directory sync means the LIVE authority is the new journal
  (correct) but the flip may not survive a POWER LOSS. That is
  the safe direction: the pre-rotation pointer state names the
  PRIOR journal — a superset holding every outstanding effect —
  so a power-loss reversion recovers everything (the pre-flip
  crash semantics the rotation always had). The warning surfaces
  in the rotation report for the operator; the caller continues
  on the authoritative journal.
- The manifest's crash window (between the pre-flip manifest
  write and the flip) leaves the new generation recorded but
  inert — harmless (the prior journal is the authority and a
  superset; a recorded-but-never-active generation is inert).
- The fault-injection statics are test-only (`#[doc(hidden)]`,
  process-local, never armed by engine code); the production
  paths carry no seams.

**The preserved invariants:** D-PP-4's architecture; the
D-PP-4a/4b rejections; the SK/MK structure; the immutable
revision-record bytes; the permanent metadata and
confirming-evidence guarantees; the fully corrected F1–F4 and
R1–R3 behaviors; PP-B1/B2 behavior; the fingerprints and
recovery contracts; no PP-B4 threading, queues, or unrelated
architecture changes.

**THE FINAL RENEWED STOP.** PP-B3's R4 corrective is delivered
and stopped for the renewed independent PASS/HOLD review. The
tested code SHA `8fc9632`; the CI 40241079631 green ×3 over it;
the workspace 614 passed + 1 intentionally ignored at it. The
next entry point: the renewed verdict or the owner's rig evidence
(§12). This record never declares the batch CLOSED. The physical
Windows acceptance is NOT yet requested. PP-B4, Stage 6, and all
further implementation remain unauthorized until separate explicit
owner GOs.

**SUPERSEDED — the renewed verdict arrived: R4 substantially
CLOSED in substance (the manifest ordering and post-flip
pipeline-poison fixes accepted, preserved); HOLD with one bounded
durability finding (R5). The corrective is delivered at `965aa5b`
and the FINAL RENEWED STOP stands in §20.**

## §20 — The renewed review's HOLD: the R5 corrective (the pointer-durability distinction)

The renewed verdict on `8fc9632`/`aeb7fdd`: **R4 substantially
CLOSED** (the manifest ordering and the post-flip pipeline-poison
fixes accepted in substance — preserved) — **HOLD with one
bounded durability finding (R5)**, authorized as a bounded PP-B3
corrective on `production-persistence-b3`. The corrective code
commit **`965aa5b`** (this docs-only verification commit amends
the record around it; the chain `aeb7fdd` → `965aa5b`).

**THE FINDING (R5) — an unsynced active-journal pointer cannot
safely precede new durable T1 writes.** The counterexample:
journal A is the currently authoritative journal; the rotation
writes and verifies journal B; the root active pointer is renamed
to B; the directory synchronization FAILS; the R4 design returned
success with a warning; the crossing rebound to B and permitted
further T1 captures; a new durable T1 transaction was appended
ONLY to B; a power loss caused the unsynced pointer to recover at
its previous value, A; the recovery read A and could not
reconstruct the newer transaction from B. **The pre-rotation
journal is a superset only of the evidence existing when the
rotation occurred — it is NOT a superset of future writes to B.**
A warning does not make this safe if the caller is still allowed
to progress authoritatively.

**THE CORRECTIVE — the pointer-durability distinction, with the
R4 ordering and authority rules preserved (the manifest-before-
flip publication ordering; the pre-flip errors leaving the old
journal usable and authoritative; the post-flip pipeline-rebuild
failures fail-closed; no asynchronous worker, queue, or
persistence-architecture redesign).** (1) **THE OUTCOME
DISTINCTION** (rotation.rs): `PointerDurability { Established,
Unresolved { detail } }` replaces the R4 `durability_warning` on
`JournalRotation` — an `Unresolved` outcome is NEVER a durably
completed transition. (2) **THE FAIL-CLOSED DURABILITY GATE**
(crossing.rs): on `Unresolved` the rotation returns the typed
`rotation/pointer-durability` refusal and the crossing enters the
`DurabilityUnresolved` state — the R4 authority guard generalized
to `JournalAuthority { Healthy, Lost, DurabilityUnresolved }` —
and EVERY authoritative operation (publish_snapshot, deactivate,
activate, commit_canonical, compact_session, publish_checkpoint,
rotate_journal, reclaim_checkpoint) refuses: no new authoritative
T1 transaction may commit while the pointer durability remains
unresolved. (3) **THE REPAIR PATH**:
`SessionCrossing::reestablish_pointer_durability` — the directory
sync re-established on demand
(`engine_persist::reestablish_pointer_durability` over the root's
pointer), then the pipeline rebuilt over the pointer-resolved
authoritative journal; a re-sync failure leaves the fail-closed
state intact; a rebuild failure escalates to the R4 `Lost` state;
the crossing re-opens (`Healthy`) only after the verified
durability. (4) **THE RECOVERY'S DURABILITY VERIFICATION**
(recover_at): immediately after the pointer resolution the
recovery completes the directory sync — a fresh recovery cannot
silently clear an unresolved durability-uncertain condition
merely by reading the currently visible pointer; a sync failure
fails the recovery closed before any progression. (5) **THE
OLD-JOURNAL ELIGIBILITY**: the fail-closed reclamation refusal
enforces it — no journal retirement while the prior journal may
still be selected after a power loss; only the post-recovery
(durability-verified) reclamation retires the old journals. (6)
**THE PLATFORM HONESTY**: `sync_parent_dir` is the Unix-real
directory fsync and the ratified Windows no-op (std exposes no
Windows directory flush) — the natural `Unresolved` failure is a
Unix-only phenomenon, the deterministic injection exercises the
fail-closed and repair machinery on ALL platforms, and NO Windows
directory-fsync guarantee is claimed.

**THE REGRESSIONS (the dir-sync pair rewritten to the R5
semantics + one new full-stack pair; every fault-armed scenario
in its own spawned child process):**
`a_pointer_dir_sync_failure_after_the_flip_fails_closed_until_reestablished`
(the child: the refused rotation with the typed
pointer-durability detail; the live-authority-changed assert; the
fail-closed publication/commit/deactivation refusals — no
authoritative T1 write while unresolved; the FAILING repair under
the still-armed fault with the state intact; the ARMED-FAULT
RECOVERY REFUSAL — a fresh recovery cannot silently clear the
unresolved condition; the disarmed repair re-opening the crossing
with the post-repair T1 evidence in the new authoritative
generation. The parent: the durable state — the new generation
carrying the post-repair evidence while the stale journal
receives nothing — the recovery verifying the durability and
resolving the same journal with the plans replayed exactly once;
the repeat rotation durably established);
`the_old_journals_retire_only_after_the_durability_is_recovered`
(the child over the two-checkpoint campaign: the refused rotation
under the armed fault; the CLOSED RECLAMATION refusing — the
prior journal may still be selected after a power loss, its
retirement waits; the repair and the new T1 evidence. The parent:
the prior root journal retained while the durability was
unresolved; the verified recovery with the plans replayed exactly
once; the reclamation retiring MK1's range AND the old root
journal; the active generation standing; the walk validating; the
nothing-eligible re-run). The R4 pre-flip manifest-failure and
post-flip swap-failure pairs PRESERVED unchanged in behavior (the
manifest pair's retry assertion updated to the
`PointerDurability::Established` form).

**The verification at `965aa5b`:** fmt and `clippy --workspace
--all-targets -- -D warnings` green; `cargo build --workspace`
green; the workspace **617 passed + 1 intentionally ignored, 0
failed** (the R4 corrective's 614 plus the 2 new test functions
plus the 4 child processes' own nested single-test reports in the
harness output — 613 distinct tests); the `pp_b3_rotation` matrix
10 passed 0 failed; the `pp_b3_reclaim` matrix 22 passed 0
failed; the checkpoint and membership matrices green unchanged;
the double release-mode selftest green both runs (identical
results modulo timings — the frozen Stage-0 fingerprint pins
hold, `engine-core` untouched). **The CI: run 40247187364 GREEN
on all three platforms, verified firsthand job-level over the
corrected head `965aa5b`** (ubuntu-latest 3m19s; macos-15 ARM64
4m2s; windows-latest 3m51s; via the PR #3 pull_request event —
the CI vehicle, never merged; the annotations are the known
runner-infrastructure notes). The protected branches verified
unchanged (main=5dbb377, stage5=047faf9, macos-port=ba395d8,
design=a47ef10, pp-b1=bfe60b7, pp-b2=00109b0); PRs #1/#2
unchanged, open/draft.

**The honest remaining limitations (stated exactly):**
- **The evidence distinction**: the deterministic directory-sync
  failure injection exercises the fail-closed and repair
  machinery on all three platforms; the natural `Unresolved`
  failure is a Unix-only phenomenon (the Windows sync is the
  ratified no-op — std exposes no Windows directory flush); NO
  physical power-loss test was performed or claimed. The
  process-crash evidence (exits 8–12) and this injection evidence
  are distinct from genuine power-loss guarantees.
- **The recovery-verification semantics**: the recovery's re-sync
  establishes the durability of whatever the pointer CURRENTLY
  names. If a power loss already reverted the pointer to the
  prior journal, the recovery establishes the reverted state and
  proceeds on the prior journal — the safe direction, because the
  fail-closed rule guarantees no new T1 writes were made to the
  new journal while the durability was unresolved (the only
  writes at risk never happened).
- The fault-injection statics are test-only (`#[doc(hidden)]`,
  process-local, never armed by engine code); the production
  paths carry no seams.

**The preserved invariants:** the R4 ordering and authority
rules; the R3 campaign isolation and conservative orphan
retention; the fully corrected F1–F4 and R1–R4 behaviors;
PP-B1/B2 behavior; the fingerprints and recovery contracts; the
ratified Windows durability-tier limitations.

**THE FINAL RENEWED STOP.** PP-B3's R5 corrective is delivered
and stopped for the renewed independent PASS/HOLD review. The
tested code SHA `965aa5b`; the CI 40247187364 green ×3 over it;
the workspace 617 passed + 1 intentionally ignored at it. The
next entry point: the renewed verdict or the owner's rig evidence
(§12). This record never declares the batch CLOSED. The physical
Windows acceptance is NOT yet requested. PP-B4, Stage 6, and all
further implementation remain unauthorized until separate
explicit owner GOs.

**SUPERSEDED — the owner's CLOSED-PASS ratification arrived: the
independent review issued IMPLEMENTATION PASS, the owner completed
the physical Windows acceptance, and the reviewer recommended
CLOSED-PASS. The final closure stands in §21.**

## §21 — The owner's CLOSED-PASS ratification: the final closure

**OWNER DECISION: PP-B3 IS APPROVED FOR CLOSED-PASS.** The
independent source-level review of the corrected implementation
issued **IMPLEMENTATION PASS**; the owner completed the physical
Windows release-mode acceptance; the independent reviewer examined
the resulting transcript and recommended **CLOSED-PASS**. This
section records the owner's explicit authorization to finalize
PP-B3's documentation-only closure. **There is no authorization
for further implementation work.**

**The authoritative baseline.** The accepted implementation SHA
`965aa5bcf93500c230a77449150227bd614c3a5e` (the R5 corrective
head); the physical Windows acceptance and documentation baseline
`cc056d07141c82145137148ca9f21834ea9bff97` (this branch's HEAD at
the acceptance); the independently verified three-platform CI run
**40247187364** (Ubuntu PASS / Windows PASS / macOS ARM64 PASS;
the workspace 617 passing reports — including the nested
child-process harness reports, NOT 617 distinct tests — with 1
intentionally ignored and 0 failures). All previously identified
implementation findings **F1–F4 and R1–R5 are CLOSED**. The
closure commit (this documentation-only commit) sits directly
above the unchanged accepted implementation in the ancestry.

**The physical Windows acceptance — owner verified.** The
evidence transcript
`PP-B3-Windows-Acceptance-20261025-212731.txt` (the owner's
machine, `C:\Users\Willp\AppData\Local\Temp\PP-B3-Windows-Acceptance-20261025-212731.txt`),
produced on Windows NT 10.0.26300.0 with Rust 1.99.1 / Cargo
1.99.1 at the branch HEAD `cc056d0`. All eight required
acceptance steps passed (EXIT 0): the release build; the full
release workspace run; the focused pp_b3_checkpoint (4 passed),
pp_b3_membership (4 passed), pp_b3_rotation (10 passed), and
pp_b3_reclaim (22 passed) suites; the persistence benchmark; and
the second full workspace run (the double selftest — the frozen
fingerprint pins), with the single previously documented
intentionally ignored SDL soak test.

**The acceptance-script correction (honest, preserved).** The
initial acceptance script's membership invocation targeted
`-p engine-slice --test pp_b3_membership`; cargo returned EXIT
101 because that target does not exist in that package. The owner
corrected the command to `-p engine-persist` (§12 above carries
the corrected form), resumed the same acceptance transcript, and
obtained EXIT 0 with all four membership tests passing. **This
was an acceptance-script error, not an engine failure** — the
first invocation is recorded here as the failed command it was,
never misrepresented as passing.

**The physical release performance and storage measurements (the
owner's transcript).** The storage sweep (the fixed-cadence
campaign at 150/600/1,200/2,400 ticks): before reclamation
1,021,074 / 1,655,458 / 2,493,357 / 4,184,159 B; after
reclamation 413,262 / 417,974 / 411,743 / 413,262 B — reductions
of 59.5% / 74.8% / 83.5% / 90.1%. The publication-cadence
sensitivity: 2 publications per range 1,655,458 → 417,974 B with
6 objects removed; 8 publications per range 6,534,373 →
1,605,054 B with 18 objects removed. The tested fixed-cadence
sweep demonstrates the intended bounded post-reclamation
footprint — NOT a constant-storage claim under arbitrary
checkpoint intervals or retention settings. The operation
latencies (milliseconds, n=5): the checkpoint publication min
16.869 / median 17.097 / p95 20.248 / max 20.248; the journal
rotation 22.568 / 22.878 / 23.280 / 23.280; the reclamation
15.536 / 15.825 / 15.841 / 15.841.

**The maintenance-operation interpretation (explicit).** These
are synchronous, explicit maintenance operations. Their latency
does not establish suitability for execution within gameplay
frames — the 60 FPS budget is approximately 16.667 ms, and the
checkpoint (17.097 ms median) and the rotation (22.878 ms
median) EXCEED it on this rig. PP-B4's scheduling/orchestration
work remains separately gated and unauthorized; the benchmark is
not an instruction to change the implementation.

**The benchmark-label clarification (honest, preserved).** The
executable contains a static benchmark description identifying
its measurements as "pod results, not physical-rig evidence" —
stale for this invocation. The acceptance transcript was produced
on the owner's physical Windows PC; the measurements above are
the owner's transcript's values, recorded by the owner and
examined by the independent review — not the implementer's own
execution. The benchmark source was NOT changed merely to fix
that wording (this closure is documentation-only).

**The remaining limitations (final).** The Windows durability
tier: the pointer directory sync is the ratified no-op (std
exposes no Windows directory flush) — no Windows
directory-fsync guarantee is claimed, and the physical
power-loss tier was NOT tested (the process-crash exits 8–12 and
the deterministic fault-injection evidence are distinct from
genuine power-loss guarantees — §19–§20). The rejected design
alternatives D-PP-4a and D-PP-4b remain rejected; the ratified
architecture unchanged.

**The final authorization boundaries and the freeze.** After the
closure commit: `production-persistence-b3` is FROZEN — no more
code or documentation commits without fresh owner authorization
(this does not authorize changing GitHub branch protection or
repository settings). PRs #1, #2, and #3 remain open, draft, and
unmerged (PR #3 is not marked ready). PP-B4, Stage 6, merges,
and all further implementation require separate explicit owner
authorization. This section is the authoritative current status;
every earlier HOLD, corrective, and renewed-STOP entry remains
historically accurate, superseded by this closure.
