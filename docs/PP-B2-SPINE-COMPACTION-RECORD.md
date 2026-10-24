# PP-B2 — The Spine History Compaction Implementation Record

> **Status: PP-B2 DELIVERED — stopped for independent review (2026-10-24).** The owner's GO (received 2026-10-24, the directive in the session attachment) authorized **Spine History Compaction (`spine-history/2`) under D-PP-3/D-PP-8, PP-B2 only** — the session-embedded spine history compaction plus the MANDATORY B4 consequence-lane extension. Delivered on `production-persistence-b2` (created from the frozen PP-B1 closure `bfe60b7`, no merges, no protected-branch modifications): the E3 complement-of-unfinished executed-membership representation; the `HistoryKind`/`Spine::compact`/`sektor7/save/2` form with the authenticated SHA-256 prefix summary and the complete ratified load-gate list; the safe accessors with the loud full-history APIs; the consequence-lane summary (the per-region eligibility boundary + the conservation aggregates, the in-band v4 codec with its fail-closed gates); the explicit `compact_session` driver; the recovery's coverage synthesis with the composition gate; the two test matrices (10 + 6 green); and the measurement section. **The cross-platform CI run `40129091546` is independently verified green (Ubuntu, Windows, macOS ARM64 — job-level, firsthand).** The batch stands for the **independent PP-B2 PASS/HOLD review**. PP-B3/B4/Stage 6 are NOT begun. The design documents on `production-persistence-design` remain binding **by commit** (never merged); this record cites them, never replaces them.
>
> **Verification summary:** **549 tests + 1 intentionally ignored, 0 failed** (the accepted PP-B1 baseline 529 + 1, every one still green, plus PP-B2's 20 new: 3 SHA-256 NIST vectors, the internal record-layout pin, the 10-test `spine_history` matrix, the 6-test `pp_b2_compaction` matrix). `cargo fmt --all -- --check`, `cargo clippy --workspace --all-targets -- -D warnings`, `cargo build --workspace` green at every commit. **Every historical fingerprint byte-identical, double-run, at every commit**: the selftest `engine=6504123a47cab66f` / `state=937bd3b6f56f07e0` — the full-fidelity encodings and every Full-instance behavior untouched throughout. The benchmark (including the new compaction section) ran green end-to-end twice on the pod. The CI run `40129091546` (https://github.com/Willpatpost/Sektor7/actions/runs/40129091546) green on all three platforms, verified firsthand job-level at the code head `abdeffe` (this record commit is documentation-only and triggers no further run — the `pull_request` path filter is `engine/**`).

## 1. The scope, the branch, the commit chain

Delivered on `production-persistence-b2` from the exact frozen PP-B1 closure `bfe60b7`, **no merges, no protected-branch modifications** (`stage5` = `047faf9`, `main` = `5dbb377`, `macos-port` = `ba395d8`, `production-persistence-design` untouched — verified at delivery):

- **16496cf** — the E3 complement-of-unfinished executed-membership representation (the live `known`/`executed` per-identity sets replaced by the frontier bounds check + the unfinished-id index).
- **fa5d3f3** — `spine-history/2` in engine-core (the internal SHA-256, `HistoryKind`, `Spine::compact`, the save/2 codec + the load gates, the safe accessors + the compacted fingerprint domains, the 10-test matrix).
- **191ad79** — the B4 consequence-lane extension + the compacted session publication/recovery (the lane summary + the v4 codec + the gates, `compact_session`, the anchor synthesis + the composition gate, the 6-test matrix).
- **abdeffe** — the compaction measurement section (benchmark section 7).
- **(this commit)** — the record, documentation only.

## 2. The E3 executed-membership representation (commit 1)

The ratified v1.8 §5.6 erratum selection, delivered first as the foundation: under the proven dense-allocation invariant, executed-membership is **derived** — a seq is executed iff `seq < next_seq` and not in the unfinished-id index — so the live spine's lifetime-growing `known`/`executed` BTreeSets are replaced by a frontier bounds check plus an index bounded by live work. The maintenance sites: inserts at allocation, removes at execution (before the handler runs — the old insert's exact sequencing) and at the reconstruction's removals; suspension/resumption/reclassification never touch the index. The save/load path now materializes **no per-identity sets** — a restored spine's memory scales with live work, never with lifetime executions. Fingerprint-invisible (no domain ever hashed either set — verified against every fingerprint's field list). The proof: the entire PP-B1 baseline (529 + 1) green unchanged through the representation swap, and the `e3_executed_membership_matches_the_ledger_ground_truth` regression in the new matrix.

## 3. The SHA-256 primitive

An internal, zero-dependency FIPS 180-4 implementation (`engine-core/src/sha256.rs`, `pub(crate)`), pinned against the published NIST vectors (the empty/abc/56-byte/two-block and one-million-'a' vectors) plus every streaming split. **engine-core stays zero-dep under the ratified P0-3 discipline** — the authenticated-summary primitive lives in-crate rather than as a dependency. Key lesson recorded: the 56-byte padding edge (a buffered message already past byte 56 spills the 0x80 into a second block).

## 4. The `spine-history/2` representation (commit 2)

**`HistoryKind`**: `Full` retains every `ExecutionRecord`; `Compacted { summarized_records, retained_records, prefix_summary }` replaces the first `summarized_records` records with the authenticated SHA-256 prefix chain and retains the tail verbatim — a derivation witness authenticating *what was discarded*, never a reconstruction of it. The spine carries `compacted: Option<CompactedHistory>` privately; `retained_records` is always the live ledger length.

**`Spine::compact(retain)`** — the explicit, versioned operation, never a silent default: the first compaction seeds the chain from the spine-history domain tag; a repeated compaction chains from the prior summary (successive compactions authenticate end-to-end); the per-record bytes hashed are the exact save encoding (pinned byte-for-byte by the internal `record_wire_bytes` test); two independent compactions of the same history produce identical representations (asserted). The typed refusals: `NothingToSummarize` (a compaction that summarizes nothing is not a compaction — the forms stay honestly disjoint) and `RetainExceedsLedger` (an exact window, never a clamped one).

**The save form** `sektor7/save/2` + version 2 + the `engine-core/spine-history/2` spine section: the authoritative active state in the full form's exact encoding (clock through held), then the **carried derived max** (the pre-cut records are summarized, so the load cannot derive it from the tail), the 32-byte prefix summary, the cumulative summarized count, and the retained tail in the full form's per-record encoding. The restore dispatches on the outer domain tag; each form rejects the other's version — never silently reinterpreted.

**The load gates** (the complete ratified list, fail-closed): the shared §5.2 gates (config bounds, identity uniqueness and frontier bounds across every unfinished structure, dense allocation, the structural conservation `executed == next_seq − unfinished`, the stated conservation identity, counter bounds, coherent blocked state) plus the compacted gates — empty-summary, the count identity (`summarized + retained == executed`), the tail's frontier/disjointness/clock-coherence/late-flag/order (the shared per-record checks — one code path for both forms), the late-count bound (the tail's late count is a lower bound on the statistic), and the carried max's coherence (present, at-or-above the tail's maximum intended due, at-or-below the clock).

