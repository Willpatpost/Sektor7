# TECH ARCHITECTURE — Engine Technical Architecture (Provisional Blueprint)

> **Status:** Engine technical architecture v0.3 · 2026-10-19 · **PROVISIONAL IMPLEMENTATION BLUEPRINT** — the six-layer architecture is accepted by the lead's three-way review with six required corrections (v0.2), plus the §3.2 clause-3 ordering-contract clarification ratified and applied in this v0.3. The **foundational technology choices are DECIDED** (2026-10-19; DECISIONS T-01–T-06): **Rust + Cargo**, **direct Vulkan as the single initial rendering backend**, **SDL3** for windowing/input, `ash` as the Vulkan-binding investigation candidate, and a minimal GLSL→SPIR-V shader path when rendering begins. **Stage 0 (workspace) and Stage 1 (SDL3 application shell) are authorized and complete (T-08, T-10); Stages 2–6 remain gated and none has begun.** Everything beyond the ratified foundation and corrected contracts — subsystem interiors, Architecture E (canonical-medium networking), the tiered-hybrid simulation architecture — remains **research hypothesis, never ratified design**. No ratified gameplay requirement is changed; no settled gameplay policy is reopened; no developer-paid recurring infrastructure is required or assumed; the Twin Summits (VISION §1) stand at full strength — the vertical-slice targets below are **architectural-feedback instruments, not performance claims**.
>
> **Ownership:** This document owns the **engine technical architecture** (provisional blueprint): initial logical subsystems and boundaries, state ownership and inter-subsystem contracts, the technology decision map (foundation decided 2026-10-19; remaining rows open or deferred with stated triggers), and the vertical-slice plan with its stage gates. It **defers to**: [`VISION.md`](VISION.md) (Twin Summits, launch commitments, collaboration charter), [`DESIGN-BIBLE.md`](DESIGN-BIBLE.md) and [`MULTIPLAYER-SPEC.md`](MULTIPLAYER-SPEC.md) (first-game policy — never redefined here), [`CUSTOM-ENGINE-RESEARCH.md`](CUSTOM-ENGINE-RESEARCH.md) (research methodology and evidence standards), [`MULTIPLAYER-ARCHITECTURE-RESEARCH.md`](MULTIPLAYER-ARCHITECTURE-RESEARCH.md) and [`E5-CANONICAL-MEDIUM-FEASIBILITY.md`](E5-CANONICAL-MEDIUM-FEASIBILITY.md) (persistence/authority hypotheses), [`SIMULATION-ARCHITECTURE-RESEARCH.md`](SIMULATION-ARCHITECTURE-RESEARCH.md) (simulation hypotheses; experiment records E-07–E-10), and [`DECISIONS.md`](DECISIONS.md) (the registry — nothing here is a decision until the lead ratifies it).
>
> **Method rule (anti-abstraction):** every subsystem boundary below is anchored either to (a) a contract already exercised in miniature by experiments E-07–E-10 with an independent oracle and deterministic fingerprints, or (b) platform reality (a windowed real-time application requires platform, render, and input). Boundaries with neither anchor are explicitly deferred (§8). No plugin registries, no speculative frameworks, no layer whose only job is indirection.

## 0. Executive summary

The simulation research phase (E-07–E-10, plus the E5 local harness on the multiplayer line) validated the load-bearing correctness contracts of the tiered-hybrid architecture at 1,000-NPC scale: LOD economics with capture-before-discard, four-lane narrative gating, true-dormancy reconstruction with a measured replay/synthesis crossover, and queue behavior under bursts and sustained overload with near-field reservation and overdue-clock honesty. This proposal consolidates that evidence into **twelve subsystems in six layers**, distinguishes reusable engine mechanics from first-game policy (policy lives in the game layer in whatever form serves it — code, data, configuration, or a future scripting layer — never inside reusable engine mechanisms), and defines the **time contract** the lead directed: one canonical campaign clock, per-region/per-class processed-through watermarks, and causal correctness for events whose intended timestamps precede their actual execution. The technology decision map proposed shortlists — **Rust** (language, alternative C++), **Vulkan** as a single initial backend (alternative D3D12-first), **SDL3** for window/input (alternatives raw platform layer / winit) — with exactly **three blocking decisions** before the first executable; the 2026-10-19 review **ratified all three** (DECISIONS T-01–T-04), and physics and asset formats remain non-blocking. The vertical slice ("Slice 0: Spine & Block") demonstrates all seven required capabilities in seven staged gates with per-stage acceptance criteria, scoped for rapid architectural feedback without reducing either summit. The six-layer architecture is accepted as a **provisional implementation blueprint** with the six review corrections applied in this v0.2; §7 records the review outcome, and Stage 0 (workspace only) is authorized and complete.

## 1. Evidence base and design principles

| Evidence | What it anchors in this architecture |
|---|---|
| **E-07** (S1/S6-lite) | Four LOD levels with honest cost shapes; durable-delta **capture-before-discard** at deactivation; pending survival with exactly-once reactivation (idempotency keys + applied-key ledger); deterministic logical fingerprints |
| **E-08** (S4-lite) | **Four-lane event classification** (gated milestones, vote-dependent decisions, ordinary routines, emergent consequences); M-07 frontier/city boundary enforcement; hold-without-expiry; revalidate-then-present; rejection with fallback supersession; dependency-ordered execution; cost tracks events, not elapsed time |
| **E-09** (S3-lite) | **True dormancy** (nothing processes); replay/synthesis parity for derivable lanes; durable lane never written by synthesis; reconstruction invariants (reachability, bounds, clock coherence); crossover measured in **event work** (~7.6–12.4 events/NPC), never elapsed time |
| **E-10** (S2-lite) | Spine **budgets with near-field reservation**; stable-under-sustainable, burst recovery, predictable overload growth; exactly-once + in-class `(due, seq)` order + cause-before-effect under overload; **overdue events never look silently current**; schedule synchrony clusters (budgets sized to cluster peaks) |
| **Multiplayer study + E5 annex** (hypotheses) | Two-lane state model (durable progression vs encounter transactions); commit pipeline with T1/T2/T3 durability tiers; canonical-medium conditional-append interface; fail-closed policy; fencing/epochs — all behind engine interfaces, never assumed |
| **Platform reality** | Window, render device, input, OS lifecycle, file I/O — required by any interactive application; kept as thin as the evidence-backed core is precise |

