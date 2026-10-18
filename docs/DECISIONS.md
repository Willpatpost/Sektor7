# DECISIONS — Canonical Project Decision Registry

> **Status:** Consolidated decision registry v0.3 · 2026-10-18. Incorporates the lead-ratified 2026-10-17 multiplayer round (M-27–M-35; M-11/15/17/18/21/23 modified) and the 2026-10-18 documentation-reconciliation round (M-36 and G-12 recorded; M-21 consolidated into M-31) per the update protocol in §7. No engine/game implementation authorized.
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
| G-10 | OPEN DECISION (narrowed 2026-10-18) | Solo persistence/durability and acknowledgment model, waiting, reputation, non-encounter death edge cases; exact Borderlands influence. Solo encounter-failure reset is decided (G-12) | [DESIGN BIBLE](DESIGN-BIBLE.md) |
| G-11 | OPEN DECISION | Combat math, NPC lives/economy/law, exact world size, progression rates, equipment catalog, quest/story content | [DESIGN BIBLE](DESIGN-BIBLE.md) |
| G-12 | DECIDED (documented 2026-10-18; previously approved) | Solo encounter failure uses the encounter-checkpoint reset principle, consistent with M-30: restore pre-encounter resources, remove uncommitted loot, preserve use-earned proficiency and committed progress | [DESIGN BIBLE](DESIGN-BIBLE.md) |

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
| M-11 | DECIDED (modified 2026-10-17) | Hosting is a technical responsibility; formal story decisions, waits and group transitions are proposed/voted, not host fiat. Player-initiated save/load votes are superseded by automatic progress-based persistence (M-29) | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-12 | DECIDED | Members may voluntarily retire; involuntary permanent removal requires unanimous approval of **remaining registered members** (target excluded) | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-13 | DECIDED | Ordinary emergent actions, including killing a normally killable quest NPC, can be individually performed and affect entire campaign; formal branching dialogue remains voted | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-14 | DECIDED | Group transitions and Skyrim-style waits require votes, are blocked when anyone relevant is chased/engaged, and transitions retain health | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-15 | DECIDED (modified 2026-10-17) | Character incapacitation and teammate revival; full-party incapacitation triggers encounter-failure transactional rollback (see M-30) | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-16 | DECIDED | Enemy HP and offensive capability scale with number of players; formula and level disparity handling open | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-17 | SUPERSEDED (2026-10-17) | Former save-category system (checkpoints/autosave/quicksave/manual) replaced by automatic progress-based persistence with canonical-storage acknowledgment (M-29) | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-18 | DECIDED (modified 2026-10-17) | Authored-world disconnect: freeze session up to 15 minutes; live suspensions may resume at the exact logical moment; terminated sessions resume at the encounter boundary | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-19 | DECIDED | Frontier unexpected disconnect: roughly 10-minute reconnect/reward protection; other players may keep playing | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-20 | DECIDED | Quest reward system should resist trivial reload rerolls; mechanism open | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-21 | DECIDED (consolidated into M-31, 2026-10-18) | Planned host handoff — full ratified statement lives in M-31; see also the voluntary-departure distinction (M-36) | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-22 | OPEN DECISION | Exact save divergence prevention/detection/recovery, world+character rollback semantics, crash consistency | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-23 | OPEN DECISION (narrowed 2026-10-17) | Remaining ballot mechanics: counting method, ties, deadlines, AFK/reconnection ballots. Resolved: removal unanimity (M-12), admission unanimity (M-32), ranked-choice confirmed (M-33), time-skip unanimity (M-34) | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-24 | PROPOSAL | Platform-independent stable account identity bound to roster/characters (implementation TBD); multiple characters per member | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-25 | OPEN DECISION | Ordinary world loot, trading, friendly fire, scaling per encounter/levels, late joining, host kick powers | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-26 | PROPOSAL (future, not launch) | Public shared servers with unrelated players, MMO-like world population, PvP playlists, seamless live host migration | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-27 | DECIDED (2026-10-17) | Every session begins in a central hub; one shared campaign clock | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-28 | DECIDED (2026-10-17) | One authoritative session per campaign; unverified or outdated campaign data cannot start authoritative gameplay | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-29 | DECIDED (2026-10-17) | Automatic progress-based persistence: milestones saved only when shared canonical storage acknowledges a complete recoverable revision; pending/confirmed UI distinction; irreversible actions gated on unconfirmed milestones; idempotent commit retries; internal backups and emergency recovery data required | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-30 | DECIDED (2026-10-17) | Encounter-failure transactional rollback: restore pre-encounter ammo/consumables/equipment condition, remove uncommitted loot, preserve use-earned skill proficiency and committed progress | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-31 | DECIDED (2026-10-17) | Planned host handoff required (volunteer → nomination → acceptance); unexpected-crash migration not launch; authority loss suspends progression with a local emergency recovery copy and renewed-validation resume | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-32 | DECIDED (2026-10-17) | Permanent admission requires unanimous approval by existing members; retired characters and progression are archived; governance votes may be cast asynchronously | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-33 | DECIDED (2026-10-17) | Ranked-choice voting for major narrative decisions with all registered members participating | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-34 | DECIDED (2026-10-17) | Deliberate time skipping requires unanimous approval of currently connected participants | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-35 | DECIDED (2026-10-17) | Fail-closed authority policy: no provisional authoritative progression, no automatic divergent-history merging, no silently weakened durability guarantees | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |
| M-36 | DECIDED (documented 2026-10-18; previously approved; host sequence settled 2026-10-18) | Voluntary departure from an authored-world session: remaining players return to the hub; unfinished encounter resets under party-wipe resource rules (M-30); committed progress and skill proficiency persist; no disconnect grace applies; distinct from planned host handoff (M-31). Departing host: attempt planned handoff first when an eligible successor is available; actual city departure returns the remaining participants to the hub | [MULTIPLAYER](MULTIPLAYER-SPEC.md) |

