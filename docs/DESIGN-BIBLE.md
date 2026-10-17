# DESIGN BIBLE — Core Gameplay Decisions

> **Status:** Early design bible v0.1 · 2026-10-16 · Consolidates game-design choices the project lead made after `VISION.md` v0.5, **without** locking speculative details. **No gameplay implementation authorized.**
>
> **Authority and document map:** [`VISION.md`](VISION.md) owns project-level aspirations, Twin Summits, original engine direction, and product identity; [`CUSTOM-ENGINE-RESEARCH.md`](CUSTOM-ENGINE-RESEARCH.md) owns research methodology; [`MULTIPLAYER-SPEC.md`](MULTIPLAYER-SPEC.md) owns all detailed multiplayer rules, saving, waiting, voting, session recovery, and roster policy. This document owns game-mechanics decisions outside that multiplayer detail. The first RPG's rules are not automatically hardcoded requirements of the reusable creation engine.
>
> **Decision tags:** **DECIDED** = explicitly accepted by lead; **WORKING PREFERENCE** = favored direction; **PROPOSAL** = idea not yet approved; **OPEN** = intentionally unanswered; **SUPERSEDED** = replaced by later lead decision. Distinguish an intended feature from a demonstrated capability. Design work does not select technologies.

## 0. Project identity and design philosophy

**DECIDED vision:** A large open-world, futuristic/cyberpunk RPG developed on **our original, reusable game creation engine**, which should eventually enable other games too. At least one large handcrafted map with districts, interior locations, and a separately accessible procedural frontier; hundreds of persistent NPCs; full day/night and weather cycles; random encounters; hover/flying vehicles; jetpacks; extensive role-playing freedom including crime as a choice. Intended Steam price roughly $20, final name and final price not locked.

**Twin Summits (VISION COMMITMENTS):** Match or exceed the **technical interactive graphical fidelity** of Cyberpunk 2077 (not its specific art direction) and pursue computational efficiency as impressive as or better than RollerCoaster Tycoon relative to meaningful workload. Do not silently bargain down either goal; measure, research, and invent new techniques where necessary. No particular reference machine defines a design ceiling or floor; recorded hardware is an experimental measurement anchor.

**DECIDED production approach:** Research-guided vertical development of the engine. Define subsystem responsibilities/boundaries before picking technologies. Windows first, Linux second, further systems later. Study existing engines as reference implementations. Strategically reuse external tools/libraries while owning the differentiating engine architecture. Research and documentation first; substantial implementation still requires project-lead authorization.

**Game inspirations (not licenses to imitate):** Skyrim, Starfield, Cyberpunk 2077, Borderlands, GTA, Red Dead Redemption, RollerCoaster Tycoon, The Sims/SimCity, Terraria, Minecraft, others. Cyberpunk is a *visual quality* comparator, not an aesthetic blueprint. Borderlands' exact feature influences are still **OPEN**.

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
- **OPEN:** Exact essential-NPC criteria, conditions for irreversible consequences, personal relationship mechanics, law/wanted response, economic effects, NPC memory, solo-world reputation, and reconciling emergent irreversible acts with democratic authored-world decisions.
- **Later multiplayer revision:** A single **shared authored-world group reputation** replaced earlier discussions of individual and nearby-companion reputation scoring. The procedural frontier has persistent consequences, but does not independently rewrite major authored-world reputation/story state. See [`MULTIPLAYER-SPEC.md`](MULTIPLAYER-SPEC.md). **Do not revive old multi-score formulas silently.**

## 7. World model, persistence, and activities

- **DECIDED:** A large authored open world of distinctive districts, with multiple interiors/cells, full daylight/nighttime cycles and dynamic weather.
- **DECIDED:** A **separate procedural frontier** supports repeatable exploration, farming, discovery, combat and long-term character progression.
- **DECIDED:** Hundreds of NPCs and multiple interconnected systems have meaningful persistent lives; the world progresses offscreen, without requiring every distant agent to execute at full visual or physical simulation frequency.
- **DECIDED:** Random encounters and authored/systemic world events contribute to a lively, reactive RPG.
- **DECIDED:** Hover/flying cars, jetpacks and other futuristic technologies support extensive vertical mobility.
- **OPEN:** Map scale, district count, interior count, frontier biome variety, transitions between the two worlds, authored-to-procedural state influence in *single-player*, schedule/economy/faction/law systems, world chronology during waiting/loads, world-generation algorithms, save representation.
- **Multiplayer rule:** Full roster for handcrafted/story world; any subset for procedural frontier, with protected main narrative state. See [`MULTIPLAYER-SPEC.md`](MULTIPLAYER-SPEC.md).

