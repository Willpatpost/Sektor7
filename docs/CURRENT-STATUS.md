# SEKTOR7 — CURRENT STATUS INDEX

**Purpose:** the maintained, always-current index of what the engine actually
is — integrated, tested, measured, and pending. The stage records in
`engine/README.md` and `docs/` are **historical acceptance evidence, retained
as history and never rewritten**; this file is the live layer on top of them.
A fresh development or review agent should be able to determine the actual
current status, active authority, source of truth, known limitations, and next
work from THIS file alone.

- **Maintained since:** 2026-11-14 (the post-audit remediation cycle, F-13).
- **Baseline at introduction:** `main` @ `a4fe0ec` + the remediation branches
  in flight (see §9).
- **Update rule:** every change that alters capabilities, test counts,
  performance evidence, or priorities updates this file in the same change.

---

## 1. Historical stage-completion records (where the history lives)

| Record | Location |
|---|---|
| The stage-by-stage workspace narrative (Stages 0–5, the stage banner) | `engine/README.md` (top) — historical, retained |
| The architecture decision ledger (T-01…, accepted and deferred) | `docs/DECISIONS.md` |
| Stage 4 physical acceptance (the owner's rig) | `docs/STAGE4-WINDOWS-ACCEPTANCE-RECORD-2026-10-21.md` |
| Stage 5 Batch 1–5 acceptance records | `docs/STAGE5-B1-WIRE-FORMAT.md` … `STAGE5-B5-LIFECYCLE-RECORD.md` |
| Slice 0 integration acceptance | `docs/SLICE0-INTEGRATION-ACCEPTANCE-RECORD-2026-10-26.md` |
| Production-persistence line records (PP-B1…B4) | `docs/PP-B1-SQLITE-PERSISTENCE-RECORD.md` … `PP-B4-ORCHESTRATION-VALIDATION-RECORD.md` |
| Stage 6 evidence schema, burst validation, soak runbooks | `docs/STAGE6-*.md` |
| The vision and design bible | `docs/VISION.md`, `docs/DESIGN-BIBLE.md`, `docs/TECH-ARCHITECTURE.md` |

These are point-in-time acceptance records. Later work does not edit them to
appear further along; later state lives in this index.

## 2. Current integrated engine capabilities (what `main` actually carries)

Five-crate workspace (`engine/`), all cross-platform (Linux, Windows, macOS
ARM64) with a five-job CI:

- **`engine-core`** — the deterministic simulation spine: `(due, seq)` event
  queue, cross-class causal dependencies, per-tick budget, execution ledger
  with first-class lateness, the safe `processed_through` watermark, FNV-1a
  fingerprints, canonical wire primitives + full-fidelity spine round-trip.
- **`engine-persist`** — the persistence stack: SHA-256 content-addressed
  revision store; `CanonicalMedium` boundary with **two production mediums**
  (LocalFile and SQLite, both WAL + `synchronous=FULL`); the durable T1
  journal (hash-chained transaction frames); the `CommitPipeline`
  (single-authority per-key lattice, canonical confirmation); the
  recovery machinery (strict temporal boundaries, authenticated ancestry
  witnesses, recovery-state classification, degradation matrices); the
  epoch-checked record constructors.
- **`engine-renderer`** — the Vulkan backend (ash): swapchain with recreation
  handling, present-mode preference with FIFO fallback, depth-correct scene,
  the dynamic region-scene vertex path (the F-02 caller-held scratch form),
  the retained overlay-vertex buffer (F-05), GPU timestamps, validation-layer
  routing, the debug bitmap overlay.
- **`engine-shell`** — the SDL3 application shell (statically-linked vendored
  SDL 3.4.18; real window + offscreen/dummy modes).
- **`engine-slice`** — the integration/demo driver: the stdin CLI, `--window`,
  `--render` (with `--regions` LOD visualization), `--sim` (headless
  deterministic), `--demo-persist` (the integrated authoritative campaign
  behind the render loop, with the soak/evidence machinery), the session
  crossing (founded-and-anchored or recovered, the Space action, the durable
  close), and the benchmark harnesses
  (`--benchmark-persistence`, `--benchmark-instrumentation`,
  `--benchmark-entity-scale`, `--benchmark-render`).

The **integrated campaign demo** (`--demo-persist`) is the vertical slice:
one authoritative, persistently-orchestrated 1000-NPC / 8-region campaign
with LOD, crash recovery, soak evidence, and rendering together.

## 3. Production mechanisms vs demo-only implementations

| Layer | Class |
|---|---|
| `engine-core` spine, `engine-persist` store/journal/pipeline/recovery, `engine-renderer` Vulkan backend, `engine-shell` | **Engine libraries** — production mechanisms (no demo coupling) |
| `engine-slice` scenarios (`stage6-v1`), the CLI modes, the benchmark harnesses, the debug overlay content | **Demo/driver layer** — deliberately not the game |
| `composite-test-fixture` (engine-persist), `crash-test-seams` (engine-slice) | **Test-only features** — compile-time isolated, never in production builds (CI re-proves the feature-disabled production path) |
| The Vulkan renderer's current shape (static scene buffers, host-visible persistently-mapped dynamic buffers, 2 frames in flight, bounded region-vertex capacity) | **Prototype-class** rendering design — correct and validated, not the final architecture (see §8) |

## 4. Current automated test coverage

The five-job CI (`.github/workflows/engine-ci.yml`) partitions the battery
exactly and gates the counts (the F2 exact-count discipline — counts are
re-measured from actual output on every change, never estimated):

| CI partition | Linux | Windows |
|---|---:|---:|
| Main shard (`--workspace --exclude engine-slice`) | 950 | 899 |
| Heavy shard (`-p engine-slice --features crash-test-seams`) | 466 | 465 |
| **Combined** | **1416** | **1364** |

(The macOS job runs its own execution partition — no combined count is
defined for it. Ignored: 1 main / 3 heavy, all deliberate measurement soaks.)

- **Determinism pins (never change):** engine `6504123a47cab66f`, state
  `937bd3b6f56f07e0`, save `len=1209 / fnv=c6c1b46c7d21843b`, the Stage 3
  and Stage 4 scenario fingerprints — all double-run and byte-compared in CI.
- **Smokes:** fingerprint/save self-tests (double-run), headless simulation,
  the three presence scenarios, the Vulkan offscreen render smokes with the
  active validation layer incl. swapchain recreation and pipeline
  replacement (Linux/lavapipe — correctness only), the bounded integrated
  soak, and the F-06 release render-path canary (distributions printed, the
  A/B geometry equality asserted, no timing thresholds).
- **Counts describe the current CI configurations** — the specific commands
  in the workflow, not every platform or feature combination. The historical
  count narratives inside `engine/README.md` describe earlier states and are
  retained as history.

## 5. Accepted architecture decisions

The ledger is `docs/DECISIONS.md`; the load-bearing accepted decisions:

- Stage-gated minimal scope with per-mechanism tests (the anti-abstraction
  ledger, TECH-ARCHITECTURE §8).
- Deterministic-first simulation (the spine owns logical time; rendering and
  pacing never enter logical state).
- Content-addressed storage with structural domain separation; the
  single-authority commit model (canonical confirmation = verified chain
  inclusion AND per-key validation); no parallel canonical writes.
- The T1 journal and the two-contract recovery (pure-function
  reconciliation; strict temporal boundaries at every begin).
- SQLite and LocalFile as peer production canonical mediums (WAL +
  `synchronous=FULL`; the provider-selection gate closed through the PP
  line).
- SDL3 vendored-static; the engine never links system SDL.
- Production-persistence decisions: `docs/PRODUCTION-PERSISTENCE-DECISIONS.md`.

## 6. Deferred design decisions (documented, not yet authorized)

- **F-03** — the region-render vertex-capacity policy: the design and the
  acceptance tests are now **DESIGN READY**
  (`docs/GPU-CAPACITY-SYNC-ANALYSIS.md` §4 — the truncation-visibility
  counter first, then the constants-derived capacity + typed refusal); the
  implementation awaits the next authorized package.
- **F-04** — GPU asset streaming / upload architecture: the measured-
  hypothesis plan is **DESIGN READY** (the same document §2.2/§3 — gated on
  the rig measurements H1/H4/H5; no refactor justified by current
  evidence).
- **F-08 / F-12** — the sustained-load scale methodology is **DESIGN
  READY** (`docs/SUSTAINED-LOAD-METHODOLOGY.md` — the six experiment
  classes, twelve dimensions, the backlog-vs-deliberate-LOD distinction);
  the implementation harness awaits its package.
- **The F-05 overlay throttle** — deliberately NOT implemented (the measured
  costs are negligible at target frame budgets; revisit only with rig
  evidence).
- Compaction follow-ups and the multiplayer/replication research
  (`docs/MULTIPLAYER-SPEC.md` is research, not an accepted design).

## 7. Actual performance evidence (and what is NOT evidence)

- **CPU render path (pod, `--benchmark-render`):** the F-02 scratch form
  p95 0.081 → 0.035 ms/frame (−57%), byte-identical geometry; the F-05
  overlay breakdown (glyph vertex rebuild 0.090–0.131 ms dominant; format
  chain and percentiles negligible). Pod CPU evidence only.
- **Entity scale (pod, `--benchmark-entity-scale`):** 1k / 5k / 20k
  entities × three LOD scenarios, bounded event execution per tick with
  double-run determinism. Bounded-cost evidence, **not** sustained-load
  proof (F-12/F-08 open).
- **Persistence (pod, the F-07 harness):** ~1295 sequential ops/s, ~9%
  worker-queue overhead, contention p99 128 ms, cold recovery ~34.7 ms,
  ~19× head-of-line amplification — pod measurements of the approved
  topology, not universal guarantees; the actionable interpretation is an
  active work item (§9).
- **Real GPU (the only real-GPU evidence):** the Stage 4 Windows acceptance
  record (2026-10-21, the owner's RTX 5070 Ti rig — e.g. 1744 fps avg
  uncapped/immediate on the Stage 4 scene). **Standing rule: no CI or pod
  number is GPU evidence** (llvipe/lavapipe are correctness smoke only).
  The current-scene hardware baseline procedure is
  `docs/WINDOWS-GPU-PROFILING-KIT.md` (F-09-corrected), awaiting the owner's
  rig.

## 8. Known limitations (the honest list)

- The renderer is prototype-class: static scene buffers, host-visible
  persistently-mapped dynamic buffers, 2 frames in flight, and a **bounded
  region-vertex capacity** (F-03 — silent-truncation risk if geometry
  exceeds it; the policy is deferred by design).
- No GPU asset streaming (F-04); dynamic content moves through per-frame
  host-visible copies.
- Sustained-load scale is unproven (F-12/F-08): a budget-limited scheduler
  can be fast while falling behind; backlog drain/lateness qualification is
  pending.
- No networking, replication, physics, combat, procedural generation, asset
  pipeline, or editor systems (§10 — not yet designed, not hidden).
- The engine is a **development foundation + demo**, not a production game
  engine; nothing in the current evidence supports production-readiness
  claims for the complete game.

## 9. Active development priorities

The **post-audit remediation cycle** (2026-11-14, the owner's directive
preserved at the coordination ledger, §10): F-09 GPU-kit/CLI correctness
(delivered), F-11 deterministic head-of-line test, F-14 epoch
trust-boundary audit, F-13 this documentation reconciliation (delivered);
Milestone 2 — the F-12/F-08 sustained-scale methodology (DESIGN READY,
`docs/SUSTAINED-LOAD-METHODOLOGY.md`), the F-07 persistence
interpretation, the F-03/F-04 GPU capacity/synchronization analysis
(DESIGN READY, `docs/GPU-CAPACITY-SYNC-ANALYSIS.md`), the F-10
fault-coverage matrix; then the Milestone-3 next-cycle proposal
(correctness + performance + forward feature progress).

Work-in-flight branches: `s6-remediation-c-renderer-docs` (F-09 + F-13
delivered; the two M2 design documents added), the parallel A (epoch
audit) and B (persistence/CI) packages per the coordinator's assignment
ledger.

## 10. The authoritative coordination ledger

- **Location:** `~/.qwen/sektor7-coordination/STATUS.md` on the development
  pod — **outside the Git repository, by charter**. The same directory holds
  the owner's directives preserved verbatim (`OWNER_DIRECTIVE_*.md`) and the
  relay/coordination records.
- **Ownership:** the coordinator session (Agent D) is the **sole writer**;
  implementation agents read it and receive byte-verified relays. The owner's
  authorizations are the preserved directive files, never paraphrase.
- **Durability:** pod-local operational storage (not committed, not on
  GitHub). It is deliberately independent of the repo's branches and
  worktrees, so branch/worktree cleanup never invalidates references to it.
  After a coordinator-session reset it is rebuildable from the preserved
  directive files plus the repository's own records (commit messages and
  `docs/`); this index carries the technical status, the ledger carries the
  campaign/operational state.

