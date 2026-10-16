# PROJECT VISION — Working Title: SEKTOR 7

> **Status:** Vision foundation, revised draft v0.5 — 2026-10-16. Incorporates both 2026-10-16 alignment rounds. Round 1: original-engine direction, launch co-op commitment, hardware/benchmarking stance, external-technology stance, CP2077's benchmark role. Round 2: networking constraints with a listen-server working preference, subsystem-definition before implementation selection, research-guided vertical development strategy, Windows-first/Linux-second platform priority, mechanics-before-lore design sequencing, and the authorized read-only Stage 0 prototype inventory.
> **Authority:** The human project lead owns the vision, priorities, and final decisions. GLM 5.3 and other AI collaborators are research, design, engineering, and implementation partners—not independent owners of the vision.
> **Read with:** [`CUSTOM-ENGINE-RESEARCH.md`](CUSTOM-ENGINE-RESEARCH.md). This document establishes **what we are trying to create and how we approach unsolved problems**. The companion document establishes **how we research the architecture and technologies of our original engine without shrinking the ambition**.
> **Project name:** “SEKTOR 7” is an expendable placeholder. Not a locked brand, title, setting, or technical identity.  
> **Status convention:** **VISION COMMITMENT** = committed requirement or destination; **WORKING PREFERENCE** = preferred direction pending research and validation, never silently treated as committed; **WORKING HYPOTHESIS** = a technical idea needing proof; **OPEN DECISION** = undecided; **EXISTING EXPERIMENT** = prototype evidence, not a commitment.

---

## 0. Read this first: our agreement

The project lead has explicitly rejected the common practice of reducing an ambitious design to what is straightforward with current tools, conventional staffing, or known production patterns. Do **not** repeatedly lecture the lead that AAA games are difficult, that solo development is hard, or that the vision should be scaled down. Those facts are understood; they do not answer the question we are working on.

Our job is to **find, develop, test, and improve the path** to the vision. Where available technology is inadequate, we may invent, extend, combine, or replace technology. We are free to revisit the engine, renderer, programming languages, simulation model, production process, content pipeline, and even the project name. **No existing prototype has veto power over a better design.**

This is not permission to pretend an experiment succeeded, ignore physics, or misrepresent performance. Technical honesty is indispensable: distinguish demonstrated results, plausible hypotheses, unknowns, and failed approaches. A failed experiment rejects or revises an *approach*; it does not automatically reject the *destination*. The right response to an obstacle is: **What would need to be true? What can we measure? What different approach can we try?**

**Operating principle:** *The vision determines the destination. Engineering research determines the path. Evidence improves the path; it does not silently rewrite the destination.*

This document is a statement of ambition **and** a disciplined research charter. It is not a promise that all technical problems are already solved.

**Alignment record:** On 2026-10-16 the lead, with AI collaborators, resolved the previously open framework questions: the **original reusable game creation engine** is the chosen direction; **co-op for 1–4 players is a launch commitment**; no specific hardware dictates design; external technology is used strategically; Cyberpunk 2077 is a technical benchmark, not an aesthetic template. These decisions are reflected throughout this document and in the §13 decision ledger. Newly surfaced questions return to the lead before becoming commitments.

**Alignment record, round 2 (2026-10-16):** networking economics — no developer-funded dedicated-server infrastructure or recurring hosting costs, with a player-hosted listen-server as a **working preference**; subsystem-definition before implementation selection; a research-guided vertical engine development strategy; Windows-first/Linux-second platform priority; gameplay mechanics and technical requirements before story, lore, and naming; and authorization of a **read-only Stage 0 inventory** of the godot prototype (no refactoring, migration, or implementation). The long-term technology horizon — exceptionally realistic graphics, extraordinary efficiency, broad portability, advanced distributed networking, and a creation engine competitive with the best — is recorded as research ambition, not claimed capability; practical constraints (such as avoiding recurring server costs) are legitimate project decisions. Committed requirements, working preferences, working hypotheses, and open decisions are recorded separately per the status convention and the §13 ledger.

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

