# TECH ARCHITECTURE — Engine Consolidation Proposal

> **Status:** Engine technical architecture proposal v0.1 · 2026-10-19 · **UNAPPROVED PROPOSAL for three-way review** (architecture-consolidation gate, lead assignment 2026-10-18/19). **No production technology is selected by this document; no implementation is authorized.** Architecture E (canonical-medium networking) and the tiered-hybrid simulation architecture are consumed here as **research hypotheses, never ratified designs**. No ratified gameplay requirement is changed; no settled gameplay policy is reopened; no developer-paid recurring infrastructure is required or assumed; the Twin Summits (VISION §1) stand at full strength — the vertical-slice targets below are **architectural-feedback instruments, not performance claims**.
>
> **Ownership:** This document owns the **engine technical architecture proposal**: initial logical subsystems and boundaries, state ownership and inter-subsystem contracts, the technology decision map (shortlists and blocking analysis — never selections), and the first vertical-slice plan. It **defers to**: [`VISION.md`](VISION.md) (Twin Summits, launch commitments, collaboration charter), [`DESIGN-BIBLE.md`](DESIGN-BIBLE.md) and [`MULTIPLAYER-SPEC.md`](MULTIPLAYER-SPEC.md) (first-game policy — never redefined here), [`CUSTOM-ENGINE-RESEARCH.md`](CUSTOM-ENGINE-RESEARCH.md) (research methodology and evidence standards), [`MULTIPLAYER-ARCHITECTURE-RESEARCH.md`](MULTIPLAYER-ARCHITECTURE-RESEARCH.md) and [`E5-CANONICAL-MEDIUM-FEASIBILITY.md`](E5-CANONICAL-MEDIUM-FEASIBILITY.md) (persistence/authority hypotheses), [`SIMULATION-ARCHITECTURE-RESEARCH.md`](SIMULATION-ARCHITECTURE-RESEARCH.md) (simulation hypotheses; experiment records E-07–E-10), and [`DECISIONS.md`](DECISIONS.md) (the registry — nothing here is a decision until the lead ratifies it).
>
> **Method rule (anti-abstraction):** every subsystem boundary below is anchored either to (a) a contract already exercised in miniature by experiments E-07–E-10 with an independent oracle and deterministic fingerprints, or (b) platform reality (a windowed real-time application requires platform, render, and input). Boundaries with neither anchor are explicitly deferred (§8). No plugin registries, no speculative frameworks, no layer whose only job is indirection.

## 0. Executive summary

The simulation research phase (E-07–E-10, plus the E5 local harness on the multiplayer line) validated the load-bearing correctness contracts of the tiered-hybrid architecture at 1,000-NPC scale: LOD economics with capture-before-discard, four-lane narrative gating, true-dormancy reconstruction with a measured replay/synthesis crossover, and queue behavior under bursts and sustained overload with near-field reservation and overdue-clock honesty. This proposal consolidates that evidence into **twelve subsystems in six layers**, distinguishes reusable engine mechanics from first-game policy (policy is data and configuration, never compiled into the engine), and defines the **time contract** the lead directed: one canonical campaign clock, per-region/per-class processed-through watermarks, and causal correctness for events whose intended timestamps precede their actual execution. The technology decision map recommends shortlists — **Rust** (language, alternative C++), **Vulkan** as a single initial backend (alternative D3D12-first), **SDL3** for window/input (alternatives raw platform layer / winit) — and identifies exactly **three blocking decisions** (language+build, rendering backend, window/input) before the first executable; physics and asset formats do not block. The vertical slice ("Slice 0: Spine & Block") demonstrates all seven required capabilities in seven stages with per-stage acceptance criteria, and is scoped for rapid architectural feedback without reducing either summit. Nothing here is selected; §7 lists the decisions the next three-way review must make.

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
- **P5 — Engine/game separation by policy-as-data.** Budget numbers, LOD thresholds, near/far classification, milestone and gating definitions are configuration and data owned by the game; the engine provides mechanisms (E-05, E-08).
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

