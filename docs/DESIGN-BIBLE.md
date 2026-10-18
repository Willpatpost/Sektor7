# DESIGN BIBLE — Core Gameplay Decisions

> **Status:** Gameplay design bible v0.3 · 2026-10-18 · Integrates subsequently approved roster and emergent-action decisions, identifies the solo-design owner, and records the previously approved solo encounter-checkpoint reset principle (§8). No game/engine implementation authorized.
>
> **Ownership:** This document owns **gamewide and single-player gameplay requirements** for the first RPG. [`VISION.md`](VISION.md) owns product vision; [`MULTIPLAYER-SPEC.md`](MULTIPLAYER-SPEC.md) owns detailed co-op governance, saving, waiting, rewards and recovery; [`CUSTOM-ENGINE-RESEARCH.md`](CUSTOM-ENGINE-RESEARCH.md) owns architecture research; [`DECISIONS.md`](DECISIONS.md) records decisions. Do not hardcode this game's policies as general-purpose engine behavior.
>
> **Decision labels:** Shared glossary in [`VISION.md`](VISION.md). DECIDED means approved by lead, WORKING PREFERENCE means favored, PROPOSAL means unapproved, OPEN DECISION means unresolved, SUPERSEDED means replaced.

## 0. Scope and sources of authority

The RPG is the first title planned for an original reusable creation engine. The Twin Summits, large open-world/cyberpunk RPG ambition, solo/AI-agent development philosophy, Windows→Linux platform priority, and no implementation authorization are defined by [`VISION.md`](VISION.md). This bible describes **gameplay** rather than duplicating those strategic statements.

Core inspirations include Skyrim, Starfield, Terraria, Borderlands, Cyberpunk 2077, GTA, Red Dead Redemption, RollerCoaster Tycoon, The Sims/SimCity, and Minecraft. Each is a reference for selected qualities, not an instruction to copy its protected content or necessarily imitate all its mechanics. Borderlands' specific influences remain an **OPEN DECISION**.

## 1. Camera and perspective

- **DECIDED:** The first game is fully playable in **both first-person and third-person**. These are supported gameplay perspectives, not merely cinematic views.
- **WORKING PREFERENCE:** Switching perspectives should be seamless and neither mode should feel neglected. In co-op, each participant can choose their own perspective independently.
- **OPEN:** Default perspective, key bindings, camera distance, shoulder swapping, aim behavior, cutscene framing, driving/flying cameras, wall collision and camera obstruction, animation/rigging pipeline.

**Engine implication (requirement to research):** Reusable camera controllers and animation/presentation layers accommodating multiple perspectives, not a fixed cyberpunk-specific camera system.

## 2. Combat and systemic interaction

- **DECIDED:** Combat is **highly systemic**, not only gunplay. Weapons/firearms, melee, abilities, hacking, physics, vehicles, and environmental interactions all play meaningful roles.
- **DECIDED guiding intention:** Interacting systems should produce emergent outcomes; e.g., weather, electrical hazards, enemy AI, hacking, and vehicles can affect the same encounter.
- **PROPOSALS, not promised individual features:** Electromagnetic interference, hackable doors/turrets, destructible machinery, wet-surface electricity, advanced cybernetics, vehicle weaponry, force-based interactions, environmental hazards.
- **OPEN:** Specific damage model, combat pacing, stealth, weapon types, firearms feel, melee mechanics, hacking limitations, physics depth, health/armor/status model, enemy AI, difficulty, destructibility, friendly fire and balance.

**Engine implication:** Gameplay systems need reusable mechanisms for discovering, exchanging, and reacting to world events and state changes, with clear boundaries and performance instrumentation. No particular event bus, ECS, physics library or scripting stack has been chosen.

## 3. Traversal and movement progression

- **DECIDED:** Mobility expands across a **progression path**, inspired by Terraria. Players begin with more constrained movement and unlock increasingly powerful traversal technologies.
- **DECIDED:** Endgame progression can eventually grant **genuinely unlimited personal flight**, not permanently fuel- or time-limited flight once its required technology is acquired.
- **DECIDED:** Mobility improvements come from a combination of **equipment** and **skill development** (in the spirit of Starfield). Equipment supplies capabilities; skills improve or expand their use.
- **PROPOSED illustrative tiers:** Ground movement → enhanced jumping/dashing/grappling → limited jetpack/hover → advanced flight → endgame unrestricted personal flight. Tier names, counts and order are *not* final.
- **OPEN:** Exact unlock paths, vertical/world-space boundaries, flight speeds, upgrade costs, energy/fuel rules for earlier tiers, fall damage, traversal physics, personal vs vehicle flight distinction.