## 4. Engineering evidence and constraints

| ID | Status | Record | Owner |
|---|---|---|---|
| E-01 | EXISTING EXPERIMENT (verified in-repo) | Godot `godot/` Stage 0 read-only inventory: 16 GDScript files, 4,273 lines, 60/60 tests, clean import, no project addons. Audit executed directly in this repository 2026-10-16; reconstruction reconciled 2026-10-17 against the recovered original — all figures matched (PROTOTYPE-INVENTORY.md §7) | [PROTOTYPE INVENTORY](PROTOTYPE-INVENTORY.md) |
| E-02 | DECIDED | Retain existing Godot prototype as read-only calibration evidence, not a chosen engine; old browser prototype retired | [PROTOTYPE INVENTORY](PROTOTYPE-INVENTORY.md) |
| E-03 | WORKING HYPOTHESIS | Deterministic and/or data-oriented/event-driven simulation may benefit efficiency, testing or replication, not automatically guaranteed | [ENGINE RESEARCH](CUSTOM-ENGINE-RESEARCH.md) |
| E-04 | OPEN DECISION | Quantified workload-specific success thresholds, calibrated render/agent benchmarks and their settings | [ENGINE RESEARCH](CUSTOM-ENGINE-RESEARCH.md) |
| E-05 | WORKING PREFERENCE | Functional internal editor plus API/CLI capabilities for AI agents; public distribution/polish later | [ENGINE RESEARCH](CUSTOM-ENGINE-RESEARCH.md) |
| E-06 | DECIDED process | No new implementation or major experiments without lead's explicit approval; the completed Stage 0 inventory did not authorize coding | [ENGINE RESEARCH](CUSTOM-ENGINE-RESEARCH.md) |

## 5. Priority unresolved questions — not delegated decisions

*2026-10-17: the requirement level of several former P0s is now ratified (M-27–M-35). The remaining P0 items are mechanism/technology research, analyzed in [MULTIPLAYER-ARCHITECTURE-RESEARCH.md](MULTIPLAYER-ARCHITECTURE-RESEARCH.md) (unapproved).*

**P0 — Research before committing persistence/network contracts**

