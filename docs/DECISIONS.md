# DECISIONS — Canonical Project Decision Registry

> **Status:** Consolidated decision registry v0.1 · 2026-10-16. Derived from the four attached project documents and the project lead's subsequent explicit decisions in the planning discussion. No engine/game implementation authorized.
>
> **Ownership:** This is the **single index of decisions, preferences, hypotheses, supersessions and unresolved approvals**. Detailed requirement wording lives in [`VISION.md`](VISION.md), [`DESIGN-BIBLE.md`](DESIGN-BIBLE.md), [`MULTIPLAYER-SPEC.md`](MULTIPLAYER-SPEC.md) and [`CUSTOM-ENGINE-RESEARCH.md`](CUSTOM-ENGINE-RESEARCH.md). If conflicts arise, preserve evidence and return to the lead; do not silently declare a new winner.
>
> **Statuses:** Defined in [`VISION.md`](VISION.md). **DECIDED / VISION COMMITMENT** reflects explicit user acceptance. **WORKING PREFERENCE / PROPOSAL / WORKING HYPOTHESIS** is not final architecture. **EXISTING EXPERIMENT** is not a design decision. **SUPERSEDED** means a later decision replaced an earlier one. Treat technical necessity inferred by an agent as an assumption/proposal unless the lead ratifies it.

## 1. Project and engine

| ID | Status | Decision / intent | Owner |
|---|---|---|---|
| V-01 | VISION COMMITMENT | Interactive technical graphical fidelity matching/exceeding Cyberpunk 2077, without copying its art direction | [VISION](VISION.md) |
| V-02 | VISION COMMITMENT | Computational efficiency as impressive as or better than RCT, measured relative to meaningful work, without trading away graphics by default | [VISION](VISION.md) |
| V-03 | VISION COMMITMENT | Large original futuristic open-world RPG: authored districts and interiors, procedural frontier, hundreds of persistent NPCs, dynamic weather/time, random encounters, futuristic travel and role freedom | [VISION](VISION.md) |
| V-04 | VISION COMMITMENT | Original **reusable creation engine** supporting many future games; this RPG is its first major game | [VISION](VISION.md) |
| V-05 | DECIDED | Strategic reuse of lawful external tools/libraries; own differentiating engine architecture | [VISION](VISION.md) |
| V-06 | DECIDED | Research-guided vertical-engine approach; define subsystem ownership/roles/interfaces before choosing implementations | [ENGINE RESEARCH](CUSTOM-ENGINE-RESEARCH.md) |
| V-07 | DECIDED | Windows first; Linux second; additional platforms later. Steam Deck optional | [VISION](VISION.md) |
| V-08 | VISION COMMITMENT | No single computer defines a mandatory performance floor/ceiling; controlled reproducible benchmarks remain essential | [VISION](VISION.md) |
| V-09 | DECIDED | Game mechanics/technical requirements before detailed world fiction, naming and content | [VISION](VISION.md) |
| V-10 | OPEN DECISION | Final engine/game names, engine public release timing, final game pricing/date and budget | [VISION](VISION.md) |
| V-11 | DECIDED | Existing engines are technical references, not contestants for replacing the original-engine direction | [ENGINE RESEARCH](CUSTOM-ENGINE-RESEARCH.md) |
| V-12 | OPEN DECISION | Graphics API, renderer architecture, engine language, jobs/data architecture, simulation, editor tech, network transport, asset pipeline | [ENGINE RESEARCH](CUSTOM-ENGINE-RESEARCH.md) |

## 2. RPG gameplay

