# STAGE 6 — S6-C1 EVIDENCE SCHEMA

> **Status:** v2 · 2026-10-27 · **S6-C1 implementation artifact** — the
> observational evidence format and collection contract for Stage 6
> (Slice 0's diagnostics & soak stage), corrected per the S6-C1 source
> review (the four evidence-reliability and measurement-accuracy
> findings). Stage 6 remains gated; this document describes only the
> S6-C1 instrumentation. No soak runner, burst injection, or S6-C2+
> work exists yet.

## 1. Scope and contract

The S6-C1 evidence collector (`engine/crates/engine-slice/src/soak_evidence.rs`)
is a **strictly opt-in, composition-level, observational** collector for
the integrated `--demo-persist` application. When no collector is
configured (`DemoOptions::evidence = None`, the default), the
application's behavior, cost, and logical fingerprints are unchanged.
The contract:

- **Never on the critical path** — sample production is a nonblocking
  `try_send` onto a bounded channel; a dedicated writer thread owns all
  serialization and disk work (batched flushes; a 200 ms receive
  timeout also flushes). All blocking flush/sync work stays on that
  thread — never the authoritative tick or render submission paths.
- **No silent loss, no unchecked failure** — every stream keeps
  `produced / written / lost / write_failed / flush_errors` counts; a
  full queue, a failing write, **or a failing flush (intermediate or
  final)** counts the loss and fails the verdict **permanently** — a
  later successful operation never clears it. Records buffered during
  a failed-flush window remain accounted as written
  (buffer-accepted) with their on-disk persistence uncertain — no
  per-record persistence knowledge is fabricated.
- **No overwrite** — the collector's destination is a **fresh,
  exclusively created** directory (`std::fs::create_dir`): a
  pre-existing destination is refused with the typed
  `EvidenceError::DestinationExists` **before the campaign opens**; a
  previous package is never overwritten, truncated, or deleted.
- **Null ≠ zero** — unsupported GPU timestamps serialize as `null`
  (countable absence), never a substituted `0.000000`; platform RSS
  likewise (`null` where no sampler exists — Windows physical runs use
  the external sampler, S6-C4).
- **Observational only** — nothing here touches logical state; the
  integration gate proves instrumentation-enabled and -disabled runs
  produce byte-identical logical fingerprints.
- **Bounded memory** — in-memory retention is the bounded channel
  (default 8,192 records) plus the writer's buffers; the interactive
  rolling diagnostic histories (the overlay's inputs, 600/128-sample
  caps) are deliberately untouched.

## 2. The package layout and the finalization protocol

An evidence directory (fresh per run, outside the tracked source tree)
contains five JSONL streams — `frames`, `ticks`, `persistence`,
`regions`, `memory` — plus `manifest.json`.

The finalization protocol (entirely on the writer thread): drain every
buffered sample → **check every flush** → `sync_all` each completed
stream file (a sync failure fails the verdict) → best-effort
directory-entry sync (Unix; unavailable through std on Windows —
recorded as the honest platform limitation in the manifest's
`dir_synced`, never a verdict failure on its own) → prepare the
manifest **only after the streams pass** → write it to
`manifest.json.tmp`, synchronize it, then **atomically publish** it by
rename (a publication failure fails the verdict — a complete durable
package is never claimed without the published manifest).

`manifest.json` carries the schema version (`s6-evidence/2`), the
scenario label, the run metadata, `dir_synced`, per-stream counts
(`produced`, `written`, `lost`, `write_failed`, `flush_errors`), byte
counts, per-stream FNV-1a integrity digests, the first failure note
(`null` when clean), and the completeness verdict. **`complete` is
true iff nothing was lost, no write failed, no flush failed, and
every produced sample was written.**

## 3. The record schemas

All `_ms` fields are **milliseconds encoded with six decimal places —
nanosecond-scale numerical increments** (the effective measurement
resolution is bounded by the clock source, not by the encoding).
Wall offsets are milliseconds since collector start. Unknown,
missing, or duplicate fields fail a strict decode (`decode_line`);
the schema is versioned and closed.

- **`frame`** — `sample` (iteration index), `wall_offset_ms`,
  `interval_ms` (the raw interval handed to the sim accumulator,
  unclamped), `presented` / `recreated` / `deferred`, `cpu_work_ms`
  (poll→present excluding pacing sleep; presented frames only),
  `input_ms` / `sim_ms` / `render_ms` / `poll_ms` (the separately
  labeled CPU phases; `null` on non-rendering frames), `gpu_ms` (the
  timestamp-derived duration; `null` when the queue family exposes no
  usable timestamp queries).
- **`tick`** — `tick`, `wall_offset_ms`, `tick_ms` (the orchestrated
  boundary: gate + transitions + spine — the established label, never
  bare spine time), `ticks_due_total` / `outstanding_ticks` (the
  ABSOLUTE time accounting and the **fixed-step time debt** — the
  session-local accumulator remainder, a separate quantity from
  event-spine obligations and never restored by recovery), the spine
  counters (`executed`, `scheduled`, `late_arrivals`,
  `late_executions`, `blocked_transitions`, `invalid_dropped`,
  `budget_exhausted_ticks`), queue/lane snapshots, the watermark pair
  (`processed_through`, `max_executed_due`), and `transitioned_regions`.
- **`persistence`** — `kind` (`recovery`, `capture`, `admission`,
  `busy`, `completion`, `confirmation`, `close` — the latency families
  stay separate, never conflated), `tick`, `latency_ms`, `outcome`
  (the truthful label, verbatim), `revision`. All measured at the
  **application boundary** (the frozen `engine-persist` interior is
  not instrumented); completion spans admission → observed terminal
  state; `close` is n=1 per run, measured separately from the soak
  wall duration.
- **`region`** — `tick` (the boundary clock at the moment of the
  transition — the same `now` the crossing machinery stamps), `region`,
  `from_level` / `to_level` (the LOD detail ordinals: 3 Full ·
  2 Simplified · 1 EventDriven · 0 Dormant — the attempted target),
  `applied` (whether the level actually changed; a coherent activation
  refusal is recorded honestly), `kind` (`activation` /
  `deactivation` / `reclassification`), **`scope`** — the boundary the
  duration covers, never conflated: `director-legacy` (the director's
  own pipeline-free transition, including the orchestrated tick's
  live↔live reclassifies, which carry no T1 journaling) or
  `orchestrated-crossing` (**the FULL six-phase durable crossing** —
  the journal staging, the T1 append with its fsync, the
  verification, and the install as one per-region boundary, **never
  the install phase alone**) — and `duration_ms` (the per-transition
  duration, `null` only where genuinely unavailable, never a
  substituted zero, **never a tick aggregate**). The run summary's
  activation/deactivation percentile lines are the separate
  128-cap overlay histories; the deactivation line is labeled as the
  crossing's director install phase.
- **`memory`** — `rss_kib` (Linux `/proc/self/status`; `null`
  elsewhere), `held_by_region` (the structural dormant occupancy),
  `pending_total` (the capture ledger's pending consequences).
  **Attribution metadata, never an allocation-exact per-NPC cost** —
  the flat-array NPC representation makes per-LOD allocation
  differences absent at Slice 0 scale.

## 4. Package verification

`soak_evidence::verify_package(dir)` independently checks a package
against its manifest, without mutating it. A **complete** verdict
requires all of the following: the **supported schema version**
(another version is a structural refusal — a foreign schema is never
interpreted); the manifest declaring **exactly the five required
stream identities** — `frames`, `ticks`, `persistence`, `regions`,
`memory` — with none missing, none duplicated, and no unknown names
(names are matched against the fixed whitelist before any path is
constructed, so a hostile name — including path traversal — never
reaches the filesystem); and every required stream file present and
passing the per-stream checks — byte lengths, record counts, strict
per-line decodability, the recomputed FNV-1a digests, and the counts'
complete-consistency (including the flush-error counts). A manifest
declaring complete while checks fail — or while also declaring a
failure — is itself a problem.

Three distinct layers, never conflated:

- **Structurally invalid** (a missing or unparseable manifest, an
  unsupported schema version): a typed `Err`.
- **Honestly incomplete** (the manifest declares `complete: false`):
  the verification reports at least the counts-consistency diagnostic
  naming WHY it is incomplete (e.g. unexecuted writes, queue losses,
  flush errors) — its **file integrity** may be fully intact (bytes,
  records, digests all matching); the diagnostic problems and the
  file-integrity checks are separate layers. `verified_complete()`
  is false, and readable files can never upgrade the verdict.
- **Valid, independently verified complete**: the manifest declares
  complete AND the verification found no problem of any kind.

## 5. Statistics helpers and overhead

`one_percent_low_fps` (the documented `ceil(0.01 × n)` worst-interval
mean-rate calculation; not claimed below 100 samples) and
`math::percentile` (the established ceil-index convention).
`engine-slice --benchmark-instrumentation` measures the produce path
(construction + the bounded enqueue) and the same deterministic
headless workload with evidence disabled versus enabled, gated by the
logical-fingerprint equality assertion — pod/dev-profile
observational numbers, never CI thresholds, never physical-rig claims.

## 6. Fault seams and limits

`EvidenceFaultControl` (the `WorkerFaultControl` discipline —
observable state, never sleeps): `fail_writes` (write-boundary
failures), **`fail_flushes` (flush-boundary failures — the HOLD-1
regression; the test waits for one complete seam pass over every
stream through the live counter before disarming, so the disarm can
never land between the writer's per-stream iterations)**, and
`park_writes` (the overflow regression's deterministic full queue).
`DemoOptions::evidence_fail_writes` is the application-level test
seam (never set by the binary), mirroring `poison_at_tick`.

**Known limits (honest, not papered over):** the writer join can
block on pathological disk stalls; a manifest-publication failure is
noted in the in-memory verdict (the outcome, not a file, is
authoritative); directory-entry synchronization is Unix-only through
std (recorded via `dir_synced`); the run's git commit SHA is not
recorded in-process (the runner records it — S6-C3); memory sampling
exists only where the platform sampler does; an errored run leaves
that tick's drained-but-unemitted transition entries in the bounded
log. This pod has no Vulkan loader — the graphical frame producers'
live exercise belongs to CI's lavapipe runners and the rig.