## 5. The accessor and fingerprint contracts (commit 2)

`ledger()` and the four ledger-hashing fingerprints (`engine`/`lane`/`state`/`lane_state`) keep their exact full-history meaning everywhere and surface a compacted instance **loudly** — a panic naming the safe path, never a silently misleading value. The safe paths lead: `history_kind()`, `ledger_retained()` (the explicitly named tail accessor), `try_ledger_full()`, and the `try_*` fingerprint `Option` variants. The compacted domains `engine-core/spine-compacted/1` and `engine-core/state-compacted/1` hash the domain tag, clock, `next_seq`, budget, near-reserve, every Stats counter, both lane queues (canonical order, the lane-aware encoding), blocked, held, the prefix-summary bytes, `summarized_records`, `retained_records`, the retained tail records, and a SHA-256 digest of the unfinished-id index — two compacted instances agree iff the same logical state **and** the same cut; an uncompacted instance and its compacted image never compare through them (equivalence is behavioral); a Full instance surfaces the compacted-domain calls loudly (the symmetric discipline). `held_fingerprint` hashes no ledger and is valid on both.

## 6. The consequence-lane extension (commit 3)

**The lane summary** (`region.rs`): `CaptureLedger` gains `summarized: Option<LaneSummary { boundary: [u64; REGION_COUNT], applied_totals: Vec<u64> }>` — the boundary is the highest fully-applied-AND-confirmed contiguous episode per region (the summarized coverage), the totals the per-NPC conservation aggregates. `summarize_applied_below` prunes the applied lane's at-or-below-boundary identities into the aggregates with the typed refusals (`BoundaryRegression` — the summarized set is monotonic; `RetainedBelowBoundary` — pending identities are referenced, never summarizable, a refusal is the fail-closed signal; `TotalsOverflow`). The ledger's frozen `capture-ledger/2` fingerprint appends the summary **only when present** — an uncompacted session's output is byte-identical (every pin mechanically preserved).