"Better than Cyberpunk 2077" is an aspiration to exceed the reference where we can, not an unsupported claim we have already achieved it. **Decided (2026-10-16): Cyberpunk 2077 is a benchmark for technical graphical fidelity, not an aesthetic template.** We aim to meet or surpass its technical visual quality, potentially exceeding it through original rendering innovations; our own art direction is a separate open design question. The exact reference settings, scenes, measurements, and comparison methodology remain to be specified.

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
- **Multiplayer commitment:** The game must support **1–4 simultaneous players at launch**. A player can experience the game entirely alone; two, three, or four players can play cooperatively. Cooperative play must be **functional and reasonably playable in the launch release**—perfect multiplayer polish and advanced features are not launch requirements, but multiplayer cannot be deferred entirely to a post-launch update. **Committed constraint:** launch multiplayer must not require developer-funded dedicated-server infrastructure or recurring hosting costs. **Working preference:** a player-hosted listen-server model; research topics include Steam Networking Sockets, P2P connectivity, relay services, authoritative simulation, shared persistence, and host migration. The engine must remain flexible enough to support different networking architectures for future games.
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
9. **Foundational cooperative multiplayer.** 1–4 simultaneous players at launch; the engine and world simulation treat multiplayer as a foundational capability, while the networking model remains an open research question.
10. **Technology as a creative medium.** We are building an **original, reusable game creation engine**—a project objective in its own right, not merely a means to ship this game. We own its distinguishing architecture, rendering systems, simulation technologies, editor, asset pipeline, and creation workflows, using external components strategically where they serve us.
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
- Co-op supporting **1–4 simultaneous players, functional and reasonably playable at launch** (networking model open).
- An **original, reusable game creation engine** as the chosen direction—a project objective in its own right, with the cyberpunk RPG as its first major game and the platform designed to support many future games.
- Ownership of the engine's distinguishing architecture, rendering, simulation, editor, asset pipeline, and creation workflows, with **strategic use of external tools, libraries, standards, and dependencies**.
- Exceptional computational efficiency **across hardware**: no specific GPU, CPU, handheld, or PC specification dictates design; known hardware serves benchmarking and measurement; Steam Deck is not a mandatory design target.
- Cyberpunk 2077 as a **technical fidelity benchmark, not an aesthetic template**.
- **Networking economics:** launch multiplayer must not require developer-funded dedicated servers or recurring hosting costs; player-hosted listen-server is the working preference.
- **Platform priority:** Windows first, Linux second, additional platforms subsequently; cross-platform architectural boundaries from the beginning; Steam Deck compatibility optional.
- **Process decisions:** define every engine subsystem's responsibilities, boundaries, dependencies, and interfaces **before** selecting implementations; research-guided vertical engine development prioritizing architectural coherence, modularity, profiling, and maintainability.
- **Design sequencing:** gameplay mechanics and technical requirements are established **before** story, lore, naming, and individual content details; architecture-affecting questions (camera, combat, traversal, world simulation, multiplayer, procedural generation, persistence) take priority.
- A willingness to reset existing prototype work completely; the project name remains a placeholder.

### 4.2 Open questions (do not silently decide)

- Final project name, world fiction, setting, lore, narrative structure, tone, and art direction.
- Camera perspective(s), combat mechanics, skill systems, classes, progression, loot, and whether Borderlands inspiration specifically means FPS/RPG combat, loot, co-op, tone, or some mixture.
- Number, size, and density of authored districts; number and kinds of interiors; total world scale.
- Travel/transition mechanism to procedural zones and whether they share a persistent timeline/state.
- How autonomous NPC lives, economies, factions, player reputation, law, and quest consequences work.
- Networking implementation within the decided constraints (no developer-funded dedicated servers; listen-server working preference): Steam Networking Sockets vs. other transports, relay usage, the authoritative-simulation split, shared-persistence format, and whether host migration is a launch requirement.
- Platform rollout beyond the decided Windows-first/Linux-second priority: which additional platforms, and when (Steam Deck remains optional).
- Benchmark and measurement hardware beyond the known high-end reference rig, and the frame-time/fidelity thresholds we measure against; these are measurement anchors, not design constraints.
- Engine subsystem technologies within the decided original-engine direction: rendering backend and graphics API, simulation framework, networking implementation, editor technology, scripting language(s), asset pipeline and formats, external-component licensing, outsourcing, and toolchain (see [`CUSTOM-ENGINE-RESEARCH.md`](CUSTOM-ENGINE-RESEARCH.md)).
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
| **Cyberpunk 2077** | **Technical** graphical-fidelity benchmark—not an aesthetic template or mechanical blueprint (the lead has not personally played it). We aim to meet or surpass its technical visual quality, potentially through original rendering innovations. |
| **RollerCoaster Tycoon (1999)** | Iconic performance-conscious programming; complex simulation relative to available hardware. |
| **The Sims / SimCity** | Persistent lives, interconnected world systems, systemic simulation. |
| **Skyrim** | Open-world freedom, player-defined roles, encounters, exploration, consequences. |
| **Starfield** | Futuristic technology and traversal inspiration, plus the relationship between authored spaces and broader exploratory content. |
| **Grand Theft Auto** | Large urban sandbox energy; crime remains optional, not the defining objective. |
| **Red Dead Redemption 1 / 2** | World detail, atmosphere, immersion, environmental craft. |
| **Borderlands (series)** | Inspiration acknowledged; the exact desired elements still need clarification. Do not assume a specific looter-shooter formula. |
| **Terraria / Minecraft** | Repeatable exploration, procedural systems, longevity, discoverability, player agency. |
| **Other games and research** | Inspiration list is open and expandable. |

