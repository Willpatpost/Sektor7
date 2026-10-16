# PROJECT VISION — Working Title: SEKTOR 7

> **Status:** Vision foundation, revised draft v0.3 — 2026-10-16  
> **Authority:** The human project lead owns the vision, priorities, and final decisions. GLM 5.3 and other AI collaborators are research, design, engineering, and implementation partners—not independent owners of the vision.  
> **Read with:** [`ENGINE-EVALUATION.md`](ENGINE-EVALUATION.md). This document establishes **what we are trying to create and how we approach unsolved problems**. The companion document establishes **how we investigate and select technology without shrinking the ambition**.  
> **Project name:** “SEKTOR 7” is an expendable placeholder. Not a locked brand, title, setting, or technical identity.  
> **Status convention:** **VISION COMMITMENT** = intended destination; **WORKING HYPOTHESIS** = a technical idea needing proof; **OPEN DECISION** = undecided; **EXISTING EXPERIMENT** = prototype evidence, not a commitment.

---

## 0. Read this first: our agreement

The project lead has explicitly rejected the common practice of reducing an ambitious design to what is straightforward with current tools, conventional staffing, or known production patterns. Do **not** repeatedly lecture the lead that AAA games are difficult, that solo development is hard, or that the vision should be scaled down. Those facts are understood; they do not answer the question we are working on.

Our job is to **find, develop, test, and improve the path** to the vision. Where available technology is inadequate, we may invent, extend, combine, or replace technology. We are free to revisit the engine, renderer, programming languages, simulation model, production process, content pipeline, and even the project name. **No existing prototype has veto power over a better design.**

This is not permission to pretend an experiment succeeded, ignore physics, or misrepresent performance. Technical honesty is indispensable: distinguish demonstrated results, plausible hypotheses, unknowns, and failed approaches. A failed experiment rejects or revises an *approach*; it does not automatically reject the *destination*. The right response to an obstacle is: **What would need to be true? What can we measure? What different approach can we try?**

**Operating principle:** *The vision determines the destination. Engineering research determines the path. Evidence improves the path; it does not silently rewrite the destination.*

This document is a statement of ambition **and** a disciplined research charter. It is not a promise that all technical problems are already solved.

## 1. The Twin Summits — simultaneous, not tradeable by default

### Summit A — Visual excellence

**VISION COMMITMENT:** Build a game with graphical fidelity **as good as Cyberpunk 2077, if not better**. This means convincing, nearly photorealistic visual presentation across the *actual interactive game*, not only a static cinematic or a carefully staged screenshot.

The bar includes, without being limited to:

- Realistic, coherent materials; fine geometry; believable scale; detailed architecture and props.
- Daylight, night lighting, emissive/neon environments, indirect illumination, shadows, reflections, transparency, and wet surfaces.
- Weather and atmospheric effects: rain, fog, storms, lightning, changing sky and environmental responses.
- Convincing characters: faces, eyes, skin, hair, clothing, movement, animation, crowds, and interactions.
- High-quality views at street level, from rooftops, during fast flight, and while transitioning between exterior and interior spaces.
- Stable image quality **in motion**: limited temporal artifacts, objectionable shimmer, popping, ghosting, or distracting discontinuities.
- A coherent artistic standard across authored, procedural, indoor, daytime, and nighttime content.

“Better than Cyberpunk 2077” is an aspiration to exceed the reference where we can, not an unsupported claim we have already achieved it. The exact reference settings, scenes, measurements, and comparison methodology remain to be specified.

### Summit B — Extraordinary computational efficiency

**VISION COMMITMENT:** Achieve computational efficiency **as impressive as RollerCoaster Tycoon (1999), if not better**, in the context of a substantially more visually and mechanically complex modern game. The spirit is Chris Sawyer's performance-conscious engineering: do impressive amounts of meaningful computation with remarkably little wasted work.

Our ambition is a dense, persistent, interactive world that runs exceptionally smoothly on **mid-tier gaming hardware** and scales further on high-end systems.

This includes efficiency across:

- Simulation throughput and memory per meaningful agent or world event.
- AI, scheduling, navigation, traffic, physics, economic and social systems, and persistence.
- Rendering CPU overhead, GPU work, data movement, frame pacing, and shader/streaming behavior.
- Asset streaming, memory allocation, save/load, network-relevant state, and background world advancement.
- Consistency during traversal, storms, crowds, combat, interiors, and heavy world events—not just an empty benchmark scene.

