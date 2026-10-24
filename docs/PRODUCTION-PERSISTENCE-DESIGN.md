# PRODUCTION PERSISTENCE — Architecture Design (Design-Only Phase)

> **Status:** A separate, DESIGN-ONLY production-persistence architecture investigation, per the lead's GO (2026-10-23): "Stage 5 implementation is CLOSED-PASS. Physical Windows acceptance has also passed at closure commit `047faf9`. Authorization: GO — a separate, design-only production-persistence architecture investigation." **No implementation GO is granted by this document.** This phase permits no production source-code changes, no new dependencies, no save-format migrations, no storage-provider implementation, no compaction, no deletion/reclamation, no asynchronous-persistence implementation, and no Stage 6 gameplay work.
>
> **Frozen reference baseline:** `047faf9` (the Stage 5 Batch 5 documentation-only CLOSED-PASS closure; the accepted implementation `0bf371e` beneath it). This document is committed to the separate `production-persistence-design` branch; `stage5`, `main`, and `macos-port` are untouched.
>
> **Evidence classes (every load-bearing claim below carries one):** **[RATIFIED]** — an existing accepted guarantee or design decision, cited to its source (the v1.8 persistence architecture `docs/STAGE5-PERSISTENCE-ARCHITECTURE-PROPOSAL.md` as ratified at T-16 through P0-1..P0-6 + Corrections A/B/C; the B1–B5 accepted implementations; `engine-persist`/`engine-slice` source at `047faf9`). **[PROPOSED]** — a new decision this document recommends, awaiting the owner's approval (the companion decision ledger `docs/PRODUCTION-PERSISTENCE-DECISIONS.md`). **[MEASURED]** — recorded empirical evidence (the Stage 5 physical Windows-rig run at `047faf9`, development profile, values relayed in the GO; the B5 pod growth sweep, overlayfs, explicitly non-authoritative). **[ASSUMED]** — a labeled assumption requiring verification before it may carry a claim. **[DEFERRED]** — explicitly not decided here, with the stated trigger.

## 0. Executive summary

Three production gaps stand between the accepted Stage 5 implementation and production persistence, each with a design answer in this document:

1. **The Windows durability boundary** [RATIFIED, v1.8 §5.3.1/§5.3.3-E4]: the custom `LocalFileMedium` on Windows carries `SyncTier::ProcessCrashOnly` from std — the N−1-valid/N-lost scenario (an acknowledged HEAD rename absent after OS crash/power loss while the previous chain stays valid and the loss is invisible to chain validation) is possible and unclaimed-against. The v1.8's own rule: "the Windows local medium is not fully accepted for production standalone campaigns until a justified abrupt-power-loss durability contract exists." **The design recommends the unified cross-platform SQLite-backed canonical medium (WAL + `synchronous=FULL`) behind the unchanged `CanonicalMedium` trait** (§2–§3) — the option the v1.8 already named "the preferred production fallback," now made concrete.
2. **Lifetime growth in two dimensions** [MEASURED, the B5 sweep]: the spine's execution ledger (~31.0 B serialized per execution) grows inside every session snapshot, and the canonical chain/journal/snapshots grow without a retention protocol. **The design makes the ratified v1.8 §5.6 compaction architecture concrete as the versioned `save/2` form plus the checkpoint/rotation/verify-before-reclaim retention protocol** (§4), resolving the bounded-total-storage question honestly (§4.3).
3. **Calling-thread persistence costs that grow with campaign duration** [MEASURED]: the serialization ~12.076 ms (dev) at the 4,800-tick / 3,492,240 B snapshot, past every frame budget at long durations. **The design proposes the single-writer-thread I/O orchestration** (§5) — capture on the authority thread (bounded by compaction), background publication, the confirmed boundary operations preserved.

The stage-gated implementation plan (§8, PP-B1..PP-B4) proposes four independently testable batches. **None is executed or authorized by this document.**

## 1. The measured baseline and the problem statement

### 1.1 The physical Windows-rig evidence [MEASURED — the lead's rig run at `047faf9`, development profile]

| Operation (the real 1000-NPC workload) | Rig (NTFS, dev profile) | Pod (overlayfs, B5 §4 — non-authoritative) |
|---|---|---|
| Snapshot publication (the B2 nine-step append incl. fsyncs) | median **57.750 ms**, p95 87.979 ms | median 152.9 ms, p95 240.9 ms |
| T1 journal transaction (the durable deactivation composite) | median **4.337 ms** | median 8.47 ms |
| Canonical confirmation (`commit_canonical`) | median **7.526 ms** | median 9.31 ms |
| Session recovery (`recover`) | median **25.905 ms** | median 64.6 ms |
| Serialization (the 4,800-tick session, 3,492,240 B) | **~12.076 ms** | 25.9 ms |

Frame budgets: 60 FPS = 16.667 ms, 144 FPS = 6.944 ms, 240 FPS = 4.167 ms per frame. **All rig numbers are the development profile** — the codec costs (serialization, and the serialization share inside publication/recovery) will drop materially under `--release`; §7 proposes the release-mode re-measurement before any performance claim is upgraded. The B5 calling-thread clarification stands [RATIFIED, the B5 record §4]: these latencies are the calling thread's synchronous time costs; boundary placement is the integration's call-site property, never a demonstrated frame-scheduling proof.

### 1.2 The growth evidence [MEASURED — the B5 growth sweep]

Linear at ~31.0 B per execution, ~23 executions/tick in the near-presence fixture: 150 ticks → 3,442 executed / 220,438 B session snapshot; 4,800 ticks → 108,984 / 3,492,240 B; the codec ~0.24 µs/record. The 10-hour fixture-rate extrapolation ≈ 8.3M executions ≈ 257 MB per snapshot with ~2 s serialization — an extrapolation from measured rates with stated occupancy uncertainty, never a guarantee [the B5 record §5, the accepted correction].

