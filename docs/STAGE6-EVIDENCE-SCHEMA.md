# STAGE 6 — S6-C1 EVIDENCE SCHEMA

> **Status:** v1 · 2026-10-27 · **S6-C1 implementation artifact** — the
> observational evidence format and collection contract for Stage 6
> (Slice 0's diagnostics & soak stage). Stage 6 remains gated; this
> document describes only the instrumentation delivered by the S6-C1
> checkpoint (its owner GO and the independent review's conditional
> pass). No soak runner, burst injection, or S6-C2+ work exists yet.

## 1. Scope and contract

The S6-C1 evidence collector (`engine/crates/engine-slice/src/soak_evidence.rs`)
is a **strictly opt-in, composition-level, observational** collector for
the integrated `--demo-persist` application. When no collector is
configured (`DemoOptions::evidence = None`, the default), the
application's behavior, cost, and logical fingerprints are unchanged.
When configured, the run additionally produces a machine-readable
evidence package:

- **Never on the critical path** — sample production is a nonblocking
  `try_send` onto a bounded channel; a dedicated writer thread owns all
  serialization and buffered disk work (batched flushes; a 200 ms
  receive timeout also flushes; fsync only at finalization). The
  authoritative simulation and render submission never block on
  evidence.
- **No silent loss** — every stream keeps `produced / written / lost /
  write_failed` counts; a full queue or a failing writer counts the
  loss and fails the verdict. An incomplete package is a failed run
  (the `Finalization::combined` discipline extends to it), reported
  independently of the graceful close — a writer failure can never
  bypass the close, which always runs first.
- **Null ≠ zero** — unsupported GPU timestamps serialize as `null`
  (countable absence), never a substituted `0.000000`; platform RSS
  likewise (`null` where no sampler exists — Windows physical runs use
  the external sampler, S6-C4).
- **Observational only** — nothing in the collector touches logical
  state; the integration gate proves instrumentation-enabled and
  -disabled runs produce byte-identical logical fingerprints.
- **Bounded memory** — in-memory retention is the bounded channel
  (default 8,192 records) plus the writer's buffers; the interactive
  rolling diagnostic histories (the overlay's inputs, 600/128-sample
  caps) are deliberately untouched.

## 2. The package layout

An evidence directory (configurable, outside the tracked source tree)
contains:

| File | Stream | Cadence |
|---|---|---|
| `frames.jsonl` | `frame` | every graphical loop iteration |
| `ticks.jsonl` | `tick` | every authoritative tick |
| `persistence.jsonl` | `persistence` | every application-boundary persistence operation |
| `regions.jsonl` | `region` | every region transition (per region) |
| `memory.jsonl` | `memory` | 1 s wall-clock cadence (graphical loop) |
| `manifest.json` | — | written once at finalization |

`manifest.json` carries the schema version (`s6-evidence/1`), the
scenario label, the run metadata (mode, platform, GPU/device/Vulkan
version, present mode, extent, swapchain images, validation, fps
target — graphical runs), per-stream counts, byte counts, per-stream
FNV-1a integrity digests, the first failure note (`null` when clean),
and the completeness verdict. **`complete` is true iff nothing was
lost, no write failed, and every produced sample was written.**

## 3. The record schemas

All `_ms` fields are **milliseconds at microsecond resolution** (six
decimal places — the documented precision contract; finer values
quantize). All wall offsets are milliseconds since collector start.
Unknown, missing, or duplicate fields fail a strict decode
(`decode_line`); the schema is versioned and closed.

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
  `budget_exhausted_ticks`), queue/lane snapshots (`near_pending`,
  `far_pending`, `blocked`, `held`, cumulative lane totals and
  backlogged-tick counts), the watermark pair (`processed_through`,
  `max_executed_due` — `null` when `None`), and `transitioned_regions`.
- **`persistence`** — `kind` (`recovery`, `capture`, `admission`,
  `busy`, `completion`, `confirmation`, `close` — the latency families
  stay separate, never conflated), `tick`, `latency_ms`, `outcome`
  (the truthful label, verbatim), `revision` (`null` when none). All
  measured at the **application boundary** (the frozen
  `engine-persist` interior is not instrumented); completion latency
  spans admission → observed terminal state (queue + append + fsync +
  readback — what the authority thread experiences); `close` is n=1
  per run and measured separately from the soak wall duration.
- **`region`** — `tick`, `region`, `from_level` / `to_level` (the LOD
  detail ordinals: 3 Full · 2 Simplified · 1 EventDriven · 0 Dormant).
  Identities come from the tick's `TickReport` and the director's
  before/after levels.
- **`memory`** — `rss_kib` (Linux `/proc/self/status`; `null`
  elsewhere), `held_by_region` (the structural dormant occupancy —
  held obligations per region), `pending_total` (the capture ledger's
  pending consequences). **Attribution metadata, never an
  allocation-exact per-NPC cost** — the flat-array NPC representation
  makes per-LOD allocation differences absent at Slice 0 scale.

## 4. Statistics helpers

`soak_evidence` provides the documented 1%-low calculation
(`one_percent_low_fps`: the `ceil(0.01 × n)` worst presented-frame
intervals, their mean rate; **not claimed below 100 samples**) and
reuses `math::percentile` (the established ceil-index convention) for
P50/P95/P99/max. Full-run distributions are computed from the raw
JSONL at analysis time (S6-C5); the interactive overlay's rolling
histories are unchanged.

## 5. Instrumentation overhead

`engine-slice --benchmark-instrumentation` measures (pod, dev-profile,
observational — never CI thresholds, never physical-rig claims):

1. the produce path (sample construction + the bounded enqueue) as a
   distribution over 10,000 samples;
2. the same deterministic headless workload (600 ticks + the action)
   with evidence disabled versus enabled — wall-time comparison and
   the logical-fingerprint equality gate.

The overhead comparison's authoritative repetition belongs to the
owner's rig (S6-C4), matching the accepted evidence-class discipline.

## 6. Fault seams and limits

The `EvidenceFaultControl` (the `WorkerFaultControl` discipline —
observable state, never sleeps) provides deterministic
`fail_writes` and `park_writes` injection for the overflow and
writer-failure tests; `DemoOptions::evidence_fail_writes` is the
application-level test seam (never set by the binary), mirroring
`poison_at_tick`.

**Known limits (honest, not papered over):** the writer join can block
on pathological disk stalls (the same class as any thread join); a
manifest write failure is noted in the in-memory verdict (the outcome,
not the file, is authoritative); the run's git commit SHA is not
recorded in-process (the runner records it — S6-C3); memory sampling
exists only where the platform sampler does; per-transition latencies
beyond the activation/deactivation percentile aggregates are deferred
to S6-C3's scenario records.