**The codec**: the director's ledger section in-band **v4** (`[u8 4][retained pending][retained applied][boundary][totals]`; v3 = the accepted full form, byte-identical when uncompacted; unknown discriminants refused). The restore's fail-closed gates: the **lane disjointness** (no retained identity at-or-below its region's boundary — the eligibility invariant) and the **episode frontier** (the boundary never exceeds a region's episode counter).

**The driver** (`crossing.rs`): `SessionCrossing::compact_session(spine, director, retain_tail)` — the explicit, never-automatic operation (the existing lifecycle paths publish full fidelity unless the operator compacts; the subsequent `publish_snapshot` takes `spine-history/2` through the spine's own save dispatch). Phase 1 the eligibility walk: an episode is summarizable iff **no pending identity exists in it** (pending = referenced — the dormant regions' outstanding held obligations and the T1 intent) **and every applied identity of it is `pipeline.is_confirmed`**; the boundary extends over the contiguous prefix, monotonic across successive compactions. Phase 2 the lane summarization. Phase 3 the spine cut (the typed `TransitionError::Compaction` variant). The `SessionCompaction` report carries the history, the boundary, and the summarized counts/amounts.

## 7. The recovery from compacted anchors (commit 3)

`build_anchor`'s third input is the chain's confirmed evidence (passed by `recover_at`): for every chain-confirmed identity at-or-below the boundary, the anchor's coverage **synthesizes from the chain's own `ConfirmedEvidence`** (the counters and the award state over the evidence's occurrence total) under the boundary's writer-attested materialization — nothing is inferred, both inputs retained. **The composition gate** (fail-closed): the per-NPC synthesized sums must equal the carried aggregates **exactly** — an identity the chain does not confirm, or a coverage disagreement, is an over-claiming summary (the halt). The credits conservation extends: retained-applied + the carried aggregates == the world's credits per NPC.

**The load-bearing rationale**: the B3 reconcile's envelope loop and closing edge require anchor coverage for every journaled-or-confirmed identity; with the journal unrotated in PP-B2 and the chain never reclaimed, the summarized (applied+confirmed) identities' coverage **must** be reconstructible or the recovery halts (`confirmed_recovered` / `ConfirmedUnmaterialized`). The boundary attestation + the chain evidence is exactly that reconstruction — the design's "summarized into the coverage boundary + the aggregate counters," made mechanical.

## 8. The acceptance-test inventory (the GO §5 list, mapped)

- **Full-format backward compatibility** — the entire PP-B1 baseline (529 + 1) green unchanged through every commit; the full form's encodings byte-identical (the director v3 path unchanged; the spine save/1 path unchanged); every frozen pin double-matched.
- **The compacted format's canonical encoding and decoding** — `spine_history`: the round trip with byte determinism; the tail-as-verbatim-suffix proof.
- **Wrong-domain and wrong-version rejection** — both directions, patched bytes (`spine_history`); the v4 discriminator refusal (`pp_b2_compaction`).
- **Truncated, corrupt, contradictory, noncanonical** — the `spine_history` corruption matrix over EVERY compacted gate through an independent test-only walker of the save/2 layout (empty-summary, count-identity, summary-length, the tail's frontier/overlap/late-flag/coherence/order, the late-count bound, max-required via a structurally consistent rebuild, max-below-tail, max-clock-coherence, trailing bytes, every truncation prefix); the director v4 gates (the lane disjointness over the journal's own plan region; the episode frontier).
- **Compaction-cut boundaries** — the typed refusals; retain 0 and mid-ledger cuts across the matrices.
- **Prefix digest chaining across successive compactions** — compact → restore → continue → compact == the single equivalent compaction of the same total history, byte-for-byte; the lane boundary's monotonicity.
- **Two independent compactions of the same eligible history producing identical representations** — asserted (bytes + summaries).
- **Unfinished-index derivation** — the E3 membership-vs-ledger proof across scheduling, execution, suspension, resumption.
- **Counter conservation and identity-frontier consistency** — the structural gates at load; the composition + credits conservation at recovery.
- **Dependency resolvability** — follow-ups depending on summarized (executed) identities schedule; unknown identities still rejected.
- **Per-identity consequence-lane preservation** — the two-phase eligibility matrix (pre-confirmation: the pending captures retained in full, nothing summarized; post-confirmation-and-reactivation: the applied-and-confirmed set summarized exactly, the applied lane conserved).
- **Full versus compacted deterministic twin continuation** — over the REAL machinery: the compacted publication + the crash + the actual B4 recovery + the identical continuation.
- **B4 recovery from compacted snapshots** — the twin test + the B5 crash-after-confirmation arc over a compacted anchor (the recovery replays the journaled work; the continuation redoes the lost in-memory work; the convergence at the compacted-safe surface: clock, stats, watermarks, held fingerprint, milestones, credits, the pending lane, the journaled and the presence-re-derivable regions' levels).
- **Repeated recovery and exact-once** — the same-cut compacted domains agree; no capture re-applied.
- **Graceful restart and crash-recovery compatibility** — the twin test's publish-drop-recover-continue arc; the crash arc above.
- **Invalid or incomplete compaction failing closed without damaging the authoritative campaign** — the over-claiming summary (a real compacted snapshot transplanted onto a fresh campaign whose chain carries no confirming revisions): the recovery halts on the composition gate, deterministically, the chain intact; the v4 restore gates; the typed compaction refusals.

## 9. The cross-platform CI evidence

The run **40129091546** (https://github.com/Willpatpost/Sektor7/actions/runs/40129091546, `pull_request` on the draft PR #2 `production-persistence-b2` → `stage5`, at the code head `abdeffe`): **green on all three platforms, verified firsthand job-level** — `windows-latest` ✓ 3m36s, `macos-15 ARM64` ✓ 3m22s, `ubuntu-latest` ✓ 4m10s (the workspace test and fingerprint gates). The annotations are the known Node.js-20 deprecation notices (informational, no failures). This record commit is documentation-only and triggers no further run (the `pull_request` path filter is `engine/**`). Draft PR #2 is the CI vehicle only — **never to be merged** (PR #1 likewise remains open, draft, never-merged).

## 10. The measurements (commit 4; POD, development profile — NON-AUTHORITATIVE under D-PP-7)

The benchmark's new section 7 (one command, `--benchmark-persistence`, the preserved full-fidelity sections beside it). **The pod numbers (development profile — never physical-rig evidence; the rig's release-mode evidence remains the owner's):**

| Measurement | Full form | Compacted (retain 100) |
|---|---|---|
| Serialization (`save_session`) | 2.20 ms median | 1.05 ms |
| Deserialization (`restore_session`) | 7.59 ms | 2.75 ms |
| Snapshot size at 150 ticks | 220,438 B | 124,968 B |
| Snapshot publication | 157.9 ms median | 58.8 ms |
| Session recovery (equal chain length) | 62.4 ms | 40.0 ms |
| Session compaction | — | 23.6 ms (150 ticks) → 777 ms (4800 ticks), ~7.1 µs/record |

**The growth sweep — the bounded steady state measured**: the compacted snapshot **flat at 124,968 B** across 150/600/1200/2400/4800 ticks while the full form grows 220,438 → 3,492,240 B (the ~31 B/execution lifetime growth eliminated — a 28× reduction at 4800 ticks, unbounded beyond). **The window sensitivity** (4800-tick history): retain 0 → 121,868 B; 100 → 124,968; 1000 → 152,868; 5000 → 276,868 — linear at ~31 B per retained record, as designed. **Against the design §7.3 projection**: the projected compacted steady state ≈250-400 KB; the measured 122-277 KB across the window range — at-or-below the projection (the projection assumed a larger retained window); both labeled, neither generalized. The recovery comparison uses its own campaign in exactly the section-5 shape (one publication plus one durable transaction — the same chain length), isolating the compacted snapshot's effect.

## 11. The physical-rig instructions (the owner's Windows release-mode validation)

From `engine/` on the physical Windows PC (the D-PP-7 discipline — the pod numbers above are development-profile and non-authoritative):

1. `cargo test --workspace --release` — expect **549 passed + 1 intentionally ignored, 0 failed**.
2. `cargo test -p engine-core --test spine_history --release` — the focused 10-test matrix (+ the internal layout pin inside the lib suite).
3. `cargo test -p engine-slice --test pp_b2_compaction --release` — the focused 6-test application matrix.
4. `cargo run -q -p engine-slice --release -- --benchmark-persistence` — the full benchmark including the PP-B2 compaction section (the release-mode numbers to record; note the harness's sample sizes: n=20 for the codec/publication samples, n=10 for the compaction/transaction/confirmation/recovery samples).
5. `cargo run -q -p engine-slice --release -- --fingerprint-selftest` (twice) — the frozen pins `engine=6504123a47cab66f` / `state=937bd3b6f56f07e0`, identical across runs.
6. `cargo fmt --all -- --check && cargo clippy --workspace --all-targets -- -D warnings && cargo build --workspace` — the static gates.

## 12. The exclusions honored (the GO §7 list)

NOT implemented, none begun: PP-B3 SK/MK checkpoint publication; checkpoint metadata revisions; canonical revision reclamation; consequence-evidence reclamation; chain-history deletion or compaction walks; journal rotation; reclaimed-record skip/index machinery; PP-B4 asynchronous workers; a new persistent-save authority model; Stage 6 features. The chain-wide idempotency-key uniqueness and the permanent confirmed-consequence evidence rule are untouched (D-PP-4a/4b's rejections stand). The approved architecture was not changed to simplify implementation — the lane-summary representation is the design's own "coverage boundary + aggregate counters" form, and the composition gate is its verification.

## 13. The limitations and the owner-only validation (stated honestly)

- **The measurements are pod, development-profile** — D-PP-7: no performance claim here is upgraded until the owner's rig run (§11) records its numbers.
- **The retain window is the caller's explicit choice** — no automatic compaction trigger is wired anywhere (the GO's explicit-only discipline; `compact_session` is the operator's call); the window sensitivity is measured, and any future default threshold remains a D-PP-4 provisional engineering default subject to its gates.
- **The lane summary's byte economics in THIS workload**: the boundary + aggregates cost ~8.1 KB fixed; the summarized applied entries cost ~24 B each — in short campaigns (few episodes, ≤125 captured identities per episode per region) the v4 form can be larger than v3 for the same lanes. The value is the **bounded retained set** (the applied lane cannot grow past the unsummarized episodes — in a long campaign with many confirmed episodes the retained lane stays bounded while the uncompacted applied lane grows forever) and the per-identity preservation discipline the GO mandates; the spine-history cut is the dominant byte saving in every measured case.
- **The prefix summary is a writer-computed derivation witness**: verifying it against the discarded records requires the full-fidelity reference — which remains chain-reachable in PP-B2 (the pre-compaction snapshot objects are never reclaimed in this batch). A recompute-and-compare verifier over the retained references is possible future work, not claimed here.
- **The synthesized coverage at recovery iterates the confirmed map** — O(confirmed identities), the same order as the B3 closing edge's existing loop; no new growth class, but the recovery's anchor construction is not cheaper on that axis.
- **The compacted spine's four ledger-hashing fingerprints are unavailable by contract** (the loud APIs) — integrations that compare those domains must use the `try_*` variants on compacted sessions; the compacted domains and the held/world comparisons are the supported surfaces.
- **The crash-arc convergence surface** is the compacted-safe set (clock, stats, watermarks, held, world, ledger totals, the journaled region's level) — the full `stage4_state_fingerprint` is a full-history API and cannot compare compacted sessions; the presence-re-derivable regions' episode counters legitimately diverge across a crash (the pre-existing B4/B5 property, unchanged).

## 14. The STOP

**PP-B2 is delivered and stopped for the independent PASS/HOLD review (2026-10-24).** The delivery HEAD is the code head `abdeffe` plus this documentation-only record commit; the branch `production-persistence-b2` is pushed with tracking; the CI run 40129091546 is green on all three platforms (verified firsthand job-level); the protected branches are untouched; PR #2 (the CI vehicle) is open, draft, and never to be merged (PR #1 likewise). The next entry point is **the owner's independent review verdict or the owner's rig evidence (§11's six commands)**. PP-B3 (the retention protocol), PP-B4 (the orchestration), and Stage 6 are not begun and require their own authorizations.