### 1.3 The two lifetime-growth dimensions (the GO's objective-2 framing)

- **Dimension 1 — the spine's session-embedded history:** `ledger: Vec<ExecutionRecord>` (one record appended per `execute_now`, serialized in full fidelity by the spine save — the B1 §5.2 full-fidelity contract), the in-memory `known`/`executed` membership structures (one entry per identity), and therefore every session snapshot. The bounded part is the active obligation state; the ledger is history.
- **Dimension 2 — the canonical revision chain, journal, snapshots/anchors, quarantine, and cold-validation costs:** one revision per publication and per confirmation batch; 55,127 B per capture-bearing T1 transaction [MEASURED]; each published snapshot object embeds the then-current history; the cold walk is O(retained revisions) (~10.5 ms/revision pod-measured; the rig's 4-revision-class walks correspondingly faster).

## 2. The storage-provider decision (objective 1)

### 2.1 The ratified starting position [RATIFIED]

The v1.8 §2.1 (Correction A) ratified: the custom file-backed medium with std OS-managed locking, **conditional on Batch 2 demonstrating correctness** — which it did (the B2 fault-injection matrix, competing processes, real child-process kills; CLOSED-PASS at `e36b122`). SQLite was ratified as **the identified fallback**, with the swap point behind the unchanged `CanonicalMedium` trait. The v1.8 §5.3.3-E4 named the known candidate gap exactly: **Windows directory-metadata durability** — and named SQLite "the preferred production fallback" with three requirements this design adopts verbatim: (1) canonical save bytes and revision-chain semantics remain ours (SQLite is the transactional/durability layer, never the format); (2) WAL with `synchronous=FULL` (FULL fsyncs on every commit — the abrupt-power-loss-safe setting; NORMAL does not meet the contract); (3) **every object required for an acknowledged canonical revision inside one transactionally consistent durability boundary** — the revision records, referenced snapshots/blobs, and the HEAD equivalent committed in the same transaction, because making HEAD durable inside SQLite does not make separately stored external blobs durable.

**The gate has now fired.** The Stage 5 acceptance is complete (B1–B5 CLOSED-PASS; the physical rig run passed at `047faf9`), and the Windows boundary stands exactly as documented: std cannot establish the power-loss/OS-crash acknowledged-revision tier on Windows (a read-only directory handle cannot `sync_all`; opening a directory for write requires platform-specific creation flags — v1.8 §5.3.1). The production question the v1.8 reserved "for the lead at the B2 gate" is now before the lead, with all the evidence the v1.8 asked for.

### 2.2 The three options

**Option A — keep the custom medium everywhere (accept the Windows tier).** Rejected for production: it contradicts the v1.8's own rule ("not fully accepted for production standalone campaigns until a justified abrupt-power-loss durability contract exists"). Development use remains fully supported — the `LocalFileMedium` is retained permanently as the reference implementation and test oracle (§6.3).

**Option B — the narrow Windows-specific durability module.** Open the campaign directory with `FILE_FLAG_BACKUP_SEMANTICS` + write access (not expressible through std; requires a Win32 binding dependency) and issue `FlushFileBuffers` on the directory handle around the HEAD rename; or `MoveFileExW(MOVEFILE_WRITE_THROUGH)` for the rename itself. **Unproven per the v1.8's own evidence standard [RATIFIED, §5.3.3-E4]:** Microsoft documents the flag for obtaining directory handles and documents `FlushFileBuffers` for file data, but does not document that flushing a directory handle establishes rename-metadata durability; the candidate was explicitly ruled "unproven until authoritative documentation or a defensible implementation establishes the guarantee — no claim is made from it." No authoritative documentation has surfaced. Even if proven, the costs: the engine's first platform-specific unsafe code, a Windows-only divergence in the durability story (two mediums to qualify, two failure models to test), and a power-loss claim that cannot be demonstrated on available hardware (the testing-honesty rule — process-kill tests verify the process-crash tier only). **Rejected as the primary recommendation; documented as the fallback-of-the-fallback** (it remains the zero-new-dependency option if the owner rejects SQLite — with the explicit consequence that the Windows power-loss claim rests on an unproven platform behavior).

**Option C — the SQLite-backed canonical medium (WAL + `synchronous=FULL`).** The documented semantics: in WAL mode with `synchronous=FULL`, the WAL is fsynced at every commit — acknowledged-transaction persistence across process crash, OS crash, and power loss per SQLite's documented contract, on all three target platforms. Decades of deployment; battle-tested crash recovery (the WAL replay); atomic multi-statement transactions (one BEGIN…COMMIT installs records + objects + HEAD together — the nine-step CAS's external coordination becomes SQLite's internal commit protocol); internal cross-platform locking; mature observability/repair tooling (the `sqlite3` CLI). Costs, honestly: a new native dependency (the bundled C amalgamation compiled via `cc` — an additional native build path on CI, Windows, and the macOS port; the SDL3/cmake lesson is the precedent that native build paths are a real maintenance cost, though `rusqlite` bundled is a single well-known crate, materially simpler than the SDL path); ~1–1.5 MB embedded runtime; page-level I/O we do not strictly need; the WAL/`-shm` sidecar files (managed by §3.5's checkpoint policy); backup discipline (§6.2 — a live WAL-mode DB is not safely file-copied without a checkpoint).

### 2.3 The comparison the GO asked for — unified SQLite vs Windows-specific