**Engine implication:** Support high-speed, 3D and vertical traversal; flexible locomotion states; streaming and physics across large spaces; and multiplayer where players occupy distant regions simultaneously.

## 4. Classless character growth and perks

- **DECIDED:** **No classes and no permanent starting specialization locks.** All skill categories are available to every character from the beginning.
- **DECIDED:** A single character can, with enough progression, eventually become proficient in **every** skill category. No mutually exclusive class choice or finite one-time perk budget may permanently prevent eventual universal mastery.
- **DECIDED:** Skills improve **through use** (Skyrim-style). E.g., piloting develops by piloting, hacking by hacking.
- **DECIDED:** Overall character leveling is **hybrid**: improving skills contributes to character advancement, **and quests independently award general XP**.
- **DECIDED:** Character level-ups grant **perk points**. A perk can be bought with a point only when the character meets the required **proficiency level in the relevant skill** (and any other legitimate prerequisite). Leveling does not bypass skill practice.
- **DECIDED:** Equipment acquisition/upgrades and skill perks complement each other across combat, traversal, hacking, vehicles, and other playstyles.
- **OPEN:** XP conversion formula, skill proficiency curves, level caps (if any), perk-point availability, respec systems, skill-tree names, perk effects, anti-grind/anti-exploit tuning. If a level cap exists, universal mastery must still remain attainable.

**Illustrative loop:** Use piloting → gain piloting proficiency; complete a quest → gain general XP, money and possibly gear; character levels up → gains perk point; high enough piloting proficiency → permits purchase of an advanced piloting perk.

## 5. Quest ecosystem and rewards

- **DECIDED:** The game has **three complementary quest systems**:
  1. **Handcrafted narrative quests**: authored stories, major decisions, memorable characters, potentially branching outcomes.
  2. **Dynamic living-world quests**: generated or triggered by actual world events, NPC needs, faction conflict, economy, and environmental systems.
  3. **Repeatable procedural missions**: exploration, combat, farming, bounties, resource-gathering and other activities, particularly in the frontier.
- **DECIDED:** Quests may reward **general XP in addition to currency, equipment, and other rewards**; doing skill-related work during a quest can also raise the relevant skill proficiency.
- **WORKING INTENT:** Quest systems can interact and affect one another rather than remaining isolated, subject to multiplayer restrictions that protect the authored story from off-roster frontier play.
- **OPEN:** Main narrative structure, quest generation grammar/algorithms, progression gating, failures, quest replayability, quest log, reward economy, quest content, and branching design.
- **Multiplayer rule:** Eligibility, equal XP/currency for connected party members, randomized per-character quest equipment, and authored-versus-frontier consequences are governed by [`MULTIPLAYER-SPEC.md`](MULTIPLAYER-SPEC.md), not restated here.

## 6. Player agency, consequences, redemption, and NPC mortality

- **DECIDED:** Player choices have **far-reaching persistent consequences**; NPCs, factions, services, quests and the world can react meaningfully to actions.
- **DECIDED:** Redemption and reconciliation exist; players can improve their standing after wrongful or damaging behavior through meaningful subsequent acts. Redemption need not erase the past.
- **DECIDED:** Some events, actions, and relationships are **irreparable/irreversible**. Full skill mastery does **not** mean a single playthrough can see every mutually exclusive narrative outcome.
- **DECIDED:** **Most NPCs are killable**, including many important quest characters. The quest/world model adapts to deaths where possible; some quests may permanently fail or change. A smaller number of protected/essential NPCs are permitted as deliberate exceptions.
- **PROPOSED design patterns:** Leadership succession, substitute quest contacts, evidence/records surviving a deceased NPC, faction fragmentation, reparations, trust restoration without amnesia. These are examples, not promised universal quest fallback logic.
- **DECIDED multiplayer distinction:** Formal major branching narrative options require collective voting, but ordinary emergent acts—including one player attacking or killing an unprotected quest-important NPC—are individually possible and produce shared consequences. Choosing companions wisely is part of the social contract. See multiplayer spec for group implications.
- **OPEN:** Exact essential-NPC criteria, conditions for irreversible consequences, personal relationship mechanics, law/wanted response, economic effects, NPC memory, solo-world reputation, and which interactions count as formal narrative choices.
- **Later multiplayer revision:** A single **shared authored-world group reputation** replaced earlier discussions of individual and nearby-companion reputation scoring. The procedural frontier has persistent consequences, but does not independently rewrite major authored-world reputation/story state. See [`MULTIPLAYER-SPEC.md`](MULTIPLAYER-SPEC.md). **Do not revive old multi-score formulas silently.**