1. Mechanism research (requirements ratified as M-28/M-29): what constitutes shared canonical storage; how revisions are transmitted, authenticated, fork-detected, and opened by a different eligible host — including the solo-player acknowledgment case (one connected member of a multi-person campaign) — without developer-paid recurring infrastructure. Research into existing third-party services is approved.
2. Governed rollback (reframed 2026-10-17 — arbitrary/voted loads are superseded by M-29): whether any explicit mechanism restores a prior canonical revision (e.g., catastrophic-bug recovery), what it does to every world-bound character, and what the recoverable atomic state is.
3. Remaining ballot mechanics only: counting method, ties, absent votes, deadlines. Thresholds for removal, admission, time-skip, and ranked-choice story votes are decided (M-32–M-34).
4. What durability and recoverability can be guaranteed when the active host, rather than a guest, crashes mid-combat?
5. What are the stable identity/privacy/invitation rules for eligibility and campaign-bound character ownership? Stable IDs are a **proposal**, not selected authentication technology.

**P1 — Design-detail decisions**

6. Define the solo persistence/durability model, waiting, and non-encounter death edge cases in the design bible (encounter-failure reset is decided — G-12).
7. Choose the relationship between formal voted narrative branches and free emergent actions, including UX of irreversible consequences; core freedom to attack remains decided.
8. Specify world drops, reward eligibility on frontier reconnect/late join, optional character slots, party scaling across unequal levels, friendly fire, trading and basic anti-grief mechanisms.
9. Clarify whether frontier economic side effects beyond character gear/money reach authored services; major story and reputation protection is already decided.
10. Specify editor minimum viable tools, platform abstraction objectives and benchmark success measures; do not select technologies yet.
11. Determine what Borderlands elements specifically inspire combat/loot/tone/co-op, and which parts of the single-player campaign must differ from multiplayer.

## 6. Important supersessions (historical, not current policy)

| Earlier idea | Status now |
|---|---|
| Save-slot/checkpoint/autosave categories and group-voted save/load | SUPERSEDED (2026-10-17): automatic progress-based persistence with canonical acknowledgment (M-29) |
| Party wipe returns to a previous save | SUPERSEDED (2026-10-17): encounter-failure transactional rollback (M-30) |
| Host departure ends the session | SUPERSEDED (2026-10-17): planned host handoff (M-31) |
| Exact-state restoration across terminated sessions | SUPERSEDED (2026-10-17): live suspensions may resume exactly; terminated sessions resume at the encounter boundary (M-18) |
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

**Change log:** 2026-10-17 — lead-ratified round 3 incorporated (authorization: lead review message, "update the approved gameplay documentation and central DECISIONS registry"). Added M-27–M-35; modified M-11/15/17/18/21/23; supersessions recorded in §6. Architecture hypotheses remain unapproved: the member-replicated "Architecture D" from the v0.1 study was explicitly NOT approved by the lead; the revised research direction is in [MULTIPLAYER-ARCHITECTURE-RESEARCH.md](MULTIPLAYER-ARCHITECTURE-RESEARCH.md) v0.2.

**Change log:** 2026-10-18 — documentation-reconciliation round (authorization: lead directive — "corrections reflecting previous approval, not new product decisions"), incorporating ChatGPT's independent full-archive review. No product decision changed: M-36 and G-12 record previously approved rules now documented; M-21 consolidated into M-31 (duplicate wording); G-10 and P1-6 narrowed. Owning-document corrections: VISION v0.8 (§4.2 open-list refresh; §10 recovery summary corrected to live-suspension-exact/termination-at-encounter-boundary), MULTIPLAYER-SPEC v0.4 (§0/§9.1 recovery wording scoped to still-live suspensions; §9.4 voluntary departure added; §10 async-ballot future-enhancement claim removed), DESIGN-BIBLE v0.3 (§8 solo encounter-checkpoint reset), README v0.2 (provenance refresh, reading order, boundaries), CUSTOM-ENGINE-RESEARCH v0.4 (§6 host-handoff wording aligned with M-31). SIMULATION-ARCHITECTURE-RESEARCH revised to v0.2 per the lead's research corrections — still UNAPPROVED. Review precision pass (same date, three-way-review authorization): recovery-snapshot semantics fixed to committed durable state + encounter checkpoint (VISION §10, SPEC §1/§0/§9.1); simulation pending-delta survival across LOD deactivation with exactly-once reactivation specified (SIM §3–§4, I1, S6); voluntary host-departure sequence recorded as settled (M-36).

**A design proposal in an AI chat is not authorization to edit production code.** Every new research spike/implementation milestone requires its own express approval.