| ID | Status | Decision / intent | Owner |
|---|---|---|---|
| G-01 | DECIDED | Playable first-person **and** third-person cameras | [DESIGN BIBLE](DESIGN-BIBLE.md) |
| G-02 | DECIDED | Highly systemic combat: weapons/melee, hacking, abilities, physics, vehicles, environments | [DESIGN BIBLE](DESIGN-BIBLE.md) |
| G-03 | DECIDED | Equipment and skill-based movement progression culminating in unrestricted personal flight late-game | [DESIGN BIBLE](DESIGN-BIBLE.md) |
| G-04 | DECIDED | Completely classless from the outset; every skill discipline eventually masterable by one character | [DESIGN BIBLE](DESIGN-BIBLE.md) |
| G-05 | DECIDED | Skills rise through relevant use; overall levels also gain from quests and skill advances; level-ups award perk points, and perk purchases require skill proficiency | [DESIGN BIBLE](DESIGN-BIBLE.md) |
| G-06 | DECIDED | Handcrafted story quests, reactive living-world quests, and repeatable procedural missions | [DESIGN BIBLE](DESIGN-BIBLE.md) |
| G-07 | DECIDED | Player decisions have lasting consequences; meaningful redemption possible; certain events are irreparable | [DESIGN BIBLE](DESIGN-BIBLE.md) |
| G-08 | DECIDED | Most NPCs (including many story-important ones) killable; some deliberately protected; quests adapt/fail where appropriate | [DESIGN BIBLE](DESIGN-BIBLE.md) |
| G-09 | DECIDED | Single-player campaigns/world-bound characters separate from multiplayer; no co-op roster/voting requirements in solo mode | [DESIGN BIBLE](DESIGN-BIBLE.md) |
| G-10 | OPEN DECISION | Detailed solo death/reload, waiting and reputation/save rules; exact Borderlands influence | [DESIGN BIBLE](DESIGN-BIBLE.md) |
| G-11 | OPEN DECISION | Combat math, NPC lives/economy/law, exact world size, progression rates, equipment catalog, quest/story content | [DESIGN BIBLE](DESIGN-BIBLE.md) |

## 3. Multiplayer and campaign governance

| ID | Status | Decision / intent | Owner |
|---|---|---|---|
| M-01 | VISION COMMITMENT | Functional solo and cooperative gameplay with **1–4 simultaneous players** at launch | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-02 | VISION COMMITMENT | No developer-funded dedicated-server infrastructure/recurring game-server budget | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-03 | WORKING PREFERENCE | Player-hosted listen-server; any eligible participant may host a *future* session using latest valid shared save | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-04 | DECIDED | Multiplayer character progression and inventory bound to a specific campaign; no cross-world portable-character imports | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-05 | DECIDED | Authored/story world only available when **all currently registered members are online** | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-06 | DECIDED | Procedural frontier available to any connected nonempty roster subset, including one member | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-07 | DECIDED | Frontier has persistent local consequences but cannot rewrite major authored reputation or narrative; player XP/equipment/money carries across environments | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-08 | DECIDED | One shared campaign reputation in authored world, affected by any member's actions; no individual/group-average formula | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-09 | DECIDED | All eligible connected party members receive equal quest XP/cash regardless of distance or direct participation; equipment rewards individually randomized | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-10 | DECIDED | Offline frontier roster members receive **no** XP, money or equipment rewards | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-11 | DECIDED | Hosting is a technical responsibility; important formal story decisions and player-initiated saves/loads, waits and group transitions are proposed/voted, not host fiat | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-12 | DECIDED | Members may voluntarily retire; involuntary permanent removal requires unanimous approval of **remaining registered members** (target excluded) | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-13 | DECIDED | Ordinary emergent actions, including killing a normally killable quest NPC, can be individually performed and affect entire campaign; formal branching dialogue remains voted | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-14 | DECIDED | Group transitions and Skyrim-style waits require votes, are blocked when anyone relevant is chased/engaged, and transitions retain health | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-15 | DECIDED | Character incapacitation and teammate revival; all active party members incapacitated triggers wipe/reload | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-16 | DECIDED | Enemy HP and offensive capability scale with number of players; formula and level disparity handling open | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-17 | DECIDED | Rolling cell checkpoints (two prior loads), 15-min autosaves, one overwritable quicksave, durable manual saves, separate interrupted-session recovery snapshot | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-18 | DECIDED | Authored-world disconnect: freeze session up to 15 minutes; then preserve exact-state recovery snapshot, return lobby; full roster resumes same logical moment, even mid-fight | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-19 | DECIDED | Frontier unexpected disconnect: roughly 10-minute reconnect/reward protection; other players may keep playing | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-20 | DECIDED | Quest reward system should resist trivial reload rerolls; mechanism open | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-21 | DECIDED | Active host leaving ends current session; seamless *live* migration is not launch requirement; alternate hosts for *future sessions* remain research preference | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-22 | OPEN DECISION | Exact save divergence prevention/detection/recovery, world+character rollback semantics, crash consistency | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-23 | OPEN DECISION | Quorums, ranking/ties, admission approval threshold, voting deadlines, AFK and reconnection ballots | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-24 | PROPOSAL | Platform-independent stable account identity bound to roster/characters (implementation TBD); multiple characters per member | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-25 | OPEN DECISION | Ordinary world loot, trading, friendly fire, scaling per encounter/levels, late joining, host kick powers | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-26 | PROPOSAL (future, not launch) | Public shared servers with unrelated players, MMO-like world population, PvP playlists, seamless live host migration | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |

## 4. Engineering evidence and constraints

| ID | Status | Record | Owner |
|---|---|---|---|
| E-01 | EXISTING EXPERIMENT (GLM-reported) | Godot `godot/` Stage 0 read-only inventory: 16 GDScript files, 4,273 lines, 60/60 tests, clean import, no project addons. Not independently reproduced in this handoff | [PROTOTYPE INVENTORY](PROTOTYPE-INVENTORY.md) |
| E-02 | DECIDED | Retain existing Godot prototype as read-only calibration evidence, not a chosen engine; old browser prototype retired | [PROTOTYPE INVENTORY](PROTOTYPE-INVENTORY.md) |
| E-03 | WORKING HYPOTHESIS | Deterministic and/or data-oriented/event-driven simulation may benefit efficiency, testing or replication, not automatically guaranteed | [ENGINE RESEARCH](CUSTOM-ENGINE-RESEARCH.md) |
| E-04 | OPEN DECISION | Quantified workload-specific success thresholds, calibrated render/agent benchmarks and their settings | [ENGINE RESEARCH](CUSTOM-ENGINE-RESEARCH.md) |
| E-05 | WORKING PREFERENCE | Functional internal editor plus API/CLI capabilities for AI agents; public distribution/polish later | [ENGINE RESEARCH](CUSTOM-ENGINE-RESEARCH.md) |
| E-06 | DECIDED process | No new implementation or major experiments without lead's explicit approval; the completed Stage 0 inventory did not authorize coding | [ENGINE RESEARCH](CUSTOM-ENGINE-RESEARCH.md) |

## 5. Priority unresolved questions — not delegated decisions

**P0 — Research before committing persistence/network contracts**

1. How is the canonical campaign saved, transmitted, backed up, authenticated, and opened by a different eligible host? How does the system detect divergent histories when no always-online coordinator exists?
2. How do manual loads/rollbacks affect the entire world-bound cast when a player has made progress in the frontier after a historical checkpoint? What is the recoverable atomic state?
3. What ballots/quorums apply to major story choices and routine group actions? Removal unanimity is already resolved, but admission/replacement, absent votes and tie methods remain open.
4. What durability and recoverability can be guaranteed when the active host, rather than a guest, crashes mid-combat?
5. What are the stable identity/privacy/invitation rules for eligibility and campaign-bound character ownership? Stable IDs are a **proposal**, not selected authentication technology.

**P1 — Design-detail decisions**

6. Define exact solo saves, death and wait behavior in the design bible.
7. Choose the relationship between formal voted narrative branches and free emergent actions, including UX of irreversible consequences; core freedom to attack remains decided.
8. Specify world drops, reward eligibility on frontier reconnect/late join, optional character slots, party scaling across unequal levels, friendly fire, trading and basic anti-grief mechanisms.
9. Clarify whether frontier economic side effects beyond character gear/money reach authored services; major story and reputation protection is already decided.
10. Specify editor minimum viable tools, platform abstraction objectives and benchmark success measures; do not select technologies yet.
11. Determine what Borderlands elements specifically inspire combat/loot/tone/co-op, and which parts of the single-player campaign must differ from multiplayer.

## 6. Important supersessions (historical, not current policy)

| Earlier idea | Status now |
|---|---|
| Choose Godot or UE5 with weighted engine-selection scorecard | SUPERSEDED by building an original, reusable engine |
| Portable character inventory/levels between unrelated multiplayer worlds | SUPERSEDED by campaign-bound characters |
| Separate individual and party-averaged reputation | SUPERSEDED by one shared authored-world campaign reputation |
| Original host decides votes, saves and transitions | SUPERSEDED by host-neutral democracy |
| All registered players must be online for *any* multiplayer | SUPERSEDED: full roster only in authored/story destination; frontier permits subsets |
| Each active player's loot goes to all registered players, even offline | SUPERSEDED: frontier rewards only to connected eligible participants |
| A missing participant forces restart of a boss encounter | SUPERSEDED: authored world exact-state recovery is default |
| Roster permanent, nobody can leave | SUPERSEDED: voluntary retirement and remaining-member unanimous involuntary removal |

## 7. Updating this registry

For an approved change, record the date, user authorization, decision ID, the superseded prior state, why the change was made, and update the **owning document** in the same revision. Retain superseded decision history, including originally competing AI recommendations where materially relevant. Do not create an independent duplicate ledger in VISION or the design bible.

**A design proposal in an AI chat is not authorization to edit production code.** Every new research spike/implementation milestone requires its own express approval.
