# ENGINE & TECHNOLOGY FOUNDATION EVALUATION

> **Status:** Research-and-decision framework, revised draft v0.2 — 2026-10-16  
> **Authority:** Human project lead; no engine, renderer, simulation architecture, or implementation language has been selected.  
> **Companion:** Read [`VISION.md`](VISION.md), especially §§0–1 (engineering philosophy and Twin Summits), §8 (technology ownership), and §12 (agent collaboration). The vision takes precedence over any convenience recommendation in this evaluation.  
> **Purpose:** Determine which **technology strategy**—including a fully original game engine—offers the strongest route to our combined objectives. We are not selecting the easiest way to reduce the game's ambitions.  
> **Evidence key:** **Known capability** = established in relevant documentation but not yet validated in our project; **Observed** = measured in our own experiment; **Hypothesis** = proposed mechanism or expected advantage; **Open** = not yet investigated or decided.

---

## 0. Mandate and anti-misinterpretation rules

The human lead's objectives are unambiguous:

1. **Graphics equal to or better than Cyberpunk 2077**, in an interactive, nearly photorealistic, large open-world RPG.
2. **Computational efficiency equal to or better than the extraordinary example of RollerCoaster Tycoon**, relative to the complexity being performed, on mid-tier gaming hardware, with additional benefits on a high-end PC.
3. **A living game world**: authored city districts, interiors, procedural exploration regions, hundreds of persistent NPCs, dynamic time/weather, encounters, futuristic traversal, RPG freedom, and a possible co-op experience.
4. **Freedom to invent the technology** rather than accept current engines as an absolute boundary.
5. **A serious, positive interest in creating our own game/creation engine**. This is not a joke option and must not be relegated to a “reopen only if everything else fails” footnote.

**This is an engineering research program, not a feasibility lecture.** We will identify difficult problems and investigate solutions. A failed benchmark invalidates a measured approach under stated conditions; it does not silently lower either summit. Equally, dreams do not license fabricated performance claims. Do not assume a custom engine is fast merely because it is custom, or that an existing renderer's advertised features automatically satisfy the target.

**Decision rule:** Compare **routes to the summits**, including the opportunity to build new technology, rather than simply tallying currently available features. A platform may have an excellent starting point yet poor modification flexibility, or little built-in functionality but an outstanding long-term foundation.

**The lead has not authorized production.** Research plans, documentation, and eventual time-boxed experiments should be aligned before major implementation begins.

## 1. What “engine” means here

“Engine choice” is not just a binary choice between Godot and Unreal. It can determine or constrain:

- Rendering architecture: APIs, geometry processing, lighting, shadows, reflections, materials, temporal reconstruction, weather, animation, GPU execution, and visibility.
- Runtime architecture: entity storage, scheduling, threading, allocation, input, physics, navigation, and data flow.
- Simulation boundaries: which subsystem owns state, how world updates advance, persistence, repeatability, and event processing.
- Large-world infrastructure: spatial hierarchy, origin/precision strategies, district and interior streaming, navigation, asset residency, procedural-region lifecycle.
- Game and content tooling: editor, DCC integration, importers, animation tools, asset build pipeline, material authoring, validation, debugging, and profiling.
- Game services: save/load, networking, platform abstraction, packaging, deployment, Steam integration, updates, and diagnostics.
- Ownership economics: source accessibility, modification rights, licensing, royalties/subscriptions, dependency lock-in, and maintainability.
- Research flexibility: how quickly an AI-assisted solo developer can test new designs, replace subsystems, or build entirely novel techniques.

**Corrective note to the previous draft:** An engine-independent simulation is a promising *hypothesis*, not something already decided or built. Such a core could reduce lock-in, but renderer, physics, authoring, serialization, streaming, and networking may still create meaningful integration costs. Similarly, building a simulation core is **not** “80% of a custom engine”; the original estimate was unsupported. A modern renderer, asset pipeline, tooling, platform layer, and editor can be substantial independent efforts.

## 2. Candidate strategies — all genuinely considered