**Design principles** (each traceable to evidence or platform necessity):

- **P1 — Cost follows work.** Simulation cost scales with meaningful change (events), not entity count × frame rate. LOD, budgets, reservations, and the crossover-in-event-work rule all express this (E-07/E-09/E-10).
- **P2 — Single-writer state with explicit lanes.** Durable / pending / provisional-encounter / temporary lanes have different owners, lifetimes, and restoration semantics (E-07/E-09; multiplayer study §3).
- **P3 — Time is a service, and honesty is part of the contract.** One canonical campaign clock; per-region and per-class processed-through watermarks; lateness recorded and exposed; overdue state is never silently presented as current (E-10; the lead's consolidation directive).
- **P4 — Determinism and testability by construction.** Execution logs, canonical fingerprints, independent oracles, and headless harness modes are engine features, not afterthoughts (all four experiments; Diagnostics subsystem).
- **P5 — Engine/game separation by placement, not form** *(corrected 2026-10-19 per review)*. Game-specific policy may be implemented in code, data, configuration, or a later scripting layer — the invariant is that it stays **outside reusable engine mechanisms**. Budget numbers, LOD thresholds, near/far classification, and milestone and gating definitions are owned by the game in whatever form serves it; the engine provides generic mechanisms (E-05, E-08).
- **P6 — Every boundary needs a use case.** A subsystem boundary exists only where two parties exchange state or services (method rule above).

## 2. Subsystem architecture

```text
 First-game policy (DESIGN-BIBLE / MULTIPLAYER-SPEC — data & config, never engine code)
 ────────────────────────────────────────────────────────────────────────
 L5 CONTENT & TOOLS     Asset Pipeline (import/build/validate/cache)
 ────────────────────────────────────────────────────────────────────────
 L4 SESSION BOUNDARY    Session Authority interface · Replication Interest · Identity
                        (consumes Architecture-E hypotheses; transport undecided)
 ────────────────────────────────────────────────────────────────────────
 L3 PRESENTATION        Render Layer (device, render graph, frame pacing)
                        Streaming (asset/world residency — three decoupled axes)
 ────────────────────────────────────────────────────────────────────────
 L2 PERSISTENCE         Commit Pipeline · Revision Store · Canonical-Medium interface
 ────────────────────────────────────────────────────────────────────────
 L1 WORLD & SIMULATION  Clock & Spine (budgets, reservation, watermarks, overload
                        accounting) · World Store · LOD Director & Reconstruction ·
                        Gate Keeper · Physics Boundary · Simulation Systems
 ────────────────────────────────────────────────────────────────────────
 L0 CORE RUNTIME        Platform Shell (lifecycle, window) · Input & Player Control ·
                        Runtime Foundation (jobs/memory/async IO) · Diagnostics
```

### 2.1 Application lifecycle and platform abstraction — *Platform Shell* (L0)

**Owns:** ordered subsystem startup/shutdown (an explicit, reviewable sequence — no runtime plugin registry unless a second application demands one), window creation and OS message pump integration, wall-clock and monotonic OS time sources, file-system paths, process exit codes.
**Contracts:** `Platform::init(subsystem_registry) → Shell`; frame-pump callbacks; `WallClock` (never used as campaign time — P3).
**State:** none authoritative (lifecycle metadata only).
**Reusable vs first-game:** fully reusable; no game policy.
**Anchor:** platform reality. Deliberately thin — OS specifics live here and nowhere else.
**Stage 1:** the window/event-pump/lifecycle seed of this subsystem is implemented in `engine/crates/engine-shell` (SDL3 window creation, OS event pump, CPU-paced loop, clean shutdown; input intents per §2.9).

### 2.2 Rendering and graphics-device abstraction — *Render Layer* (L3)

**Owns:** the single chosen graphics backend (**Vulkan — DECIDED**, §5/T-02), swapchain and frame pacing, frame recording and submission, material parameter blocks, debug overlays. A formal render graph is **deferred until concrete multi-pass, shared-resource use cases justify it** (§8; review correction 5).
**Contracts:** consumes **render proxies** (position/appearance data) published by the World Store snapshot; never reads simulation internals; `RenderGraph::build(proxies, camera) → frame`; GPU timestamp queries feeding Diagnostics.
**State:** GPU resources and derived proxies only — **never authoritative world state**.
**Reusable vs first-game:** device, graph, and pacing are reusable; the cyberpunk material/art set is first-game content.
**Anchor:** platform reality; Summit A is the long-term driver (CP2077-class fidelity is a renderer/materials/streaming campaign, not an architecture change). **One backend initially; no speculative multi-API abstraction** (§5).

### 2.3 World/entity representation — *World Store* (L1)

**Owns:** stable entity IDs (never reused — tombstones per I6), per-archetype data pools, per-region resident lists, entity→region assignment, render-proxy snapshots.
**Contracts:** batched iteration over a region's residents (data-oriented; the S1-lite full/simplified workloads define the access pattern); `Store::snapshot(region_set) → ProxyBatch` for rendering; tombstone semantics.
**State:** the *live authoritative* representation of entities within the active session (composed of the persistence lanes, §3).
**Reusable vs first-game:** storage and iteration mechanics reusable; archetypes, schedules, and NPC record schemas are game data.
**Anchor:** E-07/E-09 batched workloads; prototype MultiMesh lesson (PROTOTYPE-INVENTORY §3). **No ECS framework is selected or assumed** — storage starts as plain data pools; a framework decision is deferred until batch needs outgrow pools (§8).

### 2.4 Simulation clock, event spine, scheduling, overload accounting — *Clock & Spine* (L1)

**Owns:** the **canonical campaign clock** (single, monotonic, advances only in the spine tick loop — M-27); near/far event priority queues with deterministic `(due, seq)` queue priority; **cross-class causal dependencies** (`depends_on` links — the only cross-class ordering mechanism, §3.2 correction 2); per-tick budget with **near-field reservation**; per-region and per-class **safe processed-through watermarks** (distinct from max-executed, §3.2 correction 1); backlog/overdue/lateness accounting; the execution log; both schedule semantics — logical-time and **execution-relative** (§3.2 correction 3).
**Contracts:** `schedule(spec) → EventId` — dependency ids validated at schedule time, unknown ids rejected and never silently dropped; `tick()` processes near up to reserve, far up to budget−reserve, leftovers near-first (E-10-validated policy candidate); `max_executed(region, class)` (raw progress measure) vs `processed_through(region, class)` (safe watermark — computed live, may retreat on late arrivals); `overdue(region)`; execution records `(seq, class, intended_due, executed_tick, schedule_kind)`. The full time contract is §3.2. **Stage 0 implements the classless core of these contracts** (single queue, budget, dependencies, both schedule kinds, watermark, ledger) in `engine/crates/engine-core`, with deterministic fingerprint and contract tests; lanes and regions attach above it in later stages.
**State:** the queues, dependency registrations, and watermarks; **not** entity state.
**Reusable vs first-game:** all mechanics reusable; budget/reserve numbers, near/far classification criteria, and cluster desynchronization strategy are game configuration (E-10's synchrony finding: budgets are sized to **cluster peaks**, not average rates).
**Anchor:** E-10 (all queue behavior); E-08 (cost tracks events); Stage 0 core tests (`engine/crates/engine-core/tests/`).

### 2.5 NPC simulation LOD and region activation/deactivation — *LOD Director & Reconstruction* (L1)

**Owns:** region LOD state machine (full / simplified / event / dormant), presence-driven targeting with hysteresis and velocity-lookahead, activation **reconstruction** (replay / synthesis / adaptive by estimated event work), deactivation **capture-before-discard**.
**Contracts:** `set_presence(players)`; `activate(region, method)` reconstructs from checkpoint + durable lane; `deactivate(region)` captures pending deltas into the pipeline, stamps `next_event`, discards representations; reconstruction invariants (reachability from last observed position, region bounds, clock coherence; synthesized spans labeled).
**State:** region LOD levels, checkpoints (last-processed task, observed position, oneoffs, applied keys).
**Reusable vs first-game:** machinery reusable; LOD thresholds and the adaptive crossover budget are game configuration informed by E-09's measured crossover.
**Anchor:** E-07 (I1 capture semantics, cost shapes), E-09 (true dormancy, reconstruction parity and crossover).

### 2.6 Persistence and live authoritative state — *Commit Pipeline & Revision Store* (L2)

**Owns:** the two-lane model's write path — durable-delta capture with idempotency keys, applied-key ledger, pending-vs-confirmed status, encounter-transaction rollback records; the revision store (append log + snapshots) and verification; the **canonical-medium interface** (conditional-append semantics) with a **local file-backed implementation** for solo/single-machine use and pluggable providers (Architecture E) behind it — provider selection undecided and not required by the engine.
**Contracts:** `capture(key, delta, tick) → pending|duplicate`; `status(key) → pending|confirmed`; `rebuild(region) → state` (applied **and** pending); retry-safe by construction.
**State:** the durable lane (permanent); pending deltas until acknowledged. The **live authoritative state** of the host session = committed + pending + provisional encounter lane (SIM §3 composition) — owned by the simulation session, *backed* by this subsystem.
**Reusable vs first-game:** pipeline, ledger, verification, and the medium interface are reusable; milestone definitions and which actions gate on confirmation are game policy (M-29's irreversible-action gating list lives in the game).
**Anchor:** E-07/E-09 (exactly-once, rebuild parity); multiplayer study §2–§8 and E5 annex (as **hypotheses** the interface must accommodate: T3 acknowledgment, fail-closed, fencing).

### 2.7 Resource loading and world streaming — *Streaming* (L3)

**Owns:** async asset/world residency with **three decoupled axes** — simulation residency (LOD level), visual residency (camera-driven), replication interest (per-player relevance) — each driven independently (SIM §8).
**Contracts:** prefetch by velocity lookahead; residency budgets per axis; `residency(region, axis)` reporting to Diagnostics; interior cells as nested regions.
**State:** residency sets and in-flight loads.
**Reusable vs first-game:** mechanics reusable; budget numbers and far-field fidelity policy (per-NPC vs statistical vs visual-only life) are game configuration (SIM §11.2 open).
**Anchor:** SIM §8 design; the long-term Summit-A streaming campaign (CER Stage 4). Slice scope is minimal (§6).

### 2.8 Physics/collision integration — *Physics Boundary* (L1)

**Owns:** the fixed-step integration of a physics library **behind a thin boundary**; broad-phase tied to region activation (only full-LOD regions simulate); gameplay-critical contact events emitted **as spine events** (deterministic, ordered); character controller and vehicle integration points.
**Contracts:** `step(region, fixed_dt)` → contact events scheduled on the spine; queries (raycasts) for gameplay; **no ownership of authoritative transforms** beyond the integrated bodies it steps.
**Reusable vs first-game:** boundary reusable; library selection open (§5 — not blocking); damage/vehicle rules are game policy.
**Anchor:** platform/simulation reality; determinism requirement P4 (fixed-step, event-fed). RCT-class efficiency note: physics runs only where players are.

### 2.9 Input and player control — *Input & Player Control* (L0/L1)

**Owns:** device events → action mapping → player intents; camera control; intent delivery to the simulation session (host-authoritative in multiplayer).
**Contracts:** `Actions::poll() → IntentBatch`; intents enter the spine as near-field events (highest reservation priority); camera state owned here, published as a render-proxy consumer.
**State:** device and mapping state; camera.
**Reusable vs first-game:** mapping mechanics reusable; bindings, perspective policy (first/third-person), and control schemes are game data (DESIGN-BIBLE §1).
**Anchor:** platform reality; near-field reservation (E-10) is what makes player-intent events starvation-free.
**Stage 1:** the intent layer is implemented in `engine/crates/engine-shell/src/input.rs` — WASD axes, mouse-look deltas under relative capture, key/button edges, focus-loss clearing (anti-stuck-key and stale-delta prevention), resize, quit. Intent only, no gameplay; verified by synthetic-event dispatch tests under SDL's dummy driver (dispatch-level evidence, not hardware input verification).

### 2.10 Networking/replication integration boundaries — *Session Boundary* (L4)

**Owns:** the **interfaces only** — single-authority session model (one authoritative session per campaign, M-28), interest-scoped replication of state changes, member identity, and the surface the canonical-medium provider plugs into.
**Contracts:** `Session::open(campaign, medium) → authority` (conditional-append semantics, fencing by epoch); `Interest::scope(player, state) → replica set`; validation gate before authoritative play.
**State:** session/lease metadata; no world state.
**Reusable vs first-game:** interfaces reusable; roster rules, voting, reputation are game policy (MULTIPLAYER-SPEC) layered above.
**Anchor:** multiplayer study §2/§5/§10 — **Architecture E is a hypothesis this boundary must accommodate, not an assumed implementation**; transport and medium providers remain undecided (P0 mechanisms; E5 probe gated).
**Slice scope:** not demonstrated — the slice runs single-machine with the local medium implementation only.

### 2.11 Asset pipeline and creation tools — *Asset Pipeline* (L5)

**Owns:** import (glTF/KTX2 interchange — §5), validation, content-addressed caching (CER §7), cooked-format generation later; editor and agent-facing tooling as a later milestone (E-05 working preference).
**Contracts:** `import(source) → validated asset`; provenance and license metadata recorded (CC0 discipline from the prototype); deterministic builds.
**State:** the asset cache.
**Reusable vs first-game:** fully reusable; assets are game content.
**Anchor:** CER §7 near-term deliverables; prototype licensing lessons.

### 2.12 Diagnostics, profiling, testing, performance instrumentation — *Diagnostics* (L0, consumed by all)

**Owns:** frame-time percentiles (avg/P95/P99, 1%-lows), per-subsystem budget accounting, GPU timestamps, sim metrics (events/sec, backlog, lateness distributions, memory per NPC per LOD, activation/deactivation latency), the headless mode with **logical-state fingerprints**, and the harness pattern (e5-lab-local style) as the regression rig.
**Contracts:** every subsystem reports to a single metrics bus; overlays render via the Render Layer's debug path; CI runs headless fingerprints.
**State:** metrics only.
**Reusable vs first-game:** fully reusable — this is the Summit-B **instrument**; without it neither summit is measurable.
**Anchor:** all four experiments (fingerprints, oracles, distributions); CER §5 instrumentation requirements.

## 3. State ownership, lanes, and the time contract

### 3.1 Lanes and owners (single-writer discipline)

| Lane | Contents | Owner (writer) | Lifetime | Restoration |
|---|---|---|---|---|
| **Durable (committed)** | NPC identity/relationships/memories/oneoffs, world consequences, milestones, frontier history | Commit Pipeline (via capture) | Permanent | Exact; never rolled back |
| **Durable (pending)** | Captured, unacknowledged deltas | Commit Pipeline | Until ack | Exact; survives deactivation; exactly-once reapply |
| **Provisional (encounter)** | Pre-encounter resource snapshots, uncommitted loot | Simulation session | One encounter | Atomic commit or rollback (M-30 pattern) |
| **Temporary (representation)** | Micro-state, transforms, physics scratch | Simulation systems | One session / one LOD span | Reconstructed (replay or labeled synthesis); never persists |
| **Derived (render proxies)** | Appearance snapshots | World Store snapshot | One frame | Rebuilt every frame; never authoritative |

### 3.2 The time contract (lead directive 2026-10-18/19; corrections 1–3 ratified 2026-10-19)

1. **Canonical campaign clock.** One monotonic clock per campaign (M-27), advanced only by the spine tick loop. No other component may mutate or branch it. Wall-clock/OS time never enters simulation logic.
2. **Two progress measures; only one is a completeness claim** *(correction 1)*. Each region/class carries **`max_executed(region, class)`** — the maximum intended-due tick of events actually executed, a raw progress measure — and **`processed_through(region, class)`**, the **safe watermark**: the largest tick `T` such that (a) `T ≤ max_executed` (completeness is never claimed beyond work that has actually run, because a future late arrival may still carry an earlier due) and (b) **no pending event — queued, executing, or blocked by unresolved causal dependencies — has intended due ≤ `T`**. A watermark must not claim completeness while earlier required events remain queued, executing, or blocked. The safe watermark is **computed live from queue state, never cached**, and **may retreat when late events are scheduled**; consumers re-read rather than store. The campaign clock says "what time the world is at"; `max_executed` says "how far processing has actually reached"; `processed_through` says "how far may be treated as settled." All three legitimately differ under backlog or lateness. When no such `T` exists — nothing has executed, or a pending event sits at tick 0 — the safe watermark is `None` *(independent audit 2026-10-19, item 1)*. It is a **live progress measure, not irreversible finality**: it claims no permanence, and durable finality belongs to the Commit Pipeline's confirmed lane (§2.6), a separate subsystem *(audit item 5)*.
3. **Delayed events and causal correctness.** Every execution records `(seq, class, intended_due, executed_tick, schedule_kind)`; **lateness is first-class data**. Within a class, `(due, seq)` defines **deterministic queue priority**: each dequeue selects the minimum key among currently queued, due events. This is not a guarantee of globally monotonic dequeue or execution order. Causality-blocked events leave the active queue until their dependencies are satisfied and may subsequently reenter with their original priority; late arrivals may introduce earlier-due events after later-due events have already executed *(ordering contract clarified 2026-10-19 with the lead's ratified wording, replacing the former "execution order is `(due, seq)` — monotone and deterministic" sentence)*. Execution order is represented by the ordered execution ledger, including `intended_due`, `executed_tick`, and `late`; these fields expose timing and ordering deviations but do not necessarily identify their causes. Explicit `depends_on` relationships enforce required causal precedence, including across event classes; consumers must never infer causal guarantees solely from chronological queue priority. **Cross-class execution order remains unspecified except where explicit dependencies impose constraints** (a near event may legitimately execute before an older far event). The **only** cross-class ordering mechanism is the spine's explicit causal dependencies *(correction 2)*: an event may declare `depends_on: [EventId]`, and it cannot execute until every dependency has executed. Dependency ids must reference already-scheduled or already-executed events — unknown ids are **rejected at schedule time** (never silently dropped), which also makes dependency cycles structurally impossible, since a cycle would require depending on an event not yet scheduled. While a dependent is blocked it counts as *pending* and caps the safe watermark (clause 2). **Neither durable persistence (the Commit Pipeline) nor the narrative Gate Keeper is a general event-ordering solution** — they consume, and sit above, these spine primitives; any consumer needing ordering goes through dependencies, never by assuming clock order.
4. **Event timestamps vs execution timestamps** *(correction 3)*. Two schedule semantics exist, both first-class, and the spine never silently applies one rule to every event: **`AtLogicalTime { due }`** — logical scheduled-time semantics, the due tick fixed at schedule time, lateness recorded when execution happens after due; and **`ExecutionRelative { origin, delay }`** — due = the *scheduling event's actual execution tick* + delay, so follow-ups spawned by an overdue event anchor to actual execution rather than stacking impossible deadlines. Overdue events execute late with lateness recorded, and **state reads are watermark-labeled** — a reader sees `as_of(processed_through)` and an overdue indicator (the E-10 stale-and-pre-write flag generalized to an engine-wide query contract). Authoritative state never silently appears more up-to-date than it actually is.
5. **Clock advancement during skips/low-LOD epochs** follows the E-08/E-10 results: cost proportional to events; gated lanes hold and revalidate; routine lanes process within budget; the clock itself never branches.

**Stage 0 status (2026-10-19):** the classless core of clauses 1–4 — single queue with deterministic `(due, seq)` queue priority, cross-class `depends_on` with schedule-time validation, per-tick budget, both schedule kinds, the safe-vs-max-executed watermark with retreat on late arrivals, and the execution ledger with first-class lateness — is implemented and tested in `engine/crates/engine-core` (deterministic double-run fingerprints; contract tests for watermark capping and retreat, dependency blocking and release, execution-relative anchoring, budget exhaustion, and no silent drops). Regions and near/far lanes are not yet implemented; they attach above this core in their stages.

## 4. Contract sketches — the validated core

The five interface families below are **already exercised in miniature** by E-07–E-10 (Python harnesses with independent oracles). The vertical slice ports the *contract semantics* under the chosen technologies — it does not transplant harness code, which is evidence, not engine source.

| Contract | Sketch (contracts, not code) | Evidence |
|---|---|---|
| Clock & Spine | `advance(dt)`; `schedule(due, order_key, class, region) → seq`; `tick()` with budget B, near-reserve R, leftovers near-first; `watermark`, `overdue`, execution log | E-10 |
| Region & LOD | `set_presence(players)`; `activate(region, method ∈ {replay, synthesis, adaptive})`; `deactivate(region)` capture-before-discard; hysteresis | E-07, E-09 |
| Durable Store | `capture(key, delta, tick)`; `status(key)`; `rebuild(region)`; idempotent retries; canonical-medium interface `conditional_append(record, expected_head)` | E-07, E-09; medium semantics per multiplayer study (hypothesis) |
| Gate Keeper | `evaluate(event) → fire \| hold \| reject`; `revalidate(held_event)` before presentation; four lanes; dependency-ordered batches | E-08 |
| Reconstruction | `reconstruct(region, method)` with invariants: reachability from last observed position, bounds, clock coherence; synthesized spans labeled; adaptive chooses by estimated event work | E-09 |

Platform/render/streaming/input contracts are sketched in §2 and deliberately minimal until the slice exercises them (P6).

## 5. Technology decision map

**Evaluation criteria** (per the lead): Windows first, Linux second; modern graphical fidelity (Summit A); exceptional computational efficiency (Summit B); maintainability; developer ergonomics; long-term extensibility. Responsibilities (§2) were defined before libraries were compared. **Foundation ratified by the lead 2026-10-19 (DECISIONS T-01–T-06): the first four decision rows below are DECIDED; physics and asset formats remain deferred with stated triggers.**

| Decision | Alternatives | Trade-off summary | Recommendation (shortlist) | Blocks first executable? |
|---|---|---|---|---|
| **Implementation language** | **Rust** · C++ · C# · Zig | Rust: no GC + predictable memory (Summit B), fearless concurrency, cargo toolchain, tier-1 Windows+Linux, compiler-enforced correctness is a force multiplier for solo+agent development (prior SokomindRust experience); cons — slower gameplay iteration (mitigated by a later scripting decision), some middleware needs FFI. C++: maximal engine-ecosystem maturity; cons — memory-safety burden and build complexity for a solo+agent team. C#: fastest iteration; cons — GC and runtime control conflict with Summit B. Zig: promising; cons — pre-1.0 churn. | **DECIDED 2026-10-19: Rust** (C++ fallback retired for the initial foundation) | Resolved — T-01 |
| **Build system** | Cargo (Rust) · CMake/Bazel (C++) | Coupled to language. Cargo folds build+deps+test+CI into one low-risk toolchain. | **DECIDED 2026-10-19: Cargo** (coupled to Rust) | Resolved — T-01 |
| **Rendering backend** | **Vulkan** · Direct3D 12 · both via thin RHI | Vulkan: one code path for both decided platforms, validation layers, RenderDoc/NSight support, RT path for Summit A; cons — verbose ramp. D3D12: optimal Windows-first tooling (PIX); cons — a second backend owed for Linux. A dual-backend RHI now is a speculative abstraction (violates P6) — one backend first, contained. | **DECIDED 2026-10-19: Vulkan single initial backend**; D3D12 only later, behind a contained device interface | Resolved — T-02 |
| **Window/input integration** | **SDL3** · raw Win32+X11/Wayland · winit · GLFW | SDL3: mature, permissive, window+input+audio in one, both platforms, C API with maintained Rust bindings — buys cross-platform surface cheaply so the engine owns the abstractions that matter. Raw: maximal control; cons — duplicated Linux work. winit: Rust-native; cons — API churn history. GLFW: minimal; cons — narrow surface. | **DECIDED 2026-10-19: SDL3** (raw platform layer as fallback); **binding decided at Stage 1 (T-09): `sdl3` 0.20 over `sdl3-sys` 0.7.2, vendored SDL 3.4.18 built from source and statically linked** | Resolved — T-04, T-09 |
| **Physics integration** | Jolt · Rapier · Bullet/PhysX · custom-minimal | Boundary (§2.8) is thin and fixed-step regardless. Jolt: MIT, modern, AAA-proven, deterministic-friendly (C++, bindings). Rapier: Rust-native, well-maintained. Custom: only for deterministic gameplay-critical contacts. | **Jolt / Rapier shortlist; slice uses built-in minimal collision** | **NO** — slice needs spheres/boxes only |
| **Asset formats / pipeline** | glTF 2.0 · FBX/interchange · custom cooked | glTF: open interchange, CC0 availability, tooling; KTX2 for textures; cooked engine formats later with content-addressed cache (CER §7). | **glTF 2.0 + KTX2 interchange for the slice; cooked formats deferred** | **NO** — early slice stages use procedural primitives |

**Deferred sub-decisions (none block the slice; each has a stated trigger):** shader toolchain (**minimal GLSL→SPIR-V path when rendering begins — ratified as the starting approach 2026-10-19, T-05**; the permanent shader-language decision — e.g. Slang when material complexity grows — remains deferred); **Vulkan binding** (`ash` is the initial investigation candidate when renderer work begins — T-03; not yet a dependency, not yet selected); ECS framework representation (plain data pools first; revisit when batch needs outgrow pools); scripting layer (none until first-game gameplay code grows — then decide, with agent-authorability per E-05 in mind); audio backend; cooked-format schema; multiplayer transport and medium provider (P0 mechanisms; E5 probe gated on account-owner prerequisites); production physics middleware (Jolt/Rapier shortlist stands — T-06).

**Which decisions truly blocked the first executable:** (1) language + build, (2) rendering backend, (3) window/input integration — **all three decided 2026-10-19** (T-01, T-02, T-04). Everything else has a minimal path in the slice.

**Environment note (honest constraint):** development happens on a GPU-less Linux pod (software Vulkan via lavapipe is available — the Godot kit proved the pattern) plus the lead's Windows reference rig (Ryzen 7 9800X3D / RTX 5070 Ti). Stage 0 CI (`engine-ci`) runs headless on Linux and Windows runners — no GPU work exists yet, so lavapipe smoke enters with Stage 2; performance acceptance runs on the reference rig.

## 6. Vertical-slice plan — "Slice 0: Spine & Block"

**Goal:** a minimal but **genuine** interactive application that exercises the architecture end-to-end and produces measurable feedback. **Explicitly not** a fidelity or efficiency demonstration: slice targets are instrumentation milestones; the Twin Summits are pursued by the research/experiment program after the architecture proves itself (VISION §0 — no silent ambition reduction).

**Scope:** one small authored block (ground plane, a few simple structures or one CC0 glTF building), first-person movable camera, ~1,000 NPCs across 8 surrounding regions with schedules (the harness schedule model as game data), a day cycle visible through NPC activity and sky color, and the seven required capabilities:

1. Application startup and shutdown — ordered lifecycle, clean teardown.
2. Window, rendering, input, movable camera.
3. Simple 3D world with region activation — camera presence drives LOD.
4. Persistent NPC identities, schedules, and simulation LOD — all four levels live.
5. World clock and event scheduling — spine with budget, reservation, watermarks.
6. Local persistence through a reusable interface — the pipeline + local file-backed medium.
7. Diagnostics and measurable CPU/GPU performance — overlays + headless soak.

### Stages (each gated by acceptance criteria; no stage starts without the prior stage's report)

| Stage | Content | Acceptance criteria (measured) | Risks / dependencies |
|---|---|---|---|
| **0 — Workspace** | **COMPLETE 2026-10-19** (record below): Rust workspace `engine/` — `engine-core` (clock, spine, watermark, dependencies, fingerprints, diagnostics) + `engine-slice` (interactive CLI, self-test), zero external dependencies, deterministic fingerprint tests, CI on Linux and Windows | Build/test/fmt/clippy green on Linux (direct) and Windows (CI); double-run fingerprint test green in-process and cross-process | Met — blocking decisions resolved 2026-10-19 (T-01–T-04) |
| **1 — App shell (no GPU)** *(re-split per review correction 6)* | **COMPLETE 2026-10-19** (record below): `engine-shell` — SDL3 window creation, OS event pump, input intents (WASD, mouse-look, edges, focus, capture, resize, quit), CPU-paced loop, clean shutdown; **no presentation** | Met: 60-s soak 3601 frames (avg 16.67 ms, p99 16.76 ms, RSS 4656→4852 KiB); 100 create/close cycles green; input dispatch verified on Linux pod + Windows CI; frame pacing measured | Met — binding selected under the delegated Stage-1 authorization (T-09) |
| **2 — Presentation & first render** *(absorbs GPU work per correction 6)* | Vulkan instance/surface (the `ash` investigation lands here), swapchain configuration, **vsync acceptance**, then depth-correct primitives/one glTF asset; camera look/move; GPU timestamps; debug overlay | Correct present on rig + lavapipe smoke; vsync acceptance measured; 6-DoF camera; overlay shows frame ms + P95/P99 | Backend verbosity (mitigation: validation layers, RenderDoc) |
| **3 — Clock & spine in-app** | Campaign clock at fixed tick rate decoupled from render; 1,000 NPC schedules as data; budget + near reservation; backlog/lateness/watermark overlay; headless fingerprint mode | Deterministic fingerprints in headless CI; artificially lowered budget shows reported backlog (never silent); 200/tick synchrony-cluster spike drains without drops (E-10 behavior, in-app) | Tick/render decoupling correctness |
| **4 — Regions & LOD** | 8 regions; presence-driven LOD with hysteresis; render proxies only for non-dormant regions; activation/deactivation transitions | Dormant regions ≈ zero per-tick cost (verified by metrics); activation of a 125-NPC region < 50 ms (E-09 predicts ~ms); deactivation captures deltas (log-verified) | None new — ports E-07/E-09 contracts |
| **5 — Systems & persistence** | Full/simplified closed-form workloads (S1-lite definitions as game data); NPCs visibly moving on schedule paths; player "shove" input → encounter event with witnesses → durable memory records; pipeline → local file-backed store; quit → relaunch restores committed state | Restart round-trip: restored fingerprint == pre-exit committed fingerprint; double-commit retry is a no-op; pending/confirmed visible in overlay; encounter rollback restores pre-encounter resources | Serialization care; no networked medium in slice |
| **6 — Diagnostics & soak report** | 15-min automated soak; cluster-burst test; frame-time distribution; final slice report to the lead | Report contains: frame avg/P95/P99, sim tick ms, events/sec, memory per NPC per LOD, activation/deactivation latency, persistence commit latency, zero drops in burst test | — |

**Stage 0 record (2026-10-19, complete; corrective pass same date per independent audit):** workspace `engine/` — crates `engine-core` (library: `Tick` clock seed; event spine with deterministic `(due, seq)` queue priority, cross-class causal dependencies validated at schedule time, per-tick budget, execution ledger with first-class lateness, safe `processed_through` watermark distinct from `max_executed` — computed live, retreating on late arrivals, `None` when nothing is provably complete; FNV-1a fingerprints with length-prefixed strings, schema v2; level-filtered diagnostics) and `engine-slice` (interactive CLI + deterministic self-test scenario). **Zero external dependencies** (`cargo tree`: the two workspace crates only). Tests: 33 (7 unit, 16 integration — including the required deterministic double-run fingerprint test, contract tests for watermark capping/retreat/tick-0 `None`, dependency blocking/release, execution-relative anchoring, budget exhaustion, no silent drops, and two counterexample tests documenting that execution order deviates from `(due, seq)` dequeue priority under causal blocking and late arrival — and 10 session tests, including the u64::MAX payload regression). Verification: `cargo build` / `test` / `fmt --check` / `clippy -D warnings` green on Linux (dev pod, rustc 1.99.1); cross-process fingerprint double run matches (self-test: `engine=6504123a47cab66f`, `state=937bd3b6f56f07e0`); Windows via CI (`engine-ci`, ubuntu-latest + windows-latest). Corrective pass (independent audit 2026-10-19, lead-relayed): watermark tick-0 defect fixed (item 1); in-class ordering clarified in code as queue priority with recorded deviations (item 2 — the §3.2 clause 3 ordering contract now carries the lead's ratified clarification, applied in v0.3); demo-world payload arithmetic checked (item 3); fingerprint strings length-prefixed, schema tags v2, fingerprints re-baselined (item 4); watermark documented as progress-not-finality, tick saturation documented, `run_to_completion` termination scoped to finite event graphs (item 5). Deliberately absent: renderer, SDL3, ash, physics, ECS, scripting, audio, networking, editor, threading, registries, render graph (§8). No gameplay policy touched.

**Stage 1 record (2026-10-19, complete):** `engine/crates/engine-shell` — `Platform` (SDL3 initialization; driver selection with a process-sticky dummy mode for headless environments), `ShellApp` (real window with the OS event pump and a CPU-paced loop with frame statistics and reset-on-fallback cadence), and the input-intent state machine (`input.rs`: WASD movement axes, mouse-look deltas collected only under relative capture, key/button edges, key-repeat counting, focus-loss clearing of held keys and pending deltas, resize, sticky quit). OS monotonic time drives frame pacing only; the campaign clock is absent by design (Stage 3). **Binding and native strategy (T-09):** `sdl3` 0.20.0 (vhspace/sdl3-rs, MIT) over `sdl3-sys` 0.7.2 (maia-s/sdl3-sys-rs, Zlib), building vendored **SDL 3.4.18** from source with CMake and linking statically; a target-scoped `sdl-unix-console-build` feature permits headless Unix builds (desktop backends still autodetected where dev packages exist); the wrapper's satellite crates are locked but not enabled. The **single documented `unsafe`** in project-owned code selects the dummy drivers via environment variables (SDL3 removed the SDL2-era driver hints) — a reviewed boundary with its safety contract in `platform.rs`. Tests: **46 total** (33 Stage 0 preserved — fingerprints unchanged, `engine=6504123a47cab66f` / `state=937bd3b6f56f07e0` — plus 13 Stage 1: 11 synthetic-event dispatch tests and 2 lifecycle tests including 100 create/close cycles; the 60-second soak is an ignored-by-default measurement). Verification: fmt and clippy `-D warnings` clean; headless smoke 180 frames / 3.0 s at avg 16.67 ms; **60-second soak: 3601 frames, avg 16.67 ms, p50 16.67, p95 16.74, p99 16.76, max 17.03 ms, RSS 4656→4852 KiB** (no growth trend observed — not proof that leaks are impossible); Linux + Windows via CI. **Evidence separation:** all automated Stage 1 evidence is headless (dummy driver, synthetic-event dispatch — not hardware input verification); real-window GUI evidence requires a graphical session via the manual instructions in `engine/README.md` (the lead's rig). Exclusions honored: no Vulkan, ash, swapchain, GPU rendering/presentation, vsync, physics, ECS, world simulation, gameplay logic, multiplayer, persistence, audio, or editor code; SDL's own GPU/render APIs are untouched (Stage 2 territory).

**Measurements (all recorded, none claimed as Summit evidence):** frame-time distribution (avg/P95/P99, 1%-low), GPU frame time via timestamps, sim tick cost, events/sec, backlog/lateness distributions, memory per NPC per LOD, activation/deactivation latency, persistence round-trip latency.

**Risks:** backend ramp cost (single backend, validation layers); cross-platform FP determinism (S5 remains unrun — slice fingerprints are **per-platform**; no cross-platform equality claimed); scope creep (fixed capability list; no combat, no multiplayer, no streaming beyond region residency in the slice); engine/game boundary drift (all policy numbers in config, checked in review); asset licensing (CC0 only); GPU-less pod (lavapipe for CI smoke; performance on the reference rig).

## 7. Review outcome (three-way review, 2026-10-19)

1. **Implementation language + build system: DECIDED — Rust + Cargo** (T-01). The C++ + CMake alternative is retired for the initial foundation.
2. **Rendering backend: DECIDED — direct Vulkan, single backend**, no speculative multi-API abstraction (T-02). `ash` is the initial Vulkan-binding investigation candidate (T-03) — to be evaluated when renderer work begins, not yet a dependency.
3. **Window/input integration: DECIDED — SDL3** (T-04).
4. **Vertical-slice scope and stage plan: CONFIRMED with corrections** — the seven capabilities and the instrumentation-not-Summit-claims policy stand; Stage 1 is re-scoped to window creation, event handling, input, lifecycle and clean shutdown (no GPU), with GPU presentation, swapchain configuration and vsync acceptance moved to Stage 2 (correction 6, applied in §6).
5. **Defer-list: CONFIRMED and extended** — production physics middleware, ECS frameworks, scripting runtimes, audio, cooked asset formats and multiplayer/canonical-medium providers remain deferred with stated triggers (T-06); the permanent shader-toolchain decision is deferred behind the ratified minimal GLSL→SPIR-V starting path (T-05).
6. **Six architectural corrections required and applied in this v0.2:** (1) safe processed-through watermark distinct from max-executed (§3.2 clause 2); (2) explicit cross-class causal dependencies — durable persistence and the Gate Keeper are not general ordering solutions (§3.2 clause 3); (3) event-timestamp vs execution-timestamp semantics, both first-class (§3.2 clause 4); (4) the engine/game boundary is placement, not form (§1 P5); (5) anti-speculation rules — no subsystem registries, job frameworks or render graph before concrete use cases (§2.1, §2.2, §8); (6) the Stage 1/2 re-split (§6).
7. **Implementation authorization: Stage 0 — workspace only** — authorized and completed 2026-10-19 (§6 record; T-08). *Follow-up: Stage 1 was subsequently authorized after the independent review passed Stage 0 (commit `db05bd3`) and is complete (T-10); Stages 2–6 remain gated.*

## 8. Explicitly not decided / deferred (anti-abstraction ledger)

- Foundation selected by the lead's 2026-10-19 ratification and recorded in DECISIONS (T-01–T-06, T-09): Rust + Cargo, Vulkan single-backend, SDL3 (binding: `sdl3` 0.20 / `sdl3-sys` 0.7.2 with vendored SDL 3.4.18, statically linked), `ash` investigation candidate, minimal GLSL→SPIR-V starting path. **Still not selected:** physics library, asset format beyond interchange, scripting, audio, networking provider, and every subsystem interior below the ratified contracts.
- No ECS framework; entity storage starts as plain data pools behind the World Store contract.
- **No speculative subsystem registries** — startup/shutdown is an explicit ordered sequence until a second application demands one (§2.1; review correction 5).
- **No general job/scheduler framework** — Stage 0 is single-threaded; threading primitives arrive only with a measured need (review correction 5).
- **No render graph before concrete multi-pass, shared-resource use cases** (§2.2; review correction 5).
- **No placeholder subsystems** — Stage 0 contains only mechanisms exercised by its own tests and the slice application (review correction 5).
- No multi-API render abstraction; one backend, contained.
- No multiplayer transport or canonical-medium provider (Architecture E stays a hypothesis; E5's real-provider probe remains gated on account-owner prerequisites).
- No editor, no scripting runtime, no cooked asset formats, no audio in the slice.
- No gameplay policy changes; no settled policy reopened; no developer-paid recurring infrastructure required or assumed.

---

*v0.1 prepared 2026-10-19 for the architecture-consolidation review; v0.2 applies the review outcome the same date. The foundation selections were made by the lead's ratification and are recorded in [`DECISIONS.md`](DECISIONS.md) (T-01–T-10); nothing here ratifies a gameplay requirement or upgrades Architecture E / the tiered-hybrid simulation architecture from research hypothesis to chosen design. Stages 0 and 1 were authorized and completed 2026-10-19; every later stage requires its own approval.*