| Axis (the GO's list) | Unified SQLite (all platforms) | Windows-specific module (custom stays elsewhere) |
|---|---|---|
| Durability guarantees | One documented contract everywhere: WAL+FULL, acknowledged-persistence across power loss on Linux/Windows/macOS | Full POSIX on Unix (as today); Windows rests on the **unproven** directory-flush/rename-through behavior |
| Atomicity boundaries | One transaction = objects + records + HEAD (single linearization) | The nine-step CAS on every platform; Windows adds the unproven step-7 extension |
| Canonical-byte preservation | Ours, verbatim — BLOBs keyed by content hash; the chain, records, and formats unchanged | Ours, as today |
| Contention | SQLite's own writer serialization + WAL snapshot readers (audits without blocking the writer) | The HEAD.lock OS lock (working, proven in B2) |
| Cross-platform packaging | One dependency, one build path, static (no DLL redistribution) | Zero new deps, but a Win32 binding for the Windows path |
| Native dependencies | `rusqlite` bundled (C via `cc`) | A Win32 API binding (windows-sys class) on the Windows target |
| Performance | One WAL fsync per commit (vs the nine-step's multi-fsync sequence — the rig's 57.75 ms publication median is dominated by the current sequence; §7 projects the improvement, release-mode verified) | As today (the multi-fsync sequence remains) |
| Recovery | SQLite WAL replay + our chain validation on top (unchanged semantic validator) | As today |
| Backups | `VACUUM INTO` / the backup API (consistent, online) | File copy under the lock + quiescence |
| Migration | §6 (the tool; one target format) | §6 applies to the file store only on Unix |

**Recommendation [PROPOSED — D-PP-1]:** the **unified cross-platform SQLite-backed canonical medium**, as the v1.8's preferred fallback now made concrete, with `LocalFileMedium` retained permanently as the reference implementation and test oracle. One durability story, one backup story, one atomicity boundary, and the strongest available evidence class (documented + deployment-proven) behind exactly the tier the production contract needs. The Windows-specific module is documented as the fallback should the owner reject the dependency — with its unproven-guarantee consequence stated.

### 2.4 What does NOT change [RATIFIED — preserved by design]

The `CanonicalMedium` trait (the exact operation surface: `head`/`append`/`read_record`/`read_object`/`validate`/`reconcile_unknown`/`durability_tier`); the B3 `CommitPipeline`; the B4 `SessionCrossing` composition; the **T1 journal as its own file medium** [PROPOSED — D-PP-2: keep; see §3.7]; the canonical bytes and every versioned domain (B1 `save/1`, `revision-record/1`, `consequence-batch/2`, `deactivation-plan/2`, `region-director-save/1`, `npc-world-save/1`, `engine-slice/session-snapshot/1`); every frozen fingerprint (the medium is beneath every fingerprinted layer — the engine, state, lanes, presence, and save pins are mechanically preserved); the deps discipline (engine-core + sha2 — SQLite is an `engine-persist`-level addition requiring its own implementation GO).

## 3. The SQLite canonical medium — the detailed design [PROPOSED — D-PP-1]

### 3.1 The schema

One database file per campaign (e.g., `<campaign>/store.db` alongside the existing journal file — the campaign directory layout otherwise unchanged):

```text
meta(key TEXT PRIMARY KEY, value BLOB)        -- 'head' (the 32-B record hash or empty),
                                              -- 'authority' (the founding record's mode+campaign-id
                                              --   digest, set once at founding, never updated),
                                              -- 'provenance' (the migration marker, §6.1)
objects(hash BLOB(32) PRIMARY KEY, domain TEXT, bytes BLOB)
records(hash BLOB(32) PRIMARY KEY, bytes BLOB,   -- the canonical revision-record/1 bytes ARE the
        epoch INTEGER, parent BLOB(32),          -- record; hash = SHA-256(bytes); the lookup
        idem BLOB, kind INTEGER, tick INTEGER)   -- columns are derived from the decoded record
UNIQUE(idem) WHERE idem IS NOT NULL              -- the chain-wide idempotency-key uniqueness,
indexes: records(parent), records(epoch)         -- enforced by the database itself
```

**Canonical-byte preservation:** the `bytes` columns carry the exact canonical encodings verbatim — the content hashes are the same SHA-256 values the file medium computes; a migrated store's `ChainSummary` (head, height, founding, records, content_objects) must be identical (§6.1's equivalence gate). SQLite is the transactional/durability layer, never the format — the v1.8's requirement (1).

### 3.2 The append — one transaction, one linearization

`BEGIN IMMEDIATE` → read `meta.head` → compare with the caller's expected parent (the CAS; stale → `ROLLBACK`, `RejectedStaleParent { observed }`) → `INSERT OR IGNORE` every `StoredObject` (content-addressed idempotence: a retry inserts nothing; on an existing row, verify the stored bytes hash — a mismatch is the referenced-corruption fail-closed case) → insert the record row (the `UNIQUE(idem)` violation maps to the `duplicate-idempotency-key` chain rule) → update `meta.head` → `COMMIT`. **The linearization point is the `meta.head` update inside the committed transaction** — test-and-install cannot interleave; SQLite's own writer serialization (the `BEGIN IMMEDIATE` lock) is the `HEAD.lock` equivalent, OS-managed and process-death-safe. The outcome contract maps conservatively onto SQLite's commit semantics: an error at or after the first write operation in the transaction returns **`Unknown`** (never a guessed outcome — a commit error such as `SQLITE_FULL`/`SQLITE_IOERR` during the WAL sync is resolved by `reconcile_unknown`, exactly the B2 discipline: re-read head under a fresh `BEGIN IMMEDIATE`, `Installed { id }` iff head is the candidate or an ancestor with every referenced object hash-verifying, else `NotInstalled`); a `SQLITE_BUSY` after the busy-timeout maps to the CAS loss (`RejectedStaleParent` with the observed head after a read). **Every object required for an acknowledged canonical revision is inside the one transactional boundary** — the v1.8's requirement (3), satisfying the GO's explicit warning: no separately stored external blobs exist.

### 3.3 The durability tier and the platform boundary

