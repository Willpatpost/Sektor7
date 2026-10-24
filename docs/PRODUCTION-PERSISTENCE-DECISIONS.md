# PRODUCTION PERSISTENCE — Decisions Ledger (Design-Only Phase)

> **Status:** The owner-decision register for `docs/PRODUCTION-PERSISTENCE-DESIGN.md` (the design-only production-persistence investigation, per the lead's GO 2026-10-23; the frozen baseline `047faf9`; the separate `production-persistence-design` branch). **Every entry is OPEN — awaiting the owner's approval.** No implementation GO is granted by this ledger or by the design document. Each entry records: the decision, the recommended default, the alternatives considered, the costs, the risks, and the evidence needed for approval. RATIFIED items are cited, never re-decided here.

---

## D-PP-1 — The canonical-medium storage provider (production)

- **The decision:** what backs the `CanonicalMedium` trait for production standalone campaigns.
- **Recommended default:** the **unified cross-platform SQLite-backed canonical medium** — one database per campaign (`objects`/`records`/`meta` tables; the canonical bytes verbatim as BLOBs, content-addressed by the same SHA-256), WAL + `synchronous=FULL`, every object required for an acknowledged revision committed in **one transaction** (objects + record + HEAD together; no external blobs), behind the **unchanged `CanonicalMedium` trait**. The `LocalFileMedium` retained permanently as the reference implementation, test oracle, and export path.
- **Basis:** the v1.8 §2.1/§5.3.3-E4 ratified SQLite as "the preferred production fallback" with exactly these requirements; the production gate it reserved has now fired — the Windows power-loss/OS-crash acknowledged-revision tier is not establishable from std (the N−1-valid/N-lost scenario), and the Stage 5 acceptance + the physical rig run are complete.
- **Alternatives considered:** (a) keep the custom medium everywhere — rejected for production (contradicts the v1.8's own acceptance rule for Windows standalone production); (b) the narrow Windows-specific module (`FILE_FLAG_BACKUP_SEMANTICS` directory flush / `MoveFileExW(MOVEFILE_WRITE_THROUGH)`) — **unproven** per the v1.8's evidence standard (documented handle acquisition ≠ documented flush durability; no authoritative documentation has surfaced), plus the engine's first platform-specific unsafe code and a two-medium durability story; documented as the fallback-of-the-fallback with that consequence stated.
- **Costs:** a new `engine-persist` dependency (`rusqlite` bundled — the C amalgamation via `cc`, statically linked); an additional native build path on CI/Windows/macOS (the SDL3/cmake lesson — audited as a PP-B1 gate); ~1–1.5 MB binary size; WAL/`-shm` sidecar files; a disciplined backup story (checkpoint before file copy; `VACUUM INTO` for online backup).
- **Risks:** the build-path maintenance on three platforms; dependency governance (a single well-known crate); SQLite's own semantics drift across versions (mitigated: pin the bundled amalgamation version; the engine's chain validation remains the semantic validator on top).
- **Evidence needed for approval:** the documented WAL/`synchronous=FULL` semantics (the abrupt-power-loss-safe setting; `NORMAL` does not meet the contract); the PP-B1 parity suite (the `ChainSummary`-identical reads of the same logical campaign through both mediums) + the three-platform build audit + the migration interruption matrix. **Status: OPEN.**

## D-PP-2 — The T1 journal's placement

- **The decision:** whether the durable T1 journal moves into the same database.
- **Recommended default:** **keep the journal as its own file medium.** It is already power-loss-honest on both platforms (single-file append-only, the WRITE-access handle so `sync_all` works on Windows, one fsync per completed transaction, the hash-chained torn-tail rules) — the Windows gap does not exist for it; the T1-before-T3 two-domain sequencing is by ratified design; the fold-in buys no required atomicity.
- **Alternatives considered:** folding the journal into the DB (a journal table; one DB transaction per journal commit preserving the one-fsync contract) — **DEFERRED** with the trigger: a separately authorized consolidation batch, only after the medium swap is proven.
- **Costs of the default:** two storage artifacts to back up/migrate (the operational rule: a backup omitting the journal is not a campaign backup).
- **Risks:** minimal — zero contract drift; the B3/B4 recovery composes the two domains by design.
- **Evidence needed for approval:** none beyond D-PP-1 (this is a scoping default within it). **Status: OPEN (a sub-decision of D-PP-1).**

## D-PP-3 — The compacted session save, `spine-history/2`

- **The decision:** the versioned save form that bounds the spine's session-embedded execution history.
- **Recommended default:** the **ratified v1.8 §5.6/§5.6.1 architecture made concrete**: the cut, the authenticated prefix summary (the SHA-256 chain over the discarded records, seeded from the prior compaction), the unfinished-index derivation (`executed == [0, next_seq) \ unfinished`; `known == [0, next_seq)`), the writer-computed carried counters, the ratified load-gate list, the compacted domains (`engine-core/spine-compacted/1`, `engine-core/state-compacted/1`) — **plus the consequence-lane aggregates** (the B4 extension the v1.8 predates): an identity's lane state is retained in full iff any outstanding obligation, pending T1 intent, or unmaterialized confirmed evidence references it; fully materialized-and-confirmed identities below the cut are summarized into the coverage boundary + aggregates, with the conservation gates extended to the lanes.
- **Basis:** [RATIFIED] the v1.8 Correction C architecture (complement-of-unfinished; the bitset alternative already rejected); [MEASURED] the ~31.0 B/execution linear growth the B5 sweep established.
- **Alternatives considered:** a stored executed bitset (rejected by the v1.8 erratum E3 — it grows one bit per ever-allocated id, unbounded); no compaction (rejected — the 10-hour extrapolation ~257 MB/snapshot with ~2 s serialization).
- **Costs:** a new versioned domain + the `HistoryKind` surface (the safe accessors `ledger_retained()`/`try_ledger_full()`/the `try_*` fingerprints — all specified by the ratified §5.6.1); the equivalence-test burden (the extended twin suite).
- **Risks:** the deliberate unavailability must stay honest (the historical fingerprints and per-record history below the cut are not reconstructible after reference reclamation — the truth table in DESIGN §4.2); the lane-aggregate extension is new design (not ratified) — it carries the PP-B2 equivalence burden specifically.
- **Evidence needed for approval:** the PP-B2 gates — every historical fingerprint byte-identical on `Full` instances; the twin-equivalence suite (the compacted image's continuation behavior identical to the uncompacted control, including the B4 recovery from a compacted anchor); the steady-state size/capture measurements confirming the projections. **Status: OPEN.**

## D-PP-4 — The retention/rotation protocol and its numerical defaults

- **The decision:** how the chain, journal, snapshots, and quarantine are bounded (the second lifetime-growth dimension).
- **Recommended default:** the **checkpoint/rotation/verify-before-reclaim protocol** (DESIGN §4.2): the checkpoint revisions carrying the compacted anchor + the chain-prefix digest (the compacted walk verifies the digest, then walks only the post-checkpoint span); the T1 journal rotation at checkpoints (the B4 "legitimate journal retirement" machinery as the existing hook); the seven-class eligibility rules (pending T1 never reclaimed; confirmed evidence until materialized in a **verified** anchor; applied identities with the prefix; frontiers never; the snapshot window per P0-4; ambiguous outcomes until resolved; corrupted evidence never); the two-phase idempotent reclamation with the audit ledger. **Numerical defaults are measured proposals, not inventions** (per the ratified P0-4): compact when `summarized_records` would exceed ~2× the active obligation count at a save point; a checkpoint every ~32 publications; the full-fidelity reference retained for the last few generations (equivalence testing); the snapshot window of prior anchors small and fixed.
- **Basis:** [RATIFIED] P0-4 (verify-before-reclaim; the only valid recovery path never deleted; defaults by measurement); [MEASURED] the growth rates (55,127 B per capture-bearing journal transaction; the publication growth 230 KB→3.5 MB at 150→4,800 ticks; ~10.5 ms/revision walk).
- **Alternatives considered:** indefinite retention (rejected — the unbounded chain/journal/snapshot growth); aggressive automatic reclamation (rejected — a reclamation is never automatic at first implementation; PP-B3 delivers it behind an explicit, logged operation).
- **Costs:** the protocol machinery (the eligibility marks, the reclamation ledger, the rotation); the honest communication that total storage is **not strictly bounded** — the confirmed consequence lane is deliberately linear at KB-class per consequence (~6 MB per 10-hour fixture-rate session, vs the bounded ~30 MB/h save-point rate; DESIGN §4.3's resolution).
- **Risks:** a wrong cut breaks a proven recovery guarantee (mitigated: the seven eligibility rules are each traceable to a ratified contract; the two-phase procedure is interruption-safe; the interruption matrices are PP-B3 gates).
- **Evidence needed for approval:** the PP-B3 gates — the interrupted-compaction/reclamation matrices; the long-campaign bounded-storage sweep (the steady state held over an order-of-magnitude duration increase); the recovery-from-checkpoint suites. **Status: OPEN.**

## D-PP-5 — The persistence orchestration (async I/O)

- **The decision:** how the calling-thread costs are removed from the frame path.
- **Recommended default:** the **single-writer-thread I/O orchestration** (DESIGN §5.2): the authority thread keeps the journal transactions + confirmations synchronous (the T1-before-T3 contracts) and performs the capture (the bounded post-compaction serialization; the clone-handoff deferred pending release-mode measurements); one dedicated worker thread owns the medium connection and executes the publication transactions FIFO from a bounded queue (backpressure = a typed busy result, the campaign continues, re-publication idempotent by content hash; cancellation drops queued-unstarted jobs only; a started transaction runs to completion); the graceful close drains + checkpoints; the worker `Unknown`s reconcile exactly as the synchronous path; unverifiable exclusive authority fails closed (the crossing HALTs authoritative progression). **No threading is introduced in this phase** — the worker is PP-B4's deliverable behind its own GO.
- **Basis:** [MEASURED] the rig's dev-profile costs (publication 57.750 ms — fits no frame budget; the journal 4.337 + confirmation 7.526 — fit 60 FPS only); [RATIFIED] P0-6 (measure-first synchronous implementation; the I/O-worker ownership boundaries designed — this is that design made concrete).
- **Alternatives considered:** full async (the journal + confirmations too) — rejected: the pipeline's verified-read-back confirmation and the T1 sequencing are ratified synchronous contracts; serialize-then-hand-off with a cloned spine — DEFERRED (the release-mode capture measurements are the trigger).
- **Costs:** one thread + the queue machinery; the completion-reporting surface; the mid-publication kill matrix (PP-B4).
- **Risks:** ordering/backpressure bugs (mitigated: one worker FIFO + the CAS + the idempotency keys; the ordering proofs are PP-B4 gates); the 144/240 FPS story remains gated on the release-mode re-measurement (no stronger claim is made).
- **Evidence needed for approval:** the PP-B4 gates — the mid-publication process-kill matrix; the ordering/monotonicity proofs; the release-mode frame-budget measurements on the rig. **Status: OPEN.**

## D-PP-6 — Migration, backup, and compatibility policy

- **The decision:** the safe path from the existing development saves to the production provider.
- **Recommended default:** the **staged, idempotent migration** (DESIGN §6.1): read the old store via its own verified walk (never modified) → content-addressed idempotent inserts → the `ChainSummary`-identical verification → the authority marker commits **last** (an interrupted migration is safely restartable and never authoritative) → the cutover is the caller's store-path change. Rollback: pre-cutover trivial; post-cutover the export path (the DB → a fresh file store via the retained reference writer) or the retained old store until advanced. Downgrade after advancement: **refused, with the export path documented** (never silent). Backups: `VACUUM INTO` (online, consistent); file-level copy only after a TRUNCATE checkpoint under quiescence; the journal file travels with the store. The original formats remain permanently as independently verifiable references.
- **Costs:** the migration tool + its interruption matrix (PP-B1); the operational backup discipline.
- **Risks:** a partial-migration authority confusion (mitigated: the marker-last rule + the open-time marker check); a live-copied WAL database (mitigated: the checkpoint discipline is documented and tooling-enforced).
- **Evidence needed for approval:** the migration interruption matrix (kills at every stage, old-or-new authority, never partial); the parity suite (D-PP-1's). **Status: OPEN.**

## D-PP-7 — Release-mode verification

- **The decision:** the empirical upgrade path for every performance claim.
- **Recommended default:** the **rig re-run at `--release`** (the benchmark distributions + the growth sweep + the lifecycle suites) before any claim in the design is upgraded; the dev/release distinction is explicit (the codec costs drop materially; the fsync costs do not).
- **Costs:** one rig session.
- **Risks:** none (it is pure evidence gathering).
- **Evidence needed for approval:** the recorded release-mode numbers (the owner's rig run, the same §7-of-B5 discipline). **Status: OPEN.**

## D-PP-8 — The stage-gated implementation plan

- **The decision:** the sequencing of the (separately authorized) implementation.
- **Recommended default:** **PP-B1 the SQLite medium → PP-B2 the spine compaction → PP-B3 the retention protocol → PP-B4 the orchestration** — dependency-driven (the medium first: everything composes over the seam; compaction second: it bounds what retention and orchestration optimize; retention third; orchestration last so it cannot mask correctness work). Each batch independently testable/reviewable with its own gates (DESIGN §8) and its own GO.
- **Alternatives considered:** compaction first (rejected: the equivalence suites are cleaner over a settled medium); orchestration earlier (rejected: it must not mask correctness work).
- **Costs:** four review cycles.
- **Risks:** none structural — the plan's whole point is independence.
- **Evidence needed for approval:** the owner's GO per batch. **Status: OPEN — no batch is started by the design phase.**

---

## Standing constraints (preserved throughout, from the GO and the ratified baseline)

No production source changes, new dependencies, save-format migrations, provider implementation, compaction, deletion/reclamation, or async implementation in this phase; `stage5`/`main`/`macos-port` untouched; the frozen baseline `047faf9`; the deps discipline (engine-core + sha2; SQLite would be an `engine-persist`-level addition behind D-PP-1's approval); every frozen fingerprint mechanically preserved (the medium and the save-domain extension sit beneath the fingerprinted layers or beside them on `Full` instances); the ratified v1.8 contracts (the outcome semantics, the two-contract recovery, the four-tier suppression, P0-1..P0-6) never re-opened — only built upon.

**Every entry OPEN. No implementation GO is granted. Stopping for independent architecture PASS/HOLD review.**
