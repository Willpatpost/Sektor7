# PROJECT VISION — Working Title: SEKTOR 7

> **Status:** Consolidated vision foundation v0.7 · 2026-10-16. This edition integrates the four supplied Markdown files, decisions from the subsequent design conversation, and GLM 5.3's reported Stage 0 audit. The reported repository v0.6 and original `PROTOTYPE-INVENTORY.md` were not supplied for direct comparison. No implementation authorized.
>
> **Authority / ownership:** The human project lead sets the vision and ratifies decisions. This document owns the product goals, creative/engineering philosophy, and high-level constraints. [`DESIGN-BIBLE.md`](DESIGN-BIBLE.md) owns first-game mechanics; [`MULTIPLAYER-SPEC.md`](MULTIPLAYER-SPEC.md) owns multiplayer gameplay policy; [`CUSTOM-ENGINE-RESEARCH.md`](CUSTOM-ENGINE-RESEARCH.md) owns research methodology; [`DECISIONS.md`](DECISIONS.md) is the decision registry; [`README.md`](README.md) maps the folder.
>
> **Name:** “SEKTOR 7” is a disposable working title for the game, not a finalized game or engine brand.
>
> **Shared status vocabulary:** **VISION COMMITMENT** (non-negotiable destination/launch requirement), **DECIDED** (explicitly chosen design or process rule), **WORKING PREFERENCE** (favored but unratified approach), **WORKING HYPOTHESIS** (requires technical validation), **PROPOSAL** (suggested, not approved), **OPEN DECISION** (unresolved), **EXISTING EXPERIMENT** (evidence, not architectural authority), **SUPERSEDED** (formerly discussed, replaced). An externally reported result must identify its source; it is not independently verified unless the underlying evidence was examined.

## 0. Read this first: our agreement

The project lead has explicitly rejected the common practice of reducing an ambitious design to what is straightforward with current tools, conventional staffing, or known production patterns. Do **not** repeatedly lecture the lead that AAA games are difficult, that solo development is hard, or that the vision should be scaled down. Those facts are understood; they do not answer the question we are working on.

Our job is to **find, develop, test, and improve the path** to the vision. Where available technology is inadequate, we may invent, extend, combine, or replace technology. We are free to revisit the engine, renderer, programming languages, simulation model, production process, content pipeline, and even the project name. **No existing prototype has veto power over a better design.**

This is not permission to pretend an experiment succeeded, ignore physics, or misrepresent performance. Technical honesty is indispensable: distinguish demonstrated results, plausible hypotheses, unknowns, and failed approaches. A failed experiment rejects or revises an *approach*; it does not automatically reject the *destination*. The right response to an obstacle is: **What would need to be true? What can we measure? What different approach can we try?**

**Operating principle:** *The vision determines the destination. Engineering research determines the path. Evidence improves the path; it does not silently rewrite the destination.*

This document is a statement of ambition **and** a disciplined research charter. It is not a promise that all technical problems are already solved.

**Alignment record:** On 2026-10-16 the lead, with AI collaborators, resolved the previously open framework questions: the **original reusable game creation engine** is the chosen direction; **co-op for 1–4 players is a launch commitment**; no specific hardware dictates design; external technology is used strategically; Cyberpunk 2077 is a technical benchmark, not an aesthetic template. These decisions are reflected throughout this document and in the §13 decision ledger. Newly surfaced questions return to the lead before becoming commitments.

**Alignment record, round 2 (2026-10-16):** networking economics — no developer-funded dedicated-server infrastructure or recurring hosting costs, with a player-hosted listen-server as a **working preference**; subsystem-definition before implementation selection; a research-guided vertical engine development strategy; Windows-first/Linux-second platform priority; gameplay mechanics and technical requirements before story, lore, and naming; and authorization of a **read-only Stage 0 inventory** of the godot prototype (no refactoring, migration, or implementation). Subsequent gameplay alignment defined a fully classless, systemic, first/third-person RPG and the democratic two-destination multiplayer campaign. Its detailed rules live in the owning documents. The long-term technology horizon — exceptionally realistic graphics, extraordinary efficiency, broad portability, advanced distributed networking, and a creation engine competitive with the best — is recorded as research ambition, not claimed capability; practical constraints (such as avoiding recurring server costs) are legitimate project decisions. Committed requirements, working preferences, working hypotheses, and open decisions are recorded separately per the status convention and the §13 ledger.

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

### 4.1 Confirmed product and design direction

