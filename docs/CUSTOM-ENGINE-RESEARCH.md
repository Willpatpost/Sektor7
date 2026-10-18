---

# CUSTOM ENGINE — ARCHITECTURE & TECHNOLOGY RESEARCH FRAMEWORK

> **Status:** Research framework v0.4 · 2026-10-18 · Consolidated after Stage 0 audit report and subsequent game/multiplayer decisions; the 2026-10-18 correction aligns §6's host-handoff wording with M-31 (planned handoff required; only crash-time seamless migration is not). No implementation authorized.
>
> **Ownership:** This file defines **engine research methodology, domain boundaries, staged experiments and evidence standards**. [`VISION.md`](VISION.md) owns the Twin Summits and original-engine mandate; [`DESIGN-BIBLE.md`](DESIGN-BIBLE.md) owns gameplay; [`MULTIPLAYER-SPEC.md`](MULTIPLAYER-SPEC.md) owns multiplayer policy; [`PROTOTYPE-INVENTORY.md`](PROTOTYPE-INVENTORY.md) is reconstructed evidence; [`DECISIONS.md`](DECISIONS.md) is the canonical decision registry.
>
> **Authority:** Human project lead. **Original reusable engine: DECIDED.** No rendering backend, graphics API, language, simulation, networking stack, editor framework, scripting or asset-pipeline technology has been selected. Evidence key: **documented elsewhere**, **observed in project (with artifacts)**, **reported by an agent**, **hypothesis**, and **open**. Do not conflate them.

## 0. Mandate and anti-misinterpretation rules

The lead's objectives, unchanged by the engine decision:

1. **Graphics equal to or better than Cyberpunk 2077** — a benchmark for *technical* graphical fidelity, not an aesthetic template — in an interactive, nearly photorealistic, large open-world RPG.
2. **Computational efficiency equal to or better than the extraordinary example of RollerCoaster Tycoon**, relative to the complexity being performed, as an ambition **across hardware**. No specific GPU, CPU, handheld, or PC dictates design; known hardware serves benchmarking and measurement. Steam Deck is not a mandatory design target.
3. **A living game world** (VISION §2): authored districts, interiors, procedural exploration regions, hundreds of persistent NPCs, dynamic time/weather, encounters, futuristic traversal, RPG freedom — with **co-op for 1–4 simultaneous players functional at launch** as a foundational engine capability.
4. **An original, reusable game creation engine** — a project objective in its own right, not merely a means to ship this game. The cyberpunk RPG is its first major game; the platform must be designed to support many different games in the future. We own its distinguishing architecture, rendering systems, simulation technologies, editor, asset pipeline, and creation workflows.
5. **Strategic external technology.** Building an original engine does not require reinventing every underlying component. We may use, modify where permitted, or replace established tools, libraries, standards, and dependencies when doing so is beneficial; we own the architecture and the technologies that differentiate our engine.
6. **Measurement over dictation.** We will benchmark on known hardware, measure performance objectively, and compare improvements under controlled conditions. Measurements inform engineering; particular hardware specifications do not dictate the limits of the vision.

**This is an engineering research program, not a feasibility lecture.** We identify difficult problems and investigate solutions. A failed benchmark invalidates a measured approach under stated conditions; it does not silently lower either summit. Equally, dreams do not license fabricated performance claims: do not assume a custom engine is fast merely because it is custom, that an advertised feature of any library or reference engine automatically satisfies our target, or that a technique proven elsewhere is optimal for our workloads.

**Decision rule:** Compare **architectures, algorithms, components, and technologies** for *our* engine on evidence of cost, quality, capability, ownership, and fit with the Twin Summits. Existing engines — Godot, Unreal, Unity, and others — are **reference implementations, benchmarks, and sources of architectural lessons**: studied seriously and fairly, no longer competitors for selection, and studying them does not reopen the selection decision.

**The lead has not authorized implementation.** Research plans, documentation, and eventual time-boxed experiments must be aligned before major coding begins.

## 1. What we are building

### 1.1 Decided scope of ownership