**Important measurement note:** A 1999 management sim and a modern 3D RPG cannot be compared with a single honest “efficiency score” merely by comparing FPS. We will develop meaningful, reproducible metrics—e.g., useful state transitions per CPU time, memory per persistent agent, world scale at target frame-time percentiles, performance per unit of visual fidelity. This clarifies the summit; it does **not** lower it.

### Neither summit is a sacrifice offered to the other

The central challenge is to accomplish **both**. We do not automatically sacrifice visual fidelity to gain performance, or accept wasteful simulation because the rendering looks spectacular. We first investigate better algorithms, representations, caching, locality, scheduling, level of *computation*, visibility, temporal reuse, parallelism, and hardware-specific execution while preserving the intended experience and observed fidelity.

Settings and hardware tiers may exist, but “turn down quality” is **not our primary research strategy**. When a goal is currently unsolved, record it as an open engineering challenge with possible paths—not as a canceled feature.

## 2. Game pitch and intended identity

**VISION COMMITMENT:** A large-scale, open-world, futuristic/cyberpunk **role-playing game** with the density and presentation of a AAA RPG, authored districts and explorable interiors, a living simulated population, meaningful player freedom, futuristic traversal, dynamic time/weather, random encounters, and a separately accessible procedurally generated frontier for combat, progression, farming, and exploration.

- **World:** At least one large authored open-world map, composed of distinctive districts; multiple interior cells/locations; vertical spaces; plus separate procedural areas.
- **Population:** Hundreds of NPCs and additional interacting systems that generate substantial computation. Citizens should have persistent state and believable ongoing lives, not exist merely as disposable visual set dressing.
- **Player freedom:** A variety of role-playing paths and identities; criminal activity is **possible**, not the central premise or mandated role. Avoid designing the whole game as a GTA-style crime campaign by default.
- **Traversal:** Hover/flying cars, jetpacks, and other futuristic technologies; movement through the city is an important expression of the setting.
- **World dynamism:** Full day/night and weather cycles; ambient, systemic, and authored events; Skyrim-style encounters and emergent surprises.
- **Progression and exploration:** Distinct procedural destinations for XP, combat/kill farming, repeatable exploration, and other activities. Exact progression systems are open.
- **Multiplayer aspiration:** Co-op shared-world play is part of the full dream. Whether it ships at launch is **OPEN**; early architecture should investigate its implications so the goal is not casually blocked.
- **Release aspiration:** A commercial Steam release around **USD $20**. This is an intended positioning, not a locked final price or financial model.
- **Originality:** Inspirations are references for qualities, not instructions to copy protected art, characters, lore, music, maps, code, or trademarks. The game should develop an original identity.

The pitch describes the intended completed experience. It does not dictate the order in which systems must be researched or implemented.

## 3. Design pillars

1. **A world with continuity.** The world retains causal state when the player is elsewhere. NPCs, schedules, resources, events, and consequences continue to advance meaningfully.
2. **Extraordinary efficiency.** Engineering excellence is a creative objective, not a post-launch cleanup job.
3. **No compromise in visual ambition.** Reach toward benchmark-level or better real-time photorealism through existing and invented methods.
4. **Role-playing freedom.** The player can choose occupations, alignments, activities, and relationships; crime is one option among many.
5. **A reactive world.** Time, weather, population, and player actions influence one another.
6. **Vertical futuristic mobility.** Vehicles, flight, rooftops, and vertical city design are integral rather than decorative.
7. **Authored craft plus procedural longevity.** Dense handcrafted districts coexist with distinct generated exploration zones.
8. **Emergent and authored activity.** Random encounters and systemic interactions make the world feel alive.
9. **Potential shared adventures.** Preserve a deliberate architectural path to co-op without pretending netcode has already been solved.
10. **Technology as a creative medium.** We may create our **own game engine, renderer, simulation framework, tools, or asset pipeline**. The lead is personally excited about building an original engine; that aspiration must receive genuine consideration, not be dismissed reflexively.
11. **Comprehensive documentation before major commitment.** Design and research first; major production investment after deliberate alignment.

## 4. Confirmed intent vs open design questions

### 4.1 Confirmed intended features (as of 2026-10-16)

- At least one large open-world map with authored districts.
- Multiple interior locations/cells.
- Hundreds of NPCs plus other computationally demanding systems.
- Persistent, meaningful offscreen world progression.
- Day/night and weather cycles, including daytime gameplay.
- Random encounters/events in the spirit of Skyrim.
- Futuristic systems, notably hover/flying cars and jetpacks.
- Broad RPG freedom, including but not limited to the option of crime.
- Distinct procedurally generated regions for exploration and XP/combat farming.
- AAA-quality ambition, Steam commercialization, and approximately $20 positioning.
- Co-op as part of the long-range vision, with architectural consequences considered early.
- A willingness to reset existing work completely and reconsider even the engine and name.

