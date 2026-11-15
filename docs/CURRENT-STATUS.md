# SEKTOR7 — CURRENT STATUS INDEX

**Purpose:** the maintained, always-current index of what the engine actually
is — integrated, tested, measured, and pending. The stage records in
`engine/README.md` and `docs/` are **historical acceptance evidence, retained
as history and never rewritten**; this file is the live layer on top of them.
A fresh development or review agent should be able to determine the actual
current status, active authority, source of truth, known limitations, and next
work from THIS file alone.

- **Maintained since:** 2026-11-14 (the post-audit remediation cycle, F-13);
  reconciled for Cycle 1 on 2026-11-15.
- **Baseline at introduction:** `main` @ `a4fe0ec` — that remediation
  candidate was subsequently **merged** (`main` has sat at `900b0ea` since).
  This revision reflects the **Cycle-1 integration candidate**
  (`s7-cycle1-integration`, §9) with its measured counts.
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
  fingerprints, canonical wire primitives + full-fidelity spine round-trip;
  plus the Cycle-1 **player movement/collision prototype module**
  (kinematic mechanics, std-only, behind a narrow `CollisionWorld` query
  contract — §8 scope).
- **`engine-persist`** — the persistence stack: SHA-256 content-addressed
  revision store; `CanonicalMedium` boundary with **two production mediums
  carrying distinct durability contracts** — SQLiteMedium
  `TransactionalWalFull` (WAL + `synchronous=FULL`, both verified by
  querying the actual configuration at open) and LocalFileMedium
  `FullPosixSequence` on supported POSIX / `ProcessCrashOnly` on Windows
  (see §5 and §8); the durable T1 journal (hash-chained transaction
  frames); the `CommitPipeline`
  (single-authority per-key lattice, canonical confirmation); the
  recovery machinery (strict temporal boundaries, authenticated ancestry
  witnesses, recovery-state classification, degradation matrices); the
  epoch-checked record constructors.
- **`engine-renderer`** — the Vulkan backend (ash): swapchain with recreation
  handling, present-mode preference with FIFO fallback, depth-correct scene,
  the dynamic region-scene vertex path (the F-02 caller-held scratch form),
  the retained overlay-vertex buffer (F-05), GPU timestamps, validation-layer
  routing, the debug bitmap overlay, and the Cycle-1 **dynamic-geometry
  truncation counters** (F-03 Step 1: per-frame region/overlay
  omitted-vertex counts with supplied/capacity context, reported in every
  run summary — fail-visible over the former silent clamps).
- **`engine-shell`** — the SDL3 application shell (statically-linked vendored
  SDL 3.4.18; real window + offscreen/dummy modes).