We own and design the engine's distinguishing architecture, rendering systems, simulation technologies, editor, asset pipeline, and creation workflows. The engine is a **reusable creation platform**: it must serve the cyberpunk RPG as its first major game while remaining general enough to support many different games in the future. It is a general creation platform, **not a cyberpunk-specific runtime**. Design sequencing (decided 2026-10-16): gameplay mechanics and technical requirements are established before story, lore, naming, and content details; the architecture-affecting questions — camera, combat, traversal, world simulation, multiplayer, procedural generation, persistence — take priority.

### 1.2 What "original engine" means — and does not mean

An original engine is a modular system that may use Vulkan/Direct3D/Metal backends, open standards, third-party libraries, and external authoring tools where they serve us. Original ownership **does not require** inventing every file format, graphics API, math routine, or editor widget. Every external dependency is judged on:

- Legal compatibility with a commercial Steam release and possible standalone-engine reuse.
- Reliability, performance, and modification rights.
- Ongoing maintainability and maintenance risk.
- Whether the component is **differentiating** (we should own it) or **commodity** (reuse is fine).

### 1.3 Module map — research scaffold, not committed design

```text
Our games / content
        |
Editor + asset import/build + test/profiling tools
        |
World / scene / streaming / serialization
        |
Simulation + gameplay + events + AI + navigation
        |
Render graph + visibility + materials + lighting + animation
        |
Platform + graphics backend + jobs/memory + input/audio/network
```

These layers can be replaced, merged, or separated. This is a discussion scaffold, not an assertion that this decomposition is optimal.

### 1.4 Development strategy — DECIDED (2026-10-16): research-guided vertical engine

1. **Research the architectural foundations first** — every subsystem's responsibilities, boundaries, dependencies, and interfaces are defined **before** implementation selection.
2. **Develop a small but coherent original engine** — a genuinely-ours vertical runtime, not a fragment collection.
3. **Integrate experimental technologies** into that coherent core as research validates them.
4. **Expand it into the reusable creation platform** the vision requires.

Priorities throughout: **architectural coherence, modularity, profiling, and maintainability.** Standalone research harnesses (simulation core, render-loop experiments) remain legitimate Stage 1 spikes, and temporary host harnesses — including existing engines used strictly as test beds — may serve individual experiments. Do not force the engine to reproduce the entire city slice immediately; progress is judged against its own documented milestones as well as the common game workloads. Implementation itself is not yet authorized.

### 1.5 Reference implementations

| Reference | What we study it for |
|---|---|
| **Godot 4.x** | MIT-licensed full source; low-level server APIs; scene/editor model; our existing prototype's evidence. |
| **Unreal Engine 5.x** | Nanite/Lumen-class rendering; World Partition large-world streaming; MassEntity data-oriented crowds; AAA character and content pipeline. |
| **Unity 6 family** | HDRP; Entities/DOTS data-oriented stack; C# tooling productivity; simulation/render separation experience. |
| **Other engines & research** | Additional source-available or well-documented engines, plus academic and industry techniques. Open and expandable. |

Rules for lessons: neither assume a reference engine's approach is necessary for our goals, nor dismiss it because "we are building our own." Extract mechanisms, measured costs, and failure modes — not brand loyalty or reflexive rejection.

## 2. Technology research domains — ALL OPEN; do not select yet

**Decided discipline (2026-10-16):** define every subsystem's responsibilities, boundaries, dependencies, and interfaces **before** selecting implementations. Do not prematurely commit to programming languages, graphics APIs, physics libraries, ECS implementations, rendering architectures, or other technologies; evaluate alternatives through research and experimentation.