| Strategy | Why we investigate | Research question |
|---|---|---|
| **A. Godot 4.x** | Existing prototype; MIT-licensed source; GDExtension/C++; lower-level rendering/physics server interfaces; light iteration footprint. | Can built-in, extended, or significantly modified Godot achieve our graphical and simulation summits? |
| **B. Unreal Engine 5.x** | High-end rendering ecosystem (Nanite, Lumen, advanced character/content workflows), C++ source access under license, World Partition, large-world and network facilities, MassEntity-related technology. | Can we retain the visual/tooling advantages while eliminating or bypassing runtime costs that conflict with our efficiency ambition? |
| **C. Unity 6 family** | HDRP, C# tooling, data-oriented Entities/DOTS family, established publishing and content ecosystem. | Might its combined rendering, ECS, iteration, and extension characteristics create a superior middle or hybrid option? |
| **D. Our own engine** | Deep ownership of architecture, render pipeline, memory layout, scheduling, tooling, and optimization strategy. **Personally exciting to the project lead.** | Could purpose-built technology outperform general-purpose engines on the *specific* workloads and image-quality needs of this game? What should we reuse rather than reinvent? |
| **E. Hybrid / deeply extended engine** | An existing editor and asset pipeline coupled with an independent simulation runtime, specialized renderer, modified engine fork, or custom GPU subsystems. | Can we get the best of ownership and existing production capability without prohibitive integration costs? |

These are **strategies**, not mutually exclusive brands. “Our own engine” may use Vulkan/Direct3D/Metal backends, libraries, open standards, third-party importers, or existing tools; original ownership does **not** require rewriting every dependency. A hybrid might eventually evolve into a standalone engine. Each strategy should be compared fairly.

### Candidate A — Godot

**Potential strengths:** permissive licensing and source control, adaptable architecture, scene/editor productivity, scriptable workflows, low-level server APIs, compiled extensions, useful existing prototype and tests.

**Research questions:** high-end dynamic lighting and reflections; film-quality character pipeline; high-density open-world streaming; CPU/render-thread costs of large scenes; ability to replace renderer components or maintain an engine fork; GDExtension ABI and deployment; automated asset workflows; co-op architecture.

**Neither assume** “Godot cannot achieve photorealism” **nor** “source access makes every modification easy.” Test both.

### Candidate B — Unreal

**Potential strengths:** sophisticated real-time rendering and virtualized geometry, large-world tooling, mature authoring ecosystem, character workflows, C++ integration, potential use of MassEntity and existing streaming/network infrastructure.

**Research questions:** actual CPU/GPU costs at *matched perceptual quality*, minimum engine overhead, expensive feature interactions, how far subsystems can be bypassed, source modification burden, tool/build iteration, asset-license constraints, target-platform behavior, and independence of custom simulation logic.

**Neither assume** “Nanite/Lumen guarantee performance” **nor** “Unreal is inevitably too heavy.” Test both.

### Candidate C — Unity

**Potential strengths:** HDRP, data-oriented ECS, C# workflows, established tool ecosystem, potential efficient separation of logical simulation and rendering.

**Research questions:** contemporary photoreal ceiling in motion, high-density simulation at scale, large-world tooling, renderer extension rights, ecosystem/license requirements, and solo-plus-agent productivity.

**Correction from prior draft:** Unity should not be excluded solely on the basis of the historical 2023 runtime-fee proposal; the fee was subsequently canceled. We still need to verify **current** Unity license terms and source/modification conditions for the exact version and intended use, rather than relying on old assumptions.

### Candidate D — Original engine: a primary research path

**This is not “parked.”** We will explicitly investigate creating our own game engine and, if warranted, pursue it as a major creative and technological objective. The lead is enthusiastic about it. Building an original engine could be an end-to-end project of its own, and is a legitimate potential foundation for the game.

**Potential strengths to test:** tailored data layout, exact control over frame pipeline and synchronization, removal of irrelevant general-purpose overhead, a purpose-built lighting/geometry/visibility stack, intimate control over streaming and simulation, direct introspection, custom editor workflow, reuse as a future engine platform.

**Costs and challenges to quantify, not wield as automatic vetoes:** renderer and shader development, GPU portability, character animation, tools, asset import, editor, authoring pipeline, debugging, documentation, platform services, networking, and maintenance.

**High-value distinction:** We can build a **modular custom engine using existing components** (math, windows/input, asset decoding, rendering APIs, audio, physics libraries as appropriate) and invest original engineering precisely where it provides unique value. Evaluate legal compatibility, reliability, modification rights, performance, and ongoing maintainability of each chosen dependency.

**Initial research should explore at least three original-engine variants:**

1. **Lean 3D engine:** own scene/runtime/render orchestration with established cross-platform libraries and APIs.
2. **Research-first renderer + simulation:** build core differentiating technology with standalone test harnesses before building the rest of the engine.
3. **Incrementally emancipated hybrid:** start with a mature editor/host, replace bottleneck subsystems, and split into a standalone engine only where justified.

Do **not** force the custom option to reproduce the entire city slice immediately merely to earn consideration. Give it a fair staged research plan and compare its progress against its own documented milestones as well as the common game workload.

### Candidate E — Hybrid or engine fork

Potential shapes include:

- Godot editor/assets + custom native simulation + heavily modified renderer.
- Unreal production pipeline + specialized native simulation, traffic/crowd, streaming, and selective custom rendering.
- Custom engine consuming interoperable asset formats and external authoring tools.
- Distinct offline asset-build pipeline shared across candidate runtimes.

A hybrid can gain leverage or accumulate painful coupling. Measure the boundaries and build/test/integration costs, not just theoretical portability.

## 3. Technical research principles that apply to every option

1. **Own the measurement methodology.** Separate CPU simulation, draw submission, GPU rendering, streaming, synchronization, and background jobs.
2. **Compare at equivalent output quality.** A higher FPS from visibly weaker materials/lighting/characters is not proof of a more efficient implementation of the desired experience.
3. **Also compare at equivalent workload.** Report how much logical world computation is actually performed; don't simulate 1,000 full agents in one candidate and 24 scripted props in another.
4. **Beware of visual sleight of hand.** Temporal reconstruction, camera tricks, low-frequency systems, fake crowds, and proxy models may be useful techniques, but document their tradeoffs and whether they preserve the intended player experience.
5. **Treat novelty as a hypothesis.** An original algorithm wins through evidence, not by being original.
6. **Keep failures and counterexamples.** Benchmarks should be reproducible and falsifiable, not promotional material.
7. **Don't equate determinism with efficiency.** Fixed-step and replay may help testing and networking; network authority and synchronization remain separate design decisions.
8. **Make quality comparable across time of day, weather, altitude, interiors, and motion.** One wet neon street at night cannot establish overall fidelity.
9. **Value tooling as part of the engine.** Asset import, iteration, character animation, debugging, and automation are necessary parts of the finished system—even if we build them ourselves.
10. **Avoid silent scope changes.** Report unsolved items with next experiments; the human lead decides whether any vision-level requirement changes.

## 4. Evaluation dimensions and provisional weights

The scorecard is only one decision aid. It **cannot** prove a vision is impossible or automatically select a winner. There are two equal-priority summits, plus a distinct emphasis on architectural freedom and innovation.

| ID | Dimension / criterion | Proposed weight | Evidence to collect |
|---|---|---:|---|
| **VG-1** | Final interactive visual fidelity | **5** | Matched quality captures, expert review, temporal stability |
| VG-2 | Advanced rendering capability and room to innovate | 4 | Lighting, geometry, materials, characters, custom GPU work |
| VG-3 | Quality-preserving efficiency techniques | 3 | GPU cost at matched visual quality, frame stability |
| VG-4 | Character, animation, and photoreal content fidelity | 3 | Hero character and crowd quality in real movement |
| **EF-1** | Simulation efficiency and scaling | **5** | Events/sec, agents/tick, memory/agent, behavior fidelity |
| EF-2 | CPU/GPU frame cost and overhead | 4 | Empty/loaded baseline, frame-time decomposition |
| EF-3 | Data, memory, streaming, and scheduling control | 4 | Allocations, RAM/VRAM, stalls, locality, throughput |
| EF-4 | Repeatable performance instrumentation | 2 | Profiler access, automated reproducibility |
| **IN-1** | Ownership, extensibility, source/renderer control | **5** | Modify/replace measured hot spots, licensing |
| IN-2 | Ability to build novel algorithms and subsystems | 4 | Research prototype integration and isolation |
| IN-3 | Reusability and long-term technology independence | 4 | Portability, data formats, coupling, fork burden |
| **PR-1** | Solo + AI-agent engineering productivity | 4 | End-to-end effort, build/edit/debug cycle, automation |
| PR-2 | High-quality asset/content pipeline | 3 | DCC import, materials, animations, license provenance |
| PR-3 | Licensing, operations, and release economics | 3 | Actual obligations, deployment, distribution |
| **WS-1** | Large-world and vertical streaming | 4 | Fast traversal, interiors, origin precision, stutter |
| WS-2 | Persistent-world systems and save/load | 3 | State continuity, persistence, background simulation |
| WS-3 | Co-op integration path | 3 | Authority boundaries, replicability, server/client costs |
| | **Total weight** | **63** | Maximum weighted result **315** (if 1–5 scoring) |

**Weights are a proposed discussion starting point, not approved policy.** Visual and efficiency groups each have weight **15**, reflecting the Twin Summits as peers. Innovation/ownership has weight **13**; production/tooling and world/co-op have weights **10** each. Review with the human lead before adoption.

For every scored criterion, attach **evidence confidence** (`observed`, `documented only`, `hypothesis`, or `unknown`) and links to the experiment or primary documentation. **Do not assign an attractive numerical score to an untested custom engine merely because its theoretical ceiling is high.** Unknown remains unknown.

### Qualitative status alongside scorecard