### 4.2 Open questions (do not silently decide)

- Final project name, world fiction, setting, lore, narrative structure, tone, and art direction.
- Camera perspective(s), combat mechanics, skill systems, classes, progression, loot, and whether Borderlands inspiration specifically means FPS/RPG combat, loot, co-op, tone, or some mixture.
- Number, size, and density of authored districts; number and kinds of interiors; total world scale.
- Travel/transition mechanism to procedural zones and whether they share a persistent timeline/state.
- How autonomous NPC lives, economies, factions, player reputation, law, and quest consequences work.
- Exact meaning and launch scope of co-op (party size, dedicated/listen server, shared saves, etc.).
- Target OS/platforms; Linux support and Steam Deck support are **not yet mandatory decisions**.
- Precise mid-tier reference hardware and target frame-time/fidelity thresholds; high-end reference details below.
- Engine/renderer/simulation architecture, programming languages, asset formats, licensing, outsourcing, and toolchain.
- Development schedule, release date, budget, final price, and business model.

**Do not interpret an open question as permission to pick a permanent default.** Temporary assumptions are allowed for experiments if labeled and documented.

## 5. World model: authored, procedural, interior, persistent

### Authored city

A large handcrafted open world of distinctive, dense, believable districts. Visual quality, navigation, traversal, crowd behavior, and environmental storytelling must hold up from street level through high-speed vertical traversal.

### Procedural frontier

Separately accessible procedural areas for repeatable exploration, combat/XP farming, discovery, and emergent content. These are part of the same game, not a browser-side experiment. Their generation method, transitions, persistence, and progression relationship to the city are open for design.

### Interiors

Multiple indoor locations/cells with appropriate continuity, streaming/loading decisions, and environmental response. “Cells” describes an organizational concept, not a mandated implementation.

### Persistent world simulation

World state advances even when districts are not visible. **Persistence of consequence does not imply rendering or recomputing every NPC at full near-player detail each frame.** Possible research directions include discrete-event simulation, hierarchical simulation fidelity, scheduled transitions, compact data-oriented state, deterministic replay, and background processing. These are **working hypotheses**, not locked architectural requirements. Fidelity is judged by consistent outcomes and believable behavior when the player encounters the world.

## 6. Inspirations and what they mean

| Inspiration | Desired qualities; not a requirement to imitate the entire game |
|---|---|
| **Cyberpunk 2077** | Graphical benchmark and urban visual ambition. The lead has **not personally played it**; it is **not** automatically the mechanical blueprint. |
| **RollerCoaster Tycoon (1999)** | Iconic performance-conscious programming; complex simulation relative to available hardware. |
| **The Sims / SimCity** | Persistent lives, interconnected world systems, systemic simulation. |
| **Skyrim** | Open-world freedom, player-defined roles, encounters, exploration, consequences. |
| **Starfield** | Futuristic technology and traversal inspiration, plus the relationship between authored spaces and broader exploratory content. |
| **Grand Theft Auto** | Large urban sandbox energy; crime remains optional, not the defining objective. |
| **Red Dead Redemption 1 / 2** | World detail, atmosphere, immersion, environmental craft. |
| **Borderlands (series)** | Inspiration acknowledged; the exact desired elements still need clarification. Do not assume a specific looter-shooter formula. |
| **Terraria / Minecraft** | Repeatable exploration, procedural systems, longevity, discoverability, player agency. |
| **Other games and research** | Inspiration list is open and expandable. |

## 7. Hardware, experience targets, and benchmarking principles

**Known high-end reference configuration:** AMD Ryzen 7 9800X3D, NVIDIA GeForce RTX 5070 Ti, 32 GB DDR5-6400 MT/s RAM.

**Desired minimum class:** a typical **mid-tier gaming PC**. The exact CPU, GPU, RAM, storage, resolution, and OS must be selected deliberately. **Steam Deck-class GPU is not synonymous with “mid-tier PC,” and Deck support must not be silently promoted to a launch requirement.**

**Experience intent:** extremely smooth performance on mid-tier hardware, with additional visual or computational headroom on high-end systems. The RCT analogy expresses the ambition to exceed expectations for the amount of meaningful work being performed—not a claim that frame rates on radically different games/hardware are directly comparable.