| # | Domain | Decisions eventually required | Initial research questions |
|---|---|---|---|
| 1 | **Rendering backend & graphics APIs** | Vulkan / Direct3D / Metal / abstraction layer | Portability cost vs control; explicit synchronization; debug tooling; editor and multi-window needs |
| 2 | **Graphics architecture** | Render graph; light transport (GI, shadows, reflections); virtualized geometry/textures; transparency; upscaling and temporal techniques | Path to CP2077-class *interactive* quality at unusually low cost per frame; which innovations are ours to invent |
| 3 | **Character & animation rendering** | Skin/hair/eyes; cloth; crowds; animation runtimes | Fidelity in real motion; DCC pipeline; crowd scale |
| 4 | **Simulation framework** | Data-oriented layouts; ECS variants; event-driven and hierarchical fidelity; fixed-step and determinism; scheduling | RCT-class throughput; hundreds of persistent agents; replication-friendliness for co-op |
| 5 | **Networking** | Authority; relayed/P2P transport; listen-server; player identity; voting; replication; prediction; interest management; save ownership | 1–4 launch players without developer-funded servers; democratic world-bound campaigns, rotating eligible hosts, snapshot consistency and divergent-history detection |
| 6 | **Editor & tooling** | Editor architecture; programmatic and GUI interfaces; tool scripting; profiling/replay/validation | Investigate usable internal world/material/asset tooling for game one with agent-accessible automation; public engine distribution/polish is a separate OPEN milestone |
| 7 | **Scripting** | Language(s); hot reload; performance; safety | Gameplay and tool authoring; scripted-vs-native boundary; agent authoring |
| 8 | **Asset pipeline** | Formats; importers; DCC integration; content-addressed caching; validation | Provenance and licensing; determinism; streaming-friendly builds |
| 9 | **World infrastructure** | Streaming; large-world representation; origin precision; interiors; save/load | Fast vertical traversal; persistent-world continuity; co-op state |
| 10 | **Platform & services** | OS rollout — Windows first, Linux second (decided 2026-10-16), additional platforms later; packaging; distribution (Steam); diagnostics; updates | Cross-platform architectural boundaries from the start; which additional platforms and when (Deck optional) |
| 11 | **Runtime foundation** | Memory allocation; job system; threading; asynchronous I/O | Control, instrumentability, worst-case behavior |

Each domain accumulates evidence in `TECHNOLOGY-RESEARCH.md` (the external components/libraries matrix with dated references and explicit unknowns) as research proceeds.

## 3. Research principles that apply to every domain

1. **Own the measurement methodology.** Separate CPU simulation, draw submission, GPU rendering, streaming, synchronization, and background jobs.
2. **Compare at equivalent output quality.** A higher FPS from visibly weaker materials/lighting/characters is not proof of a more efficient implementation of the desired experience.
3. **Also compare at equivalent workload.** Report how much logical world computation is actually performed; don't simulate 1,000 full agents in one approach and 24 scripted props in another.
4. **Beware of visual sleight of hand.** Temporal reconstruction, camera tricks, low-frequency systems, fake crowds, and proxy models may be useful, but document their tradeoffs and whether they preserve the intended player experience.
5. **Treat novelty as a hypothesis.** An original algorithm wins through evidence, not by being original.
6. **Keep failures and counterexamples.** Benchmarks must be reproducible and falsifiable, not promotional material.
7. **Don't equate determinism with efficiency.** Fixed-step and replay may help testing and networking; network authority and synchronization remain separate design decisions.
8. **Make quality comparable across time of day, weather, altitude, interiors, and motion.** One wet neon street at night cannot establish overall fidelity.
9. **Value tooling as part of the engine.** Asset import, iteration, character animation, debugging, and automation are necessary parts of the finished system.
10. **Avoid silent scope changes.** Report unsolved items with next experiments; the human lead decides whether any vision-level requirement changes.

## 4. Research program: staged, fair, ambitious

### Stage 0 — Align and inventory (reported completed; no production code changes)

**Status reported by GLM 5.3:** The authorized read-only `godot/` audit was completed: **4,273 lines** across **16 `.gd` files** (3,901 source + 372 test), **60/60 headless checks passing**, clean import, no project addons, and a clean `godot/` working tree. The claim and methods are summarized in [`PROTOTYPE-INVENTORY.md`](PROTOTYPE-INVENTORY.md). The audit was executed directly in this repository; its original record was subsequently recovered from the pre-sync stash and reconciled with a first-hand verification record ([`PROTOTYPE-INVENTORY.md`](PROTOTYPE-INVENTORY.md) §7) — every reported figure matched. The prototype supplies calibration evidence, not architectural authority.