## 8. Multiplayer summary — linked authority, not duplicated

**DECIDED:** Functional 1–4 player cooperative gameplay at launch alongside entirely separate single-player campaigns. Multiplayer is world-bound, democratically governed, generally listen-server-oriented for research, and does not depend on developer-funded dedicated servers. The authored/story world requires the complete registered roster; the procedural frontier allows independent sessions with subsets of that roster. Shared quest XP/money and separately randomized equipment, voting, shared reputation, incapacitation, checkpoints, waiting, reconnect grace, and exact mid-combat recovery are specified exclusively in [`MULTIPLAYER-SPEC.md`](MULTIPLAYER-SPEC.md).

**OPEN:** How the engine implements networking, canonical save exchange between eligible hosts, authority isolation and conflict resolution. These are not invitations to select specific technologies before approval.

## 9. What is explicitly **not** decided

| Domain | Open issues |
|---|---|
| **Identity and narrative** | Final names of engine and game, detailed fiction, lore, factions, cast, central story, tone, art direction. |
| **Combat** | Weapon taxonomy, hacking details, enemy/vehicle AI, stealth, combat economy, player-versus-player mechanics. |
| **Progression** | Skill-tree catalog, exact numbers/formulas, respec rules, XP rates, perks, itemization and rarity. |
| **World** | District/world dimensions, procedural generation technology, detailed economies, travel interfaces and environmental systems. |
| **Multiplayer** | See the P0/P1 open decision table in `MULTIPLAYER-SPEC.md`. Do not infer final quorum, loot, host transfer technology, or save conflict policy. |
| **Engine technology** | Programming languages, graphics backends, renderer structure, ECS/data model, physics, scripting, editor/toolchain, asset/animation formats, networking stack. |
| **Production/business** | Schedule, release date, production budget, hiring/outsourcing, final Steam price, engine commercialization/distribution. |

## 10. Superseded and modified choices

| Prior thought | Current direction |
|---|---|
| Defined classes or permanent backgrounds | **SUPERSEDED:** Fully classless, all skills open from the outset, eventual universal mastery. |
| Overall levels only from raising skills | **SUPERSEDED:** Skill advancement **and** quest general XP both contribute to overall leveling. |
| Choosing a combat specialization that excludes others | **SUPERSEDED:** Systemic combat plus optional specialization by investment; eventual mastery of all skills. |
| NPCs always redeemable or all quest characters invulnerable | **SUPERSEDED:** Some truly irreversible outcomes; most NPCs killable, a few protected. |
| Individual player/faction reputation averaged with party presence | **SUPERSEDED for multiplayer:** Single shared authored-world group reputation; details in `MULTIPLAYER-SPEC.md`. |
| Portable character progression between unrelated multiplayer worlds | **SUPERSEDED:** World-bound multiplayer campaign characters. |
| Only the host controls campaign choices | **SUPERSEDED:** Democratic governance; hosting is a technical role. |

## 11. Suggested next planning order

1. **Preserve and validate decisions in this document and `MULTIPLAYER-SPEC.md`** before more game-mechanics discussion. Do not label proposals as decided.
2. **Resolve architecture-shaping gameplay questions** that impact reusable engine subsystem contracts: camera/animation, systemic interaction, traversal/streaming, world state and NPC persistence, save/restore, co-op authority.
3. **Create subsystem responsibility/boundary research**, then evaluate technical choices experimentally (`CUSTOM-ENGINE-RESEARCH.md`).
4. **Develop detailed quests, factions, art direction, lore, balance and asset specifications** after the relevant mechanic contracts are clear.
5. **Implement only when expressly authorized** by the human project lead; current docs are not a coding instruction.

### GLM 5.3 synchronization request

- Cross-reference this file from `VISION.md` rather than pasting every detailed rule into the vision foundation.
- Add a few high-signal decision-ledger updates: first/third person, systemic combat, tiered unlimited endgame flight, classless mastery, skill-use/perk gates and quest XP, three quest types, meaningful consequences/redemption, killable NPCs with limited protection, and democratic two-destination multiplayer.
- Reconcile older conflicting statements (especially the old multi-score reputation and host-authority policies). See the definitive multiplayer spec.
- Retain the verified `godot/` inventory as **existing experiment evidence**, not implementation authority. A separate `PROTOTYPE-INVENTORY.md` is appropriate for that audit.
- List genuinely unresolved questions; do not independently invent mechanics, choose engine subsystem technologies, or modify code/saves/assets without authorization.

---

*Design record synthesized from explicit project-lead choices in the current multi-agent planning conversation. Example skill perks, encounter scenarios and system mechanisms remain examples, not commitments.*