WAL + `synchronous=FULL` + the documented commit semantics → `SyncTier` gains one additive variant (e.g., `TransactionalWalFull`): acknowledged-revision persistence across process crash, OS crash, and power loss, on all three platforms, per the documented SQLite contract (not a test claim — the testing-honesty rule applies identically: process-kill fault injection verifies the process-crash tier; the broader tier rests on documented semantics, now documented by a database engine whose core business is exactly this guarantee). `durability_tier()` returns it; `Accepted` carries it. The existing variants remain (the file medium's tiers unchanged). **The v1.8 §5.3.1 tier table gains a row, never a silent upgrade of the old ones.**

### 3.4 Validation, the trusted cursor, and readers

`validate()` walks the chain via the records table (head → parent links → the founding record; every record's `bytes` hash-verified; every `content_refs` object hash-verified; the chain rules enforced — epoch progression, parent coherence, founding presence, campaign-id coherence; the `UNIQUE(idem)` constraint double-checked by the walk for the migrated parity case). The §5.3.2 trusted-cursor model carries over unchanged in semantics: session-scoped in-memory derived state; O(1) per append (the new record's parent must equal the cursor's verified head); suffix-walk on divergence; sound invalidation forces the full walk; no persistent cache. **The audit mode improves materially:** a read-only connection takes a WAL snapshot — the full walk never blocks the writer (with the file medium, the audit walk holds the lock for its duration). Tooling and verification gain a non-interactive read path.

### 3.5 The WAL lifecycle and checkpoint policy

The `-wal`/`-shm` sidecars live beside `store.db`. In operation: SQLite's auto-checkpoint (the default passive 1000-page cadence). At the graceful session close and before any file-level copy, export, or backup that is not `VACUUM INTO`: a `TRUNCATE` checkpoint. **A live WAL-mode database is never file-copied without a checkpoint under quiescence** — the documented backup rule (§6.2).

### 3.6 Failure model and corruption

A corrupt page/structurally unreadable DB surfaces as typed Io/`CorruptHead`-class errors — fail closed, never silently repaired; the chain validation remains the semantic validator on top (a record/object hash mismatch is the same referenced-corruption fail-closed case as the file medium; evidence is preserved — with SQLite the "evidence" is the DB file itself, never modified in place by the engine outside transactions). The `sqlite3` CLI `.recover` tool is documented as an **owner-operated escape hatch, out of engine scope** — the engine never attempts automated repair. Quarantine as a storage-layer protocol disappears (there are no orphan files — an uncommitted transaction leaves nothing); the v1.8 §5.3.3 orphan semantics apply to the file medium, which remains the reference.

### 3.7 The T1 journal stays a file [PROPOSED — D-PP-2]

The journal is already power-loss-honest on both platforms [RATIFIED, the B3 implementation]: single-file append-only, the write handle opened with WRITE access so `sync_all` works on Windows (`FlushFileBuffers` — the B2 lesson), one fsync per completed transaction, hash-chained frames with the torn-tail/committed-corruption rules. No rename, no directory metadata, no multi-file coordination — the Windows gap does not exist for it. Folding the journal into the database (a journal table, one DB transaction per journal commit preserving the one-fsync-per-transaction contract under WAL+FULL) is **evaluated and DEFERRED**: the T1-before-T3 two-domain sequencing is by ratified design (the B3 contracts), the fold-in buys no required atomicity (the domains are deliberately separate), and it would grow the migration/qualification surface. Trigger for reconsideration: a separately authorized consolidation batch, only after the medium swap is proven.

### 3.8 Concurrency and ownership

The campaign authority is single-writer by the engine's ratified design (P0-1/the crossing's exclusive session); SQLite enforces it additionally at the DB level (`BEGIN IMMEDIATE` serializes writers process-wide — the same guarantee the `HEAD.lock` provides, by a different mechanism). The crossing's per-operation fresh-medium-handle pattern [source: `crossing.rs` opens `LocalFileMedium::open` per publication/recovery/commit] maps to one session connection; §5's orchestration assigns the connection's ownership.

### 3.9 Packaging and the build-cost audit

`rusqlite` with the `bundled` feature (the C amalgamation compiled via `cc`, statically linked — no DLL redistribution). **[ASSUMED — to be discharged in PP-B1]** the build cost on all three CI platforms + the macOS port (the SDL3/cmake lesson: the audit is a batch gate, not an afterthought) and the binary-size delta (~1–1.5 MB expected). The dependency is an `engine-persist` addition only; engine-core stays zero-dep [RATIFIED, P0-3].

## 4. Bounded execution history and retention (objective 2)

### 4.0 The foundation is ratified — this section makes it concrete

The v1.8 §5.6/§5.6.1/§5.6.2 (Correction C + errata E3) **ratified the compaction architecture**: the complement-of-unfinished executed representation (`executed == [0, next_seq) \ unfinished`, the unfinished-id index bounded by live work; the bitset alternative rejected — it grows one bit per ever-allocated id); the authenticated prefix summary (a running SHA-256 chain over the discarded records' canonical bytes, carried in the compacted anchor, chaining successive compactions, authenticating *what was discarded* — never reconstructing it); `HistoryKind::Full` vs `HistoryKind::Compacted { summarized_records, retained_records, prefix_summary }`; the safe accessors (`ledger_retained()`, `try_ledger_full()`, the `try_*` fingerprint variants, `history_kind()`); the compacted domains (`engine-core/spine-compacted/1`, `engine-core/state-compacted/1`); the exact load-time gate list; writer-trusted anchors; two independent compactions of the same history producing identical summaries; the P0-4 retention rules (verify-before-reclaim; the only valid recovery path never deleted; numerical defaults by measurement, never invented); the five storage classes with the honest scoping. **This design does not re-open any of it.** What follows specifies the production form the ratified architecture left at "designed now, implemented by measured evidence."

### 4.1 Dimension 1 — the compacted session save, `spine-history/2` [PROPOSED — D-PP-3]

A versioned save domain alongside `sektor7/save/1` (which remains the full-fidelity reference, byte-identical, forever readable):

- **The cut C** (a ledger record count): records `[0, C)` are summarized; `[C, executed)` are retained verbatim (the existing per-record encoding).
- **The authenticated prefix summary:** the SHA-256 chain over the `[0, C)` records' canonical bytes, seeded from the prior compaction's summary (the first compaction seeds from the domain tag) — the ratified derivation witness, chaining compactions end-to-end.
- **The unfinished-id index:** the retained obligations' identity set — already part of the serialized active state (queues/blocked/held carry the identities); the load derives `executed == [0, next_seq) \ unfinished` and `known == [0, next_seq)` structurally [RATIFIED, the dense-allocation invariant, asserted by the B1 gate].
- **The carried counters (writer-computed, anchor-authenticated):** `next_seq`, the Stats counters, `max_executed_index`, `summarized_records`, `retained_records` — verified at load by the ratified gate list (`summarized + retained == stats.executed`; `stats.executed == next_seq − unfinished_count`; the tail-record checks; conservation; clock coherence; dependency resolvability; `waiting_on` cross-check).
- **The consequence-lane aggregates (the B4 extension the v1.8 predates):** the B4 recovery builds its `RecoveryAnchor` counters from the ledger's per-identity pending/applied lanes and the CaptureLedger. Rule: **an identity's lane state is retained in full iff any outstanding obligation, pending T1 intent, or unmaterialized confirmed evidence references it; identities fully materialized and confirmed below the cut are summarized** — their materialized coverage folds into the anchor's coverage boundary and their counts into the aggregate counters. The conservation gates extend to the lanes (the per-identity aggregates must sum to the counters the B3 reconcile checks). The B4 recovery gains the compacted-anchor case: per-identity counters derive from the aggregates, not the per-record history — proven by the extended twin equivalence (§7.4).
- **The invariants preserved:** the frontier monotonicity (EventId/ConsequenceId frontiers carried); the no-cancellation invariant (enforced structurally by the conservation gate); the follow-up chains' schedule fidelity (`deactivation-plan/2` unchanged — the plan form is independent of the ledger's retention); every historical fingerprint on `Full` instances (all Stage 0–4 scenarios run uncompacted — the pins mechanically preserved).

**The capture-cost consequence [MEASURED → PROPOSED]:** the serialized size becomes O(active state + the retained window + carried counters) — the 150-tick scale (~220 KB) is the steady-state reference, independent of campaign duration; the serialization cost correspondingly bounded (§7 projects the release-mode number).

### 4.2 Dimension 2 — the retention protocol over the chain, journal, and snapshots [PROPOSED — D-PP-4]

- **The checkpoint revision:** a compaction publishes through the normal append (the v1.8: "a compaction IS a new snapshot revision on the chain") — a regular revision whose content object is the compacted session anchor (§4.1's form + the director state + the materialization evidence, the existing `session-snapshot/1` composition with the compacted spine section) **plus the chain-prefix digest**: the SHA-256 chain over the canonical record bytes of the walked prefix (the walker's own running hash, computed during validation). **The compacted walk mode:** founding → the latest checkpoint, verifying each prefix digest against the checkpoint's claim (the walker hashes as it walks — or, after reclamation, verifies the digest against the retained claim without the records), then the checkpoint → HEAD span walked as today. Cold-open cost: O(post-checkpoint revisions + the digest verifications) — bounded by the checkpoint interval. The audit mode (pre-reclamation) still walks every retained record.
- **The T1 journal rotation:** at each checkpoint, the compacted anchor carries the unfinished complement (the actionable-unconfirmed intents + the unmaterialized confirmed evidence — reconstructed by the B4 recovery's reconcile from the anchor + the journal); the journal file rotates (a fresh file; the B4 "legitimate journal retirement" machinery is the existing hook — the recovery's missing-journal path plus the anchor-carried state covers the rotation boundary). The retired journal file is reclaimable per the protocol below.
- **The verify-before-reclaim eligibility [the GO's list, each answered] — a class may be reclaimed only when its rule holds, and only after the new checkpoint anchor is fully persisted AND verified [RATIFIED, P0-4]:**
  1. **Pending T1 intents:** retained until confirmed-or-canceled (the cancellation record is the supersession authority) — a pending intent is never reclaimed.
  2. **Confirmed evidence (the consequence-batch objects + confirming revisions):** retained until the corresponding state is materialized in a **verified** checkpoint anchor (the anchor's coverage subsumes the confirmed identities) — then the *prefix copies* below the checkpoint are reclaimable; the confirming revision records themselves follow rule 5's class.
  3. **Applied identities:** the chain-tier suppression evidence (the confirming batch's per-key records) — reclaimable with the prefix once the checkpoint anchor's coverage subsumes them; the coverage boundary + the aggregates carry the suppression authority forward (the B3 four-tier suppression consults materialized coverage — tier 3 — which the anchor provides).
  4. **Monotonic frontiers:** carried in every anchor (`next_seq`, the episode/identity frontiers) — never reclaimed.
  5. **Recoverable snapshots:** the current anchor + the policy window of prior anchors; the P0-4 rule — the new anchor verified before the older become eligible; the window bounds class 3 of the five storage classes.
  6. **Ambiguous outcomes (an unresolved `Unknown` append):** the evidence is never reclaimed while unresolved; resolution (the reconcile) precedes eligibility.
  7. **Corrupted historical evidence:** quarantined/retained forever — never reclaimed (the fail-closed evidence; the v1.8 §5.3.3 rules).
- **The reclamation procedure (two-phase, interruption-safe):** (1) the eligibility mark — a reclamation-ledger revision (a normal chain revision recording what becomes eligible: the digests, the class, the cut) — itself verified before anything is touched; (2) the reclaim — the DB deletes (the prefix records/objects), the retired journal files, the superseded snapshot objects — idempotent by content addressing (a re-run deletes nothing already gone). Interruption anywhere: the anchor/HEAD state is unchanged by reclamation (the chain below a checkpoint is referenced only by digests after the eligibility mark); the reclaim is restartable. **A reclamation is never automatic at first implementation** — PP-B3 delivers it behind an explicit, logged operation (the owner-invoked or policy-invoked with the reclamation ledger as the audit trail).
- **What remains reconstructible vs deliberately unavailable after reclamation (the truth table):** *Reconstructible:* the active state in full; the frontiers; the per-identity aggregates and coverage boundaries; the retained window's per-record history; the authenticated digests of everything reclaimed (the prefix summaries and chain digests — verifiable claims about *what* was discarded, never its content); the confirmed-lane consequence records per rule 5's scoping. *Deliberately unavailable:* the per-execution records below the cut (which NPC, when, which schedule-kind); the historical `engine_fingerprint`/`lane_fingerprint` of pre-cut states; the full `ledger()` below the cut; the pre-checkpoint chain records' bytes (post-reclamation — the digest stands in); any audit that requires the original record bytes. The design never silently returns partial history as complete [RATIFIED, §5.6.2's retention-truth rule].

### 4.3 The bounded-total-storage resolution (the GO's explicit conflict)

The five v1.8 storage classes, scoped honestly under this design:

| Class | Under this design | Basis |
|---|---|---|
| 1. Execution-history storage | **Bounded** — allocation-independent (complement-of-unfinished; the prefix summarized; fixed-size counters only) | [RATIFIED, §5.6] + §4.1 |
| 2. Active-simulation indexes | **Bounded by live work** | [RATIFIED, §5.6] |
| 3. Snapshot/anchor retention | **Bounded by the retention window** (the policy defaults, §7.3) | [PROPOSED] |
| 4. Canonical revision-history retention (the confirmed lane) | **Linear in confirmed consequence count, at KB-class per consequence — deliberately unbounded** | [PROPOSED — the resolution below] |
| 5. Full-walk cost | **Bounded by the checkpoint interval** (the compacted walk) | [PROPOSED] |

**The conflict resolved explicitly:** indefinite confirmed-record retention and strictly bounded total storage are incompatible, and the design does not claim otherwise. The B3 contracts (no false canonical confirmation; the two-contract recovery; the chain-tier suppression) **require** the confirmed per-key evidence to remain durable — summarizing it away after materialization would break the ratified suppression/recovery guarantees, and is rejected. What compaction bounds is the *per-execution MB-class* history (the 31 B/execution ledger, the old full snapshots, the journal frames, the chain prefix). What remains linear is the *per-consequence KB-class* confirmed lane — quantified from the measured evidence [MEASURED]: the capture-bearing journal transaction is 55,127 B (**reclaimable** at rotation — not part of the steady state); the durable confirmed-lane cost per capture-bearing deactivation is the consequence-batch object (~125 keys × ~60–80 B ≈ 8–10 KB) plus one ~200 B revision record; the tour fixture's ~11 deactivations per 600 ticks ≈ one per ~55 ticks → at the 10 Hz demo cadence ≈ **~1.5–1.8 KB/s ≈ ~6 MB per 10-hour session** (fixture-rate extrapolation with stated uncertainty). **The honest production claim: total storage = O(bounded classes) + O(confirmed consequences × KB)** — per-execution history bounded; confirmed evidence linear at a rate ~three orders of magnitude below the unbounded ledger's. Any stronger claim is expressly not made.

## 5. Gameplay-safe persistence orchestration (objective 3) [PROPOSED — D-PP-5]

### 5.1 The measured cost structure and the frame budgets

At the rig's dev-profile numbers: the journal transaction (4.337 ms) and the confirmation (7.526 ms) each fit a 60 FPS frame budget (16.667 ms) but exceed 144/240; the publication (57.750 ms) fits none; the serialization (~12.076 ms at 4,800 ticks) fits 60 only at short durations and grows linearly. **With §4's compaction the serialization is bounded** (O(active + window) — the steady-state scale); **with the background publication (below) the fsync sequence leaves the calling thread**; the journal transaction and the confirmation remain synchronous boundary costs by design (the T1-before-T3 sequencing; the pipeline's verified read-back). The 144/240 FPS targets then hinge on: the release-mode re-measurement (§7.2) and the boundary-operation cadence policy (the confirmation occurs per deactivation boundary, not per frame — its amortized per-frame cost is the deactivation rate × 7.5 ms; at the fixture's ~one per 55 ticks ≈ 0.14 ms/tick amortized). **No frame-responsiveness claim is made beyond the measured numbers; §7.2's release-mode run is the gate for any stronger statement.**

### 5.2 The single-writer-thread I/O orchestration (the P0-6 "I/O-worker ownership boundaries designed," made concrete)

- **The authority thread (the simulation's single writer):** runs the ticks; performs the T1 journal transactions (4.337 ms measured — synchronous, on-thread); performs the canonical confirmations (7.526 ms — synchronous, on-thread; the pipeline's contract unchanged); performs the **capture** — serializes the session bytes (the bounded post-compaction cost; v1 keeps serialization on the authority thread; the clone-handoff alternative — copy the spine, serialize on the worker — is **DEFERRED** pending the release-mode capture measurements) — and hands the immutable byte buffer to the worker as a publication job.
- **The I/O worker (one dedicated thread, owns the campaign's medium connection):** executes the publication transactions (the §3.2 append — objects + record + HEAD in one commit) FIFO from a **bounded queue**; reports completions (and `Unknown`s) back to the authority thread.
- **Backpressure:** the queue is bounded (a small fixed depth). Full → the boundary operation returns a typed busy result; **the campaign continues** — the publication is retryable by construction (the content-hash idempotency key makes re-publication a no-op at the chain level); no simulation state is ever held hostage by the queue.
- **Cancellation:** queued-but-unstarted jobs may be dropped (a superseded snapshot publication); a started transaction runs to completion (atomicity — the GO's ordering requirement); the idempotency key absorbs any retry overlap.
- **Ordering:** one worker, FIFO — the monotonic canonical revision ordering is preserved by construction; the queue depth bounds out-of-orderness visibility.
- **Shutdown:** the graceful close drains the queue, checkpoints (TRUNCATE), and exits; the crash case leaves exactly the committed-transaction state — the existing recovery (unchanged, §5.4).
- **Recovery and error handling:** a worker `Unknown` is reconciled exactly as the synchronous path (§3.2's reconcile under the worker's connection — never a guessed outcome); a medium that cannot establish exclusive authority (a persistent `SQLITE_BUSY` from a foreign writer, a corrupt DB) fails closed — the crossing HALTs authoritative progression, the ratified contract.
- **The preserved invariants (the GO's list, each mapped):** exclusive campaign authority (the worker is the only medium toucher; the engine's single-writer discipline + SQLite's serialization); no false canonical confirmation (the confirmation stays synchronous with the verified read-back — the pipeline contract untouched); correct pending-save status (the busy/dropped/queued states are typed and surfaced, never conflated with published); monotonic canonical revision ordering (FIFO + the CAS); **no campaign-clock advancement from save completion** (the queue is fully decoupled from the tick loop — a publication completing late advances nothing; the ratified time contract preserved); gating of irreversible dependent actions (the T1/confirmation path is untouched and synchronous); fail-closed on unverifiable exclusive authority (above).
- **Scope discipline:** this section is design-only. **No threading is introduced in production code by this phase** (the GO's rule); the worker is PP-B4's deliverable behind its own GO.

## 6. Migration, backup, and compatibility (objective 4) [PROPOSED — D-PP-6]

### 6.1 The staged, idempotent migration (development saves → the SQLite store)

1. **Read** the existing store via the `LocalFileMedium`'s own verified walk (`validate()` — the founding→HEAD walk, every record and object hash-verified; the old store is **never modified**).
2. **Write** into the target DB: the objects and records as content-addressed idempotent inserts (restartable; a re-run inserts nothing new); the chain rules verified as each record lands (the same checks the append performs).
3. **Verify:** the DB's `validate()` must produce the **identical `ChainSummary`** (head, height, founding, authority, campaign_id, records, content_objects) — the equivalence gate.
4. **Cut over:** the authority marker — a final transaction writing `meta.provenance` (the source medium, the source path, the migrated head, the timestamp) — commits **last**. An interrupted migration (any stage) leaves a partial DB with no marker: safely restartable, and never authoritative (the session open checks the marker).
5. **The cutover itself** is the caller's store-path parameter change (the crossing opens the DB instead of the directory); the marker is verified at open. **Rollback:** pre-cutover — trivial (the old store is intact and authoritative); post-cutover — the export path (§6.3) rebuilds a file store from the DB, or the retained old store serves until the DB advances beyond it. **Downgrade policy:** after the DB advances, downgrade is refused with the export path documented (the explicit, non-silent rule).
6. **Interruption at every stage** is covered by the design's idempotence + the marker-last rule; PP-B1's fault injection kills the migration at each stage and asserts old-or-new, never partial authority.

### 6.2 Backup and export

- **The consistent online backup:** `VACUUM INTO` (a transactionally consistent single-file copy without stopping the writer) — the documented SQLite mechanism. The file-level copy of a WAL-mode DB is safe **only** after a TRUNCATE checkpoint under quiescence (the graceful-close discipline, §3.5).
- **Export/import for tooling and downgrade:** the DB → a fresh `LocalFileMedium` store (the reference writer — mechanical; both formats are ours and content-identical); the reverse is the §6.1 migration.
- **The journal file** travels with the store (it is the separate T1 domain; a backup that omits it is not a campaign backup — the design states this operational rule).

### 6.3 The original formats as independently verifiable references

The `LocalFileMedium` (reader and writer) is **retained permanently** — the reference implementation, the test oracle (the PP-batch equivalence suites run the same logical campaign through both mediums and compare `ChainSummary`s and recovered states), and the export/downgrade path. The B1 wire format, `revision-record/1`, and every versioned domain are unchanged by the provider swap; every frozen fingerprint is beneath the seam and mechanically preserved.

## 7. Measurements and acceptance gates (objective 5)

### 7.1 The empirical baseline [MEASURED]

§1.1's table (the rig at `047faf9`, dev profile) + the B5 growth sweep (§1.2). These are the design's only measured numbers; everything below is projection labeled as such.

### 7.2 Release-mode verification [PROPOSED — D-PP-7]

The rig re-run at `--release`: the `--benchmark-persistence` distributions + the growth sweep + the lifecycle suites. **The dev/release distinction is explicit:** the serialization and its share of publication/recovery are the codec costs most affected; the fsync costs are not. No performance claim in this document is upgraded until this run records its numbers.

### 7.3 Cost projections and proposed targets (uncertainty stated; from the measured rates)

- **Snapshot size, steady state (with §4.1 compaction):** the ~220 KB active-state scale (the 150-tick measurement) + the retained window (the proposed default: compact when `summarized_records` would exceed ~2× the active obligation count at a save point — the window then ~O(active); the exact default is a measured proposal for the owner, per P0-4's no-invention rule) → **projected steady-state snapshot ≈ 250–400 KB**, vs the unbounded 257 MB at 10 h fixture-rate.
- **Disk rate at a save-point policy:** (the snapshot + the revision + the batch) × the cadence — e.g., at one publication per minute: ~0.5 MB/min ≈ 30 MB/h bounded, plus the confirmed lane's ~6 MB/10 h [§4.3's arithmetic] + the journal's reclaimable 55 KB per capture-bearing transaction.
- **Cold open:** O(post-checkpoint revisions) + the anchor load — with the proposed checkpoint interval (every ~32 publications, a measured proposal): tens of revisions walked + the bounded anchor deserialization — projected well under the rig's current 25.905 ms recovery class; the release-mode run is the gate.
- **Recovery:** the anchor load + the journal scan (bounded by rotation) + the reconcile — same class.
- **Frame budgets:** §5.1's analysis; the strong claims gated on §7.2.
- **Storage capacity:** §4.3's resolution — the bounded classes + the linear-KB confirmed lane.

### 7.4 The acceptance gates for the future implementation batches (each independently testable)

- **Deterministic equivalence:** the twin regressions extended — an uncompacted control and its compacted image produce identical continuation behavior (scheduling decisions, executions, conservation, watermark values; the compacted domains agree; the B4 recovery from a compacted anchor reaches the twin's state) — the v1.8 §5.6 P0-2 protocol, extended to the consequence lanes.
- **Format compatibility:** `save/2` readers reject `save/1`-as-`save/2` and vice versa by version; the checkpoint revisions walk-validated in both modes; the SQLite and file mediums read the same logical store (the `ChainSummary` parity).
- **Fault injection:** the interrupted compaction at every step (the v1.8's orphan/orphan-equivalent rules — with SQLite, the unpublished anchor is an uncommitted transaction); the interrupted migration at every stage; the interrupted reclamation; the real process termination mid-async-publication (the committed-or-not state + the reconcile); the DB-commit failure injection (disk-full-class) mapping to `Unknown`.
- **Real process termination:** the b3/b4/b5 process-kill suites over the SQLite medium.
- **Cross-platform validation:** the three CI platforms + the macOS port + the build-cost audit; the rig's release-mode run.
- **Performance:** the §7.2 release-mode distributions vs the §7.3 targets — evidence, not gates, until the owner sets thresholds.

## 8. The stage-gated implementation plan [PROPOSED — D-PP-8; NOT EXECUTED]

Four batches, each independently testable and reviewable, each behind its own GO; the sequencing is dependency-driven (the medium first — everything else composes over the seam; compaction second — it bounds what the retention and orchestration optimize; retention third; orchestration last — it must not mask correctness work):

- **PP-B1 — the SQLite canonical medium.** The schema, the append/validate/reconcile implementations behind the unchanged trait, the `TransactionalWalFull` tier, the migration tool (§6.1), the WAL/checkpoint policy. Gates: the full B2/B3/B4/B5 suites green over the SQLite medium; the `ChainSummary` parity vs the file medium on identical campaigns; the migration interruption matrix; the three-platform build audit; the rig release-mode baseline (§7.2). No behavior change over the file medium — the swap is beneath every fingerprinted layer.
- **PP-B2 — the spine compaction (`spine-history/2`).** `HistoryKind`, the unfinished-index derivation, the prefix summary, the consequence-lane aggregates, the compacted domains, the load gates. Gates: every historical fingerprint byte-identical on `Full` instances (all existing pins); the extended twin-equivalence suite (§7.4); the compacted steady-state size/capture measurements (§7.3's projections checked).
- **PP-B3 — the retention protocol.** The checkpoint revisions + the compacted walk mode, the journal rotation, the verify-before-reclaim eligibility, the reclamation ledger + the two-phase procedure. Gates: the interrupted-compaction/reclamation matrices; the long-campaign bounded-storage sweep (the §4.3 claim demonstrated: the steady state held over an order-of-magnitude duration increase); the recovery-from-checkpoint suites.
- **PP-B4 — the orchestration.** The I/O worker, the bounded queue/backpressure/cancellation, the shutdown drain, the worker `Unknown` reconciliation. Gates: the mid-publication process-kill matrix; the ordering/monotonicity proofs; the release-mode frame-budget measurements on the rig; the invariants mapping (§5.2) demonstrated by regression.

**Nothing in this plan is started by this document.** Each batch's GO is the owner's; the DECISIONS ledger (the companion document) records what each approval needs.

## 9. The evidence-class register (the summary)

- **RATIFIED (cited, not re-opened):** the `CanonicalMedium` trait and outcome semantics; the nine-step CAS and its file-medium tiers; the T1 journal's four-state discipline and one-fsync contract; the two-contract recovery and the four-tier suppression; the v1.8 §5.6 compaction architecture (complement-of-unfinished, the prefix summary, `HistoryKind`, the load gates, P0-4's retention rules); the five storage classes' scoping; the time contract (no clock advancement from save completion); the single-writer campaign authority; every frozen fingerprint.
- **PROPOSED (this document, awaiting approval — see `docs/PRODUCTION-PERSISTENCE-DECISIONS.md`):** D-PP-1 the unified SQLite medium (WAL+FULL, everything-in-one-transaction); D-PP-2 the journal stays a file; D-PP-3 `spine-history/2` with the consequence-lane aggregates; D-PP-4 the checkpoint/rotation/verify-before-reclaim protocol + the retention defaults; D-PP-5 the single-writer-thread orchestration; D-PP-6 the migration/backup policy; D-PP-7 the release-mode verification; D-PP-8 the PP-B1..B4 plan.
- **MEASURED:** §1.1's rig table (dev profile); the B5 growth sweep and rates; the journal's 55,127 B per capture-bearing transaction; the consequence-batch arithmetic inputs.
- **ASSUMED (labeled, with discharge paths):** the SQLite build cost on the three platforms + the macOS port (PP-B1's audit); the binary-size delta; the release-mode codec improvements (§7.2's run).
- **DEFERRED (with triggers):** folding the T1 journal into the DB (a post-swap consolidation batch); the clone-handoff capture (the release-mode capture measurements); any Windows-specific module (only if the owner rejects SQLite — with the unproven-guarantee consequence stated); all numerical retention defaults beyond the measured proposals (the owner's approval per P0-4).

---

**This document is design-only. No implementation GO is granted. Stopping for independent architecture PASS/HOLD review.**