## 7. World model, persistence, and activities

- **DECIDED:** A large authored open world of distinctive districts, with multiple interiors/cells, full daylight/nighttime cycles and dynamic weather.
- **DECIDED:** A **separate procedural frontier** supports repeatable exploration, farming, discovery, combat and long-term character progression.
- **DECIDED:** Hundreds of NPCs and multiple interconnected systems have meaningful persistent lives; the world progresses offscreen, without requiring every distant agent to execute at full visual or physical simulation frequency.
- **DECIDED:** Random encounters and authored/systemic world events contribute to a lively, reactive RPG.
- **DECIDED:** Hover/flying cars, jetpacks and other futuristic technologies support extensive vertical mobility.
- **OPEN:** Map scale, district count, interior count, frontier biome variety, transitions between the two worlds, authored-to-procedural state influence in *single-player*, schedule/economy/faction/law systems, world chronology during waiting/loads, world-generation algorithms, save representation.
- **Multiplayer rule:** Full roster for handcrafted/story world; any subset for procedural frontier, with protected main narrative state. See [`MULTIPLAYER-SPEC.md`](MULTIPLAYER-SPEC.md).

## 8. Single-player design ownership

**DECIDED:** The main menu offers a single-player mode with separate world-bound characters and solo saves. Solo worlds have no campaign roster, multiplayer voting, player-host approval, or full-roster restriction. A solo player may play the authored city and procedural frontier as intended. The general classless progression, NPC mortality, narrative consequences, quest systems and movement rules also apply.

**WORKING DIRECTION / OPEN DETAILS:** The RPG should support solo saving, loading, waiting/time advancement, and death/retry, but the precise solo persistence rules are **not yet explicitly approved**. The co-op context now uses **automatic progress-based persistence with canonical-storage acknowledgment and encounter transactions** ([MULTIPLAYER-SPEC](MULTIPLAYER-SPEC.md) §8, 2026-10-17); the former autosave/quicksave/manual-save/checkpoint categories are superseded there. Do not silently transplant multiplayer acknowledgment thresholds, encounter-transaction boundaries, or governance rules to solo play. Potential shared engine primitives are a technical topic; exact solo player-facing policy will be designed here.

**DECIDED (documented 2026-10-18; previously approved principle):** Solo **encounter failure** (defeat/death inside an encounter) uses the **encounter-checkpoint reset principle**, consistent with the co-op encounter transaction ([MULTIPLAYER-SPEC](MULTIPLAYER-SPEC.md) §7, M-30): pre-encounter ammunition, consumables, and equipment condition are restored; uncommitted loot is removed; use-earned skill proficiency and previously committed progress persist. **Still open:** the solo persistence and durability model — whether and how solo milestones are acknowledged and stored — plus the edge cases below.

**Solo questions to resolve:** Whether solo uses the same progress-based persistence and acknowledgment model (and what acknowledges a solo milestone); non-encounter death edge cases and death outside a definable encounter; safety restrictions on waiting; solo law/reputation state; whether solo choices can ever affect other independent saves (not currently planned); independent character slots; local backup and corruption recovery.

## 9. Multiplayer boundary — linked authority

**DECIDED:** Functional 1–4 player cooperative play at launch alongside independent solo campaigns. Multiplayer characters are bound to a specific campaign; the handcrafted/story world requires the complete registered roster, and the procedural frontier allows a subset. The group has a single shared authored-world reputation, democratically governs major dialogue and group actions, and can unanimously (among remaining registered members) remove a participant involuntarily; voluntary retirement is permitted. Rewards, saves, waiting, reconnect grace, and exact mid-combat recovery are defined **only** in [`MULTIPLAYER-SPEC.md`](MULTIPLAYER-SPEC.md).

