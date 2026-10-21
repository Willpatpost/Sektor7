# Sektor7 — Stage 5 Persistence and Commit Pipeline Architecture (v1.4)

> **Status:** Architecture proposal **v1.4** · 2026-10-20 · **Supersedes v1.3** (`a11d154`; v1.2 at `a883f96`, v1.1 at `deb8acb`, v1.0 at `68e42e2`, ratification record at `eb79d1d` — all preserved in history). The v1.3 review approved the P0 decisions, major boundaries, CAS-locking direction, durability classifications, and five-batch structure, and returned **HOLD on final acceptance pending a focused correctness erratum** — five items: **(1)** correct durable-transition recovery (pre/post/incompatible classification; causal, not positional, supersession), **(2)** a genuine all-or-nothing live mutation boundary, **(3)** the executed-history representation (complement-of-unfinished vs bitset, justified selection), **(4)** the Windows directory-flush candidate marked unproven with SQLite evaluated as the preferred production fallback, **(5)** safe orphan reconciliation (a hash-failing object is not automatically an orphan). The corrections are delivered as the focused erratum [`STAGE5-V1.3-ERRATUM.md`](STAGE5-V1.3-ERRATUM.md) (same commit) and integrated below — **only the affected passages change; no unrelated material is rewritten and nothing already accepted is reopened.** **Stage 5 implementation remains NOT authorized.** Restrictions in force: `stage4-review` frozen at `2e71770` (physical Windows acceptance pending); `main` untouched; `macos-port` untouched by this stream; no merges; no new engine dependencies yet; documentation-only changes on `stage5-design`.
>
> **Evidence key:** [RATIFIED-P0-n] = the lead's ratified decision (ratification doc §1); [RATIFIED-Correction-X] = the lead's required correction (ratification doc §2–§4); [RATIFIED-Clarification-X] = ratification doc §5; [PROPOSAL] = this document's design judgment awaiting review; [PROJECT EVIDENCE] = demonstrated in this repository; [ASSUMPTION] = labeled guess pending measurement; [OPEN-P1] = the lead's decision before the relevant implementation gate.

---

## 0. Executive summary

Stage 5 builds a new L2 crate **`engine-persist`** over the existing, untouched engine-core spine, now under the ratified decisions:

1. **Canonical save encoding** — fixed-width, versioned binary wire format (no `usize`, no native-layout leakage) mirroring the `Fingerprinter` discipline; full-fidelity reference encoding first [RATIFIED-P0-2].
2. **Revision store** — content-addressed snapshots and hash-chained append-only revision records; **SHA-256 via the `sha2` crate** for all object integrity [RATIFIED-P0-3], computed over versioned canonical bytes, type-unambiguous, verified on every load; FNV-1a fingerprints unchanged as regression instruments.
3. **The canonical-medium interface** with a **local file-backed provider** whose CAS is **OS-managed exclusive file locking** (`std::fs::File::lock()`, persistent lock file, never unlinked in ordinary operation) around the nine-step conditional-append protocol [RATIFIED-Correction-A] — with an explicit **Unknown-outcome** result class and reconciliation path [RATIFIED-Clarification-B].
4. **The commit pipeline** — the ratified **five-state durability model** (confirmed canonical / locally-persisted pending / in-memory pending / provisional encounter / reconstructible routine) [RATIFIED-P0-5], adaptive progress-aware persistence (milestone commits + measured-criteria recovery snapshots) [RATIFIED-P0-4], **synchronous-first I/O** with threading only on measured evidence [RATIFIED-P0-6], and campaign **authority modes** (standalone-local vs shared) with no silent transitions [RATIFIED-P0-1].
5. **The durable CaptureLedger crossing** — six-phase transactional deactivation with typed failures, the confirmed chain as the sole applied-key authority, and monotonic consequence identities over **all issued durable identities** [RATIFIED-Correction-B].
6. **A real compaction design** — dense-sequence insight: `known` is derivable from the allocation frontier, `executed` is a dense bitset, and the per-record history is replaceable by an authenticated prefix summary — with full-fidelity retained as the reference representation [RATIFIED-Correction-C].

The five-batch plan is retained with the verdict's clarified per-batch acceptance; **Batch 2 is the particularly important independent review gate**. No batch begins without the prior batch's independent review — an author's own success report never suffices.

**v1.2 adds the three review-required specifications:** the compaction/Spine interface contract — `HistoryKind`, `ledger()` semantics, fingerprint availability, the compacted domain, exact load gates, and the anchor trust model (§5.6.1); the prepared-transition integration for durable deactivation — `DeactivationPlan` + `collect_matching`, journal transaction frames, and the ten failure/crash/retry/reconciliation cases (§5.5.1); and the local medium's durability boundary, acceptance semantics, and anchor-bounded validation cost model (§5.3.1–§5.3.2).

**v1.3 finalizes the implementation contract** with the three consistency resolutions: the public-API safety and retention-truth design (§5.6.1 amended + §5.6.2); the durable-transition lifecycle with fail-closed divergence prevention and idempotent application (§5.5.2); and the acknowledged-revision-persistence boundary with the orphan-cleanup protocol, the Windows alternatives and required evidence, and the adopted lead recommendation that Windows production-standalone acceptance requires a justified abrupt-power-loss durability contract (§5.3.3, amending §5.3.1) — plus the O(1) cursor-advancement validation cost model (§5.3.2 amended).

**v1.4 integrates the correctness erratum** ([`STAGE5-V1.3-ERRATUM.md`](STAGE5-V1.3-ERRATUM.md)): the corrected durable-transition recovery — **pre-state / post-state / incompatible** classification with **causal, not positional, supersession** (§E1 → §5.5.2); the genuine all-or-nothing mutation boundary — the staged **prepare/install** commit bundle (§E2 → §5.5.2); the executed-history representation **selection: complement-of-unfinished**, eliminating the lifetime-growing bitset with the resulting growth model stated (§E3 → §5.6/§5.6.1/§5.6.2); the Windows directory-flush candidate marked **unproven** with **SQLite as the preferred production fallback** (WAL + `synchronous=FULL`, all revision objects inside one transactional boundary — §E4 → §5.3.3); and the **safe orphan-reconciliation** rules — quarantine, never auto-delete; GC never destroys the only surviving evidence (§E5 → §5.3.3).

---

## 1. Requirements and scope analysis

### 1.1 Requirement traceability (v1.0 R1–R10 stand, now with ratified sources)

The v1.0 requirement table (R1–R10) remains the traceability spine; each row's source is now strengthened: R3/R4 ← [RATIFIED-P0-5] (five precise durability classes), R4 ← [RATIFIED-P0-4] (adaptive persistence), R5 ← [RATIFIED-P0-1] (authority modes), R6 ← [RATIFIED-P0-2/P0-4/P0-6], R7 ← [RATIFIED-Correction-A/B/C]. §11 maps every requirement to specific acceptance tests.

**Scope discipline [RATIFIED-Clarification-E]:** Stage 5 is persistence and the commit pipeline plus a minimal encounter demonstration sufficient to prove rollback and durability behavior — nothing more. General NPC movement, broader gameplay, asset streaming, physics, networking, and unrelated systems are excluded; the richer systems content of the TECH-ARCH §6 Stage 5 row is a separate scope question (§9 P1-5), not silently included. The demo encounter demonstrates the ratified M-30/G-12 rules without becoming a production gameplay subsystem [RATIFIED-Clarification-D].

### 1.2 What exists today (unchanged from v1.0, verified at `2e71770`)

The v1.0 inventory stands: the FNV-1a canonical-serialization discipline; the fully enumerable spine state (queues, blocked, waiting_on, known, executed, ledger, held, Stats — canonical vs derived vs observational verified field-by-field); `NpcWorld`/`RegionDirector` state; the Stage 4 in-memory boundary (explicitly awaiting this stage); the E5-Lab protocol-logic evidence; sizing (≈5.0 MiB ledger per 6,000 ticks; KB-class commit records). Two facts gain load-bearing roles in v1.1:

- **Dense, gap-free sequence allocation (verified in `spine.rs`):** every `schedule`/`schedule_held` call allocates `seq = next_seq; next_seq += 1` — the set of ever-issued identities is exactly `[0, next_seq)`. This makes `known` *derivable* (a bounds check) and `executed` *densely representable* (a bitset) — the foundation of the compaction design (§5.6).
- **Packed consequence-key layout (verified in `region.rs`):** `consequence_key = (region << 54) | (episode << 10) | npc` — region occupies bits 54–63 (10 bits, max 1,024), episode bits 10–53 (44 bits), npc bits 0–9 (10 bits, **max 1,024 — only 24 headroom above today's 1,000 NPCs**). This audit drives Correction B's identity work (§5.5).

---

## 2. Persistence and commit architecture comparison

The v1.0 comparison (full-snapshot-only / event-log-only / **hybrid checkpoint+log** / embedded DB / CoW / platform APIs) and its verdict stand: the hybrid content-addressed chain is the recommended shape, now ratified in substance by P0-2/P0-3/P0-4.

### 2.1 SQLite reevaluation [RATIFIED-Correction-A]

The verdict requires a justified comparison of SQLite as the local provider's *transaction layer* — not as the canonical wire format (canonical revision bytes stand either way). Across the verdict's seven dimensions:

| Dimension | Custom file-backed + std OS locking | SQLite (e.g., `rusqlite` bundled) |
|---|---|---|
| Logical data format | Our canonical versioned bytes, directly the persisted form | Ours at the logical layer; SQLite pages as an internal representation requiring export/import to produce chain bytes |
| Local transaction implementation | One CAS point (HEAD under OS lock) + immutable objects + rename; ~200 LOC of reviewable medium code | Battle-tested multi-statement transactions, WAL, mature crash recovery |
| Portability | `std::fs` only; no native build path | C library compiled from source (bundled) — an additional native build path on CI, Windows, and the macOS port (the SDL3/cmake lesson: native build paths are a real maintenance cost) |
| Crash consistency | Must be *demonstrated* (B2's fault-injection matrix, real child-process kills, cross-process contention) — the risk the verdict correctly targets | Demonstrated by decades of deployment — the strongest argument for it |
| Locking semantics | `std::fs::File::lock()` — OS-managed, auto-released on process death, persistent lock file | Internal POSIX/Windows locking, solved |
| Maintenance burden | Ours, small surface, no dependency governance | Dependency upgrades, build flags, packaging (though a single well-known crate) |
| Runtime/packaging overhead | None beyond our bytes | Embedded DB runtime in every binary; page-level I/O we do not need |

**Recommendation [PROPOSAL]:** the custom file-backed implementation with std OS-managed locking, **conditional on Batch 2 demonstrating correctness** under the verdict's full test obligations (competing independent processes, real child-process termination, the fault-injection matrix, no acknowledged-commit loss within the documented storage failure model). The required operation surface is minimal — a single-pointer CAS plus immutable content-addressed blobs; there are no multi-key transactions, no queries, no in-place mutable structures (immutability + atomic rename does the transactional work). SQLite is **the identified fallback**: if B2's testing exposes a guarantee `std` cannot establish on a supported platform (the known candidate: Windows directory-metadata durability, §10), the medium layer swaps to a SQLite-backed provider behind the unchanged `CanonicalMedium` trait — the verdict's own criterion ("acceptable if simpler **and demonstrably correct**") is met by evidence, not assertion. No database selection changes in this document.

---

## 3. Recommended architecture

### 3.1 The shape (updated)

```text
 First-game policy (engine-slice demo: milestone definitions, gating list, encounter rules —
                     the EncounterJournal never decides which resources survive [Ratified-Clarification-D])
────────────────────────────────────────────────────────────────────────────────────────
 engine-slice (B4)
   NpcWorld / RegionDirector codecs · six-phase durable deactivation ·
   shove-encounter demo · pending/confirmed overlay (T3-local labeled) · commit triggers
────────────────────────────────────────────────────────────────────────────────────────
 engine-persist (B2–B3)
   CommitPipeline   five durability states · idempotency keys · applied-key state grounded
                    in the canonical chain · reconciliation of Unknown outcomes ·
                    EncounterJournal (generic transactional machinery)
   RevisionStore    content-addressed snapshots + records (SHA-256) · compaction (§5.6) ·
                    retention (verify-before-reclaim) · chain validation walker
   CanonicalMedium  append(record, expected_head) → Accepted | Rejected | Unknown ·
                    LocalFileMedium (std File::lock CAS, crash-consistent HEAD publication)
   Authority modes  founding metadata: StandaloneLocal | Shared + medium identity
                    (no silent transitions; conversion protocol designed, not implemented)
────────────────────────────────────────────────────────────────────────────────────────
 engine-core (B1, additive)
   Encoder/Decoder (fixed-width wire spec) · spine save/load · derived-state rebuild ·
   load-time integrity gates · full-fidelity reference encoding
```

### 3.2 The five durability states [RATIFIED-P0-5 — supersedes v1.0's four-state table]

| State | Definition | Survives |
|---|---|---|
| **Confirmed canonical progress** | The configured canonical medium accepted the conditional append, verified persistence, and complete chain validation places the revision on the valid chain | Everything covered by the medium's durability contract; **never silently rolled back**. Canonical acknowledgment does *not* guarantee survival of destruction/deletion/loss of the medium itself — replicas and DR are separate concerns |
| **Locally persisted pending (T1 journal)** | A successfully fsynced journal entry | Process termination (retry/reconciliation), provided local storage survives intact; remains pending until canonical acknowledgment |
| **In-memory pending** | Captured in memory only, before the journal write completes | **Not guaranteed across an arbitrary crash — and never claimed to be** [RATIFIED-P0-5] |
| **Provisional encounter state** | Encounter-local transaction lane | Not recovered after full session termination; uncommitted effects roll back per the ratified checkpoint/resource rules |
| **Reconstructible routine state** | Routine simulation detail | Reconstructed by labeled replay/synthesis; consequential progress is never synthesized |

**The governing guarantee, in the ratified words:** persistence transitions never discard consequential state unless the discarded information has a **verified recoverable representation**, and confirmed progress is never falsely represented or silently lost through recovery logic. We do not promise that every unconfirmed consequence survives every possible crash.

**Session distinction (ratified gameplay policy, preserved):** a still-live suspension resumes the exact authoritative logical state; after termination, authored encounters resume from the appropriate encounter boundary; a graceful exit may support exact restoration of committed logical state where consistent with gameplay policy — exact technical serialization must never silently override the encounter-boundary rules. Stage 5 must also prove that a world revision and its associated player progression cannot be independently rewound or advanced (§6 I-S5.12).

### 3.3 Authority modes [RATIFIED-P0-1]

- **Standalone-local campaign:** the local file-backed revision store *is* the campaign's canonical medium — the authoritative ordering point; conditional append + verified persistence + complete chain validation constitute canonical confirmation for that campaign. No external service or peer acknowledgments required. This is legitimate **local-canonical confirmation (T3-local)** — not an assertion of replication or protection against loss of the player's machine. Diagnostics and documentation label it `T3-local (standalone)`, never bare `T3`.
- **Shared campaign:** the configured shared canonical medium remains authoritative; the local store is T1 persistence and potentially a T2 replica. Local acknowledgment never substitutes for shared T3 confirmation. **Playing alone in the frontier does not convert a shared campaign into a standalone campaign** — solo frontier play in a shared campaign goes through the shared medium (session-open CAS, commits at shared T3), and unverifiable shared authority suspends authoritative progression under the existing fail-closed policy. Previously confirmed shared progress stays distinguishable from locally pending progress.
- **Founding metadata:** the campaign's founding record identifies the authority mode and canonical-medium identity. **No silent transitions.** A future standalone→cooperative conversion is an explicit, verified migration/founding protocol — *compatibility designed now* (chain records are mode-agnostic bytes; the conversion re-founds the chain on the shared medium with a founding record referencing the standalone head), *implementation not required in Stage 5*.
- One interface, honestly differentiated: the common `CanonicalMedium` trait supports both modes **without pretending their failure guarantees are identical** — the mode is campaign state, surfaced in every status/diagnostic surface.

### 3.4 Tradeoffs accepted (updated)

Full-fidelity reference encoding first, compaction designed now and implemented by measured evidence [RATIFIED-P0-2]; `sha2` in `engine-persist` only (engine-core stays zero-dep) [RATIFIED-P0-3]; synchronous persistence in v1 with the I/O-worker extraction seam designed in [RATIFIED-P0-6]; retention numbers from measurement, never invented now [RATIFIED-P0-4]; the custom local medium conditional on Batch 2's demonstrated correctness, with SQLite the identified fallback (§2.1).

---

## 4. Ownership boundaries, interfaces, and data flow

### 4.1 Crate and layer boundaries — unchanged from v1.0

`engine-core` (additive: encoder, `SaveWorld` codec trait, spine save/load, integrity gates) → `engine-persist` (formats, chain, medium, pipeline, journal, compaction) → `engine-slice` (codecs, crossing, demo policy, overlay). Milestone definitions, the irreversible-action gating list, and which encounter resources survive failure are game policy outside the engine mechanisms [RATIFIED-Clarification-D].

### 4.2 Interface sketches (contracts, not code — updated)

```rust
// engine-core — fixed-width canonical wire format (§5.1)
pub struct Encoder { /* .. */ }   // u8/u32/u64/tick/bytes/str(length-prefixed) — NO usize on the wire
pub struct Decoder<'a> { /* .. */ } // strict: typed errors, max-length/count rejection, unknown-version rejection
pub enum SaveError { Schema, Truncated, Integrity(String), UnsupportedVersion(u32), ResourceExhaustion, .. }

// engine-persist — the medium, with Unknown outcomes [Ratified-Clarification-B]
pub enum AppendOutcome {
    Accepted(RevisionId),
    RejectedStaleParent { observed: RevisionId },
    Unknown { reason: UnknownReason },   // timeout, lost ack, I/O error, temporarily unverifiable
}
pub trait CanonicalMedium {
    fn head(&self) -> Result<RevisionId, MediumError>;
    fn append(&mut self, record: &[u8], expected: RevisionId) -> Result<AppendOutcome, MediumError>;
    fn read_record(&self, id: &RevisionId) -> Result<Vec<u8>, MediumError>;
    fn read_snapshot(&self, hash: &Hash) -> Result<Vec<u8>, MediumError>;
}
pub struct LocalFileMedium { /* campaign dir root; persistent HEAD.lock via std File::lock */ }

// engine-persist — the pipeline over the five durability states
pub struct CommitPipeline { /* .. */ }
impl CommitPipeline {
    pub fn capture(&mut self, key: ConsequenceId, delta: Delta, tick: Tick)
        -> Result<CaptureReceipt, CaptureError>;   // typed failures; NO success-shaped receipt
                                                   // before required persistence succeeds [Ratified-Correction-B]
    pub fn commit(&mut self, batch: CommitBatch) -> Result<CommitReceipt, CommitError>;
    pub fn status(&self, key: &ConsequenceId) -> KeyStatus;  // InMemory | JournaledT1 |
                                                             // AppliedToLivePending | InConfirmedChain
    pub fn reconcile_unknown(&mut self, pending: &PendingAttempt) -> Reconciliation; // never auto-interprets
    pub fn retry_pending(&mut self) -> RetryReport;
}
pub struct EncounterJournal { /* .. */ } // generic begin/commit/rollback — policy-free machinery
```

### 4.3 Milestone data flow (updated walkthrough)

1. The workload handler's real execution accrues a pending consequential signal (in-memory pending — not guaranteed across crash, honestly labeled).
2. At a deactivation or milestone boundary, the **six-phase transactional capture** runs (§5.5) — durable-before-discard with typed failures.
3. `commit` builds the revision record `{campaign, authority mode, parent = head, epoch, milestones[], keyed deltas, idempotency keys, dependencies, signer}` and calls `medium.append`. Outcomes: `Accepted` → validate-after-append → confirmed; `RejectedStaleParent` → re-validate, re-base, retry with the same keys; `Unknown` → **never auto-interpreted** — the attempt retains its idempotency key, the head is re-read and canonical history verified where possible, and confirmation fails closed where authority cannot be established.
4. Pending/confirmed distinction reaches the UI (`T3-local (standalone)` labeled for standalone campaigns); irreversible dependent actions stay gated until confirmation.
5. A later activation applies the captured consequence exactly once, grounded in the confirmed chain's applied-key state (§5.5).
6. **Graceful quit:** a full-fidelity session snapshot is committed as a recovery anchor; relaunch loads, validates, resumes exactly where gameplay policy says (committed logical state; encounter-boundary rules never silently overridden).
7. **Crash:** validate chain → latest anchor → apply subsequent revision deltas exactly-once → reconstruct the unrepresented interval with labels → re-drive journaled pending with the same keys → reconcile Unknowns. Consequences: zero loss through persistence transitions (per §3.2's governing guarantee). Routine state: regenerated, labeled.

### 4.4 The CaptureLedger crossing (Correction B — see §5.5)

The Stage 4 in-memory ledger keeps its session-fast role; the crossing makes the *chain* the sole authority of canonical commitment, the journal the T1 recovery record, and identity allocation monotonic over every issued durable identity.

---

## 5. Contracts

### 5.1 Canonical wire format v1 (Clarification A — the format specification)

**Wire types:** `u8`, `u32`, `u64`, `Tick` (u64), length-prefixed UTF-8 strings (u64 length + bytes), byte arrays (u64 length + bytes), enums as tagged bytes, collections as u64 count + canonically ordered elements. **No `usize` or architecture-dependent widths anywhere on the wire** — native counts convert through checked u64 boundaries. **Accepted maxima:** every length/count field has a defined maximum (fail `ResourceExhaustion`, never allocate trusting input); malformed inputs are rejected, never repaired. **Uniqueness/order:** map/set sections require strictly unique, canonically ordered entries — duplicates or disorder are integrity failures. **Domain separation:** every section opens with a versioned domain-tag string; content hashes are computed over `SHA-256(domain-tag || canonical bytes)` so object types and schemas are **unambiguous** [RATIFIED-P0-3]. **Unknown versions are rejected** with the versions named in the error. **Format evolution:** additive fields require a version bump; readers never guess across versions; a migration path is a ratified future mechanism, never silent. **Floating-point configuration values:** serialized as LE IEEE-754 f64 bytes *as configuration metadata* — the default region centers are exact-rational and platform-identical; authoritative simulation state remains integer-only. **A stable format specification document is produced in Batch 1 before the save-format version is frozen.** FNV-1a fingerprints remain regression instruments, never cryptographic integrity proofs [RATIFIED-P0-3].

### 5.2 Save schema v1 (full-fidelity reference encoding)

Unchanged from v1.0: header / spine core / near+far lanes / blocked / held / ledger (full) / world blob / director + capture ledger, with derived state (`waiting_on`, `known`, `executed`, `held_index`, `max_executed_index`) rebuilt from canonical sources and equivalence proven behaviorally by continuation-invisibility tests. **Load-time integrity gates (fail-closed, typed):** schema/domain match; conservation `scheduled == executed + queued + blocked + held`; `stats.executed == ledger length`; `next_seq > max seq` with no duplicate seqs across sections; clock coherence (`max_executed_index ≤ clock`); dependency resolvability against `[0, next_seq)`; `known` density (`known == [0, next_seq)` — the dense-allocation invariant, now an explicit gate). The clock is initialized at construction from a verified save — the sanctioned P3 exception, documented, with incoherent saves rejected rather than time-traveled.

### 5.3 The local CAS protocol [RATIFIED-Correction-A — replaces v1.0 §5.3's rename-degenerate and the v1.0-review lock-file sketch]

**Locking: OS-managed exclusive file locking.** A persistent lock file `HEAD.lock` (a regular file, **never unlinked or recreated as part of ordinary lock release**) is opened once by the medium and locked with `std::fs::File::lock()` — blocking exclusive acquisition; `try_lock` for non-blocking paths. Lock ownership is OS-managed and **automatically released when the process or file handle terminates** — the crashed-writer/slow-writer ambiguity that defeated stale-lock-file deletion cannot arise: a live-but-slow writer *holds* the lock and the contender blocks; a dead writer's lock is gone by OS action, not by our deletion. (Rust 1.99.1 std provides these APIs; B2 verifies the cross-platform semantics with competing independent processes.)

**The nine-step conditional append:**

1. **Acquire** the exclusive cross-process coordination (`HEAD.lock`, OS-managed).
2. **Validate** the current storage `HEAD` fully (chain reachability from the current base anchor).
3. **Compare** with the caller's expected revision.
4. **Reject stale parents** without modifying authoritative `HEAD` (release, `RejectedStaleParent { observed }`).
5. **Durably prepare** the new immutable revision and every referenced object: each written to its content-addressed final path (`create_new` — insert-once; a retry verifies the hash and reuses), file `fsync`, directory `fsync` (Unix).
6. **Atomically publish** the replacement `HEAD`: write `HEAD.tmp`, `fsync`, rename onto `HEAD`.
7. **Synchronize** filesystem metadata: directory `fsync` after the rename (Unix); on Windows, the file `fsync` + rename ordering is the available guarantee — the known platform difference (§10), verified on the rig and in CI, with the SQLite fallback (§2.1) if B2 demonstrates a gap. **Never silently downgraded.**
8. **Release** the lock only after the operation reaches a defined outcome (Accepted / Rejected / error-with-Unknown).
9. **Reconcile ambiguous results** after crashes or I/O errors: the append either installed (HEAD reflects it — confirmed by re-read under the lock) or did not (HEAD unchanged — retry-safe by idempotency keys). An I/O error between steps 5 and 6 yields `Unknown`, never a guessed success.

**Linearization point (explicit):** the `HEAD` rename in step 6, under the held OS lock. Test-and-install cannot interleave; exactly one writer wins per expected head; every loser observes the changed head at step 3/4.

**Crash-consistency order (the verdict's list, mapped):** revision object creation (5) → snapshot object creation (5, same discipline) → journal append (pipeline T1, its own temp+fsync+append discipline, before any discard per §5.5) → temporary HEAD creation (6) → HEAD replacement (6) → directory metadata synchronization (7) → **recovery after interruption at every boundary** (B2's fault-injection matrix kills/injects at each numbered step and asserts the outcome: old-or-new, never partial; orphan objects are unreferenced and collectable; an installed HEAD is chain-valid).

**If std cannot establish the guarantee on a supported platform:** the limitation is identified in §10 with the proposed remedy (platform-specific flush implementation or the transactional storage provider fallback) — the guarantee is never silently downgraded.

### 5.3.1 Durability boundary and acceptance semantics [v1.2 — Finding C]

`LocalFileMedium`'s promises are tiered by failure class. **A guarantee is claimed only where the platform's documented operation semantics support it** — a failure need not be observed in testing for a guarantee to remain unsupported, and passing tests never create one:

| Failure class | Linux (ext4/xfs/btrfs via std) | Windows (NTFS via std) |
|---|---|---|
| Atomic visibility of HEAD/objects | **Promised** — single atomic rename | **Promised** — `MoveFileEx(REPLACE_EXISTING)` is atomic to observers |
| Concurrent-writer exclusion | **Promised** — OS-managed `File::lock()` | **Promised** — same |
| Process-crash consistency | **Promised** — immutable objects + single rename; recovery = re-read under the lock | **Promised** — same |
| OS-crash recovery | **Promised for acknowledged revisions** — the fsync sequence establishes persistence *before* `Accepted` is returned (§5.3.3) | **Filesystem consistency only — NOT acknowledged-revision persistence** [v1.3]: NTFS journaling replays to a *consistent* state, but an acknowledged HEAD rename may be absent after recovery while the previous HEAD (N−1) remains intact and internally valid — **chain validation of N−1 cannot reveal that acknowledged progress was lost** (§5.3.3) |
| Abrupt power loss | **Promised where the full POSIX sequence applies** — record fsync → `HEAD.tmp` fsync → rename → parent-directory fsync, all returning success | **NOT promised from std** — the std surface exposes no pre-rename parent-directory metadata flush (a read-only directory handle cannot `sync_all`; opening a directory for write requires platform-specific creation flags). NTFS journaling makes rename-after-fsync durable *in practice*; the *claim* is not establishable from the proposed operations — best-effort with detection-at-validation |
| Physical medium loss | **Never promised** (replicas/DR are separate concerns — P0-5, E5 T3-vs-DR) | Same |

**Exactly when `append` may return `Accepted`:** only after the full obligated sequence completes with success — records/snapshots written and fsynced (step 5), HEAD published by rename (step 6), and every metadata synchronization the platform exposes (step 7) returned success. Any error or uncertainty at any point from step 5 onward returns **`Unknown` — never `Accepted`**. `Accepted` therefore means "the obligated sync sequence completed." **What that establishes differs by platform [v1.3]:** on Linux, the full POSIX sequence establishes **acknowledged-revision persistence** across process crash, OS crash, and power loss (the documented fsync contract, not a test claim); on Windows, `Accepted` establishes the **process-crash tier** under the documented std sequence — **acknowledged-revision persistence across OS crash or power loss is not claimed from std**, because the N−1-valid/N−lost scenario (§5.3.3) is possible and undetectable by chain validation. `Accepted` is never presented as a proven Windows power-loss guarantee.

**Successful rename followed by failed or uncertain metadata synchronization:** the outcome is `Unknown`. Reconciliation re-acquires the lock and re-reads: HEAD reflecting the new revision **with every referenced object hash-verifying** ⇒ installed (the same idempotency keys make any retry a no-op at the chain level); HEAD unchanged ⇒ not installed, retry-safe. Uncertainty is resolved by observation under the lock, never by assumption. If a rename survived but record data did not (a window beyond the documented contract), **the next chain validation detects it** (record hash mismatch) and the §5.4 corruption procedure governs — detection and fail-closed recovery, never a false confirmation. The pipeline never converts `Unknown` into confirmed without the verified re-read; if verification is impossible, the milestone stays pending and gates hold.

**Definite fallback trigger:** if Stage 5 acceptance requires **sudden-power-loss durability on Windows for the local medium** (a product decision reserved to the lead), then either (a) a narrowly scoped platform-specific Windows module (a directory handle opened with write access via creation flags + `FlushFileBuffers`) behind the unchanged medium trait, or (b) the SQLite-backed local provider (whose journaling establishes the guarantee) becomes mandatory for the Windows build. The same trigger fires if B2's verifiable fault injection demonstrates a gap in any promised tier. Until it fires, the boundary above stands documented honestly and the custom medium is retained — demonstrably appropriate for atomic visibility, exclusion, process-crash, and OS-crash consistency on both platforms, and for the power-loss tier on Linux.

**Testing honesty:** process-kill fault injection verifies the process-crash tier only. The OS-crash and power-loss tiers rest on documented platform semantics, not on tests runnable on available hardware; the rig confirms observable behaviors (validation-clean loads after forced termination), never the power-loss claim itself.

### 5.3.2 Validation cost model [v1.2 — chain-validation clarification; v1.3 refinement]

Step 2's "full validation of the current storage HEAD" is **not** an O(total historical revisions) walk per commit — and, per the v1.3 refinement, **not a repeated anchor→HEAD walk either** (repeatedly re-walking the span accumulates avoidable work and is not strictly bounded until snapshot-cadence limits are established):

- **Session open performs the full-chain walk** (founding record → HEAD) — the existing validation gate (M-28); the any-verifier property is preserved (any party can always do the full walk — the audit mode).
- **Within a session, each accepted append advances the trusted cursor** [v1.3]: verifying a newly appended descendant is **O(1)** — the new record's parent must equal the cursor's verified head, and its content hash is checked; on acceptance the cursor moves to the new head (chain height + 1). The anchor→HEAD span is walked **once** (at session open, or when advancing to a new anchor), never per commit.
- **External divergence** (observed head ≠ cursor head — another writer acted, or storage changed unexpectedly): re-validate from the cursor to the observed head — bounded by the divergence length, never total history.
- **Sound invalidation:** any mismatch (record hash, parent link, epoch regression, duplicate idempotency key) discards the cursor and forces the full re-walk; if the full walk still fails, the last-valid-prefix/corruption procedure governs (fail closed). The cursor records (verified head id, head content hash, chain height); the ancestry check (parent == cursor head) is the O(1) link every append must present — cache hits are never trusted across an unverified boundary.
- **No persistent trusted-cache file** — the cursor is session-scoped derived state, recomputable by the full walk at any time. Performance characteristics: **O(1) per append; O(divergence) on external change; O(total) on invalidation and in the audit mode.**

### 5.3.3 Acknowledged-revision persistence and orphan safety [v1.3 — review Finding 3]

**Recoverable filesystem consistency is not acknowledged-revision persistence.** The review's scenario is the governing case: revision N is acknowledged; a sudden OS or machine failure occurs; the previous HEAD, N−1, remains intact and internally valid; revision N is no longer reachable. **Normal chain validation of N−1 cannot reveal that acknowledged progress was lost** — N−1 is a perfectly valid chain, and nothing in it references N. The v1.2 tier table's "OS-crash recovery: promised (NTFS journaling)" conflated these two properties; v1.3 corrects it (§5.3.1): NTFS journaling guarantees the filesystem *recovers to a consistent state* — it does not guarantee that an acknowledged rename survived.

**What `Accepted` and post-error reconciliation actually establish, per platform:**

1. **Linux (std):** the full POSIX sequence (record fsync → `HEAD.tmp` fsync → rename → parent-directory fsync, each returning success) establishes **acknowledged-revision persistence** across process crash, OS crash, and power loss — the documented fsync contract, not a test claim. `Accepted` on Linux carries that tier.
2. **Windows (std):** `Accepted` carries the **process-crash tier** only. Acknowledged-revision persistence across OS crash or power loss is **not claimed** — the N−1-valid/N−lost scenario is possible and undetectable by chain validation. Detection exists only where the loss is *structurally visible* (a surviving-but-torn record object fails its hash at the next validation); a cleanly absent HEAD rename is invisible to validation.
3. **Reconciliation establishes the tier, not current visibility:** `Unknown` outcomes are resolved by lock-held re-read + hash verification — never by assuming that visibility implies durability. Where the tier cannot be established from std, it is explicitly unclaimed.

**Orphan reconciliation and cleanup [v1.4 — erratum §E5; supersedes the v1.3 auto-delete protocol].** A hash-failing content-addressed object is **not automatically an orphan** — it may be corrupted canonical evidence referenced by the current HEAD or an earlier confirmed revision. Corrected rules, before any deletion or rewrite of a mismatching object: **(1) reference check** — walk the authoritative and recoverable history (the chain from HEAD plus retained/recoverable anchors): does any record reference this object's expected identity? **(2) referenced ⇒ fail closed and preserve the evidence** (the §5.4 corrupt-history case governs — never deletion). **(3) unreferenced ⇒ classify** — a genuinely incomplete write (a `create_new` object from a failed/interrupted preparation, with no commit record and no journal transaction referencing it) vs. a potentially important unacknowledged/ambiguous operation (an object whose write may have been part of an in-flight commit whose outcome is unknown). **(4) quarantine, never auto-delete** — questionable objects move to a quarantine area (bytes, original name, metadata preserved) and are surfaced in diagnostics. **(5) reconcile before permanent reclamation** — pending commit attempts and journal transaction identities are reconciled per §5.5.2's recovery rules before any quarantined object is permanently reclaimed; **garbage collection never destroys the only surviving evidence of an ambiguous or lost acknowledged revision**. **(6) the retry-rewrite path** applies only after the reference check confirms unreferenced **and** the journal reconciliation confirms the transaction never became durable or acknowledged — and even then the superseded bytes go to quarantine first. A session-open sweep verifies every object reachable from HEAD and quarantines (never deletes) unverified unreferenced files; referenced-and-valid objects are never touched. B2's fault injection exercises this at every write/sync boundary.

**Windows alternatives and the required evidence [v1.4 — erratum §E4; no selection made]:**

- **(a) Narrowly scoped platform-specific Windows module — UNPROVEN.** Open the campaign directory with write access via creation flags (`FILE_FLAG_BACKUP_SEMANTICS` + `GENERIC_WRITE` — not expressible through std) and issue `FlushFileBuffers` on the directory handle around the HEAD rename. Microsoft documents the flag for *obtaining* directory handles; that alone does **not** establish that flushing such a handle provides the rename-metadata durability this contract requires. **The candidate is explicitly unproven until authoritative documentation or a defensible implementation establishes the guarantee — no claim is made from it.** Cost if proven: the engine's first platform-specific code, contained in one module behind the unchanged `CanonicalMedium` trait.
- **(b) SQLite-backed local provider — the preferred production fallback.** Its journaling/sync discipline is documented and battle-tested for exactly this tier. Requirements if selected: **canonical save bytes and revision-chain semantics remain ours** (SQLite is the transactional/durability layer, never the format — chain bytes export/import unchanged); **suitable durability settings — WAL with `synchronous=FULL`** where appropriate (FULL fsyncs on every commit — the abrupt-failure/power-loss-safe setting; NORMAL trades durability for speed and does not meet the contract); and **every object required for an acknowledged canonical revision inside one transactionally consistent durability boundary** — the revision records, referenced snapshots/blobs, and the HEAD equivalent stored as in-database objects committed in the same transaction, because **making HEAD durable inside SQLite does not make separately stored, unsynchronized external blobs durable**. Cost: the bundled C build path on CI, Windows, and the macOS port (§2.1). Evidence required: the documented WAL/FULL semantics, the build-cost audit on all three platforms, and the provider passing the B2 obligations behind the unchanged trait.
- **The selection remains the lead's at the B2 gate**, after the alternatives are properly evaluated — (a) pending its missing evidence, (b) carrying documented semantics plus the build cost.

**Adopted lead recommendation (review §3):** the Windows local medium is **not fully accepted for production standalone campaigns until a justified abrupt-power-loss durability contract exists** — established by (a) or (b) above. Until then it is accepted for development and for the process-crash tier, with the boundary documented and never silently downgraded; unsupported guarantees remain explicitly unclaimed (§5.3.1's tier table is the contract of record).

### 5.4 Recovery contracts

**Graceful:** load anchor → integrity gates → exact resume of committed logical state (encounter-boundary rules govern live encounters).

**Crash:** validate chain → latest verified anchor → apply subsequent revision deltas exactly-once → labeled reconstruction of the unrepresented interval (I3 clock coherence; fail-closed to a fallback anchor) → re-drive journaled pending with the same keys → reconcile Unknowns by head re-read.

**Corrupt canonical history [RATIFIED-Clarification-C — strengthened from v1.0]:** finding a valid prefix does **not** by itself authorize rolling authoritative state back to that prefix. The system distinguishes: (a) *unconfirmed operations* — may be safely discarded; (b) *previously confirmed operations whose durability can still be established* — must be preserved; (c) *corruption that makes prior confirmation impossible to verify* — fail closed, preserve evidence, require an appropriate recovery procedure; (d) *genuine divergent histories* — detected, never merged. **Previously confirmed milestones are never silently discarded; missing confirmed records cannot be declared nonexistent.** An independent replica or previously verified backup may provide a recovery route. If no recovery path consistent with committed state can be verified: fail closed, preserve evidence, surface the recovery procedure. Divergent save directories (solo): both chains validated, divergence reported with verification results, resolution is the player's — never auto-merge.

**Conflicting revisions / schema incompatibility / storage unavailable:** as v1.0 §6.2 (detect-never-merge; explicit version rejection naming both versions; commits stay pending and gates hold — never a silent T1→T3 relabeling).

### 5.5 Durable consequence application and identity [RATIFIED-Correction-B — supersedes v1.0 §4.4 and the v1.0-review recovery rule]

**Six-phase transactional deactivation** (a recoverable all-or-nothing boundary; mutations accounted across region state, spine held-event state, checkpoints, consequential scratch, the in-memory CaptureLedger, and the T1 journal):

1. **Validate** the proposed transition without irreversible mutation (the Stage 4 Phase-0 discipline, extended to journal writability).
2. **Determine** all consequential captures and their stable identities.
3. **Persist** the necessary recovery records to T1 (journal append + fsync).
4. **Verify** the durable-write result.
5. **Only then** perform destructive or reconstructive state transitions (suspend, checkpoint, sentineled discard).
6. **On failure**, retain or restore a coherent pre-transition state — typed failure results at every phase; the permitted side effects per phase are documented and tested.

`capture()` returns typed failures; **no success-shaped receipt exists before required persistence has succeeded**. Batch 4 tests failures after *every individual operation* in the sequence, not merely before/after the whole.

**Applied-key authority:** the **confirmed canonical revision chain is the authoritative source** of which consequential changes are canonically committed. A separately persisted `applied` file is a cache / auxiliary recovery index only — it can never independently establish canonical confirmation (a v1.0 contradiction, resolved: the file is advisory and cross-checked, the chain decides). The pipeline distinguishes four key states — `InMemory` / `JournaledT1` / `AppliedToLivePending` / `InConfirmedChain` — and an effect may legitimately exist in the authorized live world while its canonical commit is pending; that is never conflated with durability. On restart, the system reconciles pending state against the confirmed chain and journal *before* deciding whether to reapply an effect. The crash windows between journal, application, and HEAD publication never produce duplication, silent loss, or false confirmation (B3's injected-failure matrix proves each window).

**Monotonic consequence identities — high-watermark over all issued durable identities:** recovery derives each region's episode frontier as the maximum across (a) the snapshot's episode counters, (b) episode values present in **confirmed chain records**, (c) episode values in **pending T1 journal entries**, (d) **restored checkpoint metadata**, (e) any persisted allocation-frontier record — plus one. **No identity previously issued into durable state is ever reused.** Retries of an existing operation retain their original idempotency key; genuinely new operations receive fresh identities. The frontier itself is journaled when advanced, so the rule is reconstructible.

**Packed-key audit and the versioned structured identity:** the current `u64` packing caps npc at 1,024 (24 headroom over today's 1,000) and region at 1,024 — too restrictive for future engine scale [RATIFIED-Correction-B]. v1.1 therefore introduces **`ConsequenceId`, a versioned structured identity** (`engine-persist/consequence-id/1`: domain-tagged fixed-width tuple — schema `u8`, region `u32`, episode `u64`, npc `u32`), used by the durable pipeline (journal, chain records, applied-key cache) from day one. The Stage 4 in-memory ledger's packed `u64` keys remain untouched — `capture-ledger/2` and every historical Stage 4 fingerprint stay byte-identical — with the triple↔packed↔structured mappings proven by **compatibility tests** and runtime bounds checks (rejection, never wrap). Migration of the in-memory keying to the structured form is deferred with a stated trigger (NPC/region scale approaching the packed bounds) and would arrive as a new versioned domain (`capture-ledger/3`) — historical behavior is never changed casually.

### 5.5.1 The prepared-transition integration [v1.2 — Finding B]

Stage 4's deactivation arm currently mutates first (`suspend_matching`, then checkpoint/capture/discard). The six-phase protocol requires a **non-destructive preparation stage**. The concrete integration:

**Mechanism — a prepared-transition object plus one read-only Spine query.** New engine-core API: `Spine::collect_matching(pred) -> Vec<ObligationView>` — a read-only enumeration of the queued/blocked obligations a subsequent `suspend_matching` with the same predicate would move (the `held_view` pattern extended; no mutation, no accounting change). The director's phases 1–2 build a **`DeactivationPlan`** from read-only sources only: the collected obligation views (EventId, due, schedule, payload, deps, lane per entry), the world's `pending_signals` scratch, cursor/positions, `represented_through`, the region's episode counter, target/from levels — **no spine mutation, no checkpoint write, no ledger capture, no scratch clear, no LOD change**. (Alternatives considered and rejected: a two-phase prepare/commit spine-suspend API would duplicate suspension logic; bare inspection without a plan object would force re-derivation at commit time and reopen mutation windows.)

**Atomic journal transaction.** All captures of one deactivation form a single T1 journal **transaction frame**: the keyed capture records, then a **transaction-commit record** carrying the transaction id, the capture count, a digest over the full record set, the complete `DeactivationPlan` (checkpoint contents and obligation identities included — KB-class), and the advanced episode frontier. The frame is written and **fsynced once at the commit record** — one durability barrier per deactivation. A transaction is **actionable iff its commit record is present and its digest verifies**; capture records without a valid trailing commit are *incomplete preparation*, inert by construction.

**Failure and crash semantics (the review's ten cases):**

1. *Collection without mutation* — the plan above; a phase-1/2 abort costs nothing.
2. *Mechanism* — the `DeactivationPlan` object + `collect_matching` (narrowly scoped: one read-only query).
3. *One deactivation as an atomic journal transaction* — the transaction frame with commit record + digest (above).
4. *Incomplete preparation vs durable actionable batch at recovery* — the commit-record rule: journal recovery groups records by transaction id; valid-commit transactions are durable actionable batches; anything else (no commit record, count mismatch, digest mismatch) is discarded — the source state was never mutated, so nothing is lost.
5. *Persistence fails after only some capture writes* — the transaction never commits; phase 4 (verify) fails; the deactivation aborts with a typed `TransitionError::JournalWriteFailed { record }`; the pre-transition state is intact (nothing was mutated); the partial records are inert and swept. **Abort, not rollback** — nothing to restore.
6. *Crash after the journal succeeds but before the region transition completes* — the durable actionable batch carries the complete plan; at restart, recovery applies it idempotently against the reconstructed state: consequence deltas exactly-once by `ConsequenceId` (applied-key reconciliation against the confirmed chain), and the region's dormant state rebuilt from the plan's checkpoint/obligation data during load — the recovery constructs the spine's held state directly from the plan's obligation identities (an id-exact reconstruction on the load path, not a live-mutation API).
7. *Typed transition failure to the simulator* — the transition path returns `Result<_, TransitionError>` (JournalWriteFailed, JournalVerifyFailed, Phase0KeyIncoherent, …); the tick boundary records the failure in the existing invalid/failed-transition diagnostics and leaves the region at its current level with obligations untouched — **proceeding with the tick is correct for pre-durable failures only** (nothing was journaled, so no divergence exists); a failure *after* durability instead fails closed per the §5.5.2 lifecycle (never continue with a durable-but-unapplied intent). No partial transition exists to advance through either way.
8. *No advancing through an invalid partial transition* — all-or-nothing: the only observable states are pre-transition (abort) or fully transitioned (success); transitions remain between ticks (the Stage 4 discipline).
9. *Retries preserve consequence IDs without duplicating application* — identity derivation is deterministic from (region, episode, npc), and the episode frontier advances **only when a transaction commits durably** (the commit record carries the new frontier); an aborted-then-retried deactivation re-derives identical keys; any duplicate application is a no-op via applied-key reconciliation; across restarts the §5.5 monotonic high-watermark governs.
10. *Restart reconciliation (chain / journal / in-memory)* — ordered: validate the chain from HEAD (§5.3.2); load the latest verified anchor → base state; scan the journal — actionable transactions not yet reflected in the chain are applied to the live state exactly-once and their commits re-driven to the medium with the **same** keys; incomplete transactions are discarded; the applied cache is rebuilt from the chain (authority) + live application state. In-memory pending that never reached the journal is honestly lost (P0-5); routine state reconstructs with labels.

**Stage 4 preservation:** the pure in-memory Stage 4 path (no pipeline attached) is untouched — historical scenarios never construct plans or journal transactions, so conservation, event identity, checkpoint validity, and every pinned fingerprint are mechanically preserved. When the pipeline is attached, the six-phase protocol governs and the new versioned domains carry the additional durable state.

### 5.5.2 The transition lifecycle and live-state ordering [v1.3 — review Finding 2]

**The principle:** a journaled operation and the authoritative live simulation must never evolve into incompatible histories. A durable intent cannot simply be abandoned while simulation continues, unless an equally durable cancellation/supersession protocol establishes that it is no longer actionable.

**The lifecycle (explicit states):**

- **`Prepared`** — the `DeactivationPlan` exists in memory; nothing durable. Freely abandonable — an abort costs nothing and the pre-transition state stands.
- **`Durable`** — the journal transaction frame's commit record is written, fsynced, and digest-verified. The intent is actionable.
- **`Applied`** — the live mutation completed exactly as the plan describes. Application is **idempotent by construction**: re-applying a compatible plan is a no-op (already-held ids stay held; already-keyed captures no-op via applied-key reconciliation; the checkpoint rewrites identical content) — so no separate completion marker exists or is needed; recovery may apply without knowing whether it already ran.
- **`Superseded`** — the intent is no longer actionable **only** through an equally durable, explicitly authorized path [v1.4 — erratum §E1]: (i) in-order recovery reaches a **causally consistent** later transition for the region (one whose source episode presupposes this intent was applied), or (ii) an explicit durable **`CancellationRecord`** (journal-sequence ordered, referencing the canceled transaction id, reason-coded) covers it. **A higher journal sequence alone never authorizes supersession**, and an unapplied transition is never archived merely because a later record exists. Superseded intents are archived, never applied.

**Binding (case 6):** the transaction-commit record carries (journal sequence, the campaign head observed at preparation, the campaign tick, region, source episode, plan digest) — the identity against which recovery checks compatibility.

**Ordering and the no-divergence discipline (cases 1–5, 7–8):**

- Phases 1–4 (prepare → journal → verify) precede any mutation. A failure there is **pre-durable** → soft abort: the region stays at its level, obligations untouched, the failure counted in diagnostics, the tick proceeds, and a later boundary may retry (re-deriving identical keys — the frontier advances only on durable commit).
- Phase 5 (apply) runs **at the same tick boundary, immediately after durability, before the next tick**, as a **staged commit bundle** [v1.4 — erratum §E2] — a strict `prepare`/`install` discipline, because a batched `suspend_ids` alone does not make multi-object mutation atomic: **`prepare` performs every fallible step and every precondition check before any authoritative mutation** — it constructs off-stage the complete replacement state for *every* affected object (the held `Pending` records from the plan's full obligation identities, the checkpoint, the ledger entries, the replacement `RegionState`, the world's post-transition scratch) and then **re-validates every precondition immediately before installation** (ids present in expected locations, none executed, ledger key-status, episode equality) — a failure here aborts coherently with nothing mutated; **`install` is non-fallible by construction** — every structure was fully built during `prepare`, so installation consists only of removals of the enumerated ids (non-allocating), insertions of the **pre-built** values into `held`/`held_index`/the ledger/the regional state (moves of already-constructed values; the only allocation involved aborts the process under Rust's failure model rather than returning a recoverable error), and pre-allocated writes/swaps — **there is no code path that installs part of the transition and returns `Err`**. The spine-side step is the narrow `Spine::suspend_ids(&[EventId])` (the id-exact batch variant of `suspend_matching`); the cross-object atomicity rests on the prepare/install construction, **not** on that method being batched. **No partial state is ever observable**: install runs inside the single-threaded tick boundary (no simulation step between install steps; snapshots never run mid-transition), and a crash *during* install kills the partially-installed in-memory state with the process — it is unobservable; recovery rebuilds from durable state and reclassifies per the rules below. **Recovery from a process crash at any stage is idempotent.**
- **Phase 5 contains no I/O.** Its only failure modes are defensive-validation failures (a detected plan-vs-live incompatibility), which are **bugs, not runtime conditions** — the correct response is a **fail-closed session halt** (stop authoritative progression, preserve state, report the evidence), never "continue with the region unchanged," which is precisely the divergence v1.2 permitted and v1.3 closes (cases 1, 5). **The simulation clock advances past a boundary only when every transition that reached `Durable` there is `Applied`** (case 4); the only durable-but-unapplied states that persist are crash states, handled by recovery.
- **Crash between journal and apply (cases 6–7, 10) [v1.4 — erratum §E1; supersedes the v1.3 highest-sequence-wins rule]:** recovery walks the journal in **sequence order** and classifies each durable transaction against the recovered state into exactly three cases. **Pre-state** — the transition was not applied: the plan's source preconditions verify (every plan obligation id unfinished and located where the plan expects; the region's episode equals the source episode; the scratch still holds the signals) ⇒ **apply the plan**. **Post-state** — the transition was already applied and the recovered state matches the plan's **expected result**: every plan obligation id is held (or consumed by later valid history), the episode has **advanced to the plan's target** (source+1 or beyond via later applied transitions), the retained checkpoint matches the plan's digest, the captured keys are present ⇒ **treat as completed, no duplicate application** — a post-state **must not be rejected merely because its episode is no longer the plan's source episode**; advanced episode metadata and suspended obligations are the signature of a correctly applied transition. **Incompatible** — neither verifies ⇒ **fail closed, preserve evidence, governed resolution**. Supersession during replay is **causal, not positional**: encountering a later transition with source episode E+1 is reachable only after E was applied or explicitly canceled (episodes advance only on durable commit *and* application); **an unapplied transition whose predecessor-relation shows a later record while it is itself neither applied nor covered by a `CancellationRecord` is itself evidence of divergence — fail closed**, never archived merely because a later record exists. Consequence preservation: pre-state application applies the keyed deltas exactly-once; post-state recognition never re-applies; incompatible-and-uncanceled never silently drops — no window loses or duplicates a consequential capture.
- **Retries vs genuinely new operations (case 8):** a retry re-uses the same (region, episode, plan digest) — the journal recognizes the transaction id and the idempotency keys; a genuinely new deactivation after a *successful* one uses episode+1 (fresh identities); after recovery, "retry" is recovery's apply-or-supersede, not a new preparation.
- **Uncertain journal-write outcome (case 9):** before declaring preparation failed, the journal write is reconciled — re-open, scan for the transaction id, hash-verify the commit record under the single-writer discipline: present and valid ⇒ the intent IS durable (proceed to apply, or fail closed); absent ⇒ not durable (soft abort). Never guessed.

A narrow, robust protocol — no general-purpose transaction framework: one plan object, one read-only query, one batched id-exact spine operation, one journal frame format, one reconciliation rule.

### 5.6 Compaction, retention, and recovery anchors [RATIFIED-Correction-C — replaces v1.0's reserved-format-tag]

**What must remain directly available:** the confirmed revision records (the durable progression lane — KB-class, retained in full); the latest recovery anchor; the allocation frontiers (`next_seq`, episode counters, applied-key high-watermark); executed-status for every identity a surviving obligation can reference.

**What can be replaced by authenticated summaries:** the per-record execution *history* below a compaction cut. The design rests on the verified dense-allocation invariant:

- `known == [0, next_seq)` **exactly** — membership is a bounds check; zero storage.
- `executed` → **the complement of unfinished [v1.4 — erratum §E3; supersedes the v1.3 bitset]**: because allocation is dense and successful obligations are never canceled, every allocated id is exactly one of Executed/Queued/Blocked/Held at every stable boundary — so **`executed == [0, next_seq) \ unfinished`**, derived from an **unfinished-id index** maintained at the two allocation/execution sites and bounded by the **live obligation count** (thousands; scales with active work, not history). Membership is O(log u) — negligible at every call site (`unmet_dependencies`, `is_executed`, ExecCtx checks); suspension/blocking/unblocking do not change unfinished-ness. Conservation becomes **structural**: `stats.executed == next_seq − unfinished_count` is the derivation's own check, and the existing conservation identity validates the invariant the derivation bakes in (no cancellation — any future cancellation lifecycle requires an explicit versioned-domain representation change; it is in no ratified scope). The bitset alternative was compared and rejected (erratum §E3's table): it grows one bit per ever-allocated id for the campaign's lifetime — 12.5 MB per 100 M events, unbounded — so **compaction with a bitset would not bound storage independently of lifetime event allocation**. **Under the selected representation the growth model is genuinely allocation-independent:** nothing persisted or in-memory grows with lifetime event allocation except fixed-size counters (`next_seq`, Stats) — confirmed revision records are bounded by milestone rate + retention, anchors by cadence + retention, the unfinished index by live work, prefix summaries by compaction count + retention. The live spine's `executed: BTreeSet<u64>` is replaced by the unfinished index with continuation-invisibility proofs — **fingerprint-invisible** (the executed set was never hashed by any Stage 0–4 domain, verified).
- The discarded record span → an **authenticated prefix summary**: a running `SHA-256` chain over the canonical record bytes up to the cut, carried in the compacted anchor. It authenticates the derivation (the retained full-fidelity reference can be hashed and compared; two compactions of the same history must agree), chains successive compactions, and preserves verifiability of *what was summarized* without retaining *the records*.

**How the Spine reconstructs its invariants:** dependency validation is unchanged and exact — `unmet_dependencies` consults the executed derivation (`id < next_seq ∧ id ∉ unfinished`); `schedule` validates dependencies against the `[0, next_seq)` bounds; identity and sequence allocation remain monotonic (frontiers serialize); cause/effect relationships remain verifiable through the retained confirmed-revision records and the prefix summary; conservation recomputes from Stats + the unfinished structures (`stats.executed == next_seq − unfinished_count`); `max_executed_index` serializes; `waiting_on` rebuilds from blocked as today. **Compaction never reintroduces duplicate execution or permits reused identities** — allocation is frontier-driven, and the no-cancellation invariant (every allocated id executed-or-unfinished, forever) is enforced structurally by the conservation gate.

**Recovery anchors:** a compacted anchor is valid iff (a) its content hash verifies (type-unambiguous, §5.1), (b) it passes **all load-time integrity gates — the semantic-completeness requirement: a matching hash alone never qualifies an anchor**, and (c) it is chain-reachable from `HEAD`. **Compaction never silently removes the only data required to reconstruct a confirmed campaign revision** — confirmed revision records are never compacted away; only execution history is summarized.

**Retention and reclamation [RATIFIED-P0-4]:** a new recovery anchor must be fully persisted **and verified** before dependent older anchors or revisions become eligible for reclamation; **the only valid recovery path is never deleted**. Numerical retention defaults are determined by Batch 5 measurement and presented for final approval — no arbitrary generation counts are invented now.

**Active readers/writers:** compaction runs under the single-writer discipline at a quiescent point (v1: session boundaries or synchronous between ticks); it is published through the same nine-step append (a compaction *is* a new snapshot revision on the chain), so the CAS protocol governs writer interaction; readers hold open handles to immutable objects, which POSIX unlink semantics and Windows open-handle semantics both keep valid through the switch (documented; B2 tests the Windows path).

**Interrupted compaction:** the unpublished anchor is an orphan object (GC); `HEAD` and the previous anchor remain valid — the atomic-publication property makes half-compacted states unrepresentable. **Detection of incomplete/inconsistent compaction:** content hash + integrity gates + chain validation — identical to any anchor. **Temporary storage:** one additional anchor's worth during compaction, bounded and measured.

**Verification and sequencing [RATIFIED-P0-2]:** the full-fidelity reference encoding is preserved so compacted recovery is compared against the original logical history (equivalence tests: an uncompacted control and its compacted image produce identical continuation behavior — scheduling decisions, executions, conservation, watermark values — and identical values in a new versioned domain `engine-core/spine-compacted/1`, which hashes the prefix summary, the cut frontier, and post-cut state; historical `spine/2`/`spine-lanes/1` semantics are untouched and remain producible from uncompacted state). Batch 1 ships full fidelity; the compaction architecture is settled here; implementation is scheduled by measured evidence — and **before final Stage 5 acceptance, either a verified bounded-retention implementation or an explicit, measured, independently reviewed justification for deferring a specified part is delivered. Indefinite-campaign storage scalability is never claimed without evidence.**

### 5.6.1 Spine interface compatibility [v1.2 — Finding A]

**The canonical distinction.** The Spine's *logical* state — what all scheduling behavior derives from — is identical under both representations: clock, `next_seq`, budget, reservation, Stats, both lane queues, blocked, held, executed-membership, `max_executed_index`. Compaction changes **only the retention of per-record execution history**: `HistoryKind::Full` retains every `ExecutionRecord`; `HistoryKind::Compacted { summarized_records, retained_records, prefix_summary }` replaces the first `summarized_records` records with the SHA-256 summary and retains the tail — **executed-membership needs no stored section at all** under the complement-of-unfinished representation (§5.6, erratum §E3): it is derived from the already-serialized unfinished structures and `next_seq`. **The authenticated prefix summary is a derivation witness, never a stand-in for the records** — it authenticates what was discarded and chains compactions; it cannot reconstruct discarded records, and nothing in this design claims interchangeability. The full-fidelity reference encoding (P0-2, retained) is the independent correctness oracle on demand.

**`ledger()` and the safe accessors [v1.3 — supersedes the v1.2 tail-on-`ledger()` position].** `ledger()` **keeps its exact historical meaning — the full execution history — on every instance; it never silently returns a retained tail.** On `Compacted` instances the full history is not retained, so the safe paths the review required lead: **`ledger_retained() -> &[ExecutionRecord]`** — the explicitly named retained-tail accessor (identical to `ledger()` on `Full`) — and **`try_ledger_full() -> Option<&[ExecutionRecord]>`** — the safe full-history request (`Some` on `Full`, `None` on `Compacted`: reports unavailability, never crashes, never fabricates). `ledger()` itself on a `Compacted` instance is a **documented contract violation surfaced loudly** (a clear panic naming the safe accessors) — the guard-rail for code that ignored `history_kind()`, not the mechanism: production code uses the capability check or the fallible accessors, and no production path trips the loud one.

**Existing fingerprint APIs [v1.3 — safe paths first].** `engine_fingerprint` / `lane_fingerprint` / `state_fingerprint` / `lane_state_fingerprint` keep their exact behavior on `Full`. For compacted instances the **safe paths lead**: new fallible variants **`try_engine_fingerprint()` / `try_lane_fingerprint()` / `try_state_fingerprint()` / `try_lane_state_fingerprint()` -> `Option<u64>`** (`None` on `Compacted`) alongside the `history_kind()` capability check — production code never needs to trip the loud path, so nothing unexpectedly crashes. The original infallible methods remain full-history APIs; calling one on a `Compacted` instance is a documented, loudly-surfaced contract violation (a clear panic naming the `try_*` remedy) — fail-visible, never a silently misleading value, and unreachable by any code that used the provided safe paths. Historical fingerprints remain computable (a) on any uncompacted instance — all Stage 0–4 scenarios are uncompacted, so every pinned baseline is mechanically preserved — and (b) on demand for a compacted campaign by loading the full-fidelity reference **where the retention policy still holds it** (§5.6.2 — not a standing guarantee). `held_fingerprint` hashes no ledger and is valid on both.

**The compacted domain.** New accessors `compacted_fingerprint()` / `compacted_state_fingerprint()` (domains `engine-core/spine-compacted/1`, `engine-core/state-compacted/1`) hash: the domain tag, clock, `next_seq`, budget, near-reserve, all Stats counters, both lane queues (canonical `(due, seq)` order, the existing record encoding), blocked, held, the prefix-summary hash, `summarized_records`, `retained_records`, the retained tail records (the existing per-record encoding), and a SHA-256 digest of the unfinished-id index [v1.4 — erratum §E3]. Comparison semantics: two compacted instances agree iff the hashed content agrees (same logical state **and** the same compaction cut); an uncompacted instance and its compacted image do **not** compare through this domain — their equivalence is established by the §5.6 verification protocol (identical continuation behavior against the reference oracle).

**Exact load-time integrity checks for compacted instances** [v1.4 — erratum §E3] (in addition to the §5.2 gates): `summarized_records + retained_records == stats.executed`; **`stats.executed == next_seq − unfinished_count`** (the structural conservation/derivation check — executed is the derived complement, so this single identity validates the derivation and the no-cancellation invariant together); every retained-tail record's seq is `< next_seq` and **not in the unfinished index** (each tail record is executed); no duplicate seqs in the tail; the prefix summary present and well-formed (32 bytes); conservation `scheduled == executed + queued + blocked + held` (with `executed == stats.executed`); `next_seq > max seq` across queues/blocked/held/tail; `max_executed_index ≤ clock` (the clock-coherence gate's compacted form); dependency resolvability against `[0, next_seq)`; `waiting_on` rebuilt from blocked and cross-checked (every registered waiter is in the blocked set).

**How previously verified history becomes a trusted compacted anchor.** Trust derives from the writer, not the summary: compaction is performed by the single authoritative writer on a live, previously verified full-fidelity spine; the summary is computed over the exact canonical bytes being discarded at that moment; the resulting anchor is published through the nine-step append (CAS + validation) and is itself content-hashed and chain-reachable. The summary authenticates the derivation — it does not reconstruct the records: if the records themselves are needed (a historical fingerprint, an audit), the retained reference is loaded. Two independent compactions of the same history must produce identical summaries (the determinism check), and each compacted anchor carries the prior summary so the compaction sequence is verifiable end-to-end from the founding record.

**Public interface changes (complete list, v1.3; v1.4 additions per erratum §E3) and historical preservation.** Additions: `HistoryKind` + `history_kind()`; `ledger_retained()`; `try_ledger_full()`; `try_engine_fingerprint()` / `try_lane_fingerprint()` / `try_state_fingerprint()` / `try_lane_state_fingerprint()`; `compacted_fingerprint()` / `compacted_state_fingerprint()`; `Spine::collect_matching` (read-only, §5.5.1); `Spine::suspend_ids` (the batched id-exact all-or-nothing suspension, §5.5.2); the save/load path's compacted encoding; **the unfinished-id index replacing the live spine's internal `executed` set (fingerprint-invisible — that set was never hashed)**. Modifications: `ledger()`'s documentation (full-history meaning everywhere; loud, documented contract violation on compacted instances); the four ledger-hashing fingerprint methods' documentation (same discipline; the `try_*` variants are the safe paths). **No existing signature changes and no behavior changes on full-fidelity instances** — every Stage 0–4 scenario runs uncompacted, so all pinned fingerprints, conservation identities, and checkpoint semantics are mechanically preserved. `is_executed`, `max_executed_due`, `min_pending_due`, `processed_through`, and all scheduling behavior are representation-independent (the unfinished index is an internal executed-membership structure with identical semantics).

### 5.6.2 Retention truth and verifiable compacted metadata [v1.3 — review Finding 1]

**Testing reference vs production retention — explicitly distinguished.** The full-fidelity reference encoding serves two roles v1.2 conflated:

- **The testing / verification oracle** — used by the B-batch equivalence tests and on-demand audits. It is **retained per retention policy** (e.g., the most recent K full-fidelity generations for equivalence testing) and is **reclaimable — never a hidden prerequisite for compaction correctness**.
- **Production retention** — the compacted chain: confirmed revision records + anchors + the unfinished structures (already part of authoritative state) + prefix summaries [v1.4 — erratum §E3; no stored executed section]. **Storage is bounded by compaction plus the retention policy; the reference is not part of that bound.** Compaction correctness rests on the writer-trusted anchor, the summary, and the load gates — not on permanent full retention.

**If the reference for a span is reclaimed, the loss is acknowledged:** the original record bytes below the cut, and any historical fingerprint of that span, are **not reconstructible**. The prefix summary remains as the authenticated witness of *what* was discarded (how much, in which compaction, chained from the founding record), but it cannot regenerate the records. Post-reclamation audits verify the chain, the summaries, and the compacted domains — never the discarded records. The design does not claim otherwise, and never silently returns partial history as complete.

**Verifiable compacted-prefix metadata for derived aggregates.** `max_executed_index` and the other derived historical aggregates (the Stats counters, `summarized_records` / `retained_records`) are **writer-computed carried values in the compacted anchor**, authenticated by the anchor's content hash and chain reachability — not recomputed from discarded records (impossible) and not merely plausibility-bounded. The load gates verify their **consistency with the genuinely retained information**: `max(max_executed_index_at_cut, max retained-tail intended_due) == the serialized max_executed_index`; `stats.executed == next_seq − unfinished_count` [v1.4 — erratum §E3: the structural derivation check]; `summarized + retained == stats.executed`. Every compacted load gate asserts only over retained information (the unfinished structures, tail, counters, queues) or over the carried values' internal and cross-consistency — **none assumes knowledge of discarded records beyond what the summary authenticates**.

---

## 6. Invariants and failure behavior

### 6.1 Invariants (v1.1 set — I-S5.8 rewritten per P0-5; I-S5.12–I-S5.14 added)

- **I-S5.1 Round-trip invisibility** — byte-level: save→load→re-encode equality; continuation equals the never-saved control at equal ticks (files and behavior).
- **I-S5.2 Conservation across load** — `scheduled == executed + queued + blocked + held`, recomputed and gated at load.
- **I-S5.3 Identity preservation** — EventId/seq, `(due, seq)` priority, lanes, schedules, payloads, `depends_on` intact through round-trips and compaction.
- **I-S5.4 Exactly-once durable application** — grounded in the confirmed chain; the `applied` cache is advisory; no duplication across retries, restarts, duplicate deliveries, or the journal→application→HEAD crash windows.
- **I-S5.5 No false confirmation** — Confirmed ⇔ CAS accepted ∧ chain-valid (validate-after-append); medium failure and Unknown outcomes leave milestones pending, never confirmed; gates hold.
- **I-S5.6 Atomic visibility** — anchors, revisions, and HEAD are fully present (hash-verified) or absent; no partial state observable.
- **I-S5.7 Fail-closed load and recovery** — typed rejections; never best-effort repair; never auto-merge; corrupt history preserves evidence and requires the recovery procedure; previously confirmed milestones are never silently discarded.
- **I-S5.8 Verified-recoverable discard (ratified wording)** — persistence transitions never discard consequential state unless the discarded information has a verified recoverable representation; confirmed progress is never falsely represented or silently lost through recovery logic. In-memory pending state is honestly labeled: it does not survive arbitrary pre-journal crashes, and nothing claims otherwise.
- **I-S5.9 Encounter atomicity** — encounter-local effects commit atomically or roll back to the pre-encounter state; the *rules* of what survives are game policy outside the journal [RATIFIED-Clarification-D].
- **I-S5.10 Reconstruction honesty** — labeled replay/synthesis only; consequential progress is never synthesized.
- **I-S5.11 No wall-clock in authoritative state** — save metadata timestamps informational; campaign time is the spine clock; save completion never advances it [RATIFIED-P0-6].
- **I-S5.12 World+character atomic revision** — a world revision and its associated player progression can never be independently rewound or advanced [RATIFIED-P0-5].
- **I-S5.13 Monotonic identity allocation** — no identity previously issued into durable state is ever reused; frontiers are durable and reconstructible from all issued-identity sources [RATIFIED-Correction-B].
- **I-S5.14 Mode honesty** — `T3-local (standalone)` and shared-canonical `T3` are distinct, labeled, and never conflated; no silent authority-mode transitions [RATIFIED-P0-1].

### 6.2 Failure scenarios

The v1.0 table stands with these strengthenings: **termination during save** — old-or-new via the nine-step protocol, every boundary tested; **corrupted checkpoints** — §5.4's four-case distinction, never silent rollback of confirmed milestones; **storage unavailable** — pending + gates hold, no T1→T3 relabeling; **conflicting revisions** — detect, never merge; **network partition during confirmation** — the `Unknown` outcome and reconciliation path (contract designed, local fault-injection exercised); **host crash + rejoin** — §5.4 recovery; **replay/duplicate commits** — exactly-once; **schema incompatibility** — explicit rejection naming versions. **Interrupted compaction** is now a first-class crash-recovery case (§5.6).

---

## 7. Stage-gated implementation plan (retained five batches; verdict-clarified acceptance)

House cadence unchanged: gates green, historical fingerprints byte-identical, new versioned domains for new state, report, **stop for independent review — no batch begins merely because the previous batch's author reports success**.

| Batch | Scope | Required acceptance (per the verdict) |
|---|---|---|
| **B1 — Canonical encoding + spine round-trip** (engine-core, additive; **no filesystem authority implementation**) | Wire-format spec document (before version freeze); Encoder/Decoder; full-fidelity save/load; derived-state reconstruction | Fixed-width versioned encoding; strict decoder; integrity + conservation gates; exact event-identity and scheduling semantics; cross-process and cross-platform byte equality; save-invisibility continuation tests; historical Stage 0–4 fingerprints unchanged |
| **B2 — Revision store + local canonical medium** (**the particularly important review gate**) | Content-addressed records; SHA-256; true cross-process CAS; crash-consistent HEAD publication | OS-managed locking (or justified alternative); the nine-step protocol with its linearization point; fault-injection matrix at every boundary; **real child-process termination tests**; competing independent processes; **no acknowledged-commit loss within the documented storage failure model**; Unknown-outcome reconciliation; complete chain validation |
| **B3 — Commit pipeline + encounter journal** | Five-state pipeline; T1 journal crash recovery; transactional encounter machinery | Explicit logical/pending/T1/confirmed distinctions; no false confirmation; idempotent retries; reconciliation of ambiguous commits; **applied-key state correctly grounded in canonical revisions and recoverable pending state**; transactional encounter commit/rollback |
| **B4 — Application integration** | NpcWorld/director codecs; the six-phase crossing; the demo encounter | Durable-before-discard transitions; **monotonic consequence identity recovery**; checkpoint and held-obligation integrity; pending/confirmed UI demonstration; irreversible-action gating demonstration; correct encounter-boundary behavior; **conservation across crashes, recovery, and regional transitions**; per-operation failure injection |
| **B5 — Lifecycle, CI, performance, documentation** | End-to-end lifecycle; measurement; rig plan; final report | End-to-end save/restart/recovery tests; **actual process-kill tests**; cross-platform CI; corruption and divergence detection; measured snapshot/journal/commit/recovery latency (60/144/240 FPS implications included); **quantified ledger growth and compaction readiness**; retention and recovery-cost recommendations; Windows-rig evidence; formal acceptance report |

**Deviations from the v1.0 plan (explained per the verdict §7.5):** none to the batch structure. Compaction *implementation* is scheduled by measured evidence (§5.6) — it lands within B5 or a bounded follow-up batch per the lead's decision at the B4/B5 gate, with the P0-2 acceptance condition (verified bounded retention or reviewed deferral) due before final Stage 5 acceptance. The ratification record (`eb79d1d`) is a new documentation artifact on this branch, not a plan deviation.

---

## 8. Testing and benchmarking strategy

v1.0's strategy stands, strengthened per the verdict: **real child-process contention and termination** (not just in-process handles) in B2; **failure injection after every individual operation** in the six-phase deactivation (B4); the corruption corpus, E5-Lab T1–T5 ports, differential mutation testing, and the independent test-only reference decoder (Clarification A) throughout; the Unknown-outcome fault injections (Clarification B); compaction equivalence tests (§5.6). **Performance measurement now explicitly includes 60/144/240 FPS implications, not only the 16.7 ms frame budget** [RATIFIED-P0-6]: snapshot, journal, commit, and recovery latency measured (pod instruments; rig authoritative), with a proposed asynchronous architecture returned *with evidence* only if measurements demonstrate unacceptable stalls — threading never introduced without review. Save completion never advances the campaign clock; acknowledgments bind to revisions + keys; completion order never overrides revision order; pending stays visible while writes are outstanding.

---

## 9. Decisions register

### 9.1 Ratified (settled — recorded in the ratification doc, incorporated above)

| ID | Decision | Where incorporated |
|---|---|---|
| P0-1 | Solo T3 semantics with explicit campaign-mode distinctions; founding metadata; no silent transitions; conversion compatibility designed now | §3.3, §4.2, I-S5.14 |
| P0-2 | Full-fidelity reference encoding first; compaction designed now; verified bounded retention or reviewed deferral before final acceptance | §5.2, §5.6, §7 |
| P0-3 | SHA-256 via `sha2` in `engine-persist`; engine-core zero-dep; versioned canonical bytes; verified loads; unambiguous addressing; integrity ≠ authenticity | §2, §5.1, §5.3 |
| P0-4 | Adaptive progress-aware persistence (milestone commits + measured-criteria recovery snapshots); verify-before-reclaim; no invented generation counts | §4.3, §5.6, §8 |
| P0-5 | Five precise durability classes with the governing guarantee; session/encounter-boundary policy preserved; world+character atomicity | §3.2, I-S5.8, I-S5.12 |
| P0-6 | Measure-first, synchronous initial implementation; I/O-worker ownership boundaries designed; 60/144/240 evaluation | §3.4, §8 |
| Correction A | OS-managed locking; nine-step CAS; crash-consistency order; SQLite reevaluation | §2.1, §5.3 |
| Correction B | Six-phase transactional deactivation; chain-authoritative applied keys; monotonic identities over all issued durable identities; key audit + structured identity | §5.5 |
| Correction C | Real compaction design; semantic anchor validity; retention rules; interrupted-compaction recovery | §5.6 |

### 9.2 P1 — open for the lead's decision before the relevant gates (recommended defaults + alternatives)

| # | Question | Recommendation | Alternative | Rationale |
|---|---|---|---|---|
| P1-1 | Save-location platform policy | OS-conventional per-user directories via a platform-path helper (`%APPDATA%`/XDG data home/`~/Library/Application Support`), campaign subdirectories per campaign id | Executable-relative portable saves | Per-user is the convention players and backup tooling expect; portable mode is additive later and needs no format change |
| P1-2 | Save-migration framework horizon | v1 ships the version gate only (explicit rejection naming versions); migration framework deferred with trigger (first shipped schema change) | Build migration tooling now | No shipped schema exists to migrate from; premature tooling has no test substrate |
| P1-3 | Signer/identity fields in solo | Structured signer field carrying a documented `local-unverified` placeholder — never presented as authentication | Omit the field until multiplayer | The chain format must be stable before the version freeze; the placeholder is explicit and labeled (P0-3's integrity≠authenticity) |
| P1-4 | The demo encounter's boundary definition | A bounded per-NPC resource trio mirroring M-30's categories (consumable, equipment-condition analog, and a use-earned durable accrual) — demonstrative, not a gameplay system | Richer encounter model | Sufficient to prove commit/rollback/durability behavior (the verdict's scope); policy stays in the demo |
| P1-5 | The TECH-ARCH §6 Stage 5 row's systems content (workloads, visible movement) | A separate scope question for a later stage — Stage 5 remains persistence + minimal demo | Fold into B4/B5 | The verdict's scope discipline (Clarification E); the §6 row predates the persistence-focused authorization |
| P1-6 | Retention/generation numerical defaults | Deferred to B5 measurement + final approval (per P0-4) | Fixed defaults now | The verdict explicitly forbids inventing arbitrary generation counts |
| P1-7 | In-memory ledger migration to structured keys | Deferred with a stated trigger (scale approaching the packed 1,024 npc/region bounds); durable layer uses structured ids from day one | Migrate the in-memory ledger in B4 | Preserves `capture-ledger/2` and all historical Stage 4 fingerprints; the trigger is measurable |
| P1-8 | Windows directory-fsync limitation handling | Document + rig/CI verification of the std file-fsync + rename ordering; escalate to the SQLite-backed provider only if B2 demonstrates a gap | Platform-specific flush code now | The verdict's "do not silently downgrade" is satisfied by evidence-first escalation; premature platform code violates the anti-speculation rules |

---

## 10. Assumptions and limitations register (honest, labeled)

| Item | Status | Verification path |
|---|---|---|
| `std::fs::File::lock()` cross-platform semantics (auto-release on process death; persistent lock file never unlinked) | [ASSUMPTION] from std documentation | B2 competing-process tests on Linux + Windows CI + rig |
| Windows acknowledged-revision persistence across OS crash / power loss is **not claimed from std** — the N−1-valid/N−lost scenario is possible and undetectable by chain validation; filesystem consistency ≠ acknowledged-revision persistence (§5.3.3) | Documented boundary [v1.3 — review Finding 3; v1.4 — erratum §E4] | The Windows medium is **not fully accepted for production standalone campaigns until a justified abrupt-power-loss durability contract exists** — the platform-specific directory-flush module is **UNPROVEN** (documented handle acquisition ≠ documented flush durability); **SQLite is the preferred production fallback** (WAL + `synchronous=FULL`, all revision objects inside one transactional boundary); the lead selects at the B2 gate |
| Process-kill tests verify the process-crash tier only; the OS-crash and power-loss tiers rest on documented platform semantics — a failure need not be observed in testing for a guarantee to remain unsupported, and passing tests never create one | Testing-honesty rule [v1.2 — Finding C, retained] | §5.3.1/§5.3.3; the rig confirms observable behavior, never the power-loss claim |
| `std::fs::rename` replaces existing files on Windows | [ASSUMPTION] from std docs | B2/B5 on Windows CI + rig |
| `sha2` builds cleanly on both platforms (pure Rust) | Crate nature | B2 CI |
| Commit records KB-class; anchors MB-class at slice scale | [ASSUMPTION] (E5 §6) + measured ledger sizing | B5 measurements |
| E5-Lab results = protocol-model behavior against an in-memory simulated medium; **not yet demonstrated**: real third-party CAS semantics, persistence acknowledgments, network partitions, credentials, read-after-write, quotas, provider suitability; **new Stage 5 obligations**: actual filesystem durability, process-level contention, crash injection, local journal recovery, correct local CAS. The simulated results are never presented as proof of a production storage provider; Cloudflare R2 remains experimental, the real-provider probe gated | [RATIFIED-Clarification-F] | Retained verbatim in spirit; B2/B5 discharge the local obligations |
| The networked provider as a future drop-in | **An architectural objective, not a proven equivalence** — providers may expose materially different consistency, acknowledgment, and failure semantics | The Unknown-outcome interface + per-provider qualification (future E5 probe) |
| Dense `[0, next_seq)` allocation underlies compaction | [PROJECT EVIDENCE] verified in `spine.rs` at `2e71770` | B1 gate asserts it on every load |
| Config f64 values exact-rational and cross-platform identical | Verified for the default ring layout | B1 cross-platform round-trip tests |
| Presence scenarios/camera input are inputs, not campaign state — excluded from saves | Design decision (v1.0) | B4; the lead may override |
| Construction-time clock initialization as the sanctioned P3 exception | Design decision (v1.0 §5.2) | Ratified in this review cycle; documented in B1 |
| Every unconfirmed consequence surviving every crash is NOT promised — in-memory pending is honestly bounded | [RATIFIED-P0-5] | Stated; the governing guarantee is I-S5.8 |

---

## 11. Requirement → acceptance-test traceability

| Requirement | Tests (batch) |
|---|---|
| R1 durable persistence / atomicity / corruption / migration / safe in-flight+held+reconstruction state | B1 round-trip + gates; B2 fault matrix + child-process kills; B5 end-to-end restart/recovery + process-kill |
| R2 Stage 0–4 guarantees preserved | B1 historical fingerprints byte-identical; conservation/identity gates; B4 Stage 4 suite untouched-green |
| R3 four→**five** state distinctions | B3 state-machine tests incl. no-false-confirmation under injected medium failure; B4 UI demonstration |
| R4 milestone acknowledgment / gating / retry idempotence | B3 retry storms + lost-ack + Unknown reconciliation; B4 gating demo |
| R5 multiplayer foundation / authority modes | B2 CAS + chain validation; B3 mode labeling; founding metadata in B2 |
| R6 efficiency | B5 latency measurements (60/144/240); storage growth quantification; compaction readiness |
| R7 failure/adversarial scenarios | B2 boundary-by-boundary injection; B3 journal crash recovery; B4 per-operation failure injection; B5 corruption/divergence detection |
| R8 engine/game separation | B4 (policy outside the journal); review inspection |
| R9 platform independence | B1 cross-platform byte equality; B2 Windows CI; no OS-specific APIs outside the medium layer |
| R10 slice acceptance (restart round-trip, double-commit no-op, pending/confirmed visible, encounter rollback) | B5 end-to-end |
| Correction A (CAS + crash safety) | B2: competing independent processes; nine-step boundary injections; linearization-point proof |
| Correction B (durable crossing + identities) | B4: six-phase per-operation failures; B3: applied-key grounding; identity high-watermark recovery tests |
| Correction C (compaction) | B5 (or the follow-up batch): equivalence tests vs full-fidelity reference; interrupted-compaction recovery; anchor semantic-validity gates |
| Finding A (compaction / Spine interface) | §5.6.1 spec; B-batch tests: `history_kind()` contract, fingerprint unavailability on compacted instances (fail-visible), compacted-domain equality at equal cut, the exact compacted load-gate list, reference-oracle equivalence |
| Finding B (durable deactivation boundary) | §5.5.1 spec; B4 tests: the ten cases — per-operation failure injection, journal-transaction commit-record recovery, crash-between-journal-and-transition reconstruction, retry identity preservation, restart reconciliation |
| Finding C (durability boundary) | §5.3.1 spec; B2 tests: process-kill tier + competing processes + Unknown reconciliation; the power-loss boundary documented, never test-claimed |
| Chain-validation cost | §5.3.2 spec (v1.3): O(1) per-append cursor advancement (parent == cursor head); the span walk once at session open; divergence-bounded re-walks; invalidation forces the full walk |
| v1.3 Finding 1 (public API + retention truth) | §5.6.1/§5.6.2 spec; tests: `ledger()` loud-fail on compacted, `ledger_retained()`/`try_ledger_full()` contracts, `try_*` fingerprint safety (no unexpected crashes), carried-aggregate consistency gates, reference-reclamation honesty (records + span fingerprints not reconstructible) |
| v1.3 Finding 2 (transition lifecycle) | §5.5.2 spec; tests: staged `suspend_ids` all-or-nothing application, fail-closed on post-durable validation failure (no continue-with-unapplied-intent), idempotent re-application, supersession-ordered recovery, uncertain-journal reconciliation before declaring preparation failed |
| v1.3 Finding 3 (acknowledged persistence) | §5.3.3 spec; tests: orphan cleanup/retry at every write/sync boundary, hash-verify-or-rewrite; the Windows durability-contract decision (module vs SQLite) at the B2 gate with the required evidence |
| Erratum §E1 (recovery/supersession) | §5.5.2 (v1.4); B3 tests: pre/post/incompatible classification incl. a post-state with advanced episode (recognized, not rejected); in-order replay; CancellationRecord handling; a later record with an unapplied-uncanceled predecessor → fail closed; no-loss/no-duplication through every window |
| Erratum §E2 (all-or-nothing mutation) | §5.5.2 (v1.4); B4 tests: precondition failure at every fallible step aborts coherently; install non-fallibility audit (no recoverable-error path); mid-install process-kill → recovery rebuilds from durable state; the clock never advances through an inconsistent transition |
| Erratum §E3 (complement-of-unfinished) | §5.6/§5.6.1/§5.6.2 (v1.4); B1 tests: continuation-invisibility vs a BTreeSet control; the structural conservation/derivation gate; identical dependency semantics incl. blocked-on-held; fingerprint-invisibility (the executed set never hashed); B5: long-campaign growth curves flat in executed history |
| Erratum §E4 (Windows candidates) | §5.3.3 (v1.4); B2: the platform module carries NO claim until proven (authoritative documentation or defensible implementation); the SQLite provider (if selected) under WAL+synchronous=FULL with all revision objects in one transactional boundary + chain-bytes export/import equality |
| Erratum §E5 (safe orphan rules) | §5.3.3 (v1.4); B2 tests: referenced-corrupt → fail-closed evidence preservation; unreferenced → quarantine-not-delete; reconcile-before-reclamation; rewrite only after both checks; GC never destroys the only surviving evidence |

---

## 12. Design-decision and correction log (v1.0 → v1.1)

| v1.0 position | v1.1 position (source) |
|---|---|
| Four-state commit model; "pending" conflated in-memory and T1 | Five states with the ratified governing guarantee; in-memory pending honestly bounded (P0-5) |
| §5.3 "the local single-writer case degenerates to a compare-and-rename"; review-response lock-file sketch with stale-lock deletion | OS-managed `File::lock()` on a persistent lock file; nine-step protocol; explicit linearization point; stale-lock deletion rejected as unsafe (Correction A) |
| `applied` file persisted explicitly and cross-checked (co-authority) | The confirmed chain is the sole applied-key authority; the file is a cache (Correction B) — v1.0 internal contradiction resolved |
| Monotonic recovery from snapshot + applied-max only | High-watermark over ALL issued durable identities (Correction B) |
| Packed `u64` consequence keys, unaudited | Bounds audit (npc cap 1,024 — 24 headroom) + versioned structured `ConsequenceId` in the durable layer; in-memory migration deferred with trigger; compatibility tests (Correction B) |
| Compaction = reserved `PrefixHash+Tail` format tag | Full compaction design on the dense-allocation insight: derivable `known`, bitset `executed`, authenticated prefix summary, anchor semantic validity, retention rules, interrupted-compaction recovery (Correction C) |
| Snapshot cadence proposal with K/G numbers | Criteria-based adaptive persistence; numbers from B5 measurement + final approval (P0-4) |
| P0 questions open | All six RATIFIED with their requirements; P1 separated with recommendations (P0-1…6) |
| SQLite rejected partly on internal-representation grounds | Re-evaluated across the verdict's seven dimensions; custom-with-evidence recommended, SQLite the identified fallback (Correction A) |
| Medium result = Accepted/Rejected | `Unknown` outcome class + reconciliation; local fault injection; drop-in = objective, not proven equivalence (Clarification B) |
| Last-valid-prefix fallback described simply | Four-case corrupt-history distinction; confirmed milestones never silently discarded (Clarification C) |
| Wire format allowed `usize` (write_usize) | Fixed-width u64 wire types, maxima, uniqueness, domain separation, stable spec before version freeze (Clarification A) |

**v1.1 → v1.2 (review findings A–C + the chain-validation clarification):**

| v1.1 gap | v1.2 position (source) |
|---|---|
| Compaction left `ledger()` and fingerprint behavior on compacted instances unspecified | §5.6.1: `HistoryKind`; `ledger()` returns the retained tail, detectable by contract; the four ledger-hashing fingerprints are explicitly unavailable on compacted instances (fail-visible) with the full-fidelity reference as the on-demand retained representation; the compacted domain defined with exact comparison semantics; the exact compacted load-gate list; the writer-derived anchor trust model (the summary is a derivation witness, never a record substitute); the complete public-interface change list (Finding A) |
| The six-phase protocol lacked a non-destructive preparation stage, transaction framing, and crash semantics | §5.5.1: `DeactivationPlan` + the read-only `Spine::collect_matching`; journal transaction frames with commit records + digests (one fsync per deactivation); the ten failure/crash/retry/reconciliation cases; Stage 4 preservation on the pipeline-free path (Finding B) |
| Durability promises risked conflating process-crash with power-loss; the Windows boundary was implicit | §5.3.1: the six-class failure tiering per platform; the exact `Accepted` condition; rename-then-failed-sync → `Unknown` with lock-held re-read reconciliation; the Windows power-loss honesty (not promised from std, best-effort with detection-at-validation); the definite fallback trigger; the testing-honesty rule (Finding C) |
| "Full validation of HEAD" could be read as O(total historical revisions) per commit | §5.3.2: session-open full walk + anchor-bounded incremental validation with sound invalidation; no persistent trusted cache — the any-verifier full walk always available as the audit mode (chain-validation clarification) |

**v1.2 → v1.3 (review consistency findings 1–3 + the validation-cost refinement):**

| v1.2 gap | v1.3 position (source) |
|---|---|
| `ledger()` returned the retained tail on compacted instances (a documented but real semantics change); the fingerprint APIs' only compacted behavior was a panic | §5.6.1 amended: `ledger()` keeps full-history meaning on every instance (loud, documented violation on compacted — never a silent tail); the safe paths lead — `ledger_retained()` (explicitly named), `try_ledger_full()`, and `try_*` fingerprint variants (`Option`, never an unexpected crash) with `history_kind()` detection (Finding 1) |
| The full-fidelity reference was ambiguously "retained" — a hidden-prerequisite risk; `max_executed_index` was gated only plausibility-bounded | §5.6.2: the testing oracle vs production retention distinguished (the reference reclaimable, never a correctness prerequisite; storage bounded by compaction + policy); reclaimed-span losses acknowledged (record bytes + span fingerprints not reconstructible); aggregates as writer-computed, chain-authenticated carried values with retained-info-consistency gates (Finding 1) |
| A post-durable application failure could leave the region unchanged while the tick proceeded — durable intent vs live history divergence | §5.5.2: the explicit lifecycle (Prepared / Durable / Applied / Superseded); staged all-or-nothing application via `suspend_ids`; post-durable failure = fail-closed session halt; the clock advances only past `Applied`; idempotent application (no completion marker needed); binding to (journal sequence, head, tick, episode, plan digest); ordered supersession-based recovery; uncertain-journal reconciliation before declaring preparation failed; §5.5.1 case 7 amended to the pre/post-durable distinction (Finding 2) |
| "OS-crash recovery: promised (NTFS journaling)" conflated filesystem consistency with acknowledged-revision persistence | §5.3.1 amended + §5.3.3: the N−1-valid/N−lost scenario stated (undetectable by chain validation); Linux carries acknowledged-revision persistence via the POSIX sequence, Windows does not claim it from std; the orphan-cleanup/retry protocol (hash-verify-or-rewrite under the lock + session-open sweep); the Windows alternatives (platform-specific module vs SQLite) with required evidence; the adopted lead recommendation — Windows production-standalone acceptance requires the durability contract (Finding 3) |
| Per-append validation could be read as repeated anchor→HEAD walks (accumulating avoidable work, unbounded until cadence set) | §5.3.2 amended: the trusted cursor advances per accepted append — O(1) verification of each new descendant (parent == cursor head); the span walk happens once at session open; divergence-bounded re-walks; sound invalidation; explicit performance characteristics (validation-cost refinement) |

**v1.3 → v1.4 (the correctness erratum — [`STAGE5-V1.3-ERRATUM.md`](STAGE5-V1.3-ERRATUM.md), same commit):**

| v1.3 gap | v1.4 position (erratum) |
|---|---|
| Recovery only checked pre-state compatibility; a correctly applied transition would be misread as incompatible; a higher journal sequence alone authorized supersession | §E1 → §5.5.2: the three-case recovery classification (pre-state apply / post-state recognize — an applied plan's signature is advanced episode + held ids, never rejected for episode inequality / incompatible fail-closed); in-order replay; causal supersession only (a later record presupposes the earlier applied or explicitly canceled via `CancellationRecord`); an unapplied transition with a later record but neither applied nor canceled is itself divergence evidence — fail closed |
| The atomicity claim rested on `suspend_ids` being batched | §E2 → §5.5.2: the staged **prepare/install** commit bundle — every fallible step and precondition in `prepare` (off-stage construction of all affected structures + immediate pre-installation re-validation); `install` non-fallible by construction (removals, moves of pre-built values, abort-on-allocation) — no code path installs partially and returns `Err`; no observation point between install steps; mid-install crash unobservable (memory dies; recovery rebuilds from durable state); crash recovery idempotent |
| The executed bitset grew one bit per ever-allocated id for the campaign lifetime — compaction did not bound storage independently of lifetime event allocation | §E3 → §5.6/§5.6.1/§5.6.2: **complement-of-unfinished selected** (`executed == [0, next_seq) \ unfinished` from an unfinished-id index bounded by live work; O(log u) membership; structural conservation; the no-cancellation invariant gated; the live `executed` BTreeSet replaced fingerprint-invisibly) — the growth model is now genuinely allocation-independent, stated explicitly |
| The Windows directory-flush module was presented as able to establish the tier "per documented platform semantics" | §E4 → §5.3.3: the candidate is **UNPROVEN** (documented handle acquisition ≠ documented flush durability — no claim made); **SQLite is the preferred production fallback** (canonical bytes and chain semantics remain ours; WAL + `synchronous=FULL`; every revision object inside one transactional boundary — HEAD-durable-in-SQLite does not make external unsynchronized blobs durable); the lead selects at B2 |
| The orphan protocol auto-deleted hash-mismatching objects | §E5 → §5.3.3: reference check first; referenced ⇒ fail closed + preserve; unreferenced ⇒ classify; **quarantine, never auto-delete**; reconcile pending commits + journal identities before permanent reclamation; GC never destroys the only surviving evidence of an ambiguous or lost acknowledged revision |

---

*Prepared 2026-10-20 as v1.4 on `stage5-design` (v1.3 preserved at `a11d154`; v1.2 at `a883f96`; v1.1 at `deb8acb`; v1.0 at `68e42e2`; ratification record at `eb79d1d`; the v1.3 erratum at [`STAGE5-V1.3-ERRATUM.md`](STAGE5-V1.3-ERRATUM.md) in the same commit). Every remaining judgment above is [PROPOSAL] pending independent review; Stage 5 implementation remains unauthorized until the revised design passes review **and** Stage 4 receives final acceptance.*