**Owns:** ordered subsystem startup/shutdown (dependency-registered, not hardcoded), window creation and OS message pump integration, wall-clock and monotonic OS time sources, file-system paths, process exit codes.
**Contracts:** `Platform::init(subsystem_registry) → Shell`; frame-pump callbacks; `WallClock` (never used as campaign time — P3).
**State:** none authoritative (lifecycle metadata only).
**Reusable vs first-game:** fully reusable; no game policy.
**Anchor:** platform reality. Deliberately thin — OS specifics live here and nowhere else.

### 2.2 Rendering and graphics-device abstraction — *Render Layer* (L3)

**Owns:** the single chosen graphics backend (decision map §5), swapchain and frame pacing, render-graph recording and submission, material parameter blocks, debug overlays.
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

**Owns:** the **canonical campaign clock** (single, monotonic, advances only in the spine tick loop — M-27); near/far event priority queues ordered `(due, seq)`; per-tick budget with **near-field reservation**; per-region and per-class **processed-through watermarks**; backlog/overdue/lateness accounting; the execution log; follow-up scheduling at **execution-relative** due ticks.
**Contracts:** `schedule(due, order_key, class, region) → seq`; `tick()` processes near up to reserve, far up to budget−reserve, leftovers near-first (E-10-validated policy candidate); `watermark(region, class)`; `overdue(region)`; execution records `(seq, intended_due, executed_tick, class, region, cause)`. The full time contract is §3.2.
**State:** the queues and watermarks; **not** entity state.
**Reusable vs first-game:** all mechanics reusable; budget/reserve numbers, near/far classification criteria, and cluster desynchronization strategy are game configuration (E-10's synchrony finding: budgets are sized to **cluster peaks**, not average rates).
**Anchor:** E-10 (all queue behavior); E-08 (cost tracks events).

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

### 3.2 The time contract (lead directive: canonical clock vs processed-through watermarks)

1. **Canonical campaign clock.** One monotonic clock per campaign (M-27), advanced only by the spine tick loop. No other component may mutate or branch it. Wall-clock/OS time never enters simulation logic.
2. **Processed-through watermarks.** Each region carries `watermark(region, class)` — the maximum **intended-due** tick of events actually executed for that region and class. The campaign clock says "what time the world is at"; the watermark says "how far this region's consequences have actually been processed." They legitimately differ under backlog.
3. **Delayed events and causal correctness.** Every execution records `(seq, intended_due, executed_tick)`; **lateness is first-class data**. Within a class, execution order is `(due, seq)` — monotone and deterministic (E-10). **Cross-class ordering is NOT guaranteed** (a near event may legitimately execute before an older far event); any consumer needing global ordering goes through the durable lane or the Gate Keeper, never by assuming clock order.
4. **Events whose intended timestamps precede their actual execution** (overdue events): they execute late with lateness recorded; follow-ups they spawn are scheduled at **execution-relative** due ticks (`executed_tick + delay`), so overdue causes shift their effects honestly rather than stacking impossible deadlines; and **state reads are watermark-labeled** — a reader sees `as_of(watermark)` and an overdue indicator (the E-10 stale-and-pre-write flag generalized to an engine-wide query contract). Authoritative state never silently appears more up-to-date than it actually is.
5. **Clock advancement during skips/low-LOD epochs** follows the E-08/E-10 results: cost proportional to events; gated lanes hold and revalidate; routine lanes process within budget; the clock itself never branches.

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

**Evaluation criteria** (per the lead): Windows first, Linux second; modern graphical fidelity (Summit A); exceptional computational efficiency (Summit B); maintainability; developer ergonomics; long-term extensibility. Responsibilities (§2) were defined before libraries were compared. **Nothing below is a selection.**

| Decision | Alternatives | Trade-off summary | Recommendation (shortlist) | Blocks first executable? |
|---|---|---|---|---|
| **Implementation language** | **Rust** · C++ · C# · Zig | Rust: no GC + predictable memory (Summit B), fearless concurrency, cargo toolchain, tier-1 Windows+Linux, compiler-enforced correctness is a force multiplier for solo+agent development (prior SokomindRust experience); cons — slower gameplay iteration (mitigated by a later scripting decision), some middleware needs FFI. C++: maximal engine-ecosystem maturity; cons — memory-safety burden and build complexity for a solo+agent team. C#: fastest iteration; cons — GC and runtime control conflict with Summit B. Zig: promising; cons — pre-1.0 churn. | **Rust (primary candidate), C++ (fallback)** | **YES — blocks everything** |
| **Build system** | Cargo (Rust) · CMake/Bazel (C++) | Coupled to language. Cargo folds build+deps+test+CI into one low-risk toolchain. | Coupled: **Cargo if Rust; CMake if C++** | **YES (coupled to language)** |
| **Rendering backend** | **Vulkan** · Direct3D 12 · both via thin RHI | Vulkan: one code path for both decided platforms, validation layers, RenderDoc/NSight support, RT path for Summit A; cons — verbose ramp. D3D12: optimal Windows-first tooling (PIX); cons — a second backend owed for Linux. A dual-backend RHI now is a speculative abstraction (violates P6) — one backend first, contained. | **Vulkan as the single initial backend (primary candidate); D3D12 as a later optional Windows path behind a contained device interface** | **YES** |
| **Window/input integration** | **SDL3** · raw Win32+X11/Wayland · winit · GLFW | SDL3: mature, permissive, window+input+audio in one, both platforms, C API with maintained Rust bindings — buys cross-platform surface cheaply so the engine owns the abstractions that matter. Raw: maximal control; cons — duplicated Linux work. winit: Rust-native; cons — API churn history. GLFW: minimal; cons — narrow surface. | **SDL3 (primary candidate); raw platform layer (fallback)** | **YES (coupled to renderer via surface creation)** |
| **Physics integration** | Jolt · Rapier · Bullet/PhysX · custom-minimal | Boundary (§2.8) is thin and fixed-step regardless. Jolt: MIT, modern, AAA-proven, deterministic-friendly (C++, bindings). Rapier: Rust-native, well-maintained. Custom: only for deterministic gameplay-critical contacts. | **Jolt / Rapier shortlist; slice uses built-in minimal collision** | **NO** — slice needs spheres/boxes only |
| **Asset formats / pipeline** | glTF 2.0 · FBX/interchange · custom cooked | glTF: open interchange, CC0 availability, tooling; KTX2 for textures; cooked engine formats later with content-addressed cache (CER §7). | **glTF 2.0 + KTX2 interchange for the slice; cooked formats deferred** | **NO** — early slice stages use procedural primitives |

**Deferred sub-decisions (none block the slice; each has a stated trigger):** shader language (GLSL→SPIR-V first for zero-dependency start; Slang candidate when material complexity grows); ECS framework representation (plain data pools first; revisit when batch needs outgrow pools); scripting layer (none until first-game gameplay code grows — then decide, with agent-authorability per E-05 in mind); audio backend; cooked-format schema; multiplayer transport and medium provider (P0 mechanisms; E5 probe gated on account-owner prerequisites).

**Which decisions truly block the first executable:** (1) language + build, (2) rendering backend, (3) window/input integration. Everything else has a minimal path in the slice.

**Environment note (honest constraint):** development happens on a GPU-less Linux pod (software Vulkan via lavapipe is available — the Godot kit proved the pattern) plus the lead's Windows reference rig (Ryzen 7 9800X3D / RTX 5070 Ti). The slice's CI runs headless + software-rendered smoke on the pod; performance acceptance runs on the reference rig.

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
| **0 — Workspace** | Post-decision skeleton (engine core, slice app, test crates), CI on Linux (pod, headless + lavapipe smoke) and Windows, diagnostics scaffold, harness-pattern regression rig | Build+test green on both platforms; one fingerprint test passes in CI | Requires the three blocking decisions; pod toolchain availability |
| **1 — App shell** | Window, vsync frame loop, input (WASD + mouse), clean shutdown | 60-s soak: no leaks, clean shutdown ×100; frame P99 reported (target: vsync-stable on reference rig; pod runs uncapped) | Window/input library ramp |
| **2 — Render** | Depth-correct primitives/one glTF asset; camera look/move; GPU timestamps; debug overlay | Correct present on rig + lavapipe smoke; 6-DoF camera; overlay shows frame ms + P95/P99 | Backend verbosity (mitigation: validation layers, RenderDoc) |
| **3 — Clock & spine in-app** | Campaign clock at fixed tick rate decoupled from render; 1,000 NPC schedules as data; budget + near reservation; backlog/lateness/watermark overlay; headless fingerprint mode | Deterministic fingerprints in headless CI; artificially lowered budget shows reported backlog (never silent); 200/tick synchrony-cluster spike drains without drops (E-10 behavior, in-app) | Tick/render decoupling correctness |
| **4 — Regions & LOD** | 8 regions; presence-driven LOD with hysteresis; render proxies only for non-dormant regions; activation/deactivation transitions | Dormant regions ≈ zero per-tick cost (verified by metrics); activation of a 125-NPC region < 50 ms (E-09 predicts ~ms); deactivation captures deltas (log-verified) | None new — ports E-07/E-09 contracts |
| **5 — Systems & persistence** | Full/simplified closed-form workloads (S1-lite definitions as game data); NPCs visibly moving on schedule paths; player "shove" input → encounter event with witnesses → durable memory records; pipeline → local file-backed store; quit → relaunch restores committed state | Restart round-trip: restored fingerprint == pre-exit committed fingerprint; double-commit retry is a no-op; pending/confirmed visible in overlay; encounter rollback restores pre-encounter resources | Serialization care; no networked medium in slice |
| **6 — Diagnostics & soak report** | 15-min automated soak; cluster-burst test; frame-time distribution; final slice report to the lead | Report contains: frame avg/P95/P99, sim tick ms, events/sec, memory per NPC per LOD, activation/deactivation latency, persistence commit latency, zero drops in burst test | — |

**Measurements (all recorded, none claimed as Summit evidence):** frame-time distribution (avg/P95/P99, 1%-low), GPU frame time via timestamps, sim tick cost, events/sec, backlog/lateness distributions, memory per NPC per LOD, activation/deactivation latency, persistence round-trip latency.

**Risks:** backend ramp cost (single backend, validation layers); cross-platform FP determinism (S5 remains unrun — slice fingerprints are **per-platform**; no cross-platform equality claimed); scope creep (fixed capability list; no combat, no multiplayer, no streaming beyond region residency in the slice); engine/game boundary drift (all policy numbers in config, checked in review); asset licensing (CC0 only); GPU-less pod (lavapipe for CI smoke; performance on the reference rig).

## 7. Decisions requiring the next three-way review

1. **Implementation language + build system** (recommendation: Rust + Cargo; alternative C++ + CMake) — *the* blocking decision.
2. **Rendering backend** (recommendation: Vulkan single-backend start; alternative D3D12-first with a later Linux backend) — blocking.
3. **Window/input integration** (recommendation: SDL3; alternatives raw platform layer / winit / GLFW) — blocking.
4. **Vertical-slice scope and stage plan** (§6) — confirm the seven capabilities, the stage gates, and the policy that slice targets are instrumentation milestones, not Summit claims.
5. **Defer-list confirmation** (§5/§8): physics library, shader language, ECS representation, scripting, audio, cooked formats, transport/medium provider — each deferred with a stated trigger, none blocking.

## 8. Explicitly not decided / deferred (anti-abstraction ledger)

- No language, build system, renderer, window/input library, physics library, asset format beyond interchange, scripting, audio, or networking provider is selected by this document.
- No ECS framework; entity storage starts as plain data pools behind the World Store contract.
- No multi-API render abstraction; one backend, contained.
- No multiplayer transport or canonical-medium provider (Architecture E stays a hypothesis; E5's real-provider probe remains gated on account-owner prerequisites).
- No editor, no scripting runtime, no cooked asset formats, no audio in the slice.
- No gameplay policy changes; no settled policy reopened; no developer-paid recurring infrastructure required or assumed.

---

*Proposal prepared 2026-10-19 for the architecture-consolidation review. Nothing in this document selects a technology, authorizes implementation, ratifies a gameplay requirement, or upgrades Architecture E / the tiered-hybrid simulation architecture from research hypothesis to chosen design. On review, ratified decisions are to be recorded in [`DECISIONS.md`](DECISIONS.md) and the owning documents updated per the §7 protocol.*
