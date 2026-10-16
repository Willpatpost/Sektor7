# SEKTOR 7 — Vision Foundation

> **Status:** Draft v0.2 · 2026-10-16 · Alignment reached — see §8. Everything here is provisional, including the project name (a naming pass is scheduled early). Nothing built so far is set in stone; a complete reset of the existing prototypes is on the table. This document is the anchor that everything else gets planned against.

## 1. The Twin Summits

This game pursues two summits at once and refuses to trade one against the other:

1. **Graphics: as good as Cyberpunk 2077 — if not better.** Nearly photorealistic in every way, with no loss of quality in any sense.
2. **Simulation: as computationally efficient as RollerCoaster Tycoon — if not better.** Runs as smooth as RCT on mid-tier hardware, and better still on high-end hardware.

The working philosophy: **dreams are not toned down to fit reality — dreams bend reality.** Where existing technique cannot reach a summit, this project's job is to invent technique until it can. Feasibility concerns are inputs to pathfinding, never reasons to lower a target.

**Hardware references**

| Tier | Reference | Bar |
|---|---|---|
| Mid-tier (floor) | typical mid-tier gaming PC | "smooth as RCT" — the minimum acceptable experience |
| High-end | Ryzen 7 9800X3D · GeForce RTX 5070 Ti · 32 GB DDR5-6400 | scales up — the game should exploit hardware like this, not merely run on it |

## 2. The Pitch

An open-world cyberpunk RPG with the feel of a AAA title: a large authored city of dense districts backed by a procedural frontier for farming and exploration; hundreds of simulated citizens living on schedules under full day/night and weather cycles; hovercars and jetpacks opening the city vertically; the freedom of Skyrim — be a mercenary, a criminal, or whoever you decide to be. Shipped on Steam for around $20.

## 3. Design Pillars (draft — review and amend)

1. **Living world** — hundreds of NPCs and other computational loads, simulated as real systems that keep running whether observed or not (Sims/RCT lineage), not decoration.
2. **Time and weather are gameplay** — full day/night cycle (the game will not stay night forever) and a weather cycle the world reacts to.
3. **Freedom of path** — crime is possible, not the premise; role-play choice in the Skyrim/Starfield tradition.
4. **Vertical, futuristic traversal** — hover/flying cars, jetpacks, and other Starfield-style mechanics.
5. **Something always happening** — Skyrim-style random encounters and events.
6. **Two worlds, one game** — authored open-world districts *and* separately navigable procedurally generated areas (XP/kill farming, exploration).
7. **AAA feel, indie price** — ~$20 on Steam.
8. **Built to be shared** — co-op is in the dream; the architecture is netcode-aware from day one so the world can eventually be played together without a rewrite.

## 4. Feature Commitments (2026-10-16 session)

- At least one large open-world map, organized into authored districts
- Multiple interior cells/locations
- Hundreds of NPCs + other heavy simulation
- Day/night cycle
- Weather cycle
- Random encounters/events
- Hover/flying cars, jetpacks, and further futuristic mechanics
- Optional criminal path (RPG freedom)
- Always-on world simulation — the world runs whether observed or not
- Co-op play in the full dream (netcode-aware architecture from day one)
- Steam release, ~$20 price point

## 5. World Structure

- **Authored districts** — handcrafted density; where the Cyberpunk/Skyrim feel lives.
- **Procedural zones** — reached separately from the authored world; generated areas for XP/kill farming and exploration. (Travel mechanism still open: gate, fast travel, edge-of-map, other.)
- **Interiors** — discrete cells/locations, loaded separately from the open world.

## 6. Inspirations

| Game | What we take from it |
|---|---|
| Cyberpunk 2077 | The graphical bar. (Not played by the author — the reference is the look, not the mechanics.) |
| Skyrim | Open-world freedom, random encounters, be-anything role-play |
| Starfield | Futuristic mechanics, traversal tech, procedural frontier beside authored space |
| GTA | Urban sandbox energy; crime as an option, not the premise |
| RDR1/RDR2 | World detail and immersion craft |
| RollerCoaster Tycoon | The efficiency bar: massive simulation on modest hardware |
| The Sims / SimCity | Living-world simulation depth |
| Borderlands (all) | *(draft — to confirm: FPS-RPG blend, loot, tone, co-op?)* |
| Terraria / Minecraft | Procedural worlds, long-tail playability |
| …and more | To be expanded |

## 7. Current State and Reset Policy

| Line | Status |
|---|---|
| `browser/` | Retiring. A one-night experiment ("GLM 5.3, make me a cyberpunk game I could play in the browser"); no longer used. |
| `godot/` | Active prototype (milestone 2). Also subject to the reset policy. |

Nothing is set in stone — not the architecture, not the engine investment, not the name. The reset option stays open until the plan says otherwise.

## 8. Alignment Decisions (2026-10-16)

1. **Engine: open — under structured evaluation.** Engine selection is not locked. Godot and Unreal (and any other contender) will be weighed against the Twin Summits before major investment. The existing Godot prototype is one input to that evaluation, not a commitment.
2. **Multiplayer: co-op is in the dream.** Borderlands-style shared play is part of the end vision. The core architecture is netcode-aware from day one — the simulation is designed to be deterministic and replicable, whether co-op ships at launch or arrives later.
3. **Offscreen simulation: always simulating.** The whole world runs on schedules and systems whether observed or not — Sims/RCT lineage, the truest form of the living-world pillar.
4. **Name: rename soon.** "Sektor7" is a placeholder; a proper naming pass is an early task, before documentation and code compound under the wrong label.
5. **Asset sourcing: under evaluation.** Landscape mapped 2026-10-16 (PolyHaven / Fab / marketplaces / AI-generated PBR / photogrammetry / commissioned / procedural materials); the decision will be documented per asset class once the pipeline plan exists.

## 9. Architectural Directives (consequences of §8)

- **The simulation core is the crown jewel — and it is engine-agnostic.** An always-on, co-op-capable world of hundreds of NPCs at RCT efficiency implies a deterministic, data-oriented simulation core: compact state, fixed update order, compiled hot paths. Whichever engine wins the evaluation, we own that core. The engine is chosen as the platform that *hosts* it — rendering, asset pipeline, platform/Steam layer — which sharply reduces engine-lock-in risk and means the current prototype's gameplay knowledge carries over regardless of outcome.
- **Determinism serves both summits at once.** A deterministic, fixed-timestep simulation is what makes RCT-style efficiency tractable *and* what makes co-op replication cheap (lockstep and server-authoritative models both benefit). One design choice, two summits.
- **Netcode-aware from day one.** State boundaries, ownership, and replication surfaces are designed in from the start. Retrofitting co-op is a rewrite; designing for it is a discipline.

---

*Alignment reached 2026-10-16. Next documents: engine evaluation, naming pass, design bible (systems), technical architecture, asset pipeline, world layout, production plan.*