Each candidate also gets:

- **Best current evidence** — what has actually been demonstrated.
- **Summit potential hypothesis** — why it might ultimately excel.
- **Known blockers** — with reproducible description, not rhetorical absolutes.
- **Innovation paths** — credible research directions to attack blockers.
- **Integration/production cost** — estimated and later measured, with uncertainty.
- **What a successful next experiment looks like** — concrete, testable criteria.
- **What could change our minds** — re-evaluation triggers.

No single weighted total overrides this qualitative record or the lead's decision.

## 5. Evaluation program: staged, fair, ambitious

The previous draft proposed a single 40 m × 40 m street and declared the custom engine parked. We retain that visual benchmark, but **expand the research program** to measure the game we actually aspire to build. A short visual scene by itself cannot settle simulation, streaming, or technology sovereignty.

### Stage 0 — Align and inventory (documentation, not new production)

Before implementation:

- Read the current vision and confirm all constraints and open questions with the lead.
- Inventory the existing `godot/` prototype: source, features, systems, tests, assets, licenses, and actual benchmark evidence. The earlier document described about 3.3k lines of code, milestone 2, and approximately 60 headless checks; **verify against the repository** rather than accepting historical estimates as current fact.
- Map every candidate's source rights, renderer/control boundaries, plugin/extension interfaces, target platform, and existing debug/profiling tools.
- Establish an asset and camera reference pack that can be used lawfully and fairly among candidate engines.
- Decide which experiments are needed **before** anyone assumes their outcome. Obtain explicit go-ahead before substantial coding.

**Output:** experiment charter, workload specification, candidate capability matrix, current evidence inventory, outstanding decisions.

### Stage 1 — Research spikes, including the original-engine path

Give each candidate **bounded, meaningful initial experiments** rather than requiring identical implementation cost prematurely.

- **Godot:** measure rendering overhead, advanced material/lighting extensions, native sim integration, and engine-fork accessibility.
- **Unreal:** measure packaged runtime overhead, high-quality rendering at real gameplay frame rates, MassEntity/custom sim integration, modification boundaries, and iteration cost.
- **Unity:** review HDRP/Entities and implementation reality; advance to a full spike if initial evidence justifies it.
- **Custom engine:** create a research design for modular runtime + high-end renderer + efficient simulation; enumerate reusable libraries/APIs; prototype one differentiating technique or minimal benchmark renderer **when authorized**. Measure the real cost of getting pixels, simulation data, assets, and instrumentation through the pipeline.
- **Hybrid:** prototype a well-defined custom subsystem in an existing host, recording all integration overhead.

**Output:** technical feasibility *hypotheses*, measured early costs, unsolved research questions, invention opportunities. **Do not** remove a candidate merely because its first prototype is immature.

### Stage 2 — Experiment A: “One Corner of the City” (visual reference)

Preserve the original scene specification as a reproducible target:

- Approximately **40 m × 40 m** city intersection.
- Wet asphalt/puddles; one hero building with lit windows and neon; secondary facades, cables, lamps, and detailed street props.
- Weather: rain, fog, a lightning flash. Full **day and night** captures rather than night-only beauty shots.
- **24 scheduled NPCs** and **2 hovercars** (they must be driven by documented comparable logic or clearly labeled proxies).
- Representative character closeups, hair/skin/clothing, animation, reflections, transparent surfaces, and one glimpse of an interior.
- Identical scripted camera positions and paths plus free-motion gameplay, including representative fast movement.
- Same assets where legally and technically practical; document changes where pipelines differ.

**Two complementary modes:**

1. **Visual-quality mode:** pursue the best actual interactive image each implementation can produce on the reference rig, with full settings disclosure and comparable reference captures.
2. **Efficiency-at-quality mode:** match image *appearance and content* as closely as practical at the same output resolution, then measure rendering cost. Record any remaining quality gap, not just FPS.

**Do not confuse internal and output resolution.** Record native/render resolution, upscaler, render scale, target display resolution, dynamic-resolution behavior, anti-aliasing, ray tracing, and frame generation independently. Frame generation must not be counted as equivalent simulation/render throughput.

Reference Cyberpunk 2077 using documented source scene, camera framing, settings, lighting, weather, capture hardware, version, and video as well as stills. It is a **quality comparator**, not an asset to copy.

### Stage 3 — Experiment B: persistent simulation stress test

Use an engine-neutral, published workload specification for logical populations of **100, 500, and 1,000** agents (these are benchmark scales, **not** revised game-feature commitments). Include:

- Schedules, goals, travel, events, state changes, and interactions.
- Multiple districts, near-player and far-away agents, and persistent offscreen progression.
- Work generated by NPCs **and** other systems: traffic, event processing, weather impacts, inventories/economy proxies as appropriate.
- Measurable behavioral fidelity: a faster method is not equivalent if it silently removes required state transitions or breaks world continuity.
- Adjustable simulation update architectures (per-frame, fixed-step, event-driven, hierarchical, batched, etc.), with reproducible seeds where useful.
- Save/load and consistent state reconstruction; optional replay/divergence tests if determinism is being investigated.

Collect **CPU time per simulation step**, events/state transitions per second, memory per agent, scaling curves, allocated bytes, cache-related behavior where instrumented, worst-case spikes, background progress accuracy, and isolation of rendering versus simulation.

A custom engine or standalone runtime can run this experiment headlessly; no full editor is needed to demonstrate a genuine simulation advantage.

### Stage 4 — Experiment C: large-world, streaming, and vertical traversal

Construct a representative **multi-block** environment with high-detail exteriors, rooftops, vertical flight, a traffic/crowd corridor, at least one explorable interior, and a transition boundary for a generated region. The exact physical dimensions are an experiment parameter, not a prematurely fixed world-size commitment.

- Stress rapid hovercar/jetpack travel and rapid turning.
- Load/unload geometry, textures, animations, NPC representations, and interior state.
- Test district continuity, origin/world precision, navigation, persistence, and event consistency.
- Record stutter, latency, IO pressure, memory residency, pop-in, and visual quality at speed.
- Compare streaming approaches and custom algorithms, not only default engine settings.

### Stage 5 — Experiment D: the innovation challenge

Each strong strategy should receive a targeted experiment that attacks one hard barrier found in A–C: for example, geometry representation, dynamic GI, reflection reuse, GPU-driven crowds, event-scheduled city simulation, streaming/asset residency, or engine overhead.

For every innovation candidate record:

1. **Bottleneck:** measured, reproduced symptom and hardware/settings.
2. **Hypothesis:** why a different algorithm or architecture could improve it.
3. **Baseline:** existing implementation and same workload/quality target.
4. **New technique:** design, dependencies, source references, expected failure modes.
5. **Measurement:** before/after cost, visual/state equivalence, variance, and development effort.
6. **Result:** succeeded, failed under current conditions, or inconclusive.
7. **Next idea:** refinement, alternate technique, or research pivot.

This is where a custom or hybrid engine might establish a genuinely differentiated advantage—and where an existing engine may prove equally adaptable.

## 6. Hardware and benchmark methodology

### Hardware roles

| Tier | Present understanding | Decision status |
|---|---|---|
| **Mid-tier desktop (required ambition)** | Typical contemporary mid-range gaming PC; reference parts/settings to be agreed | **Specification OPEN** |
| **High-end reference** | Ryzen 7 9800X3D; RTX 5070 Ti; 32 GB DDR5-6400 | **Known target system** |
| **Steam Deck** | Useful optional compatibility research target | **Requirement OPEN**; *not* a synonym for mid-tier PC |
| **Other systems** | Additional hardware to validate scaling and platform portability | **OPEN** |

Do not invent a fixed minimum spec, 30/60/120 FPS promise, or mandatory Deck support and present it as a prior decision. Propose measurable provisional goals for the lead to approve, preserving the long-term graphical and efficiency ambition.

### Required instrumentation

- CPU: simulation time, main/render thread, task scheduler, frame spikes, allocations, cache/branch counters where accessible.
- GPU: actual GPU time, render passes, draw/dispatch activity, shader and resource stalls, quality settings.
- Frame pacing: average, median, **P95 and P99 frame times** (and 1%-low FPS if useful), not average FPS alone.
- Memory/storage: resident RAM, peak/average VRAM, fragmentation where measurable, IO rate, shader compilation, load/streaming stalls.
- Visual quality: side-by-side stills, frame sequences, motion review, character/material closeups, interior/day/night/weather examples.
- Simulation: agents, events/state transitions, work fidelity, per-agent memory, responsiveness, persistent-state correctness.
- Productivity: person-hours/agent-hours, edit-build-test loop time, debugging effort, asset handling, automation, build size, dependency problems.
- Reliability: crashes, save/load correctness, reproducibility, platform builds, test coverage.

Test **optimized standalone/package builds** under the same capture protocol. Record engine/runtime/driver versions and exact settings; repeat runs and report variability. Don't claim that headless simulation performance represents complete game performance, or that a beauty shot predicts large-world frame pacing.

### Apples-to-apples fairness

