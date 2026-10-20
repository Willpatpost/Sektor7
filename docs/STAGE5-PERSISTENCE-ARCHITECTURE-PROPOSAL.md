# Sektor7 — Stage 5 Architecture Proposal: Persistence & Commit Pipeline

> **Status:** Architecture proposal v1.0 · 2026-10-20 · **UNRATIFIED — nothing in this document is a decision until the lead ratifies it.** Prepared under the 2026-10-20 Stage 5 Architecture and Research Authorization: research, design, and planning only; **implementation NOT authorized**; no modification of `main`, `stage4-review`, or `macos-port`; no merges; ratified contracts and gameplay policies are consumed, never reopened. Grounded in a first-hand review (2026-10-20, at `stage4-review@2e71770`) of `VISION.md` (v0.11), `TECH-ARCHITECTURE.md` (v0.3), `DECISIONS.md` (v0.12, T-01–T-15 / M-27–M-36 / G-10 / G-12 / E-07–E-10), `MULTIPLAYER-SPEC.md` (v0.4), `MULTIPLAYER-ARCHITECTURE-RESEARCH.md` (v0.2), `E5-CANONICAL-MEDIUM-FEASIBILITY.md` (v0.3), `SIMULATION-ARCHITECTURE-RESEARCH.md` (v0.6), and the engine source (`engine-core` fingerprint/event/spine/lod; `engine-slice` npc/region/sim).
>
> **Evidence key (project convention):** [RATIFIED] = the lead has decided it (cited); [PROJECT EVIDENCE] = demonstrated in this repository; [ESTABLISHED] = documented external capability class; [PROPOSAL] = this document's recommendation awaiting ratification; [ASSUMPTION] = labeled guess pending measurement; [OPEN] = decision reserved for the lead.

---

## 0. Executive summary

**Recommendation [PROPOSAL]:** Stage 5 builds a new L2 crate **`engine-persist`** (per the TECH-ARCH §2 layer map) implementing four mechanisms over the existing, untouched engine-core spine:

1. **Canonical save encoding** — a byte-level `Encoder`/`Decoder` obeying the same discipline the `Fingerprinter` already enforces (little-endian, length-prefixed strings, ordered collections only, versioned domain tags), so a save's bytes and the state's fingerprints share one canonical form and a round-trip is provable by fingerprint equality.
2. **A revision store** — content-addressed full-state snapshots plus hash-chained, append-only revision records with an atomically renamed head pointer and a chain-validation walker (the E5 "chain-complete" shape).
3. **The canonical-medium interface** (`conditional_append(record, expected_head)` → accepted/rejected) with a **local file-backed provider** — the solo/single-machine ordering point, byte-contract-identical to the remote media E5 studies, so the networked provider is a future drop-in rather than a redesign.
4. **The commit pipeline** — the four-state model the authorization requires (logical → pending → locally persisted → canonically confirmed), idempotency keys, a persistent applied-key ledger, fail-closed confirmation (CAS acceptance **plus** validate-after-append), and the encounter-transaction journal (M-30 rollback).

The Stage 4 `CaptureLedger` crosses the Stage 5 boundary it was built for: `capture` begins writing durable pending records (durable-before-discard), `apply` drives confirmation, and Stage 4's labeled reconstruction becomes the crash-recovery mechanism for reconstructible state — **graceful exit resumes exactly; crash recovery preserves every consequence and reconstructs routine state with labels**, which is the engine-layer reading of the ratified M-18 live-suspension/encounter-boundary distinction.

**Five independently reviewable batches** (§7): B1 canonical encoding + full-fidelity spine round-trip (engine-core, additive only) → B2 revision store + local medium + validation/fault-injection → B3 commit pipeline + milestone semantics + encounter journal → B4 application composition (NpcWorld/director codecs, the CaptureLedger crossing, the shove-encounter demo, overlay) → B5 session lifecycle, CI, docs, measurements, Windows acceptance plan.

**P0 decisions needed from the lead before implementation** (§9): solo-T3 semantics confirmation; ledger-growth/compaction policy; the content hash dependency; snapshot cadence/retention; the two-mode recovery acceptance; background-I/O threading.

---

## 1. Requirements and scope analysis

### 1.1 Requirement traceability