---

## The capability-readiness matrix

Classification per the owner's directive (2026-11-14 §10): **Implemented and
validated** / **partially implemented** / **prototype only** / **designed but
not implemented** / **not yet designed**.

| Capability | Classification | Basis |
|---|---|---|
| Deterministic simulation & scheduling | Implemented and validated (demo scale; sustained-scale qualification in progress, §8) | `engine-core` spine; the determinism pins; the CI battery; the entity-scale benchmark |
| Persistence & recovery | Implemented and validated | `engine-persist` store/journal/pipeline; the crash/recovery matrices; real process-kill proofs; cross-platform CI |
| Rendering (Vulkan presentation) | Partially implemented (prototype-class design) | `engine-renderer`; lavapipe + rig validation; F-03/F-04 open |
| Shader/material architecture | Prototype only | Hand-written build-time naga shaders; no material system |
| GPU resource streaming | Not yet designed | F-04 deferred; host-visible copies only |
| NPC/world representation (LOD) | Partially implemented | The region director, four LOD levels, activation/reconstruction — validated at 1000 NPCs / 8 regions; game-scale behaviors undesigned |
| Physics and collision | Not yet designed | — |
| Combat and gameplay systems | Not yet designed | — |
| Networking and replication | Not yet designed | Research only (`docs/MULTIPLAYER-SPEC.md`) |
| Listen-server authority | Not yet designed | — |
| Host handoff | Not yet designed | — |
| Co-op roster governance | Not yet designed | — |
| Procedural world generation | Not yet designed | — |
| Dense city streaming | Not yet designed | F-08 deferred |
| Assets and content pipeline | Not yet designed | — |
| Editor/creation tools | Not yet designed | — |
| Performance instrumentation | Implemented and validated | The benchmark flags; the run summaries; the F-06 canary; the GPU kit |
| Production deployment | Not yet designed | The CLI is a development driver, not a product |