## 7. Hardware and benchmarking principles

**Decided stance (2026-10-16):** We are **not selecting a specific GPU, CPU, handheld, or mid-tier PC as a constraint that dictates our design.** Our ambition is exceptional computational efficiency **across hardware**. We will still benchmark on known hardware, measure performance objectively, and compare improvements under controlled conditions; **measurements inform engineering—particular hardware specifications do not dictate the limits of the vision.** Steam Deck is not a mandatory design target.

**Known high-end reference configuration (measurement anchor, not a design constraint):** AMD Ryzen 7 9800X3D, NVIDIA GeForce RTX 5070 Ti, 32 GB DDR5-6400 MT/s RAM.

**Benchmarking intent:** we will deliberately choose known machines (including mid-class systems) as reproducible measurement anchors—not as design ceilings—and define the frame-time/fidelity thresholds we measure against. The experience intent remains exceptionally smooth performance on modest hardware with headroom on high-end systems. The RCT analogy expresses the ambition to exceed expectations for the amount of meaningful work being performed—not a claim that frame rates on radically different games/hardware are directly comparable.

**Evaluation discipline:** compare not only FPS, but actual image quality, simulation throughput, CPU/GPU frame times, stutter, RAM/VRAM, asset streaming, and real gameplay behavior. Report exactly what was measured, on what hardware, with what settings. “Looks good” and “runs fast” are starting observations, not complete evidence.

## 8. Technology philosophy: ownership and invention

**Engine direction is DECIDED (2026-10-16): we are building our own original, reusable game creation engine.** It is a project objective in its own right—a platform designed to support many different future games, with the cyberpunk RPG as its first major game. We own its distinguishing architecture, rendering systems, simulation technologies, editor, asset pipeline, and creation workflows. **No engine subsystem technology has been selected yet**: rendering backends, graphics architectures, simulation frameworks, networking, editor technology, scripting, asset pipelines, and other subsystems are open research questions (see [`CUSTOM-ENGINE-RESEARCH.md`](CUSTOM-ENGINE-RESEARCH.md)). Existing engines—Godot, Unreal, Unity, and others—remain **reference implementations, benchmarks, and sources of architectural lessons**, not competitors for selection.

Within the original-engine direction, we will decide per component whether to:

- Build our own from scratch where the technology differentiates the engine.
- Use, modify (where permitted), or wrap an established library, standard, or tool where it is commodity.
- Re-derive an approach proven in a reference engine to our needs and constraints.
- Replace an earlier choice if experiments provide evidence that a different foundation is better.

An original engine **does not require** inventing every file format, graphics API, math routine, or editor widget; strategic reuse of external components is decided policy.

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
| `godot/` | An active experimental prototype, previously described as milestone 2. Useful evidence, reusable ideas, and architectural lessons may exist; Godot is **not** selected—the prototype now serves the original-engine research as evidence and a reference implementation, not a candidate foundation. A **read-only Stage 0 inventory** was authorized 2026-10-16 (verify functionality, tests, claims, dependencies, assets; no refactoring or migration). |
| Existing code, tests, art, documents | Reuse selectively if they advance the new plan. Audit rather than assume compatibility. |
| Project name `SEKTOR 7` | Placeholder to revisit in an early naming pass. |

**Full reset is explicitly permitted.** Past investment is information, not an obligation. Before deciding what to preserve, inventory actual working features, dependencies, code ownership, asset provenance, test coverage, and measured performance.

## 10. Multiplayer: a launch commitment with an open implementation

**DECIDED (2026-10-16): the game must support 1–4 simultaneous players at launch.** A player can experience the game entirely alone; two, three, or four players can play cooperatively. Cooperative play must be **functional and reasonably playable in the launch release**—perfect multiplayer polish and advanced features are not launch requirements, but multiplayer cannot be deferred entirely to a post-launch update. The original engine must therefore treat multiplayer as a **foundational capability**: world simulation, persistence, entity ownership, synchronization, and gameplay systems account for multiple simultaneous players by design.

**Committed constraint (2026-10-16):** launch multiplayer must not depend on developer-funded dedicated-server infrastructure or recurring hosting costs. **Working preference:** a player-hosted listen-server model. **Open for research:** Steam Networking Sockets, P2P connectivity, relay services, the authoritative-simulation split, shared persistence, host migration, replication, prediction, and interest management. The requirement is the multiplayer experience, not a predetermined implementation—and the engine remains flexible enough to support different networking architectures for future games.