| # | Requirement | Source (ratified) | Consequence for the design |
|---|---|---|---|
| R1 | Durable local persistence: deterministic serialization, versioned schemas, forward compatibility, crash-consistent checkpoints, WAL-or-equivalent, atomic saves, corruption detection/recovery, save migration, safe treatment of in-flight events / outstanding obligations / held regions / reconstruction state | Stage 5 authorization §1 | Canonical byte encoding; temp+fsync+rename atomicity; hash-verified records; chain validation with last-valid-prefix fallback; version gate; recovery contract covering queued/blocked/held obligations and checkpoints |
| R2 | Preserve Stage 0–4 scheduling, event identity, causal, and conservation guarantees | Authorization §1; T-07/T-14/T-15 | Round-trip must preserve EventId/seq/`(due,seq)` priority, `depends_on` semantics, `scheduled = executed + queued + blocked + held`, and produce byte-identical historical fingerprints |
| R3 | Commit pipeline distinguishing logical progress / speculative-pending / locally persisted / canonically confirmed | Authorization §2 | Four-state machine; the two persistence writes (pipeline journal T1, medium chain T3) stay distinct even when both are local files |
| R4 | A milestone is never durably saved before canonical acknowledgment; gameplay continues while pending; irreversible actions gated on unconfirmed milestones; retries never duplicate | Authorization §2; M-29 | Confirmation = medium CAS acceptance **and** validate-after-append (E5 §9.1); status API for UI/gates; idempotency keys + applied-key ledger |
| R5 | Foundation for the approved co-op model (one authoritative session, listen-server, ≤4 players, planned handoff, canonical save verification, authority-loss suspension, local emergency copies, no dev-paid servers) without assuming a vendor or guaranteeing distributed exclusivity without a coordinator | Authorization §3; M-28/M-31/M-35; E5 §1 | The medium interface and revision-chain format carry the E5 record schema (parent, epoch, keys, dependencies, signer fields); fail-closed validation gate any verifier can run; networked providers, leases, ballots = future stages |
| R6 | Efficiency: incremental vs full snapshots, retention/compaction, serialization overhead, allocation behavior, background I/O without violating authoritative ordering, recovery latency, disk-space, checkpoint frequency | Authorization §4 | Measure-first discipline; content-addressed dedup; KB-class revision deltas vs MB-class snapshots; single-threaded v1 with a stated threading trigger |
| R7 | Failure/adversarial behavior for: termination during save, partial writes, corrupted checkpoints, interrupted compaction, storage unavailable, conflicting revisions, network partition during confirmation, host crash + rejoin, replay/duplicate commits, schema incompatibility — each with required behavior, Stage 5 vs future split | Authorization §5 | §6.2 failure table; every listed scenario has a defined Stage 5 behavior or an explicit future-stage marker |
| R8 | Engine mechanism, not game policy: milestone definitions and the irreversible-action gating list are game-side | TECH-ARCH §2.6, P5; M-29 | Pipeline exposes status/gates; the demo workload defines what is a milestone and what gates |
| R9 | Platform-independent (Windows, Linux, eventually macOS); no interference with `macos-port` | Authorization restrictions | `std::fs` only at the medium layer; no OS-specific APIs; no Vulkan/renderer coupling |
| R10 | Slice acceptance: quit → relaunch restores committed state; restored fingerprint == pre-exit committed fingerprint; double-commit retry is a no-op; pending/confirmed visible; encounter rollback restores pre-encounter resources | TECH-ARCH §6 Stage 5 row | §5/§7 acceptance criteria are built directly from this row |

### 1.2 What exists today (verified in code at `2e71770`)

- **Canonical serialization discipline already exists as hashing**: `engine-core/src/fingerprint.rs` — FNV-1a 64-bit, little-endian integers, **length-prefixed strings**, ordered collections only, versioned domain tags (`engine-core/spine/2`, `spine-lanes/1`, `spine-held/1`, `state-lanes/1`; `engine-slice/region-state/3`, `capture-ledger/2`, `stage4-state/1`). Every fingerprint is a deterministic function of logical state only; timing/observational state is deliberately excluded (T-14/T-15).
- **The authoritative spine state is fully enumerable** (verified field-by-field): clock, `next_seq`, near/far queues (`BinaryHeap<Reverse<Pending>>` with `Pending{due, seq, event}`), `blocked: BTreeMap<seq, Event>`, `waiting_on` (derivable — see §5.2), `known`/`executed` sets (derivable from ledger + queues + blocked + held), the execution ledger (`Vec<ExecutionRecord>`, ~32–36 B serialized per record), `Stats` (all fields logical/fingerprinted), `held: BTreeMap<seq, Pending>` + derived `held_index`, `max_executed_index` (derived). Observational: diagnostics, lane reports, suspension counters, timing histories.
- **The application state is enumerable**: `NpcWorld` (config, `steps`, `routine_cursor`, `positions`, `represented_through`, `milestones`, `pending_signals`, `credits`, `region_lods`) and `RegionDirector` (config, `states: Vec<RegionState>` with checkpoints, `CaptureLedger{pending, applied}`, plus observational diagnostics/latency/fault-injection — excluded).
- **The Stage 4 boundary is explicit and crossed by design**: the `region.rs` module header states the capture ledger and checkpoints are the smallest honest in-memory mechanisms — "NOT canonical commits, NOT durable, NOT disk-persisted. The production Commit Pipeline and file-backed persistence arrive in Stage 5" [RATIFIED in code docs + T-15].
- **Protocol logic is proven at the contract level**: `experiments/e5-lab-local` (authorized, run 2026-10-17) — CAS contention (100/100 exactly-one-winner), stale-parent/stale-epoch fencing, ambiguous retries (50/50 exactly-once), crash recovery by head re-read, revision validation (5/5 injected faults rejected), determinism (fingerprint `527220de7c202777`) [PROJECT EVIDENCE]. E-07 proved capture/idempotency/applied-key semantics in miniature; E-09 proved labeled replay/synthesis reconstruction; the E5 R2 probe remains gated on account-owner prerequisites [RATIFIED state].
- **Sizing evidence [PROJECT EVIDENCE]**: 6000-tick tour = 162,916 executions ≈ 5.0 MiB of ledger records (32 B each) — full-fidelity snapshots at slice scale are single-digit MB, ~1.6 MB at the 300-tick fingerprint scenario; queues ≈ 50 KiB; NpcWorld arrays ≈ 60 KiB. Commit records are KB-class [ASSUMPTION, E5 §6].

### 1.3 In scope / out of scope

**In scope [PROPOSAL]:** everything in R1–R10: encoding, revision store, local medium, commit pipeline, encounter journal, the CaptureLedger crossing, application codecs, quit/relaunch lifecycle, crash recovery, CI gates, measurements, docs, the Windows acceptance plan, and the minimal encounter exerciser needed to test the two-lane model (the §6 "shove" demo).

**Out of scope (future stages, not silently started):** networked medium providers and the E5 R2 probe; session-open/lease/epoch/handoff records as *live* protocol (the format reserves the fields); member signatures and identity infrastructure (a local signer stub fills the field); ballot aggregation; interest/replication; cross-medium migration; production save migration beyond a version gate; asset/world streaming; any gameplay policy.

**Scope question for the lead [OPEN]:** the TECH-ARCH §6 Stage 5 row also bundles simulation-systems content ("full/simplified closed-form workloads as game data; NPCs visibly moving on schedule paths"). The authorization scopes Stage 5 to Persistence & Commit Pipeline. Proposal: include only the minimal encounter exerciser the two-lane model requires; defer the richer workload/systems content to a later stage or a lead decision (§9 P1-6).