**Evaluation discipline:** compare not only FPS, but actual image quality, simulation throughput, CPU/GPU frame times, stutter, RAM/VRAM, asset streaming, and real gameplay behavior. Report exactly what was measured, on what hardware, with what settings. “Looks good” and “runs fast” are starting observations, not complete evidence.

## 8. Technology philosophy: ownership and invention

**Engine choice is OPEN.** We are considering Godot, Unreal, Unity, a wholly original engine, and hybrid approaches. The user is **genuinely enthusiastic about designing an original creation/game engine**, potentially with a custom renderer and purpose-built simulation. This must be treated as a serious aspirational option and researched on its merits. No engine is a winner, finalist by decree, or forbidden option merely because of inherited prototype work.

We may choose to:

- Use an existing engine with its standard facilities.
- Modify a source-available engine or build substantial custom renderer/simulation subsystems within it.
- Develop an engine-independent library or specialized runtime hosted by an existing engine.
- Build an original modular engine, using open standards and carefully chosen libraries where useful (custom engine **does not require** inventing every file format, graphics API, math routine, or editor widget).
- Transition between these approaches if experiments provide evidence that a different foundation is better.

**Architectural sovereignty:** favor understandable, inspectable data formats; decoupled domain logic where it improves testing or portability; versioned interfaces; automated tests; documented ownership of state; and explicit licensing constraints. The degree of engine independence, determinism, source modification, and custom technology must be **investigated**, not asserted as a solved design.

### Research hypotheses worth exploring (not mandates)

- Data-oriented layouts, cache-aware algorithms, packed state, batch processing, spatial indexing.
- Event-driven or hierarchical world simulation instead of identical-rate per-agent updates.
- GPU-driven rendering, instancing, geometry culling, temporal reuse, virtualized resources, and carefully profiled shading.
- Threading, asynchronous I/O, work scheduling, content-addressed caching, and streaming.
- Purpose-built geometry, lighting, material, crowd, navigation, or tooling systems.
- Reproducible fixed-step or otherwise testable simulation, with explicit choices about determinism and networking.

A new technology is valuable when it measurably improves our goals; novelty alone is not sufficient. Likewise, existing technology is valuable when it accelerates the summit without adding unacceptable constraints.

## 9. Current prototypes and reset policy

| Existing line | Status and meaning |
|---|---|
| `browser/` | Retired experimental line: a one-night browser-based cyberpunk prompt to GLM 5.3. It is not a product or architecture commitment. |
| `godot/` | An active experimental prototype, previously described as milestone 2. Useful evidence and reusable ideas may exist; Godot is **not** selected. |
| Existing code, tests, art, documents | Reuse selectively if they advance the new plan. Audit rather than assume compatibility. |
| Project name `SEKTOR 7` | Placeholder to revisit in an early naming pass. |

**Full reset is explicitly permitted.** Past investment is information, not an obligation. Before deciding what to preserve, inventory actual working features, dependencies, code ownership, asset provenance, test coverage, and measured performance.

## 10. Multiplayer and determinism: aspirations, not shortcuts

The goal of co-op influences data authority, world persistence, serialization, state ownership, player interactions, synchronization, and time advancement.

**Working hypothesis:** a clean simulation model, possibly deterministic in its authoritative subsystems, could ease repeatability, testing, and network replication. **Not established:** that all simulation must be deterministic, that deterministic lockstep is necessarily optimal, or that determinism alone makes multiplayer cheap. Explore server authority, prediction, interest management, snapshots, divergence detection, replay, and practical development cost as separate questions.

We must avoid locking the future game into an architecture that casually prevents co-op. We must also avoid over-engineering a network stack before evaluating the appropriate design.

## 11. Process: alignment, research, then development

We are **not** entering full production merely because a prototype exists. The current priority is achieving deep alignment and comprehensive documentation, then moving into targeted experiments and technical research. Development begins in earnest when the lead deliberately authorizes it.

Suggested planning deliverables (subject to amendment):

1. `VISION.md` — ambition, game identity, principles, intent, open questions (this file).
2. `ENGINE-EVALUATION.md` — technology candidates, tradeoffs, research and experimental protocol.
3. `DESIGN-BIBLE.md` — systems, loops, player roles, encounters, progression, world rules, co-op concept.
4. `WORLD-DESIGN.md` — districts, interiors, procedural areas, vertical traversal, living-world requirements.
5. `TECH-ARCHITECTURE.md` — simulation state, renderer, threading, tools, saves, networking, platform separation.
6. `RENDERING-RESEARCH.md` and `SIMULATION-RESEARCH.md` — hypotheses, literature, prototypes, evidence.
7. `ENGINE-ROADMAP.md` — **if** pursuing an original engine or hybrid, modules, build order, dependencies, editor/tooling needs, integration contracts.
8. `ASSETS.md` — provenance, licenses, scans, characters, animation, photogrammetry, procedural and commissioned content.
9. `PERFORMANCE-BENCHMARKS.md` — machines, scenes, capture settings, frame metrics, simulation metrics, repeatability.
10. `DECISIONS.md` — decisions, alternatives, assumptions, tradeoffs, re-evaluation triggers.
11. `PRODUCTION-PLAN.md` — milestones, agent workflow, QA, release process, budget, platform targets.