**OPEN:** Actual networking and persistence implementations belong in [`CUSTOM-ENGINE-RESEARCH.md`](CUSTOM-ENGINE-RESEARCH.md). No particular transport, serialization format, ECS or deterministic algorithm has been selected.

## 10. What is explicitly **not** decided

| Domain | Open issues |
|---|---|
| **Identity and narrative** | Final names of engine and game, detailed fiction, lore, factions, cast, central story, tone, art direction. |
| **Combat** | Weapon taxonomy, hacking details, enemy/vehicle AI, stealth, combat economy, player-versus-player mechanics. |
| **Progression** | Skill-tree catalog, exact numbers/formulas, respec rules, XP rates, perks, itemization and rarity. |
| **World** | District/world dimensions, procedural generation technology, detailed economies, travel interfaces and environmental systems. |
| **Multiplayer** | See the P0/P1 open decision table in `MULTIPLAYER-SPEC.md`. Do not infer remaining ballot mechanics (counting method, ties, deadlines), loot, host transfer technology, or save conflict policy; the removal/admission/time-skip unanimity and ranked-choice thresholds are decided (M-12, M-32–M-34). |
| **Engine technology** | Programming languages, graphics backends, renderer structure, ECS/data model, physics, scripting, editor/toolchain, asset/animation formats, networking stack. |
| **Production/business** | Schedule, release date, production budget, hiring/outsourcing, final Steam price, engine commercialization/distribution. |

## 11. Superseded and modified choices

| Prior thought | Current direction |
|---|---|
| Defined classes or permanent backgrounds | **SUPERSEDED:** Fully classless, all skills open from the outset, eventual universal mastery. |
| Overall levels only from raising skills | **SUPERSEDED:** Skill advancement **and** quest general XP both contribute to overall leveling. |
| Choosing a combat specialization that excludes others | **SUPERSEDED:** Systemic combat plus optional specialization by investment; eventual mastery of all skills. |
| NPCs always redeemable or all quest characters invulnerable | **SUPERSEDED:** Some truly irreversible outcomes; most NPCs killable, a few protected. |
| Individual player/faction reputation averaged with party presence | **SUPERSEDED for multiplayer:** Single shared authored-world group reputation; details in `MULTIPLAYER-SPEC.md`. |
| Portable character progression between unrelated multiplayer worlds | **SUPERSEDED:** World-bound multiplayer campaign characters. |
| Only the host controls campaign choices | **SUPERSEDED:** Democratic governance; hosting is a technical role. |
| Solo death/encounter-failure semantics left fully open | **DECIDED (2026-10-18):** encounter-checkpoint reset on solo encounter failure (resources restored, uncommitted loot removed, proficiency/committed progress kept); solo persistence/durability mechanisms remain open (§8). |

## 12. Suggested next planning order

1. **Preserve and validate decisions in this document and `MULTIPLAYER-SPEC.md`** before more game-mechanics discussion. Do not label proposals as decided.
2. **Resolve architecture-shaping gameplay questions** that impact reusable engine subsystem contracts: camera/animation, systemic interaction, traversal/streaming, world state and NPC persistence, save/restore, co-op authority.
3. **Create subsystem responsibility/boundary research**, then evaluate technical choices experimentally (`CUSTOM-ENGINE-RESEARCH.md`).
4. **Develop detailed quests, factions, art direction, lore, balance and asset specifications** after the relevant mechanic contracts are clear.
5. **Implement only when expressly authorized** by the human project lead; current docs are not a coding instruction.

### Documentation and implementation handoff

- This file is the source of truth for the first game's mechanical intentions; detailed multiplayer behavior is defined in its owning specification.
- The Stage 0 Godot audit is recorded separately in [`PROTOTYPE-INVENTORY.md`](PROTOTYPE-INVENTORY.md) as **reported experimental evidence**, not an engine choice.
- Log explicitly ratified changes in [`DECISIONS.md`](DECISIONS.md) and mark superseded directions. Ask the project lead before selecting technologies, inventing new gameplay commitments, or editing code/assets.
- Future solo-mode decisions belong in §8 until the gameplay rules grow large enough to justify a separate authoritative solo specification.

---

*Design record synthesized from explicit project-lead choices in the current multi-agent planning conversation. Example skill perks, encounter scenarios and system mechanisms remain examples, not commitments.*