---

## 2. Persistence and commit architecture comparison

The binding tests, from ratified requirements: **(a)** quit→relaunch exact round-trip at 1,000-NPC scale (R10); **(b)** milestone-granular acknowledgment with pending/confirmed distinction and gating (R3/R4); **(c)** bounded, verifiable crash recovery (R1/R7); **(d)** provider-equivalence — the local mechanism must be the same contract the future networked medium offers (R5, E5); **(e)** fail-closed under every corruption/failure mode (M-35); **(f)** efficiency — cost follows work, saves must not scale with entity count × frequency unnecessarily (R6, Summit B).

| Option | Mechanism | Against the binding tests | Verdict |
|---|---|---|---|
| **A. Full-snapshot-only saves** | Serialize the whole state on every save | (a) ✓ (b) ✗ — acknowledgment granularity is the whole world; a milestone commit becomes a MB write; pending-vs-confirmed is all-or-nothing (c) ✓ (d) ✗ — no revision chain to CAS (e) ✓ (f) ✗ — every save pays world-size cost | Rejected as the *commit* mechanism; retained as the *snapshot* tier |
| **B. Event-log-only (replay from origin)** | Append every durable change; recover by full replay | (a) ✓ (b) ✓ (c) ✗ — recovery cost grows with total history (the Stage 4 ledger-growth finding, now measured: linear, unbounded) (d) partial (e) ✓ (f) ✗ | Rejected as the sole mechanism; the revision chain *is* an append log, but bounded by snapshots |
| **C. Hybrid: checkpointed snapshots + append-only revision log** | Periodic content-addressed full snapshots; hash-chained revision records between them; recovery = latest snapshot + replay of subsequent records | (a) ✓ (b) ✓ — records are milestone-granular KB deltas (c) ✓ — recovery bounded by snapshot cadence (d) ✓ — exactly the E5 chain-complete shape ("parent-reachable from a snapshot, content-verifiable, replayable") (e) ✓ (f) ✓ — commits cost KB, snapshots amortized | **Recommended** [PROPOSAL] — also the shape MULTIPLAYER-ARCH-RESEARCH §8 already assumes ([ESTABLISHED]: Git; WAL checkpointing) |
| **D. Embedded database (SQLite / redb / LMDB / RocksDB)** | Crash consistency and CAS via a DB engine | (a) ✓ (b) ~ (c) ✓ (d) ✗ — the portable chain must be exported anyway for the remote medium; the DB becomes a private representation with a second format to maintain (e) ✓ (f) ~ | Rejected for the core: adds a dependency (C or heavy Rust), keeps canonical bytes out of our control, conflicts with "understandable, inspectable data formats" (VISION §8 sovereignty) and the engine-core zero-dependency discipline. Could later sit *behind* the medium interface as a provider — unchanged contract |
| **E. Copy-on-write persistent data structures (structural sharing at the state level)** | Snapshot deltas via shared immutable structure | (a) ✓ (b) ✗ — same granularity problem as A for milestone commits (c) ✓ (d) ✗ (f) ✓ for snapshot size | Deferred: our state is flat Vecs/BTreeMaps (structural sharing pays off on trees); noted as the future snapshot-compression axis |
| **F. Platform/cloud save APIs (Steam Cloud etc.)** | Per-user replica sync | (d) ✗ — E5 §2 excludes them as a *medium* (per-user, not shared/canonical); useful later only as a replica-cache transport | Out of Stage 5 |

**Why the hybrid wins on (d) specifically:** the E5-Lab harness already proved the *protocol logic* of C's chain (CAS, fencing, idempotent retries, crash recovery, validation) against a simulated conditional-append medium [PROJECT EVIDENCE]. A local file-backed provider that implements the same `conditional_append` contract makes Stage 5 the port of proven protocol semantics onto real storage, and the future networked provider (pending E5 qualification) a drop-in — no Stage 5 redesign when multiplayer arrives.

---

## 3. Recommended architecture

### 3.1 The shape

```text
 First-game policy (engine-slice demo: what is a milestone, what gates, encounter definitions)
────────────────────────────────────────────────────────────────────────────────────
 engine-slice (application composition — Batch 4)
   NpcWorld / RegionDirector codecs · CaptureLedger crossing ·
   shove-encounter demo · pending/confirmed overlay · commit triggers
────────────────────────────────────────────────────────────────────────────────────
 engine-persist (NEW crate, L2 — Batches 2–3)
   CommitPipeline   capture→pending→persisted(T1)→confirmed(T3) · idempotency keys ·
                    applied-key ledger · status/gates · retry/re-drive · EncounterJournal
   RevisionStore    content-addressed snapshots · hash-chained revision records ·
                    head CAS (temp+fsync+rename) · chain-validation walker
   CanonicalMedium  conditional_append(record, expected_head) · head() · read_record/read_snapshot
                    └─ LocalFileMedium (Stage 5 provider)   [future: networked providers]
────────────────────────────────────────────────────────────────────────────────────
 engine-core (additive only — Batch 1)
   Encoder/Decoder (canonical bytes, the Fingerprinter discipline) ·
   Spine save sections + load with derived-state rebuild + load-time integrity gates ·
   SaveWorld codec trait (opt-in, beside World::fingerprint)
```

### 3.2 Rationale and tradeoffs