This is an organizing proposal, **not a mandated document count or schedule**. Consolidate where useful; keep single sources of truth and avoid contradictory instructions.

## 12. Collaboration protocol for GLM 5.3 and other agents

**Read §0–§2 before recommending a technology or reducing a feature.** Treat these rules as context for all future planning:

1. **Preserve the ambitious objective verbatim.** Do not quietly change “equal or better than CP2077 / RCT” into “good for an indie game.”
2. **Explore solutions before concessions.** When encountering a bottleneck, identify its cause, possible innovations, concrete experiments, and evidence needed.
3. **Do not mistake aspiration for proof.** Tag claims as demonstrated, hypothesized, unknown, or disproven under tested conditions. Cite sources and record exact environment/version where it matters.
4. **Respect open decisions.** No engine, language, renderer, art pipeline, multiplayer model, name, or prototype branch is final until explicitly decided by the human lead.
5. **Welcome a custom engine.** The lead is excited by this route; investigate seriously and fairly. Neither promote it automatically nor dismiss it because it is unconventional.
6. **Use honest comparisons.** Define matched workloads, quality, measurement methodology, and development effort. Fairly account for any existing prototype head start.
7. **Propose changes explicitly.** Never quietly rewrite commitments, delete requirements, or turn an exploratory suggestion into a fixed architecture.
8. **Challenge the implementation, not the aspiration.** Clear technical objections are welcome; reflexive realism lectures or automatic scope cuts are not.
9. **Ask substantive alignment questions.** When a choice affects the project's identity or a major irreversible decision, bring options and tradeoffs to the human lead.
10. **Prefer original, reusable knowledge.** Research existing work, use lawful assets, maintain reproducible experiments, and document what we genuinely discover.
11. **Plan before major implementation.** Small experimental spikes are permitted when expressly authorized; don't let prototype convenience become hidden policy.
12. **Keep disagreements visible.** If GLM, ChatGPT, or another agent differs, state the differing hypothesis and how to test it; the lead decides.

### Required format for significant research proposals

For every major technical proposal, aim to document: **goal → constraints → current evidence → approaches → novel opportunities → test plan → risks/unknowns → decision needed**. If an approach fails, record what was learned and the next hypothesis.

## 13. Decision ledger (current)

| Item | Position | Type |
|---|---|---|
| Visual target | Equal/exceed Cyberpunk 2077 in interactive-game visual quality | **VISION COMMITMENT** |
| Optimization target | Equal/exceed the extraordinary efficiency exemplified by RCT | **VISION COMMITMENT** |
| Game type | Large open-world futuristic RPG with freedom of role | **VISION COMMITMENT** |
| World | Authored districts, multiple interiors, separate procedural regions | **VISION COMMITMENT** |
| Systems | Hundreds of NPCs, persistent world, cycles, encounters, futuristic traversal | **VISION COMMITMENT** |
| Commercial intent | Steam, approximately $20 | **VISION COMMITMENT / price provisional** |
| Co-op | Part of full dream, launch timing not decided | **VISION COMMITMENT / scope OPEN** |
| Custom engine | **Actively welcome as a serious option**; compare and experiment | **OPEN DECISION** |
| Godot / Unreal / Unity / hybrid | Candidates, no selection | **OPEN DECISION** |
| Engine-independent deterministic simulation | Promising research direction, **not already decided** | **WORKING HYPOTHESIS** |
| Mid-tier PC target | Required target class; exact specification pending | **VISION COMMITMENT / specification OPEN** |
| Steam Deck | No launch requirement yet established | **OPEN DECISION** |
| Existing prototypes | Evidence, not sunk-cost commitments | **EXISTING EXPERIMENT** |
| Name | “SEKTOR 7” placeholder | **OPEN DECISION** |

---

**Alignment checkpoint:** This document is a comprehensive proposed restatement of the project lead's intention. It is not self-ratifying. GLM 5.3 and ChatGPT should identify discrepancies or unaddressed choices for the lead **before** treating exploratory architectural statements as decisions.