- **`engine-slice`** — the integration/demo driver: the stdin CLI, `--window`,
  `--render` (with `--regions` LOD visualization), `--sim` (headless
  deterministic), `--demo-persist` (the integrated authoritative campaign
  behind the render loop, with the soak/evidence machinery), the session
  crossing (founded-and-anchored or recovered, the Space action, the durable
  close), the benchmark harnesses
  (`--benchmark-persistence`, `--benchmark-instrumentation`,
  `--benchmark-entity-scale`, `--benchmark-render`), and the Cycle-1
  additions: **`--benchmark-sustained-load`** (the F-08/F-12 sustained-scale
  demonstration command) with **`--sustained-load-full-matrix`** (the full
  reference matrix; the raw evidence record is archived in the
  coordination ledger — §7) and **`--player` / `--player-headless`** (the
  player prototype's interactive and deterministic paths).

The **integrated campaign demo** (`--demo-persist`) is the vertical slice:
one authoritative, persistently-orchestrated 1000-NPC / 8-region campaign
with LOD, crash recovery, soak evidence, and rendering together.

## 3. Production mechanisms vs demo-only implementations

| Layer | Class |
|---|---|
| `engine-core` spine, `engine-persist` store/journal/pipeline/recovery, `engine-renderer` Vulkan backend, `engine-shell` | **Engine libraries** — production mechanisms (no demo coupling) |
| `engine-slice` scenarios (`stage6-v1`), the CLI modes, the benchmark harnesses, the debug overlay content | **Demo/driver layer** — deliberately not the game |
| `composite-test-fixture` (engine-persist), `crash-test-seams` (engine-slice) | **Test-only features** — compile-time isolated, never in production builds (CI re-proves the feature-disabled production path) |
| The Vulkan renderer's current shape (static scene buffers, host-visible persistently-mapped dynamic buffers, 2 frames in flight, bounded region-vertex capacity with **fail-visible truncation counters** — F-03 Step 1 delivered; the Step-2 capacity/refusal policy deferred) | **Prototype-class** rendering design — correct and validated, not the final architecture (see §8) |
| The Cycle-1 player prototype (`engine-core` player module + the `--player` demo layer) | **Prototype-class** kinematic mechanics — reusable engine surface, reversible, explicitly not a ratified production physics backend |

## 4. Current automated test coverage

The five-job CI (`.github/workflows/engine-ci.yml`) partitions the battery
exactly and gates the counts (the F2 exact-count discipline — counts are
re-measured from actual output on every change, never estimated):

| CI partition | Linux | Windows |
|---|---:|---:|
| Main shard (`--workspace --exclude engine-slice`) | 955 | 904 |
| Heavy shard (`-p engine-slice --features crash-test-seams`) | 467 | 466 |
| **Combined** | **1422** | **1370** |

(The macOS job runs its own execution partition — no combined count is
defined for it. Ignored: 1 main / 3 heavy, all deliberate measurement soaks.)

**The Cycle-1 candidate** (`s7-cycle1-integration`, §9 — awaiting the
owner's mainline merge; `main` stays at the table above until then) adds
+29 main / +16 heavy platform-uniform tests and three integration
binaries, including the corrective round (R4's player boundary
regressions, R2's gradual-growth regression). Measured at the
re-integrated tree (the workflow's own awk semantics): main **984 / 1
ignored / 30 result lines**; heavy **483 / 4 ignored / 49 result
lines**. Windows expectations 933 / 482 (platform-uniform inference)
are confirmed by the candidate CI run; the workflow pins are updated in
the same change (the F2 maintenance obligation). All determinism pins
below are byte-identical at the candidate, plus the new pinned player
trace `engine-core/player-trace/1` class (`8513d40792ea88f0`, the
libm-free script class — cross-platform by construction; divergence is
an investigation, never a silent re-pin). The formerly ignored oversize
smoke is now ALSO executed by an enforced Linux CI step (the R1
corrective) with its evidence lines in the log.

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
- The two production canonical mediums with their **distinct durability
  contracts** (the provider-selection gate closed through the PP line):
  SQLiteMedium **`TransactionalWalFull`** (WAL + `synchronous=FULL`, both
  verified by querying the actual configuration at open — PP-B1/D-PP-1,
  subject to its documented configuration/storage assumptions);
  LocalFileMedium **`FullPosixSequence`** on supported POSIX (the full
  fsync sequence; the macOS std-semantics note applies) and
  **`ProcessCrashOnly` on Windows** — the process-crash tier, insufficient
  for the ratified destructive-reclamation durability requirement (the
  OC-P2 tier gates refuse the destructive arms on that platform).
- SDL3 vendored-static; the engine never links system SDL.
- Production-persistence decisions: `docs/PRODUCTION-PERSISTENCE-DECISIONS.md`.

## 6. Deferred design decisions (documented, not yet authorized)

- **F-03** — the region-render vertex-capacity policy: **Step 1 is
  DELIVERED in Cycle 1** (the fail-visible truncation counters — §2 —
  measured zero-truncation on every ordinary run, and the oversize
  lavapipe smoke proves the exact counting through the real render path);
  **Step 2** (constants-derived capacity + typed refusal at the
  `render_frame` boundary) remains deferred awaiting the owner's separate
  decision — the technical recommendation with alternatives and tradeoffs
  is delivered with the Cycle-1 P1 package (`GPU-CAPACITY-SYNC-ANALYSIS.md`
  §4 Step 2 + the delivery report).
- **F-04** — GPU asset streaming / upload architecture: the measured-
  hypothesis plan is **DESIGN READY** (the same document §2.2/§3 — gated on
  the rig measurements H1/H4/H5; no refactor justified by current
  evidence).
- **F-08 / F-12** — the sustained-load scale methodology is
  **IMPLEMENTED in Cycle 1** (the `--benchmark-sustained-load`
  demonstration + `--sustained-load-full-matrix` full harness: the six
  workload classes, twelve measurement dimensions, the measured
  composition table, the honest effective matrix — see §7), **corrected
  in the R2/R3 round** (per-logical-tick slope units; recovery horizons
  derived from measured drains with explicit bounded-observation wording
  for non-draining compositions); remaining follow-ups are documented
  limitations (§8: the class-6 director-protocol anchor is
  harness-driven, and the sustained codec's identity-capacity
  tightening), not missing methodology.
- **The F-05 overlay throttle** — deliberately NOT implemented (the measured
  costs are negligible at target frame budgets; revisit only with rig
  evidence).
- Compaction follow-ups and the multiplayer/replication **implementation
  choices** — `docs/MULTIPLAYER-SPEC.md`'s player-facing multiplayer and
  governance rules are **ratified product requirements**; what remains
  incomplete or unratified is the networking, transport, and replication
  implementation design (the spec is never treated as mere research).

## 7. Actual performance evidence (and what is NOT evidence)

- **CPU render path (pod, `--benchmark-render`):** the F-02 scratch form
  p95 0.081 → 0.035 ms/frame (−57%), byte-identical geometry; the F-05
  overlay breakdown (glyph vertex rebuild 0.090–0.131 ms dominant; format
  chain and percentiles negligible). Pod CPU evidence only.
- **Entity scale (pod, `--benchmark-entity-scale`):** 1k / 5k / 20k
  entities × three LOD scenarios, bounded event execution per tick with
  double-run determinism. Bounded-cost evidence, **not** sustained-load
  proof (F-12/F-08 open). **Read-through correction (Cycle 1, measured
  2026-11-14):** at the 5k and 20k points the benchmark sustains only
  **1,000 live routine chains** — `npc::routine_payload_parts` rejects
  identities ≥ `NPC_COUNT` (1,000), so entities above the first thousand
  execute their initial event and never chain (measured: ≈154–162
  executions/tick at ALL three populations; pending stays exactly 1,000).
  The historical at-scale numbers are one-1000-chain steady states, not
  scaled-chain evidence; the sustained-load harness below realizes full
  chains at every population with its own codec.
- **Sustained load (pod, Cycle-1; corrected per the R2/R3 corrective):**
  the F-08/F-12 reference matrix — **`--sustained-load-full-matrix`** runs
  it; **`--benchmark-sustained-load`** is the bounded demonstration
  command. The raw measurement record (every cell, double-run, with
  environment parameters and reason-coded N/A lines) is archived at the
  coordination ledger as
  `cycle1/evidence/P2-sustained-load-full-matrix-R2R3.output` — this
  summary qualifies it and replaces no raw result. **Overload onset is a
  function of population × LOD composition, not population alone**
  (measured base demand per logical tick against the 200/tick budget):
  at 1k, no scenario's base saturates (full 154.1, mixed 84.1,
  event-driven 13.7); at 5k, full and mixed saturate the budget while
  event-driven (69.2) stays sustainable; at 20k all three saturate. The
  overloaded cells measure the honest "fast-because-falling-behind"
  (per-logical-tick backlog slopes — the R2 units corrective — e.g.
  +57.7/tick at 5k-full, +199.7/tick at 1k-event-driven-overlaid; the
  gradual-growth regression pins +1/tick so slow unsustainability cannot
  masquerade as sustainable). **Recovery horizons are derived from
  measured drains** (the R3 corrective: the 10× rule over observed drain
  times — 1k drains 100–150 ticks → 1000–1500-tick horizons, one
  resource-capped at 1200, stated); at 5k/20k no complete drain occurs
  within the bounded probe, so those cells are explicit bounded
  observation windows with backlog/age trends reported and **no
  complete-recovery claim**. Mixed-LOD splits lane-vs-lane (near
  overloaded / far designed cadence). Compacted-representation variants
  show the ledger downgrade. Pod CPU evidence only, logical-time
  determinism throughout.
- **Player prototype (pod, Cycle-1 `--player-headless` / offscreen):**
  collide-what-you-see physics verified end-to-end (the scripted walk
  stops against the visible cube at the exact SKIN offset); the
  libm-free trace class is cross-platform bit-stable (pinned
  `8513d40792ea88f0`, reproduced at the integrated three-package tree).
  **No human-played run has occurred** — offscreen runs are correctness
  smokes only.
- **Persistence (pod, the F-07 harness):** ~1295 sequential ops/s, ~9%
  worker-queue overhead, contention p99 128 ms, cold recovery ~34.7 ms,
  ~19× head-of-line amplification — pod measurements of the approved
  topology, not universal guarantees; the actionable interpretation is
  delivered
  (`docs/F07-PERSISTENCE-OPTIMIZATION-ANALYSIS-2026-11-14.md`, §9).
- **Real GPU (the only real-GPU evidence):** the Stage 4 Windows acceptance
  record (2026-10-21, the owner's RTX 5070 Ti rig — e.g. 1744 fps avg
  uncapped/immediate on the Stage 4 scene). **Standing rule: no CI or pod
  number is GPU evidence** (llvipe/lavapipe are correctness smoke only).
  The current-scene hardware baseline procedure is
  `docs/WINDOWS-GPU-PROFILING-KIT.md` (F-09-corrected), awaiting the owner's
  rig.

## 8. Known limitations (the honest list)

- **LocalFileMedium on Windows is `ProcessCrashOnly`** — the documented std
  Windows sequence (file fsync + rename ordering) establishes
  acknowledged-revision persistence across process crash only, NOT OS
  crash or power loss, and it is insufficient for the ratified
  destructive-reclamation durability requirement (the OC-P2 tier gates
  refuse the destructive arms on that platform). Production Windows
  deployments use the SQLite medium (`TransactionalWalFull`).
- The renderer is prototype-class: static scene buffers, host-visible
  persistently-mapped dynamic buffers, 2 frames in flight, and a **bounded
  region-vertex capacity** (F-03 — the truncation is now COUNTED and
  reported, measured zero on every ordinary run, but the capacity limit
  itself stands; the Step-2 refusal policy is deferred by design).
- The player prototype is prototype-class: kinematic axis-aligned
  collision only (no capsules/rotated geometry/slopes/moving platforms),
  the third-person camera has no collision handling, controller state is
  **transient** (not persisted; restart resets the player), movement runs
  beside the spine (not as spine events), and **no human-played run has
  occurred** — offscreen runs are correctness smokes.
- The sustained-load harness's documented Cycle-1 limitations: class 6's
  RegionDirector-protocol anchor is harness-driven (the director's own
  transition semantics stay covered by the regions/s6 suites, not the
  matrix), and the sustained codec's 16-bit identity field contradicts
  its 2^24 restore bound (latent — no delivered path triggers it; a
  follow-up tightening).
- The entity-scale benchmark's 5k/20k dead-chain read-through (§7): the
  historical at-scale evidence is one-1000-chain steady state.
- Sustained-load scale is QUALIFIED with composition-dependent
  interpretations (sustainable at 1k across scenarios; at 5k the full and
  mixed compositions genuinely overload the 200/tick budget while
  event-driven stays sustainable; at 20k all compositions overload —
  measured, per-tick units), but production-scale claims remain out; the
  reference matrix is pod CPU evidence with logical-time determinism,
  not a universal throughput guarantee, and the 5k/20k recovery cells
  are bounded observation windows, not complete drains.
- No GPU asset streaming (F-04); dynamic content moves through per-frame
  host-visible copies.
- No networking, replication, physics, combat, procedural generation, asset
  pipeline, or editor systems (§10 — not yet designed, not hidden).
- The engine is a **development foundation + demo**, not a production game
  engine; nothing in the current evidence supports production-readiness
  claims for the complete game.

## 9. Active development priorities

The post-audit remediation cycle (2026-11-14) **is merged** — `main` @
`900b0ea` carries F-09 GPU-kit/CLI correctness, F-11, F-14, F-13, and the
Milestone-2 analyses (`docs/SUSTAINED-LOAD-METHODOLOGY.md`,
`docs/F07-PERSISTENCE-OPTIMIZATION-ANALYSIS-2026-11-14.md`,
`docs/GPU-CAPACITY-SYNC-ANALYSIS.md`,
`docs/F10-FAULT-SEAM-COVERAGE-MATRIX-2026-11-14.md`).

**Cycle 1 (2026-11-14/15, the owner's GO directive preserved verbatim with
SHA-256 at the coordination ledger, §10)** — three packages delivered,
independently cross-reviewed (all ACCEPT), consolidated on
`s7-cycle1-integration`: **P1** renderer overflow visibility (F-03 Step 1,
merged `359fbc6`), **P3** the player movement/collision prototype (merged
`7101433`), **P2** the sustained-load verification harness (F-08/F-12,
merged `0940a62`), the §8 reconciliation (`0eeba4c`), and the **ChatGPT
corrective round** (the HOLD review, hash-verified at the ledger): R1 the
enforced CI execution of the oversize regression (`47c7f8e`), R2/R3 the
per-logical-tick slope units + derived recovery horizons (merged
`2b620eb`), R4 the player-input boundary contracts (merged `ee23927`) —
each corrective independently verified by a non-author reviewer (all
ACCEPT; verdicts archived in `cycle1/reviews/`).

The **corrected Cycle-1 candidate** (full ancestry from `900b0ea`) measured
at its exact tree: fmt/clippy clean, main **984/1/30**, heavy **483/4/49**,
every determinism pin byte-identical, the P3 frozen trace reproducing, and
the cross-package combined-semantics check green (the player proxy through
the truncation counters — 0 omitted). The five-job `workflow_dispatch`
candidate CI (including the enforced R1 execution step) is dispatched at
this exact SHA; per-job conclusions are recorded in the cycle report.
**The cycle stops at the owner-reserved mainline-merge gate** — pending
ChatGPT's independent re-review and the owner's decisions on the mainline
merge itself, the F-03 Step-2 capacity/refusal policy (recommendation
delivered, not ratified), and the Milestone-3 next-cycle proposal.

## 10. The authoritative coordination ledger

- **Location:** `/home/coder/sektor7-coordination/` on the development pod
  (the live operational ledger since the 2026-11-14 clean-slate restart —
  the legacy `~/.qwen/sektor7-coordination/STATUS.md` location was wiped in
  that day's workspace purge and is obsolete) — **outside the Git
  repository, by charter**. It holds the owner's directives preserved
  verbatim with SHA-256 (`owner-directives/`, verified against
  `HASHES.sha256`), the cycle charters/matrices/challenges/reviews/deliveries
  (`cycle1/`), and the relay log (`sessions/registry.md`).
- **Ownership:** the **GLM orchestrator session** (`coder-1e`) is the sole
  writer, under the standing chain of command — Owner → ChatGPT (independent
  technical lead) → orchestrator → engineering seats. Seats receive
  provenanced relays (owner-directive + hash references, relay IDs
  registered in the ledger). The owner's authorizations are the preserved
  directive files, never paraphrase.
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
| Deterministic simulation & scheduling | Implemented and validated (demo scale; sustained-scale measured at the covered populations — §7/§8) | `engine-core` spine; the determinism pins; the CI battery; the entity-scale benchmark; the sustained-load reference matrix |
| Persistence & recovery | Implemented and validated | `engine-persist` store/journal/pipeline; the crash/recovery matrices; real process-kill proofs; cross-platform CI; the per-platform durability tiers (§5/§8) |
| Rendering (Vulkan presentation) | Partially implemented (prototype-class design) | `engine-renderer`; lavapipe + rig validation; F-03 Step 1 delivered (fail-visible truncation counters), Step-2 policy + F-04 open |
| Shader/material architecture | Prototype only | Hand-written build-time naga shaders; no material system |
| GPU resource streaming | Not yet designed | F-04 deferred; host-visible copies only |
| NPC/world representation (LOD) | Partially implemented | The region director, four LOD levels, activation/reconstruction — validated at 1000 NPCs / 8 regions; game-scale behaviors undesigned |
| Physics and collision | Prototype only | The Cycle-1 kinematic player prototype (`engine-core` player module: swept-AABB collision, fixed-step movement, FP/TP cameras) — headless/offscreen validated, collide-what-you-see; reversible and explicitly NOT a ratified production physics backend (§8) |
| Combat and gameplay systems | Not yet designed | — |
| Networking and replication | Not yet designed | The player-facing multiplayer and governance rules are **ratified** in `docs/MULTIPLAYER-SPEC.md`; the networking/transport/replication implementation choices remain unratified and undesigned |
| Listen-server authority | Not yet designed | Player-hosted listen-server is the stated working preference (unratified); no engine system |
| Host handoff | Not yet designed | Planned-required per the ratified multiplayer requirements; no engine system |
| Co-op roster governance | Not yet designed | The player-facing governance rules (roster democracy, unanimity requirements) are **ratified** in `docs/MULTIPLAYER-SPEC.md`; no engine system |
| Procedural world generation | Not yet designed | — |
| Dense city streaming | Not yet designed | F-08 deferred |
| Assets and content pipeline | Not yet designed | — |
| Editor/creation tools | Not yet designed | — |
| Performance instrumentation | Implemented and validated | The benchmark flags; the run summaries; the F-06 canary; the GPU kit; the Cycle-1 sustained-load reference matrix and the renderer truncation counters |
| Production deployment | Not yet designed | The CLI is a development driver, not a product |