- **The fingerprint discipline becomes the save discipline.** The single strongest property Stage 0–4 built is that logical state has a canonical, deterministic, versioned byte form (as hash input). Stage 5 turns the same rules into byte *output*: little-endian, length-prefixed strings, ordered collections, domain tags. A save is then verifiable (content hash), comparable (round-trip = fingerprint equality), and portable (integer-only authoritative state — no FP serialization hazards in v1; the only f64s are exact-rational configuration values, serialized as configuration metadata).
- **Two persistence writes, even locally.** The pipeline journal (T1, the emergency local copy M-31 requires) and the medium chain (T3) are separate files with separate roles: T1 survives process crashes and re-drives commits after restart with the same keys; T3 is the ordering point. Keeping them distinct when both are local files is not redundancy — it is the contract the networked case needs (E5 §4: T3 never covers owner/machine loss; the journal does), and it exercises the ambiguous-retry resolution (head re-read) the E5-Lab T3/T4 checks proved.
- **Confirmation is fail-closed by construction.** A milestone is Confirmed iff the medium's conditional append succeeded **and** the record lies on the last-valid prefix of the re-read chain (validate-after-append, E5 §9.1 — closes the unsafe-rollback window behind a jammed head). No UI assertion, no in-memory flag, can confirm.
- **Tradeoffs accepted:** full-fidelity snapshots grow linearly with the execution ledger (~32–36 B/execution; measured 5.0 MiB per 6,000 ticks) — acceptable at slice scale, with the compaction axis reserved in the format and the policy decision forwarded to the lead (§9 P0-2); SHA-256 content hashing adds one vetted dependency to the new crate (engine-core stays zero-dep; §9 P0-3); v1 is single-threaded — synchronous saves with measured latency, threading only on a demonstrated frame-budget violation (§9 P0-6).

### 3.3 The four commit states in Stage 5 (R3)

| State | Definition | Where it lives | Survives |
|---|---|---|---|
| **Logical** | In-memory simulation state (spine + world + director) | Process memory | Nothing (rebuilt from save on restart) |
| **Pending (captured)** | A keyed durable delta captured by the pipeline, not yet written anywhere durable | Pipeline memory | Nothing — but capture happens *before* discard (durable-before-discard closes this window to ~zero) |
| **Persisted (T1)** | Written + fsynced to the pipeline's local journal (the emergency copy) | `journal` file | Process crash, restart (re-driven with the same keys) |
| **Confirmed (T3-local)** | The local medium accepted the conditional append **and** chain validation places the record on the valid chain | `HEAD` + revision chain | Everything short of medium (disk) loss; the chain is the campaign's canonical history |

In Stage 5 solo, "canonical" means this campaign's local chain — the Architecture E answer for the single-machine case (the medium is the ordering point; no peer acknowledgments exist or are needed). §9 P0-1 asks the lead to confirm this reading.

### 3.4 What it deliberately is not

Not a database, not a serialization framework, not a thread pool, not a networked client, not a migration engine, not a game-policy layer. Every subsystem interior follows the anti-abstraction ledger (TECH-ARCH §8): boundaries exist only where two parties exchange state — the pipeline ↔ medium seam, the pipeline ↔ world seam, the store ↔ verifier seam.

---

## 4. Ownership boundaries, interfaces, and data flow

### 4.1 Crate and layer boundaries

| Owner | Owns | Does not own |
|---|---|---|
| `engine-core` (additive) | Encoder/Decoder; spine save sections + loader with integrity gates; the `SaveWorld` codec trait; derived-state rebuild | File I/O, formats, chains, media, policy |
| `engine-persist` (new, L2) | Revision record/snapshot formats; hash chain; store + head CAS; the medium trait + local provider; commit pipeline; applied-key ledger; encounter journal | World semantics (opaque keyed deltas); milestone definitions; gating lists |
| `engine-slice` (application) | NpcWorld/RegionDirector codecs; the CaptureLedger crossing; milestone/encounter demo policy; overlay; CLI verification modes | Engine mechanisms (all reusable above) |

`engine-persist` depends on `engine-core`; `engine-slice` depends on both. `engine-core`'s existing surface changes only additively — every historical fingerprint and test must stay byte-identical/green (batch acceptance).

### 4.2 Interface sketches (contracts, not code)

```rust
// engine-core — canonical bytes (the Fingerprinter discipline as output)
pub struct Encoder { /* .. */ }            // write_u8/u32/u64/usize/bytes/str(length-prefixed)/tick
pub struct Decoder<'a> { /* .. */ }        // strict: rejects short/long/mismatched input, typed errors
pub enum SaveError { /* Schema, Truncated, Integrity(reason), UnsupportedVersion(u32), .. */ }

pub trait SaveWorld: World {
    fn save(&self, enc: &mut Encoder);
}
pub trait WorldCodec<W: World>: SaveWorld {
    fn decode(dec: &mut Decoder) -> Result<W, SaveError>;   // app-side constructor from bytes
}
// Spine::save writes domain "engine-core/spine-save/1" (§5.2); load_spine rebuilds
// waiting_on/known/executed/held_index/max_executed_index from canonical sources.

// engine-persist — the medium (the E5 contract, provider-agnostic)
pub trait CanonicalMedium {
    fn head(&self) -> Result<RevisionId, MediumError>;
    fn conditional_append(&mut self, record: &[u8], expected: RevisionId) -> Result<RevisionId, AppendError>;
    fn read_record(&self, id: &RevisionId) -> Result<Vec<u8>, MediumError>;
    fn read_snapshot(&self, hash: &Hash) -> Result<Vec<u8>, MediumError>;
}
pub struct LocalFileMedium { /* campaign dir root */ }

// engine-persist — the pipeline (M-29 semantics)
pub struct CommitPipeline { /* .. */ }
impl CommitPipeline {
    pub fn capture(&mut self, key: CommitKey, delta: Delta, tick: Tick) -> CaptureResult;  // → Pending (+ T1 journal write)
    pub fn commit(&mut self, batch: CommitBatch) -> Result<CommitReceipt, CommitError>;    // T3: CAS + validate-after-append
    pub fn status(&self, key: &CommitKey) -> CommitStatus;  // Pending | Persisted | Confirmed
    pub fn retry_pending(&mut self) -> RetryReport;         // after restart: head re-read, same keys
    pub fn confirmed_head(&self) -> Option<RevisionId>;
}
pub struct EncounterJournal { /* .. */ }   // begin(snapshot) / commit / rollback  (M-30)
```