**Completed or reported completed:** Stage 0 prototype inspection and the initial vision/strategy alignment. The earlier estimates of ~3.3k lines and ~60 checks have been superseded by the reported audit counts. No code, asset or save modifications were authorized or reported.

**Still needed before implementation:** Confirm candidate experiment workloads and lawful comparison assets; record platform/API/library capability evidence; define subsystem boundaries; design objective success metrics; review the unresolved multiplayer persistence questions; obtain explicit lead approval for each research spike. Stage 0 completion is not permission to begin coding.

**Output references:** [`PROTOTYPE-INVENTORY.md`](PROTOTYPE-INVENTORY.md), [`DECISIONS.md`](DECISIONS.md), and future experiment charter / technology matrices.

### Stage 1 — Research spikes (bounded, meaningful, per domain)

1. **Stand-alone data-oriented city simulator:** exact state model; 100/500/1,000 agent workloads; trace and replay; quantify memory/time.
2. **Minimal instrumented render loop:** measure draw submission, geometry throughput, shading, and resource streaming under explicit control.
3. **One high-value visual technique:** e.g., dynamic lighting/reflection or geometry visibility that targets observed cost at equal fidelity.
4. **Asset/toolchain proof:** ingest complex assets, animation, materials, and character data with automated validation.
5. **Integration spike:** compare a custom subsystem inside a host harness against its stand-alone version; isolate the hosting tax.

Only proceed with substantial coding after the lead approves the relevant spike and its measurement plan.

### Stage 2 — Experiment A: "One Corner of the City" (visual reference)

A reproducible target for the engine's rendering research:

- Approximately **40 m × 40 m** city intersection.
- Wet asphalt/puddles; one hero building with lit windows and neon; secondary facades, cables, lamps, and detailed street props.
- Weather: rain, fog, a lightning flash. Full **day and night** captures rather than night-only beauty shots.
- **24 scheduled NPCs** and **2 hovercars** (driven by documented comparable logic or clearly labeled proxies).
- Representative character closeups: hair/skin/clothing, animation, reflections, transparent surfaces, and one glimpse of an interior.
- Identical scripted camera positions and paths plus free-motion gameplay, including representative fast movement.
- Same assets where legally and technically practical; document changes where pipelines differ.

**Two complementary modes:**

1. **Visual-quality mode:** pursue the best actual interactive image on the reference rig, with full settings disclosure and comparable reference captures.
2. **Efficiency-at-quality mode:** match image appearance and content as closely as practical at the same output resolution, then measure rendering cost. Record any remaining quality gap, not just FPS.

**Calibration role of reference engines:** implementations of the same scene in a reference engine (e.g., Godot, Unreal) may serve as calibration benchmarks for image quality and cost. Their results are recorded as measurements of *approaches*, not as competing bids.

**Do not confuse internal and output resolution.** Record native/render resolution, upscaler, render scale, target display resolution, dynamic-resolution behavior, anti-aliasing, ray tracing, and frame generation independently. Frame generation must not be counted as equivalent simulation/render throughput.

Reference Cyberpunk 2077 using documented source scene, camera framing, settings, lighting, weather, capture hardware, version, and video as well as stills. It is a **quality comparator**, not an asset to copy.

### Stage 3 — Experiment B: persistent simulation stress test

An engine-neutral, published workload specification for logical populations of **100, 500, and 1,000** agents (benchmark scales, **not** revised game-feature commitments). Include:

- Schedules, goals, travel, events, state changes, and interactions.
- Multiple districts, near-player and far-away agents, and persistent offscreen progression.
- Work generated by NPCs **and** other systems: traffic, event processing, weather impacts, inventories/economy proxies as appropriate.
- Measurable behavioral fidelity: a faster method is not equivalent if it silently removes required state transitions or breaks world continuity.
- Adjustable simulation update architectures (per-frame, fixed-step, event-driven, hierarchical, batched), with reproducible seeds where useful.
- Save/load and consistent state reconstruction; optional replay/divergence tests if determinism is being investigated.