**Working hypothesis:** a clean simulation model, possibly deterministic in its authoritative subsystems, could ease repeatability, testing, and network replication. **Not established:** that all simulation must be deterministic, that deterministic lockstep is necessarily optimal, or that determinism alone makes multiplayer cheap. Explore server authority, prediction, interest management, snapshots, divergence detection, replay, and practical development cost as separate questions—and avoid over-engineering a network stack before evaluating the appropriate design.

## 11. Process: alignment, research, then development

We are **not** entering full production merely because a prototype exists. The current priority is achieving deep alignment and comprehensive documentation, then moving into targeted experiments and technical research. Development begins in earnest when the lead deliberately authorizes it.

Suggested planning deliverables (subject to amendment):

1. `VISION.md` — ambition, game identity, principles, intent, open questions (this file).
2. `CUSTOM-ENGINE-RESEARCH.md` — custom-engine architecture and technology research framework (repurposed 2026-10-16 from ENGINE-EVALUATION.md after the engine-direction decision).
3. `DESIGN-BIBLE.md` — systems, loops, player roles, encounters, progression, world rules, co-op concept.
4. `WORLD-DESIGN.md` — districts, interiors, procedural areas, vertical traversal, living-world requirements.
5. `TECH-ARCHITECTURE.md` — simulation state, renderer, threading, tools, saves, networking, platform separation.
6. `RENDERING-RESEARCH.md` and `SIMULATION-RESEARCH.md` — hypotheses, literature, prototypes, evidence.
7. `ENGINE-ROADMAP.md` — for the original engine: modules, build order, dependencies, editor/tooling needs, integration contracts.
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
5. **Honor the custom-engine decision.** The original engine is the chosen direction (2026-10-16). Research its architecture and technologies with rigor and genuine curiosity; neither treat the decision as provisional nor hide evidence that a subsystem choice deserves reconsideration—course corrections go to the lead, with evidence.
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
| Co-op | **1–4 simultaneous players, functional and reasonably playable at launch**; solo fully supported; listen-server working preference | **VISION COMMITMENT** (decided 2026-10-16) |
| Networking economics | No developer-funded dedicated-server infrastructure or recurring hosting costs; engine flexible enough for multiple networking architectures | **VISION COMMITMENT** (decided 2026-10-16) |
| Networking model | Player-hosted listen-server preferred; Steam Networking Sockets, relays, host migration under research | **WORKING PREFERENCE** (2026-10-16) |
| Platform priority | Windows first, Linux second, additional platforms later; cross-platform boundaries from the start; Deck optional | **DECIDED** (2026-10-16) |
| Development strategy | Research-guided vertical engine development: foundations → small coherent engine → integrate experimental tech → expand to reusable platform | **DECIDED** (2026-10-16; implementation not yet authorized) |
| Subsystem process | Define responsibilities/boundaries/dependencies/interfaces before selecting implementations; no premature technology commits | **DECIDED** (2026-10-16) |
| Design sequencing | Gameplay mechanics + technical requirements before story, lore, naming, content details | **DECIDED** (2026-10-16) |
| Godot prototype | Read-only Stage 0 inventory authorized after document alignment; no refactor/migrate/implement; evidence, not architectural authority | **AUTHORIZED** (2026-10-16) |
| Custom engine | Build an **original, reusable game creation engine**—a project objective in its own right; cyberpunk RPG is its first major game; engine subsystem technologies not yet selected | **VISION COMMITMENT** (direction decided 2026-10-16; subsystem selections OPEN) |
| Godot / Unreal / Unity / hybrid | Reference implementations, benchmarks, and architectural lessons—not candidates for selection | **DECIDED (role)** (2026-10-16) |
| External technology | Strategic use of external tools, libraries, standards, dependencies; ownership of differentiating architecture and technologies | **DECIDED** (2026-10-16) |
| CP2077 role | Technical graphical-fidelity benchmark, not an aesthetic template | **DECIDED** (2026-10-16) |
| Engine-independent deterministic simulation | Promising research direction, **not already decided** | **WORKING HYPOTHESIS** |
| Hardware stance | Exceptional efficiency **across hardware**; no specific spec dictates design; known hardware for benchmarking and measurement only | **VISION COMMITMENT** (decided 2026-10-16; measurement anchors OPEN) |
| Steam Deck | Not a mandatory design target; optional compatibility research | **DECIDED (not required)** |
| Existing prototypes | Evidence, not sunk-cost commitments | **EXISTING EXPERIMENT** |
| Name | “SEKTOR 7” placeholder | **OPEN DECISION** |

---

**Alignment checkpoint:** This document is a comprehensive proposed restatement of the project lead's intention. It is not self-ratifying. GLM 5.3, ChatGPT, and other collaborators should identify discrepancies or unaddressed choices for the lead **before** treating exploratory architectural statements as decisions. The 2026-10-16 alignment decisions are incorporated throughout and recorded in the §13 ledger; questions that surface during continued review return to the lead before becoming commitments.
