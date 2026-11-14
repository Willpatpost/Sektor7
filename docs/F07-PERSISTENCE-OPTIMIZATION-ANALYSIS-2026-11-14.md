# F-07 — Persistence Throughput Interpretation and Optimization Analysis

**Agent B (coder-a8), 2026-11-14** — the post-audit remediation directive's §7
assignment ("Convert persistence measurements into actionable engineering
decisions"), delivered read-only-plus-analysis on the branch
`s6-remediation-b-persistence-ci` from the mainline baseline `a4fe0ec`
(the audit-cycle consolidated tree; the six-suite harness itself delivered at
`94516bb`, CI `43443797941` GREEN ×5, now integrated).

**Status: MEASUREMENT COMPLETE + DESIGN READY.** No production persistence
code is changed by this document. Every optimization below is PROPOSED with
its evidence, boundary, and regression requirements; none is implemented —
the larger production changes remain subject to the applicable design and
implementation authorization (the directive's own boundary).

---

## 1. What was measured, and where

The six suites (`engine/crates/engine-slice/tests/audit_f07_persistence.rs`)
measure the **production topology as it stands**: one `IoWorker` thread owning
the ONE `SessionStore` connection (SQLite, `journal_mode=WAL` +
`synchronous=FULL` — both set and verified by querying the actual database at
open), the bounded FIFO queue (capacity 64), and the `WorkerMedium` proxies
(every proxied operation = one job + a block on the private one-slot reply
channel). Zero production-code changes; the deep runs scale every suite
through the `F07_OPS` environment variable.

**Environments.** The CI numbers (the ones the directive quotes: ~1,295
sequential ops/s, contention p99 ~128 ms, cold recovery ~34.7 ms, ~19×
head-of-line) are **debug-mode, 1× scale (200 operations), this pod's
ephemeral overlay filesystem**. The 10×/100× characterization (2,000/20,000
operations) is **release-mode on the same overlay filesystem**. None of these
are universal engine performance guarantees — they characterize one
storage-and-CPU environment under one build configuration. A cold re-run of
the 1× battery on 2026-11-14 (during parallel agent builds on the same pod)
measured sequential throughput at 667 ops/s where the audit-time run measured
1,295 ops/s — a ~2× environment variance with **zero code changes**, which is
exactly why every number here is observational evidence, never a CI gate
(the F-11 remediation's principle, now structural).

**The raw measurement table** (release mode unless noted; the 1× debug column
is the CI-scale environment):

| Measurement | 1× debug (CI scale) | 10× release | 100× release |
|---|---:|---:|---:|
| Sequential proxied mean | ~772–1,498 µs | 470 µs | 448 µs |
| Sequential throughput | 667–1,295 ops/s | ~2,123 ops/s | ~2,225 ops/s |
| Sequential p99 | ~7.8 ms | ~850 µs | ~850 µs |
| Direct provider mean | ~1,296 µs | 431 µs | 403 µs |
| Direct throughput | 771–1,415 ops/s | ~2,314 ops/s | ~2,472 ops/s |
| Queue overhead (sequential − direct) | ~9% | ~9% | ~9% |
| Contention (8 producers) aggregate | 462–612 ops/s | 1,349 ops/s | 1,458 ops/s |
| Contention CAS retries | ~4.2–4.7/op | 72,382 (~4.5/op) | 770,420 (~4.8/op) |
| Contention p99 | ~128–138 ms | (measured; collision-dominated) | (measured) |
| Cold reopen | ~36 ms | 57 ms | 295 ms |
| Cold full validate | ~15.7 ms | 25.8 ms | 243.8 ms |
| Recovery per record | — | ~12 µs | ~12 µs |
| Head-of-line amplification | ~19–24× | (measured) | 528× (259.6 ms behind a 20k-record validate vs 492 µs baseline) |
| Retained WAL size | 0 B | 0 B | 0 B (every sampled point) |
| Retained growth | ~106 KB/200 ops | — | ~356 B/record (7.2 MB at 20k) |
| Explicit TRUNCATE checkpoint | ~2.8 ms | 2.3 ms | 5.7 ms |

---

## 2. What each number includes and excludes (the §7 validation)

### §1 Sequential baseline (proxied)

**Includes:** the caller-side record construction is EXCLUDED (constructed
before timing); the measured wall time spans the proxy's admission (the
bounded `sync_channel` send), the queue wait, the worker's dequeue, the
poison/cancellation boundary checks, the `SessionStore::append` execution
(the SQLite transaction: row inserts, the `meta.head` update, the COMMIT),
and the reply deposit + the caller's `recv`. The pipelined form (the previous
append's identity is the next parent) eliminates any head round trip, so each
sample is ONE append = one queue round trip + one commit.

**Excludes:** the founding publication (before the measured loop), any
concurrent load (one producer, an otherwise-empty queue), any validate or
checkpoint (maintenance ops never interleave in this suite).

**Therefore:** the sequential mean is the honest per-append floor of the
production path — queue round trip + serialized commit/fsync under
`synchronous=FULL`. It is NOT the spine's per-tick cost (a tick amortizes at
most a few publications); it is the marginal cost of one canonical
publication.

### §2 Direct-provider baseline

**Includes:** the same append workload against a plain `SqliteMedium` handle
(no worker, no queue): the store's append execution + the COMMIT — under
`synchronous=FULL` each append's COMMIT is an fsync of the WAL, so this
distribution IS the commit/fsync distribution.

**Excludes:** all queue mechanics, the worker thread hop, the reply channel.

**Therefore:** the ~9% gap between §1 and §2 is the queue overhead
(admission + thread hop + reply), measured on the same store shape. The
commit/fsync component is ~90% of every proxied append — the single dominant
cost, and the one the durability contract requires.

### §3 Controlled contention (8 producers)

**Includes:** eight producer threads, each with its own proxy over the ONE
worker, each appending with the CAS retry discipline (a `RejectedStaleParent`
re-derives the head — the realistic contended publication pattern). Each
producer's per-op wall time includes its own queue admission, the queue wait
behind up to 7 siblings' jobs, the execution, and — critically — the retry
loop's repeated head-read + append round trips after each CAS rejection.

**Excludes:** external writers (no connection outside the worker exists);
the queue never saturates (each producer blocks on its own reply, bounding
in-flight jobs at 8 of 64).

**Therefore:** the aggregate rate (1,349–1,458 ops/s release, BELOW the
sequential 2,225) and the CAS retry counts (~4.5–4.8 per accepted append,
770,420 retries at 100×) characterize the **collision-dominated regime**:
the collision window is the head-read → append round trip, which under the
worker topology costs TWO queue round trips (the `Head` job, then the
`Append` job) — during which the other producers' appends advance the head
and force the rejection. The p99 (~128 ms at 1× debug) is the queue-wait +
retry-storm tail, not a store limitation.

### §4 WAL growth and checkpoint pause

**Includes:** the wal file's retained size sampled mid-run (every 64 ops)
and after the load; the explicit `checkpoint()` (the provider's
`PRAGMA wal_checkpoint(TRUNCATE)`) routed through the queue, timed as a
queued operation.

**Excludes:** forced checkpoint pressure (no readers exist to block a
checkpoint; the single connection owns the wal).

**Measured facts:** the retained wal stayed **0 B at every sampled point
through 100×** — SQLite's default auto-checkpoint behavior (the 1,000-page
threshold, with no concurrent readers to hold the wal) resets it during the
run; retained growth is ~356 B/record in the main db file; the explicit
TRUNCATE costs 2.3–5.7 ms. **Inference (stated as inference):** at the
measured write rates the WAL never accumulates unboundedly and the
maintenance checkpoint is a millisecond-class queued operation — the WAL
maintenance class is NOT a measured risk at engine-representative rates.