- The authored districts, interiors, procedural frontier, hundreds of persistent NPCs, day/night and weather, random encounters, vehicles/jetpacks, and broad RPG freedom in §2 remain **VISION COMMITMENTS**.
- Both first-person and third-person are full gameplay perspectives. Combat is highly systemic: weapons, abilities, hacking, physics, vehicles, and environment interact. Refer to [`DESIGN-BIBLE.md`](DESIGN-BIBLE.md) for the authoritative mechanics decisions.
- Character growth is classless from the beginning, eventually permitting mastery of every skill category. Skills develop through use; increasing skills and completing quests contribute to overall progression; level-ups grant perk points gated by the skill's required proficiency.
- Progression combines skills with acquired equipment, and can ultimately unlock unrestricted personal flight. Quests include authored narrative, persistent-world dynamic, and repeatable procedural missions. Choices carry durable consequences, redemption exists, some outcomes are irreversible, and most NPCs—including many quest characters—can be killed.
- Separate solo worlds and world-bound multiplayer campaigns are planned. The launch multiplayer game supports 1–4 players, governed democratically, with separate handcrafted (full registered roster required) and procedural (any nonempty subset) destinations. Detailed save, reward, reputation, voting, and reconnection rules are exclusively in [`MULTIPLAYER-SPEC.md`](MULTIPLAYER-SPEC.md).
- The multiplayer group may play frontier without all members, but frontier activity cannot rewrite the major authored narrative/reputation state. Frontier XP, money, and equipment are rewarded only to connected eligible players.
- The engine is an original, reusable game creation platform. External components may be strategically used; no specific internal technology has been selected. Windows is the first platform priority, Linux second, with future systems afterward.
- No particular CPU/GPU is a mandatory design floor or ceiling. Controlled, recorded hardware benchmarks measure our progress. No developer-funded dedicated server or recurring hosting requirement at launch; a player-hosted listen-server is the current working preference.
- Mechanics/architecture requirements precede detailed lore, naming, and content production. Research-guided vertical development is the chosen process; no major code implementation is yet authorized.

### 4.2 Open questions (do not silently decide)

**Game design:** The final names, lore, art direction, faction identities, economy and law mechanics, actual map size/biome content, detailed camera controls, weapon/damage/hacking rules, NPC relationship implementation, detailed traversal and perk catalogs, and precisely which Borderlands characteristics inspire the game. The overall camera, combat, progression, quests, and NPC mortality *directions are decided*; only details remain open.

**Multiplayer policy:** Democratic vote thresholds/tally methods for most actions (involuntary roster removal is unanimously approved by the remaining registered members); new/replacement member admission; character-slot policy; conflict handling when multiple hosts possess divergent campaign saves; offline/connected and grace-period edge cases; host-crash recovery; exact rules for rewinding world-bound characters; encounter scaling and ordinary loot. See [`MULTIPLAYER-SPEC.md`](MULTIPLAYER-SPEC.md).

**Solo gameplay:** Specific saving/death/waiting/reputation mechanics beyond the decided single-player separation; refer to [`DESIGN-BIBLE.md`](DESIGN-BIBLE.md).

**Technology and research:** Rendering APIs, engine language and architecture, simulation representation, network protocol, save/snapshot architecture, editor implementation, scripting, asset handling, benchmark acceptance thresholds, and deployment details. See [`CUSTOM-ENGINE-RESEARCH.md`](CUSTOM-ENGINE-RESEARCH.md).

**Operations/business:** Release date, budget, final price, broader engine distribution model, and other platform support beyond Windows/Linux priority.

For current priorities and exact question ownership, see [`DECISIONS.md`](DECISIONS.md). Never interpret an open question as permission to lock a permanent default.

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
| `godot/` | An active experimental prototype, previously described as milestone 2. Useful evidence, reusable ideas, and architectural lessons may exist; Godot is **not** selected—the prototype now serves the original-engine research as evidence and a reference implementation, not a candidate foundation. GLM reported completing the authorized read-only Stage 0 audit, which was executed directly in this repository. A structured reconstruction of that report appears in [`PROTOTYPE-INVENTORY.md`](PROTOTYPE-INVENTORY.md); the original audit record was subsequently recovered from the repository's pre-sync stash and reconciled with a first-hand verification record (PROTOTYPE-INVENTORY.md §7 — all figures matched). |
| Existing code, tests, art, documents | Reuse selectively if they advance the new plan. Audit rather than assume compatibility. |
| Project name `SEKTOR 7` | Placeholder to revisit in an early naming pass. |

**Full reset is explicitly permitted.** Past investment is information, not an obligation. The Stage 0 report documents what GLM observed; inspect its source evidence before basing implementation decisions on specific numbers or claims.

## 10. Multiplayer: launch promise and governance

**VISION COMMITMENT:** Functional and reasonably playable 1–4 player cooperative gameplay is required at launch alongside entirely separate single-player world/character saves. A developer-funded dedicated-server fleet or recurring game-server hosting cost is not required or budgeted. A listen-server remains a **WORKING PREFERENCE**, not a selected technology. The creation engine must provide a robust foundation for multiplayer without hardcoding this RPG's party rules as universal engine behavior.