Collect **CPU time per simulation step**, events/state transitions per second, memory per agent, scaling curves, allocated bytes, cache-related behavior where instrumented, worst-case spikes, background progress accuracy, and isolation of rendering versus simulation. A custom simulation core can run this experiment headlessly; no full editor is needed to demonstrate a genuine simulation advantage.

### Stage 4 — Experiment C: large-world, streaming, and vertical traversal

Construct a representative **multi-block** environment with high-detail exteriors, rooftops, vertical flight, a traffic/crowd corridor, at least one explorable interior, and a transition boundary for a generated region. Exact physical dimensions are experiment parameters, not world-size commitments.

- Stress rapid hovercar/jetpack travel and rapid turning.
- Load/unload geometry, textures, animations, NPC representations, and interior state.
- Test district continuity, origin/world precision, navigation, persistence, and event consistency.
- Record stutter, latency, IO pressure, memory residency, pop-in, and visual quality at speed.
- Compare streaming approaches and custom algorithms, not only default settings.

### Stage 5 — Experiment D: the innovation challenge

Targeted experiments attacking hard barriers found in A–C: geometry representation, dynamic GI, reflection reuse, GPU-driven crowds, event-scheduled city simulation, streaming/asset residency, or engine overhead. For every innovation candidate record:

1. **Bottleneck:** measured, reproduced symptom and hardware/settings.
2. **Hypothesis:** why a different algorithm or architecture could improve it.
3. **Baseline:** existing implementation and same workload/quality target.
4. **New technique:** design, dependencies, source references, expected failure modes.
5. **Measurement:** before/after cost, visual/state equivalence, variance, and development effort.
6. **Result:** succeeded, failed under current conditions, or inconclusive.
7. **Next idea:** refinement, alternate technique, or research pivot.

This is where an original engine can establish genuinely differentiated advantage — and where a borrowed approach may prove equally good. Evidence decides.

## 5. Hardware and benchmark methodology

### Decided stance (2026-10-16)

No specific GPU, CPU, handheld, or PC is a design constraint. Benchmark on available hardware — including the Ryzen 7 9800X3D / RTX 5070 Ti reference machine — and expand testing when useful. Create reproducible benchmarks for rendering efficiency, simulation throughput, memory consumption, frame pacing, streaming, and other relevant workloads. Measurements are tools for engineering progress, not reasons to lower the Twin Summits. Steam Deck is optional compatibility research, not a mandatory target.

| Tier | Present understanding | Decision status |
|---|---|---|
| **High-end reference (measurement anchor)** | Ryzen 7 9800X3D; RTX 5070 Ti; 32 GB DDR5-6400 | **Known measurement system** — anchor, not a design constraint |
| **Mid-class measurement machines** | Typical contemporary mid-range gaming PCs; exact parts/settings to be agreed as anchors | **Specification OPEN** (measurement anchors, not design limits) |
| **Steam Deck** | Useful optional compatibility research target | **Not a mandatory design target** |
| **Other systems** | Additional hardware to validate scaling and portability | **OPEN** |

### Required instrumentation

- CPU: simulation time, main/render thread, task scheduler, frame spikes, allocations, cache/branch counters where accessible.
- GPU: actual GPU time, render passes, draw/dispatch activity, shader and resource stalls, quality settings.
- Frame pacing: average, median, **P95 and P99 frame times** (and 1%-low FPS if useful), not average FPS alone.
- Memory/storage: resident RAM, peak/average VRAM, fragmentation where measurable, IO rate, shader compilation, load/streaming stalls.
- Visual quality: side-by-side stills, frame sequences, motion review, character/material closeups, interior/day/night/weather examples.
- Simulation: agents, events/state transitions, work fidelity, per-agent memory, responsiveness, persistent-state correctness.
- Productivity: person-hours/agent-hours, edit-build-test loop time, debugging effort, asset handling, automation, build size, dependency problems.
- Reliability: crashes, save/load correctness, reproducibility, platform builds, test coverage.