### 4.3 Milestone data flow (the walkthrough)

1. The workload handler executes an activity; every `MILESTONE_PERIOD`-th **real** execution accrues a pending consequential signal (Stage 4 scratch — unchanged).
2. At a region deactivation (or milestone boundary), `CaptureLedger.capture(key, delta)` runs exactly as in Stage 4 — **plus** the crossing: the same keyed delta is captured into the `CommitPipeline` and journaled (T1, fsync) **before** the reconstructible scratch is discarded. The Stage 4 invariant *capture-before-discard* becomes *durable-before-discard* (I-S5.8).
3. `CommitPipeline.commit` builds the revision record — `{campaign, parent = current head, epoch, milestones[], deltas[] (keyed), idempotency_keys[], dependencies[], signer}` (the E5/MULTIPLAYER-ARCH §8 schema; the signer is a local stub in Stage 5) — and conditionally appends to the medium (head CAS). Rejected CAS ⇒ re-validate, re-base on the new head, retry with the **same keys**.
4. Validate-after-append: the chain is re-read; the record must lie on the last-valid prefix. Only then `status == Confirmed`; gates release; the overlay moves the milestone from pending to confirmed.
5. A later region activation calls `CaptureLedger.apply(key)` — exactly-once via the persisted applied-key ledger; the credit lands in `NpcWorld.credits` (reconstruction still never touches credits/milestones/steps).
6. **Graceful quit**: a full-fidelity session snapshot (spine + world + director) is written and chained; relaunch loads, validates, and resumes **exactly** (R10: restored fingerprint == pre-exit).
7. **Crash**: restart validates the chain, loads the latest snapshot, applies subsequent revision deltas exactly-once, and reconstructs the unrepresented interval via Stage 4's labeled replay/synthesis (represented-through boundaries); pending journal records re-drive from step 3. Consequences: zero loss. Routine state: regenerated, labeled.

### 4.4 The Stage 4 crossing, precisely

The `CaptureLedger` keeps its in-memory role (fast, per-session, exactly-once keyed application at reactivation) and gains a durable shadow: `capture` fans out to the pipeline journal; `apply` retains its semantics and additionally observes the pipeline status for that key. Checkpoints (`RegionCheckpoint`), held obligations, and represented-through boundaries serialize in the session snapshot — so held regions and reconstruction state survive restarts (R1: "safe treatment of in-flight events, outstanding obligations, held regions, and reconstruction state" — each has an explicit section in the save schema).

---

## 5. Contracts

### 5.1 Canonical encoding rules (v1)

Little-endian integers; `write_str` = u64 length + UTF-8 bytes (the audit-item-4 rule); collections written as `usize` count + canonically ordered elements (BTreeMap/Set order, `(due, seq)`-sorted queues); enums as tagged bytes; every section opens with a versioned domain tag string; the decoder rejects unknown versions, trailing bytes, and count/length inconsistencies (`SaveError`, fail-closed). Authoritative state is integer-only in v1 [verified: positions are `(u8,u8)` grids; f64 exists only in `RegionConfig`/presence inputs — configuration, not state].

### 5.2 Save schema v1 (grounded to the verified structs)

| Section | Contents | Source struct (verified) |
|---|---|---|
| Header | schema version, domain tag, campaign id, save kind (session-snapshot / revision payload) | new |
| Spine core | clock, `next_seq`, budget, near_reserve, all seven `Stats` counters | `Spine` fields; `Stats` |
| Near lane / Far lane | queued `Pending` records — seq, class, payload, `Schedule` (tagged), field, `depends_on` — in canonical `(due, seq)` order | `near_queue`/`far_queue` |
| Blocked | seq → Event (same record shape) | `blocked: BTreeMap<u64, Event>` |
| Held | seq → Pending (same record shape, canonical seq order) | `held: BTreeMap<u64, Pending>` |
| Ledger | full `ExecutionRecord` list (id, class, field, intended_due, executed_tick, schedule_kind, late) | `ledger: Vec<ExecutionRecord>` |
| World | opaque length-prefixed blob via `SaveWorld` (NpcWorld: config + steps + routine_cursor + positions + represented_through + milestones + pending_signals + credits + region_lods) | `NpcWorld` |
| Director | region states (level, episode, checkpoint entries incl. represented-through/cursor/position/has_signal, last_method, reconstructed_activities, last_dormancy_span) + capture ledger (pending map, applied set) + config metadata | `RegionState`, `RegionCheckpoint`, `RegionEntry`, `CaptureLedger`, `RegionConfig` |