**DECIDED product structure:** A multiplayer campaign has world-bound characters and a registered roster. The handcrafted/story world is accessible only with its complete roster; the procedural frontier permits any nonempty subset to play and grow independently. Important formal narrative decisions and group actions are democratic; hosting is a technical role without an extra vote. Involuntary removal requires unanimity among the other registered members; voluntary retirement is allowed. Most gameplay acts—including attacks on important NPCs—remain individually actionable within the normal rules and may have group-wide consequences.

**DECIDED recovery:** An unexpected guest disconnect during authored play suspends the session for 15 minutes, then saves an exact-state recovery snapshot for later full-roster resumption if reconnection fails. Frontier players have a separate approximately 10-minute unexpected-disconnect grace rule. Live host migration is not a launch commitment.

**Authoritative detail:** [`MULTIPLAYER-SPEC.md`](MULTIPLAYER-SPEC.md) owns the exact definitions, save/checkpoint policies, quest reward mechanics, frontier protection, roster voting, reconnection and open cases. [`CUSTOM-ENGINE-RESEARCH.md`](CUSTOM-ENGINE-RESEARCH.md) owns implementation research. Avoid reproducing detailed policies here or inventing a fully distributed save solution.

## 11. Process and living documentation map

We remain in research and design alignment. GLM’s reported Godot prototype audit is reference evidence, not the selected engine. No new implementation of the original engine or game has been authorized. Research plans and small experiments require explicit lead approval before substantive coding.

| File | Owns | Status |
|---|---|---|
| [`README.md`](README.md) | Folder navigation and document ownership rules | Exists |
| [`VISION.md`](VISION.md) | Non-negotiable goals, product identity, high-level constraints | Exists |
| [`DESIGN-BIBLE.md`](DESIGN-BIBLE.md) | RPG mechanics and single-player gameplay | Exists |
| [`MULTIPLAYER-SPEC.md`](MULTIPLAYER-SPEC.md) | Multiplayer gameplay policy, votes, saves, waiting, recovery | Exists |
| [`CUSTOM-ENGINE-RESEARCH.md`](CUSTOM-ENGINE-RESEARCH.md) | Technical research domains, experiments, and process | Exists |
| [`PROTOTYPE-INVENTORY.md`](PROTOTYPE-INVENTORY.md) | Reported Stage 0 findings and source limitations | Exists (reconstructed) |
| [`DECISIONS.md`](DECISIONS.md) | Canonical decision registry and unresolved questions | Exists |

**Possible future documents only when needed:** `TECH-ARCHITECTURE.md`, `WORLD-DESIGN.md`, `PERFORMANCE-BENCHMARKS.md`, `ENGINE-ROADMAP.md`, `TECHNOLOGY-RESEARCH.md`, `ASSETS.md`, `PRODUCTION-PLAN.md`, plus a technical `SAVE-ARCHITECTURE.md` when implementation research warrants it. A technical save architecture may coexist with, but must not override, player-facing save policy in `MULTIPLAYER-SPEC.md`.

New docs must declare exactly what they own and defer. Update this map and `README.md` when creating another document; record human-approved changes in `DECISIONS.md`. Do not multiply near-duplicate decision ledgers.

## 12. Collaboration protocol for GLM 5.3 and other agents

**Read §0–§2 before recommending a technology or reducing a feature.** Treat these rules as context for all future planning:

1. **Preserve the ambitious objective verbatim.** Do not quietly change “equal or better than CP2077 / RCT” into “good for an indie game.”
2. **Explore solutions before concessions.** When encountering a bottleneck, identify its cause, possible innovations, concrete experiments, and evidence needed.
3. **Do not mistake aspiration for proof.** Tag claims as demonstrated, hypothesized, unknown, or disproven under tested conditions. Cite sources and record exact environment/version where it matters.
4. **Respect open decisions.** The original-engine direction is already decided. No *internal technology* (language, renderer/backend, physics, network implementation, asset pipeline), final name, or prototype reuse plan is selected without the human lead.
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

## 13. Decision registry

The canonical, updateable decision registry is [`DECISIONS.md`](DECISIONS.md). This avoids maintaining divergent tables here and inside the multiplayer and design documents. Major established decisions include both Twin Summits; the original reusable engine; 1–4 player cooperative launch support; the separate solo and multiplayer worlds; democratic shared campaigns; the full-roster authored-world rule; frontier access for smaller groups; the classless skill-use/quest-XP progression; and the Windows-then-Linux platform priority.

The document owners contain detailed acceptance rules. If a proposed change conflicts with a decision, amend the owning document **and** the registry after explicit lead approval rather than silently treating the latest AI suggestion as policy.

---

**Alignment checkpoint:** This document is a consolidated restatement of the project lead's intention. It is not self-ratifying. GLM 5.3, ChatGPT, and other collaborators should identify discrepancies or unaddressed choices for the lead **before** treating exploratory architectural statements as decisions. The 2026-10-16 alignment decisions are incorporated throughout, with their canonical register in `DECISIONS.md`; questions that surface during continued review return to the lead before becoming commitments.