Test **optimized standalone builds** under the same capture protocol. Record engine/runtime/driver versions and exact settings; repeat runs and report variability. Don't claim that headless simulation performance represents complete game performance, or that a beauty shot predicts large-world frame pacing.

### Apples-to-apples fairness

- Reuse identical input source assets when legal and feasible; otherwise document differences in visual complexity.
- Use the same camera motion, world event sequence, agent workload, target output resolution, and target perceptual quality.
- Record any existing prototype head start as a **development advantage**, not as a falsified cold-start comparison. Report incremental and from-scratch-equivalent effort separately where possible.
- Allow original-engine research to show capabilities incrementally; do not compare a week-one renderer's feature completeness to a mature AAA engine and declare the route worthless.
- Conversely, don't ignore total engineering cost because a microbenchmark is impressive. Evaluate near-term and long-term implications.
- Reference-engine implementations of the same workload are calibration points; their results describe approaches, not competing bids.

## 6. Multiplayer as a foundational capability

**DECIDED (VISION §10):** 1–4 simultaneous players at launch, separate solo and multiplayer worlds, full-roster authored-world access, any-subset frontier access, democratic campaign policy and world-bound characters. The engine treats multiplayer, identity, consistent save/restore and session authority as foundational **capabilities** without hardcoding these particular RPG rules into every future game. Formal player-facing policy is in [`MULTIPLAYER-SPEC.md`](MULTIPLAYER-SPEC.md).

**Committed constraint (2026-10-16):** launch multiplayer must not depend on developer-funded dedicated-server infrastructure or recurring hosting costs. Any registered participant hosting a later session with the newest valid campaign save is a **WORKING PREFERENCE** requiring save provenance, integrity, authorization and conflict research; a **planned host handoff** for an intentional transfer of authority is required (M-31, 2026-10-17) — only **seamless migration after an unexpected host crash** is not a launch requirement. Unanimous remaining-member removal and voluntary retirement are player-facing rules, not host privileges. **Working preference:** a player-hosted listen-server model. **Open research (domain 5):** Steam Networking Sockets, P2P connectivity, relay services, the authoritative-simulation split, shared persistence, host migration, replication, prediction, interest management, and divergence detection/replay. The requirement is the multiplayer experience, not a predetermined implementation — and the engine must remain flexible enough to support different networking architectures for future games. Determinism remains a **WORKING HYPOTHESIS** (VISION §10), not a mandate; do not silently turn an event-driven simulator, deterministic core, ECS, or network architecture into a requirement.

## 7. Near-term deliverables

1. `TECHNOLOGY-RESEARCH.md` — external components/libraries/APIs matrix with dated references and explicit unknowns (succeeds the previously planned `CANDIDATE-RESEARCH.md`; "candidates" are now technologies, not engines).
2. `PROVING-GROUND-SPEC.md` — scene, simulation workload, streaming benchmark, camera/capture scripts, assets and settings.
3. `PERFORMANCE-BENCHMARKS.md` — exact machine specs and measurement protocol.
4. [`DECISIONS.md`](DECISIONS.md) — **existing** single decision registry and prioritized open decisions; update it when the lead ratifies changes.
5. `ENGINE-ROADMAP.md` (per VISION §11) — module build order, dependencies, editor/tooling needs, integration contracts, once research justifies sequencing.

These can be consolidated if duplication becomes unhelpful.

## 8. Decision governance and agent handoff

### Human-led decision process

1. **Confirm the vision.** Check that no document turns a summit into a negotiable aspiration or assumes mandatory Steam Deck support.
2. **Agree on research methodology.** Confirm domains, staging, hardware anchors, and experiment scope before implementing major slices.
3. **Study and inventory existing assets/code.** The prototype is evidence; the code is not architecture policy.
4. **Prioritize small experiments by information gained.**
5. **Record measured results and disagreements.** Tie every claimed advantage to a test or specific technical evidence.
6. **Choose subsystem technologies only after review with the lead**; record re-evaluation triggers and reversible boundaries.
7. **Proceed to implementation when expressly authorized.**

### Required agent behavior