- Reuse identical input source assets when legal and feasible; otherwise document differences in visual complexity.
- Use the same camera motion, world event sequence, agent workload, target output resolution, and target perceptual quality for comparison.
- Record the Godot prototype's head start as a **development advantage**, not as a falsified cold-start comparison. Report **incremental** and **from-scratch-equivalent** effort separately where possible.
- Allow original-engine research to show its special capabilities incrementally; do not compare a week-one custom renderer's feature completeness to a mature AAA engine and declare the research route worthless.
- Conversely, don't ignore total engineering cost because a custom microbenchmark is impressive. Evaluate both near-term and long-term implications.

## 7. Original-engine research agenda (serious, explicit)

This section exists because creating our own engine is of real interest to the lead. It is a **research agenda**, not a premature order to start implementation.

### 7.1 Questions to answer before selecting an architecture

- Which modules should we own completely? Renderer? World simulation? Asset processing? Scene description? Editor? Physics? Navigation? Networking?
- Which libraries, graphics APIs, file formats, compiler tools, and platform abstractions offer leverage without compromising the objective?
- What source licensing and redistribution terms allow our eventual Steam product and possible standalone engine reuse?
- What rendering strategy has the best path toward CP2077-class interactive quality **and** unusually low cost per frame?
- How can we represent a persistent city densely enough to preserve continuity without needless full-detail updates?
- What is the minimum **editor and asset workflow** that enables efficient AI-assisted content production? A runtime without usable authoring tools is not yet a production platform.
- How will we profile, reproduce bugs, replay simulation, validate GPU correctness, and compare builds?
- What platform(s) are first-class, and which renderer abstraction is appropriate for them?

### 7.2 Possible module map — for research, not committed design