### §5 Cold reopen recovery

**Includes:** after a clean worker shutdown (the channel closed, the thread
joined), a FRESH worker spawn over the same database (the provider open +
the WAL/synchronous verification + the busy-timeout install), the first
`head()` (one queue round trip), and the full authoritative-chain validation
(the O(chain) authenticated walk over every record).

**Excludes:** crash recovery (this is the CLEAN-shutdown reopen; the
crash/interruption matrix is the fault-seam corpus's territory, not this
benchmark's), and any concurrent load during recovery.

**Measured facts:** reopen 57→295 ms (10×→100×), full validate 25.8→243.8 ms
— **linear at ~12 µs/record**. The recovery class is bounded and linear;
at 20,000 records the one-time startup cost is a quarter-second.

### §6 Head-of-line

**Includes (§6b, observational):** the baseline append's latency; then the
full-chain validation submitted fire-and-forget (the worker's test-only
`submit_probe`) with one append timed behind it — the behind-append's
latency includes whatever remains of the in-flight validation's execution.

**Excludes:** any guarantee that the behind-append actually lands behind
the validation at small scales (if the validation finishes before the append
is admitted, the sample measures an unencumbered append — an honest
measurement, never a failure; the F-11 remediation removed the wall-clock
assertion and §6a now proves the queue-ordering correctness
deterministically).

**Measured facts:** the amplification is O(chain): ~19–24× at 200 records
(debug), 528× at 20,000 (release: 259.6 ms behind the 20k-record validate vs
a 492 µs baseline). **The head-of-line source is exclusively the O(chain)
operations** (the full validation); the per-op queue round trip (~9%) is
never a head-of-line factor at these rates.

---

## 3. The cost decomposition — what blocks the authoritative simulation

Separating the directive's cost classes and locating each against the
authoritative spine:

| Cost class | Where it lives | Blocks the authoritative simulation? |
|---|---|---|
| Required canonical durability work (the append's transaction + COMMIT/fsync) | The worker thread, inside `SessionStore::append` | **YES — on the publishing caller's thread.** The proxy blocks on the reply; the commit is ~90% of each append's ~403–448 µs (release). At a 60 Hz tick budget (16.6 ms) this admits ~37–41 sequential appends per tick; the demo's publication volume sits far inside that boundary. |
| Queue admission + thread hop + reply | The `sync_channel` send, the worker dequeue, the one-slot reply | **YES, additively** — the ~9% overhead rides the same blocking path. Bounded by construction (capacity 64; required admission waits for room; the worker keeps draining so admission terminates). |
| Optional asynchronous publication | The optional-op `try_submit` path (the cancellation token; a full queue is the typed `QueueFull` refusal, never a drop) | **NO by design** — optional work never blocks: it refuses at admission and is regenerated at the next boundary. |
| Worker execution (non-append ops: reads, classify, validate) | The worker thread, FIFO | **Conditionally** — reads are µs-class (never a measured factor); the full validation is O(chain) (243.8 ms at 20k) and, when submitted mid-session, everything behind it waits (the measured 528×). The production startup's one-time cold walk is fine; a mid-session validate is the only measured head-of-line source. |
| Storage fsync/COMMIT | Inside each append transaction | **YES** — the dominant serial cost (see above); the durability contract requires it and it is NOT negotiable (the never-proposed list). |
| Explicit checkpoint / WAL maintenance | The queued `Checkpoint` op; the auto-checkpoint inside commits | **NO at measured rates** — wal retained 0 B through 100×; the explicit TRUNCATE is 2.3–5.7 ms, maintenance-class. |
| Recovery (reopen + validate) | A fresh worker at startup | **NO** — one-time at startup, linear ~12 µs/record; not on the tick path. |
| Contention / CAS retries | The producers' retry loops | **YES when multi-producer** — the collision window (two queue round trips per attempt) drops the aggregate to ~60–65% of sequential and creates the ~128 ms p99 tail. Single-producer publication (the ordinary spine path) never enters this regime. |

---

## 4. The ranked, evidence-based optimization table

Ranked by measured justification × durability-neutral character. **None is
implemented; each carries its regression requirements.** The floor (never
proposed): parallelizing canonical writes across connections, relaxing
`synchronous=FULL` or the WAL mode, weakening confirmation semantics
(acknowledging before the durable commit), or bypassing the single-authority
worker model for throughput.

### C1 — The compound append-after-head queue op

| Field | Assessment |
|---|---|
| Observed bottleneck | The contention regime: ~4.5–4.8 CAS retries per accepted append (770,420 at 100×); the collision window is the head-read → append round trip = TWO queue round trips; aggregate 1,458 vs 2,225 ops/s sequential; the ~128 ms p99 tail. |
| Specific implementation boundary | A new `MediumOp` arm (the head read + the record derivation + the append inside ONE worker job); the CAS discipline moves server-side — the worker re-derives the head and appends within its own serialized execution, returning the typed stale-parent refusal only if a foreign writer intervened (which the single-connection invariant structurally excludes). |
| Expected improvement | The contention aggregate approaches the sequential rate (~2,200 ops/s, +~50%); the retry storm and the contention p99 tail collapse (the collision window becomes the worker's internal serialized section, ~µs). |
| Relevant workload | Multi-producer publication (session crossings, coordinated multi-session campaigns). The single-producer spine path is unchanged. |
| Durability impact | **None** — the same transaction, the same COMMIT/fsync, the same `synchronous=FULL`. |
| Authority impact | **None** — the single worker remains the sole serialized writer; the CAS check is preserved (moved inside the serialized section, where it is trivially satisfied absent foreign writers). |
| Recovery impact | **None** — the appended records are the same canonical shape. |
| Complexity | Medium: one op arm + the worker-side derivation + the proxy method; the caller-side retry loop remains for the foreign-writer refusal class. |
| Risks | The op is less composable (a caller-supplied parent can no longer ride the same job); the lost-reply uncertain class must be preserved for the compound (a crash mid-compound = the same `Unknown` reconciliation — the existing seams cover this pattern); the §6a deterministic FIFO proof must be extended to the compound arm. |
| Regression requirements | The §6a ordering proof over the compound arm; the CAS accounting (the retry counter reaches ~0 absent foreign writers); the lost-reply/commit-uncertain classes; the exact-count pins. |
| Production changes justified NOW? | **NO — defer with evidence attached.** The measured 1,458 ops/s contended aggregate exceeds every current workload's demand; the change touches the worker protocol (a separately authorized design). Revisit when a real multi-producer workload measures the contention tail as a gameplay-visible latency. |

### C2 — The O(chain) validation as a non-blocking / pre-warmed class

| Field | Assessment |
|---|---|
| Observed bottleneck | The head-of-line source: a mid-session full validation blocks the queue for O(chain) — 259.6 ms at 20,000 records (528× the append baseline). |
| Specific implementation boundary | Two narrow options: (a) the recovery-time cursor pre-warm (the cold validate's authenticated walk amortized into the startup reopen, so the first mid-session validate re-walks only the post-recovery records), or (b) an incremental validation window (the summary carried forward from the last validated head). NOT a priority lane (the FIFO queue has none, by ratified design). |
| Expected improvement | A mid-session append behind a validate: 259.6 ms → ~µs at 20k records; the startup cost unchanged (it is one-time and already acceptable at 243.8 ms). |
| Relevant workload | Any mid-session maintenance/close gate that requests the full validation while publication continues. |
| Durability impact | **None** — validation is read-only. |
| Authority impact | **None — but the gate contracts must hold**: the close/settlement gates REQUIRE the validation evidence before settling; an incremental/cached summary must carry the same authenticated guarantee (the fingerprint discipline), never a weaker observation. |
| Recovery impact | **None** — the recovery path itself is the pre-warm's beneficiary. |
| Complexity | Low-medium (option a) / medium (option b): the cursor/summary plumbing exists (`ChainSummary`, the walk); the invariant proof (the incremental summary ≡ the full walk's result) is the real work. |
| Risks | A stale incremental summary silently accepted as full evidence — must be excluded by the fingerprint equivalence regression; the pre-warm lengthens startup by the validate it absorbs (net zero at best). |
| Regression requirements | The byte-identical summary equivalence (incremental vs full walk, the determinism-pin discipline); the close-gate evidence contracts; the recovery pins. |
| Production changes justified NOW? | **NO — defer.** The production startup's one-time cold walk is fine (243.8 ms at 20k); no current spine path runs a mid-session full validate at scale. Revisit when a maintenance feature schedules validates against a live publication stream. |

### C3 — Batched multi-record publication (the composite receipt pattern)

| Field | Assessment |
|---|---|
| Observed bottleneck | None measured at current rates — this is a capacity-shape candidate only: under `synchronous=FULL`, N separate appends cost N commit/fsyncs; grouping a tick's publications into ONE transaction would cost 1. |
| Specific implementation boundary | The K2-C1 composite receipt (`AdmittedAppend`, the unforgeable in-crate multi-object publication) already supports multi-record commits — the pipeline's caller-side grouping is the missing piece. |
| Expected improvement | At high publication volumes, the per-record commit cost divides by the batch size (the ~403 µs commit amortizes across N records). Irrelevant below ~37 appends per 60 Hz tick (the measured single-append capacity boundary). |
| Relevant workload | A future high-volume publication burst (the dense-world state snapshots, the asset-streaming checkpoints). |
| Durability impact | **Class-preserving** — the same FULL commit covers N records (each is durably installed at the same instant); the all-or-nothing transaction COARSENS the failure granularity (one commit failure → N uncertain candidates, each reconciled independently by the existing `Unknown` machinery). |
| Authority impact | **None** — the receipt pattern is the ratified unforgeable admission. |
| Recovery impact | **None** — the records are individually canonical; the recovery walk is unchanged. |
| Complexity | Medium-high: the pipeline's publication shape changes (the tick's records accumulated then admitted as one receipt). |
| Risks | The coarsened failure granularity (above); the batching latency (records wait for the tick boundary — a logical-time decision, not a storage one). |
| Regression requirements | The composite publication's existing corpus (the K2-C1 acceptance) + the uncertain-class reconciliation over the batch + the exact-count pins. |
| Production changes justified NOW? | **NO — not needed at the measured rates** (the directive's own framing: "if the workload ever demands it"). |

### C4 — The recorded API finding (not an optimization)

`ChainSummary.height` carries the **head epoch** (the sqlite construction
`height: head_epoch`) while its doc comment says "the chain height − 1". A
doc/code mismatch, documented in the §3 suite's assertion comment and in the
F-07 delivery record; **not fixed here** (out of the analysis scope, no
behavioral impact, the fix is a one-line doc correction for a separately
authorized hygiene pass).

### Explicitly NOT proposed (no measured justification)

- **Queue capacity changes** (64): the queue never saturates in any measured
  regime (contention bounds in-flight at 8; required admission waits for
  room by design). No evidence, no change.
- **Priority lanes / queue reordering**: would violate the ratified FIFO
  serialization contract to solve a problem measured as O(chain)-sourced
  (C2's territory, at its root).
- **Anything on the never-proposed floor** (see §4's preamble): the
  durability contract (WAL + FULL), the required-admission blocking queue,
  the single-authority worker, the confirmation semantics — the audit's
  verified strength #3, restated as the hard boundary of this analysis.

---

## 5. The actionable summary

1. **The engine's persistence path is not a current bottleneck for any
   existing workload.** The measured floor (~2,225 sequential ops/s release;
   ~37–41 appends per 60 Hz tick) sits far above the demo's publication
   volume, and every maintenance class (WAL, checkpoint, recovery) is
   millisecond-class or one-time.
2. **The one measured structural risk is the O(chain) head-of-line** (528×
   at 20k) — sourced exclusively in the full-validation class, mitigated
   first by scheduling discipline (validate at startup/boundaries, not
   mid-session) and only then by C2 if a workload ever demands it.
3. **The contention regime is real but bounded** (~65% of sequential, a
   ~128 ms p99 tail under 8 producers) and is eliminable server-side (C1)
   **when** a multi-producer workload justifies the protocol change — the
   evidence and the regression requirements are attached above.
4. **Every proposal is durability-neutral by construction** and none is
   implemented: the measurements are now actionable decisions, each carrying
   its own authorization boundary.

**Disposition: MEASUREMENT COMPLETE (§2's include/exclude validation) +
DESIGN READY (C1–C3 with regression requirements). No production change
justified now; no durability/authority/recovery contract touched.**