**Derived state is rebuilt on load, not serialized**: `waiting_on` (registrations of blocked events' unexecuted deps — held deps correctly count as unmet), `known` (ledger ∪ queued ∪ blocked ∪ held), `executed` (from ledger), `held_index`, `max_executed_index`. Equivalence is proven behaviorally by continuation-invisibility tests (§8), since these maps are deliberately unfingerprinted.

**Load-time integrity gates (fail-closed, typed):** schema/domain match; conservation `scheduled == executed + queued + blocked + held` recomputed from sections; `stats.executed == ledger.len()`; `next_seq > max seq`, no duplicate seqs across sections; clock coherence (max `executed_tick` ≤ clock); every `depends_on` of queued/blocked/held resolvable in `known`; watermark consistency. The clock is initialized **at construction** from the verified save (`Spine` construction path) — the only sanctioned non-`run_tick` clock assignment, documented against P3/M-27 (no runtime mutation; a save whose clock is incoherent with its ledger is rejected, never time-traveled).

**Reserved axes (format tags present, v1 implements one encoding):** ledger encoding = `Full` now, `PrefixHash+Tail` reserved for compaction (§9 P0-2); signer = local stub now, member keys later.

### 5.3 Revision store and local medium formats

- **Campaign directory:** `HEAD` (the head revision id + hash; updated by write-temp → fsync → atomic rename), `rev/<hash>` (immutable revision records), `snap/<hash>` (content-addressed snapshots), `journal` (the pipeline's T1 emergency records), `applied` (persisted applied-key ledger — or derived from the chain walk; v1 persists it explicitly and cross-checks against the chain).
- **Hash chain:** every record carries its parent's hash and is content-addressed by its own SHA-256 [P0-3]; snapshots are named by their content hash. FNV-1a remains the fingerprint hasher — it is a determinism instrument, not an integrity mechanism, and stays untouched.
- **Head CAS:** `conditional_append` = write the record file (temp + fsync), then attempt the HEAD rename only if the observed head equals `expected` (the local single-writer case degenerates to a compare-and-rename; the *contract* is the E5 CAS so the networked provider is identical). `std::fs::rename` replaces existing files on both Unix and Windows (MoveFileEx REPLACE_EXISTING) [ASSUMPTION — verify on the rig; §10].
- **Validation walker (any verifier):** from HEAD — hash every record, verify parent links, epoch monotonicity, idempotency-key uniqueness, snapshot reachability; on a protocol-invalid record, freeze the protocol head at the last valid prefix (the E5 §9.1 storage-head/protocol-head distinction).

### 5.4 Recovery procedure

Graceful: load session snapshot → integrity gates → exact resume. Crash: validate chain → latest snapshot → apply subsequent revision deltas exactly-once (applied-key ledger) → reconstruct the unrepresented interval (labeled replay/synthesis; I3 clock coherence; fail-closed to a fallback anchor if a coherent present cannot be produced) → re-drive pending journal records with the same keys (head re-read resolves write-then-crash ambiguity) → resume. Divergent save directories (copied/ forked histories): both chains validate independently; divergence is **detected and reported, never merged** (M-35); solo resolution is the player's choice with verification results displayed [OPEN: the resolution UX].

### 5.5 Commit-state machine and the acknowledgment definition

`Pending → Persisted(T1) → Confirmed(T3)` with exactly-once keyed application and no reverse transitions except governed rollback (P0-2 of the spec's open list — out of Stage 5 scope). **Confirmed ⇔** (1) the medium accepted the conditional append (the linearization point), (2) the provider's persistence acknowledgment covers the record (fsync, locally), and (3) the record is chain-complete (parent-reachable from a snapshot, content-verifiable, on the valid chain). All three conjuncts — the E5 T3 definition verbatim, with validate-after-append folded into (3). Retries with the same keys are no-ops at every step; a lost ack resolves by head re-read.

---

## 6. Invariants and failure behavior

### 6.1 Invariants (each mapped to a test in §8)

- **I-S5.1 Round-trip invisibility:** save→load at equal logical state yields byte-identical `engine=…/state=…/engine-lanes=…/state-lanes=…/held…/stage4-state` fingerprints; continuation after load equals the never-saved control at equal ticks.
- **I-S5.2 Conservation across load:** `scheduled == executed + queued + blocked + held` holds after every load (recomputed; also a load-time gate).
- **I-S5.3 Identity preservation:** EventId/seq, `(due, seq)` priority, lanes, schedules, payloads, and `depends_on` survive round-trips unchanged (cross-region blocked-on-held chains intact).
- **I-S5.4 Exactly-once durable application:** a key's delta applies once per campaign chain — across retries, restarts, duplicate deliveries (persisted applied-key ledger).
- **I-S5.5 No false confirmation:** `Confirmed` ⇒ CAS accepted ∧ chain-valid (validate-after-append); the UI/gates can never observe confirmed earlier; medium failure leaves the milestone pending, never confirmed (M-29/M-35).
- **I-S5.6 Atomic visibility:** a snapshot/revision/HEAD update is fully present (hash-verified) or absent — temp+fsync+rename; no partial state is observable.
- **I-S5.7 Fail-closed load:** any integrity-gate failure rejects with a typed `SaveError`; never best-effort repair, never auto-merge.
- **I-S5.8 Durable-before-discard:** no consequential scratch is discarded before its keyed capture is at least T1-persisted (the Stage 4 capture-before-discard, strengthened).
- **I-S5.9 Encounter atomicity:** encounter-local effects commit atomically or roll back to the pre-encounter snapshot; use-earned durable accruals and previously committed progress stand (M-30/G-12).
- **I-S5.10 Reconstruction honesty:** crash-recovery reconstruction is labeled (Replay/Synthesis) exactly as Stage 4; consequential state (milestones, credits, deaths) is never synthesized.
- **I-S5.11 No wall-clock in authoritative state:** save metadata timestamps are informational; campaign time is the spine clock (P3).

### 6.2 Failure scenarios (authorization §5, each with the Stage 5 / future split)

| Scenario | Stage 5 required behavior | Mechanism | Split |
|---|---|---|---|
| Process termination during a save | Old-or-new, never partial: the previous HEAD/snapshot remains valid; the half-written temp is detected (hash/length) and discarded | I-S5.6 | Stage 5 |
| Partial writes | Rejected at load with a typed error | per-record SHA-256 + section lengths; walker | Stage 5 |
| Corrupted checkpoints/snapshots | Reject; fall back to the last valid prefix + earlier snapshot; report exactly what was lost (tick span, pending keys) | chain walker; last-valid-prefix | Stage 5 |
| Interrupted log compaction | Old base + full log remain valid (compaction = new snapshot + atomic base switch; interruption leaves both) | reserved axis (§9 P0-2) | Format reserved Stage 5; policy deferred |
| Storage unavailable | Commits stay pending; irreversible gates hold; gameplay continues only while authority is verifiable — locally: fail-closed suspension of *progression confirmation*, never silent T1→T3 relabeling | M-35 | Stage 5 (local); remote outage semantics = future |
| Conflicting save revisions | Both chains validated; divergence detected (head/campaign comparison); **never merged**; player/governed resolution with verification results shown | validation gate | Stage 5 detection; governed resolution UX = future |
| Network partition during canonical confirmation | N/A locally — the *contract* keeps the milestone pending and retry-safe with the same keys; head re-read resolves the write-then-partition case | E5 T3/T4 semantics | Future (designed for) |
| Host crash + later rejoin | Recovery procedure §5.4: snapshot + deltas + labeled reconstruction + pending re-drive; zero consequence loss | journal + chain | Stage 5 (local); rejoin-to-remote = future |
| Replay / duplicate commit attempts | Exactly-once; duplicates are no-ops at every pipeline step | keys + applied-key ledger | Stage 5 |
| Schema-version incompatibility | Same-version loads; an older engine rejects a newer schema explicitly (error names the versions); migration is a ratified future path, never silent | version gate | Stage 5 gate; migration framework = future |

---

## 7. Stage-gated implementation plan (independently reviewable batches)

House cadence per batch: implement → `cargo fmt` / `clippy -D warnings` / `build` / `cargo test --workspace` green (148 + 1 ignored preserved and grown) → all historical fingerprints byte-identical → new versioned domains for new state → report → **stop for independent review**. `set -o pipefail` for gate chains. No batch starts without the prior batch's verdict.

| Batch | Scope | Key tests (acceptance) |
|---|---|---|
| **B1 — Canonical encoding + full-fidelity spine round-trip** (engine-core, additive) | Encoder/Decoder; `SaveWorld`/`WorldCodec`; `Spine::save` + `load_spine` with derived-state rebuild and load-time integrity gates; save domain `engine-core/spine-save/1` | Round-trip fingerprint equality (all existing domains) in-process and cross-process; continuation-invisibility (save at 300 ticks, load, continue 300 == control 600, default + budget-30 + a dormant-region presence scenario); conservation + identity gates at load; corrupted/truncated/version-mismatched encodings rejected with typed errors; **zero behavior change to existing paths — the full Stage 0–4 suite untouched-green** |
| **B2 — Revision store + local medium + validation** (engine-persist) | Record/snapshot formats; SHA-256 chain; content-addressed store; HEAD CAS; validation walker; applied-key persistence; `LocalFileMedium` | E5-Lab T1–T5 ported to the real file medium: CAS exactly-one-winner under concurrent handles; stale-parent/stale-epoch rejection; ambiguous retry exactly-once; crash recovery by head re-read; 5/5 injected faults (tampered payload, broken parent, duplicate key, stale epoch, hash mismatch) rejected with correct reasons; fault-injection sweep (truncation at sampled offsets, bit flips, torn HEAD) — every corruption rejected, last-valid-prefix fallback verified |
| **B3 — Commit pipeline + milestone semantics + encounter journal** | Four-state machine; T1 journal; T3 CAS + validate-after-append; idempotency; retry/re-drive; status/gating API; `EncounterJournal` | Duplicate/retry storms are no-ops; injected medium failure ⇒ status stays pending, gate stays closed (I-S5.5); lost-ack resolution by head re-read; journal survives simulated restart and re-drives; encounter commit preserves durable accruals while rollback restores exactly the pre-encounter resources (I-S5.9) |
| **B4 — Application composition** (engine-slice) | NpcWorld/RegionDirector codecs; the CaptureLedger crossing (durable-before-discard); milestone commit triggers (`MILESTONE_PERIOD` alignment, keyed credits); the shove-encounter demo (witnesses → durable memory records; rollback); pending/confirmed overlay + irreversible-action gate demo | `stage4-state/1` round-trip equality including dormant checkpoints and the capture ledger; consequence conservation (`milestones = credits + pending + ledger`) across save/load/deactivate/reactivate cycles; a crash between capture and apply loses no consequence (journal re-drive); Stage 4 suite untouched-green |
| **B5 — Session lifecycle, CI, docs, measurements** | `--persistence` CLI verification modes (save-at-N / load-and-continue / corruption selftest); quit→relaunch in the headless and graphical demos; CI gates on both platforms; `engine/README.md` Stage 5 section + the Windows acceptance plan (rig commands); TECH-ARCH §6 record + DECISIONS entry; measurements + final report | R10 acceptance reproduced end-to-end: restart round-trip fingerprint equality; double-commit no-op; pending/confirmed visible; encounter rollback; CI green both platforms (round-trip + corruption-rejection smokes); measured save/commit/recovery latency + sizes + growth (pod instruments; rig authoritative) |

Dependency notes: B2–B3 depend only on B1's encoder; B4 on B1+B3; B5 integrates. Estimated diff concentration: B1 engine-core (+ encoder/save + tests), B2–B3 the new crate, B4–B5 engine-slice — mirroring the Stage 4 batch separation of engine mechanism / application composition / integration.

---

## 8. Testing and benchmarking strategy

**Correctness (CI, both platforms, headless):**
- Round-trip determinism: every presence scenario (stationary/walk/tour), default and budget-30, plus a dormant-heavy scenario — save→load→continue equals the unsaved control (the strongest statement: *saving is invisible to the simulation*).
- Cross-process and cross-platform byte-identical saves (integer-only state; the Stage 4 cross-platform fingerprint equality already proves the discipline).
- Load-time integrity gates: a generated corpus of mutated saves (bit flips, truncations, count drifts, version bumps, conservation breakers) — each rejected with the correct typed error; zero panics (fuzz-shaped, deterministic seeds).
- Protocol: the E5-Lab T1–T5 checks against the real file medium (B2) and through the pipeline (B3).
- Crash simulation: a test-only fault seam ("terminate between fsync and rename") + process restart (real child-process kill on the pod at a controlled point; the e5-lab in-process discard remains the deterministic core).
- Conservation/identity: I-S5.2/I-S5.3 asserted after every load in every round-trip test.
- Encounter transactions: commit and rollback paths with witness records (B4).

**Performance (measure-first; pod = instruments, rig = authoritative; no invented thresholds — E-10 discipline):**
- Save latency: full session snapshot at 1,000 NPCs, p50/p95/worst, debug + release (pod) and on the rig; framed against the 16.7 ms frame budget as an *observation*, with the threading trigger (§9 P0-6) decided by measurement.
- Commit latency: per-milestone record write + fsync (the dominant, platform-dependent term — NTFS vs ext4 measured separately on the rig).
- Recovery latency: snapshot load + delta apply + reconstruction, by snapshot cadence.
- Disk: snapshot size by tick count; revision-record size; growth per sim-hour; content-addressed dedup effect.
- Memory: RSS around saves/commits (encode-buffer reuse; no per-save allocation storms).
- All measurements recorded in the Stage 5 report with exact commands/profile/environment, per the house honesty rules.

**CI plan (B5):** `engine-ci` gains a `--persistence-selftest` gate (round-trip + double-run determinism) and a corruption-rejection smoke on ubuntu-latest + windows-latest; existing gates untouched.

---

## 9. Open questions requiring the lead's decision before implementation

**P0 — block the first implementation batch's final design:**
1. **Solo T3 semantics.** Confirm: in Stage 5 solo/single-machine, the local file-backed chain *is* the canonical medium — "canonically confirmed" = local CAS + chain validity — with the networked provider and peer acknowledgments deferred (Architecture E's solo case; E5 §4). The alternative (solo = permanently sub-T3 pending) contradicts the subset-play requirement the multiplayer study ratified.
2. **Ledger growth / compaction.** Full-fidelity session saves grow ~32–36 B per execution (measured 5.0 MiB / 6,000 ticks; ~1.6 MB at the 300-tick scenario). Proposal: implement full-fidelity now; reserve the `PrefixHash+Tail` ledger encoding in the format; ratify the compaction policy (which introduces a new versioned fingerprint domain for saved-state comparisons) later, before long-campaign scale. Alternative: build compaction into Stage 5 now.
3. **Content hash.** SHA-256 via the `sha2` crate in `engine-persist` (recommended: pure Rust, audited, no build complexity — engine-core stays zero-dep) vs. in-house SHA-256 vs. blake3 (faster on MB-class snapshots). FNV-1a remains the fingerprint hasher regardless.
4. **Snapshot cadence + retention.** Proposal: milestone-aligned commits always; a session snapshot on graceful quit; size/tick-bound snapshot cadence during long sessions; retain the last G generations + the full revision chain. Exact numbers = a measured recommendation in B5, ratified before merge.
5. **Two-mode recovery acceptance.** Confirm the engine-layer reading of M-18/M-29: graceful exit resumes exactly; crash recovery preserves every consequence exactly and *reconstructs* routine state with labels (never synthesizing consequential state). This is what makes "safe treatment of outstanding obligations" honest — routine obligations regenerate coherently; consequential obligations are durable.
6. **Background I/O threading.** Stage 0–4 are single-threaded (anti-speculation rule). Proposal: v1 synchronous with measured latency; a dedicated I/O thread enters only on a demonstrated frame-budget violation (the trigger is stated, not speculative).

**P1 — before or during the relevant batch:**
7. Save-location platform policy (`%APPDATA%` / XDG paths; the campaign directory layout is user-visible).
8. Migration framework horizon (v1 ships a version gate only — older engine rejects newer saves explicitly).
9. Commit-record signer/identity fields in solo (local stub now; member-key infrastructure = multiplayer stage).
10. The encounter-boundary definition for the demo (what counts as pre-encounter state in the slice's resource set).
11. The §6 Stage 5 row's systems content (workloads/visible movement) — in this stage or a later one (§1.3).
12. Emergency-copy / generation retention counts (ties to P0-4).

---

## 10. Assumptions register (unresolved, labeled — never silently decided)

| Assumption | Basis | Verification path |
|---|---|---|
| `std::fs::rename` replaces existing files on Windows (MoveFileEx + REPLACE_EXISTING) | Rust std documentation | Rig verification in B2/B5 (Windows CI exercises it too) |
| Directory-fsync discipline differs Unix vs Windows (needed on Unix for rename durability; no direct equivalent exposed for dirs on Windows) | POSIX/Win32 behavior | Medium-layer design isolates it; document per platform in B2 |
| SHA-256 via `sha2` builds cleanly on both platforms (pure Rust) | Crate nature | B2 CI |
| Commit records are KB-class; snapshots MB-class at slice scale | E5 §6 [ASSUMPTION] + measured ledger sizing | B5 measurements |
| The E5-Lab protocol semantics port to the file medium without change | Harness is contract-level [PROJECT EVIDENCE] | B2 test port |
| Serializing `NpcConfig`/`RegionConfig` (incl. exact-rational f64 centers) as campaign metadata is deterministic cross-platform | Values exactly representable; LE f64 bytes | B1 cross-platform round-trip test |
| Presence scenarios/camera input are *inputs*, not campaign state — excluded from saves | Stage 4 input/state separation | B4 design note; lead may override |
| Load-time clock initialization at construction is the sanctioned P3 exception (no runtime mutation) | M-27 letter/spirit analysis in §5.2 | Ratified in this proposal's review |

---

## 11. Deliverable cross-check

Authorization deliverable → section: (1) requirements/scope → §1; (2) architecture comparison → §2; (3) recommended architecture + rationale/tradeoffs → §3; (4) ownership/interfaces/data flow → §4; (5) serialization/checkpoint/recovery/commit-state contracts → §5; (6) invariants + failure behavior → §6; (7) stage-gated batch plan → §7; (8) testing/benchmarking strategy → §8; (9) open questions → §9; unresolved assumptions → §10. Restrictions honored: no implementation, no branch modified, no merges, no policy reopened, platform-independent, macos-port untouched (the design is std-fs only and shares no surface with that branch's Vulkan work).

*Prepared 2026-10-20 from first-hand document review and code verification at `stage4-review@2e71770`. Every recommendation above is [PROPOSAL] pending the lead's ratification; Stage 5 implementation remains unauthorized until the lead grants it after Stage 4 final acceptance.*