```text
Our game / content
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

These layers can be replaced, merged, or separated. They are a discussion scaffold, not an assertion that one specific modular decomposition is optimal.

### 7.3 First experiments worth proposing

1. **Stand-alone data-oriented city simulator:** exact state model, 100/500/1,000 agent workloads, trace and replay; quantify memory/time.
2. **Minimal instrumented render loop:** measure draw submission, geometry throughput, shading and resource streaming under explicit control.
3. **One high-value visual technique:** e.g., dynamic lighting/reflection or geometry visibility that targets observed cost at equal fidelity.
4. **Asset/toolchain proof:** ingest complex assets, animation, materials, and character data into the render loop with automated validation.
5. **Integration spike:** compare a custom subsystem inside Godot or Unreal to its stand-alone version; isolate the hosting tax.

Only proceed with substantial coding after the lead approves the relevant research spike and a plan for measuring it.

### 7.4 What would make an original engine compelling?

Evidence that owning the full or partial runtime produces a meaningful combination of: superior cost at matched visual/simulation quality; room for breakthrough techniques; reproducible scaling; acceptable authoring workflow; maintainable architecture; and a credible route to the full intended experience. It need not win every microbenchmark, and it should not be chosen merely because it is emotionally exciting. **The excitement is a valid project objective; the architectural case must still be made with technical evidence.**

## 8. Current risks and research responses

| Area | Uncertainty / risk | Research response — not a scope cut |
|---|---|---|
| Godot | Reaching top-tier visual/character fidelity with acceptable frame cost | Test native approach, custom shaders/renderer work, integrations, forks, asset tools |
| Unreal | CPU/GPU overhead, large-build iteration, engine subsystem coupling | Measure packaged overhead, bypass options, custom native systems, rendering-quality/performance curves |
| Unity | Final image quality, ECS/renderer integration, source/license constraints | Focused HDRP/Entities spike and up-to-date legal/tooling review |
| Original engine | Renderer, editor, animation, asset/tooling infrastructure, portability | Modular road map, reused components, early asset/toolchain tests, measured development velocity |
| Hybrid | Complex interfaces or hidden runtime overhead | Standalone vs hosted comparison, API contracts, automated tests |
| All | Photoreal character animation and assets | Research lawful asset sources, pipelines, rigging and animation standards, procedural/commission options |
| All | Large persistent world performance | Hierarchical/event-driven/data-oriented simulation hypotheses, stress testing, fidelity checks |
| All | Co-op complexity | Network ownership/authority research; independent latency/state correctness tests |
| All | Misleading benchmark comparisons | Fixed workloads, matched quality, complete capture/settings logs |
| All | Excessive new technology without measured benefit | Instrument first; record explicit hypotheses and useful increments |

**Licensing research requirement:** Recheck contemporary Godot, Epic/Unreal, Fab asset, Unity, middleware, and AI-generated/marketplace asset terms before decisions and again prior to shipping. Fab items may carry different licenses or technical constraints; do not generalize that all Fab photoreal assets are Unreal-only. Unreal financial thresholds and distribution obligations, Unity subscriptions, or special licenses must be checked against the actual current contracts, not memorized old summaries.

## 9. Evidence-backed scorecard template

Use **blank or unknown** rather than arbitrary scores. Record the proposed weighting only after the human lead approves it.

| Criterion | Weight | Godot | Unreal | Unity | Original engine | Hybrid |
|---|---:|---|---|---|---|---|
| VG-1 Interactive visual fidelity | 5 | ? | ? | ? | ? | ? |
| VG-2 Rendering capability + innovation | 4 | ? | ? | ? | ? | ? |
| VG-3 Quality-preserving rendering efficiency | 3 | ? | ? | ? | ? | ? |
| VG-4 Characters + animation | 3 | ? | ? | ? | ? | ? |
| EF-1 Simulation throughput | 5 | ? | ? | ? | ? | ? |
| EF-2 Runtime/frame overhead | 4 | ? | ? | ? | ? | ? |
| EF-3 Memory/streaming/scheduling control | 4 | ? | ? | ? | ? | ? |
| EF-4 Instrumentation/repeatability | 2 | ? | ? | ? | ? | ? |
| IN-1 Ownership/extensibility | 5 | ? | ? | ? | ? | ? |
| IN-2 Novel subsystem integration | 4 | ? | ? | ? | ? | ? |
| IN-3 Technology independence | 4 | ? | ? | ? | ? | ? |
| PR-1 Solo + agent velocity | 4 | ? | ? | ? | ? | ? |
| PR-2 Asset/production pipeline | 3 | ? | ? | ? | ? | ? |
| PR-3 Licensing/release | 3 | ? | ? | ? | ? | ? |
| WS-1 Large-world/flight/streaming | 4 | ? | ? | ? | ? | ? |
| WS-2 Persistent world/save-load | 3 | ? | ? | ? | ? | ? |
| WS-3 Co-op path | 3 | ? | ? | ? | ? | ? |
| **Total (max 315)** | **63** | **unscored** | **unscored** | **unscored** | **unscored** | **unscored** |

**Scoring definition (only when evidence supports it):** 1 = measured serious mismatch under tested conditions; 2 = weak fit with substantial unresolved work; 3 = adequate/credible supported fit; 4 = strong demonstrated fit; 5 = exceptional demonstrated advantage. Do not score candidate *potential* as if it were demonstrated performance. Record separately when the potential is compelling but not yet tested.

**Do not decide by arithmetic alone.** Scores summarize evidence and expose tradeoffs. Technical inventiveness, ability to evolve, strategic value of our own engine, uncertainty, and the human lead's ambitions remain explicit decision dimensions.

## 10. Decision governance and agent handoff

### Human-led decision process

1. **Confirm the vision.** Check that no document turns a summit into a negotiable aspiration, assumes mandatory Steam Deck support, or locks an engine by implication.
2. **Agree on evaluation methodology.** Confirm candidates, scoring philosophy, hardware reference, and research scope before implementing major slices.
3. **Study and inventory existing assets/code.** The prototype is evidence; the code is not architecture policy.
4. **Prioritize small experiments by information gained**, including a serious original-engine research track.
5. **Record measured results and disagreements.** Tie every claimed advantage to a test or specific technical evidence.
6. **Choose a strategy only after review with the lead**; even then record re-evaluation triggers and reversible boundaries.
7. **Proceed to full architecture/design planning** and then implementation when expressly authorized.

### Required agent behavior

- **Never** answer “this is unrealistic, therefore reduce the game” as a default engineering recommendation. Bring solutions, alternatives, and research designs.
- **Never** quietly change “equal to or better” into “inspired by” when recording either summit.
- **Never** treat an original engine as prohibited or automatically selected. Respect the lead's excitement; research it with rigor and genuine curiosity.
- **Never** declare Godot, Unreal, Unity, or a custom engine chosen without the lead's agreement.
- **Never** silently turn a proposed event-driven simulator, deterministic core, ECS, engine fork, renderer API, or network architecture into a requirement.
- **Always** distinguish product vision, working hypothesis, experiment, observed result, and final decision.
- **Always** document real bottlenecks and propose mechanisms that could remove or sidestep them before suggesting experience compromises.
- **Always** explain what would falsify the hypothesis and what to investigate if it fails.
- **Always** allow explicit, constructive disagreements among AI collaborators; don't rewrite history to produce artificial consensus. The human lead adjudicates.
- **Always** ask about genuinely identity-shaping or costly decisions when they are about to be fixed, but avoid unnecessary blocking questions before research can proceed.

### Status of prior claims from draft v0.1

| Prior assertion | Revised treatment |
|---|---|
| “Godot 4 and UE5 are the only finalists.” | Both important, but Unity, original engine, and hybrids remain meaningful routes. |
| “Custom is parked until host becomes bottleneck.” | **Reversed.** Serious primary research track because the lead is excited to invent an engine. |
| “Simulation core is already ~80% of custom engine.” | **Removed.** Unsupported; runtime, tools, renderer, asset and editor workloads must be studied. |
| “Engine-independent deterministic core is already our architecture.” | **Reclassified WORKING HYPOTHESIS.** Evaluate portability, determinism, authority, and integration separately. |
| “Godot code is 80% of the scene.” | Historical prototype estimate to verify; not a reason to preselect. |
| “Most Fab scans cannot be used outside Unreal.” | **Overgeneralization.** Check per-asset licenses and pipeline constraints. |
| “Unity runtime-fee history rules it out.” | **Not sufficient.** Assess current terms and technical fit. |
| “Deck-class is the mid-tier floor.” | **Incorrect inference.** Mid-tier desktop floor; Deck remains open. |
| “One 40m graphics scene selects an engine.” | **Insufficient.** Add simulation, streaming, innovation, and tooling experiments. |
| “A scorecard or initial failed benchmark chooses the achievable dream.” | **Rejected.** Evidence selects research strategy; it does not silently renegotiate the vision. |

## 11. Near-term deliverables and questions for the lead

### Documentation to produce before implementation

1. `CANDIDATE-RESEARCH.md` — capability/source/licensing matrix with dated references and explicit unknowns.
2. `PROVING-GROUND-SPEC.md` — scene, simulation workload, streaming benchmark, camera/capture scripts, assets and settings.
3. `CUSTOM-ENGINE-RESEARCH.md` — independent engine variants, module boundaries, candidate libraries/APIs, initial high-information experiments, technical sovereignty goals.
4. `PERFORMANCE-BENCHMARKS.md` — exact machine specs and measurement protocol.
5. `DECISIONS.md` — approval and re-evaluation log; architectural hypotheses are recorded, not quietly locked.

These can be consolidated if duplication becomes unhelpful.

### Alignment questions — **do not invent the answers**

- How much does building a truly original **reusable game engine** matter **as an independent creative goal**, separate from the released RPG? Would a purpose-built renderer/runtime count, or is owning the editor/content tools also part of the dream?
- Is Steam Deck support desirable, essential, or irrelevant? **No answer has been established.**
- What exact mid-tier hardware class should anchor the efficiency summit? Specific CPU/GPU and desired output resolutions can be proposed for approval.
- What is the intended co-op experience and what must be supported at launch versus architecturally anticipated?
- Which aesthetic references and settings should constitute the Cyberpunk-quality comparison across day, night, interiors, characters, and motion?
- How much proprietary or third-party technology would the lead welcome inside something called “our own engine”?

These are **decision points for later alignment**. They should not prevent research or document improvement today.

## 12. Official starting references (verified 2026-10-16)

These links are **research starting points**, not substitutes for reading the exact license, versioned documentation, technical source, or measured implementation before committing:

- **Godot licensing and modification rights:** [Godot official license](https://godotengine.org/license/) — MIT, including the right to study and modify the engine, subject to notices and relevant third-party licensing.
- **Unreal licensing:** [Unreal Engine licensing overview](https://www.unrealengine.com/license) — current royalty/source-access overview; consult the binding agreements for the intended release.
- **Unreal large worlds:** [World Partition documentation](https://dev.epicgames.com/documentation/unreal-engine/world-partition-in-unreal-engine) — streaming system as documented, not a benchmark of our planned game.
- **Unreal data-oriented simulation:** [MassEntity overview](https://dev.epicgames.com/documentation/unreal-engine/overview-of-mass-entity-in-unreal-engine) — relevant to the crowd/simulation architecture comparison.
- **Unity historical correction:** [Unity's Runtime Fee cancellation](https://unity.com/products/pricing-updates) — the proposed fee was canceled effective 2024-09-12; other current Unity licensing terms must still be evaluated.
- **Fab cross-engine licensing:** [Fab Standard License](https://www.fab.com/eula?lang=en) — Standard License assets are not generally restricted to Unreal, but actual product license type, format, and terms must be checked.

Store new technical research, additional sources, version numbers, and dated benchmark references in `CANDIDATE-RESEARCH.md` rather than treating this list as exhaustive.

---

**Working conclusion:** No winner declared. We will compare **Godot, Unreal, Unity, an original engine, and hybrid strategies** against the Twin Summits, technological freedom, and our game vision. We will celebrate ambitious invention, measure it carefully, and refuse to confuse conventional implementation limits with the limits of our intended destination. The next step is alignment on this framework—not automatic construction of the Godot slice or a commitment to any specific engine.