- **Never** answer "this is unrealistic, therefore reduce the game" as a default engineering recommendation. Bring solutions, alternatives, and research designs.
- **Never** quietly change "equal to or better" into "inspired by" when recording either summit.
- **Never** treat the original-engine decision as provisional, or reopen engine selection, without the lead's explicit direction. Equally, **never** treat studying reference engines as disloyalty or as reopening selection.
- **Never** declare a rendering backend, graphics API, simulation framework, networking model, editor technology, scripting language, or asset pipeline chosen without the lead's agreement.
- **Never** silently turn a proposed event-driven simulator, deterministic core, ECS, renderer API, or network architecture into a requirement.
- **Always** distinguish product vision, working hypothesis, experiment, observed result, and final decision.
- **Always** document real bottlenecks and propose mechanisms that could remove or sidestep them before suggesting experience compromises.
- **Always** explain what would falsify the hypothesis and what to investigate if it fails.
- **Always** allow explicit, constructive disagreements among AI collaborators; don't rewrite history to produce artificial consensus. The human lead adjudicates.
- **Always** ask about genuinely identity-shaping or costly decisions when they are about to be fixed, but avoid unnecessary blocking questions before research can proceed.

## 9. Lineage — what this document used to be

| Prior framework claim | Disposition after the 2026-10-16 decision |
|---|---|
| v0.1: "Godot 4 and UE5 are the only finalists." | Retired — no selection competition exists. |
| v0.1→v0.2: "Custom engine parked until a host becomes a bottleneck." | Reversed in v0.2; superseded by the decision — the original engine **is** the direction. |
| v0.2: "Simulation core ≈ 80% of a custom engine." | Withdrawn in v0.2; remains withdrawn. Renderer, tools, pipeline, and editor remain substantial independent efforts. |
| v0.2: "Engine-independent deterministic core is already our architecture." | Reclassified WORKING HYPOTHESIS; unchanged. |
| v0.2: weighted scorecard to select among engine candidates. | Retired with the competition. The scoring discipline survives only as evidence rigor within research domains. |
| v0.2: staged experiments comparing candidate engines. | Retained, reframed: experiments validate **our engine's** approaches, with reference-engine implementations as calibration benchmarks. |

## 10. Official starting references (verified 2026-10-16)

Research starting points — not substitutes for reading the exact license, versioned documentation, technical source, or measured implementation before committing:

- **Godot licensing and modification rights:** [Godot official license](https://godotengine.org/license/) — MIT, including the right to study and modify the engine. (Reference-implementation study.)
- **Unreal licensing:** [Unreal Engine licensing overview](https://www.unrealengine.com/license) — current royalty/source-access overview. (Reference-implementation study.)
- **Unreal large worlds:** [World Partition documentation](https://dev.epicgames.com/documentation/unreal-engine/world-partition-in-unreal-engine) — streaming system as documented.
- **Unreal data-oriented simulation:** [MassEntity overview](https://dev.epicgames.com/documentation/unreal-engine/overview-of-mass-entity-in-unreal-engine) — relevant to crowd/simulation architecture lessons.
- **Unity historical correction:** [Unity's Runtime Fee cancellation](https://unity.com/products/pricing-updates) — the proposed fee was canceled effective 2024-09-12; current terms must still be evaluated when Unity lessons become relevant.
- **Fab cross-engine licensing:** [Fab Standard License](https://www.fab.com/eula?lang=en) — Standard License assets are not generally restricted to Unreal; check each product's license type, format, and terms.

Store new technical research, additional sources, version numbers, and dated benchmark references in `TECHNOLOGY-RESEARCH.md` rather than treating this list as exhaustive.

---

**Working conclusion:** The engine direction is decided — an original, reusable game creation engine, built strategically on chosen external components, with the cyberpunk RPG as its first major game. This framework governs how its architecture and subsystem technologies are researched, measured, and eventually selected. Nothing subsystem-level is chosen yet. The next step is to resolve architecture-shaping game questions and research hypotheses, obtain the lead's approval of specific time-boxed research spikes, and only then begin implementation.
