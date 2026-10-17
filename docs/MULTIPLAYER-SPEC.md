# MULTIPLAYER SPECIFICATION — Shared Campaigns

> **Status:** Design specification v0.1 · 2026-10-16 · Captures decisions reached in the project-lead/ChatGPT multiplayer design discussions after `VISION.md` v0.5. **Design approved in principle; implementation not authorized.**
>
> **Authority:** Human project lead. This document is the detailed source of truth for **the first RPG's multiplayer gameplay policy**. `VISION.md` owns the overarching ambitions and launch commitments; `CUSTOM-ENGINE-RESEARCH.md` owns technology investigation. `DESIGN-BIBLE.md` references this document for multiplayer rather than duplicating it. **Do not assume this document has already been merged into the repository.**
>
> **Status vocabulary:** **DECIDED** = explicitly chosen gameplay requirement; **WORKING PREFERENCE** = favored, not final; **PROPOSAL** = assistant-origin suggestion requiring approval; **OPEN** = unresolved; **SUPERSEDED** = earlier direction replaced by a later decision. Technology choices and measurements remain research topics, even when gameplay behavior is decided.

## 0. At a glance — the experience we are building

- **DECIDED:** The game ships with fully playable **solo** and **1–4 player cooperative** experiences. Multiplayer cannot be deferred until after launch.
- **DECIDED:** The main menu separates **Single-player** and **Multiplayer**. Solo-world/character saves and multiplayer campaigns are separate, not portable between unrelated worlds.
- **DECIDED:** Multiplayer campaigns have a **registered roster** of up to four participants; characters are **bound to that campaign**. Membership and consequential campaign decisions are collectively governed, not owned politically by the current host.
- **DECIDED:** The **authored/story world** is available only with the campaign's **entire registered roster online**. Its consequential story decisions are made democratically.
- **DECIDED:** The **procedural frontier** is available to **any nonempty subset** of registered participants, including one person, for grinding, exploration, and independent advancement.
- **DECIDED:** Frontier consequences persist **within the frontier** but do not independently rewrite the authored world's **major reputation or narrative state**.
- **DECIDED:** The authored world has **one shared campaign reputation**, affected by actions of any participating player, not individually averaged reputation scores.
- **DECIDED:** Quest XP and currency are granted **equally to connected, eligible group members**, irrespective of their physical location or direct participation. Quest equipment rewards are rolled **per eligible character**. In frontier sessions, **offline members receive nothing**.
- **DECIDED:** Connected players can propose group waiting, saving/loading, and transitions for a vote; the host gets **no special voting authority**.
- **DECIDED:** A disconnection during an authored-world session suspends that session for **15 minutes**. If unrecovered, preserve a distinct **recovery snapshot** and return to lobby. With the full roster restored, resume at the **precise suspended authoritative gameplay state**, even mid-combat, after a brief reorientation countdown.
- **DECIDED:** Launch multiplayer **must not require developer-funded dedicated game servers or recurring hosting costs**. A player-hosted listen-server is the current **WORKING PREFERENCE**, not a selected networking implementation.
- **WORKING PREFERENCE:** Any registered participant who has the latest valid campaign state should be able to host future sessions. **OPEN:** The safe mechanism for distributing, validating, and reconciling campaign saves across different hosts.

**Not launch requirements:** Large-population shared online servers, MMO-like open-world concurrency, and PvP modes (e.g. team deathmatch, FFA, CTF) are **future ambitions only**. A seamless live host migration system is not currently a launch commitment.

## 1. Definitions and boundaries

| Term | Meaning |
|---|---|
| **Registered roster** | The player identities permanently associated with one multiplayer campaign, subject to democratic membership changes. A 2-person campaign needs its 2 registered members—not 4—to unlock the authored world. |
| **Connected participants** | Roster members online in the current session, plus any temporarily disconnected player whose grace-period eligibility is retained. Exact implementation of temporary eligibility remains open. |
| **Active host** | The registered participant currently running the authoritative game session on their machine. A technical role, **not** an extra vote or ownership of narrative decisions. |
| **Campaign** | One world-bound history and associated participant/character data, comprising authored world, frontier state, reputation, quests, saves, and checkpoints. |
| **Authored world** | Handcrafted story/city environment containing major shared quests, faction politics, reputation and irreversible narrative events. |
| **Procedural frontier** | Separately accessible procedurally generated exploration/farming/mission environment with its own persistent consequences. |
| **World-bound character** | A character belonging to one single-player world or multiplayer campaign, not imported into unrelated worlds. |
| **Full roster** | Every registered campaign participant currently connected, subject to explicit short disconnection recovery rules. |
| **Recovery snapshot** | Special resumable simulation checkpoint for an interrupted authored-world session; separate from ordinary save slots. |

**Engine/product distinction:** The original creation engine should expose reusable identity, authority, serialization, voting, and networking primitives where appropriate. Rules such as *full-roster authored-city access* and *equal quest XP* are policies of **this game**, not universal requirements for every game built with the engine.

## 2. Main menu, campaigns, characters, and rosters

### 2.1 Single-player and multiplayer separation — DECIDED

```text
Main menu
├── Single-player
│   └── Create/load solo worlds and the characters tied to them
└── Multiplayer
    ├── Host a registered campaign (if entitled and save is available)
    └── Join a registered campaign session
        └── Choose destination: authored world (full roster) or frontier (subset allowed)
```

- Single-player worlds and characters remain single-player experiences; they have no social-roster requirement.
- Multiplayer campaigns retain their own characters, quests, money/inventories, progression, reputation, world history, and saves. **No portable character importing** between unrelated campaigns or between solo and multiplayer by default.
- **WORKING PREFERENCE:** Each participant can potentially have multiple character slots within the same campaign, choose an eligible character on joining, and be identified by a stable player identity plus a separately displayed character name. **OPEN:** Maximum slots and whether switchable characters share any character-level rewards.
- A stable account identifier, rather than a mutable username, should establish roster membership and character ownership (**PROPOSAL**, not an authentication technology selection).

### 2.2 Democratic campaign membership — DECIDED in principle

- Campaign registration controls access; joining is not the same as importing a character from another world.
- The group, not the active host alone, governs **permanent roster additions, removals, and replacements**.
- The group must be able to continue if an original member leaves permanently; **permanent roster lock-in is SUPERSEDED**.
- **OPEN:** Unanimity vs. majority, the voting rights of the departing member, absent-player votes, deadlines, re-invitation, bans, and participant retirement/archiving.
- **PROPOSAL:** Archive retired members' characters instead of destroying them; replacement members create their own characters.
- Host moderation and an immediate kick from a live session are **OPEN**; do not assume they confer a unilateral right to erase a player from the campaign.

### 2.3 Hosting and ownership — DECIDED goals, open implementation

- **DECIDED:** Hosting is a technical service. The current host has no casting bonus or unilateral story control.
- **WORKING PREFERENCE:** Any eligible registered member can host the shared campaign using its newest valid authoritative save.
- **DECIDED constraint:** No developer-funded dedicated-server fleet or recurring game-server hosting budget for launch. Existing third-party connectivity services may be researched for terms, availability, and cost.
- **WORKING PREFERENCE:** Player-hosted listen-server with session authority on the host machine; transport, lobby, NAT traversal, relay, authentication, and engine API are **OPEN**.
- **SUPERSEDED:** A permanent host who alone owns all story decisions/saves/transitions; a permanent requirement that every member be online for **all** gameplay.
- **OPEN:** Whether the save is distributed to all participants automatically, transferred on request, or backed up through an opt-in provider. Steam Cloud does **not** by itself establish a shared cross-account canonical save; don't invent that capability.
- **OPEN:** How to avoid conflicting off-network sessions. Without a central coordinator or guaranteed communication, we can detect divergent save histories, but must not promise perfect prevention of simultaneous forks.
- If the *active* host leaves during a live session, the old rule is to disconnect/end the current session. A different member hosting **a future session** is distinct from seamless *live* host migration. Actual crash recovery is covered in §9.

## 3. Destination access, world continuity, and independent progression

### 3.1 Authored story world — DECIDED

- **Entire registered roster must be online** to enter or resume authored-world play.
- Main narrative and major faction decisions are shared and voted on; one lone member cannot advance the authored campaign's major branching story through ordinary menu decisions.
- All four players in a 4-member campaign, or all three in a 3-member campaign, are required. The rule scales to the **actual registered roster**, not a hardcoded four-person party.
- Normal world simulation, NPC decisions, and combat remain active during play, including far-reaching and potentially irreversible consequences.
- **OPEN:** Whether an individual's emergent action (such as killing a quest-important NPC) may bypass a formal group vote. Preserve the decided NPC mortality and player-agency goals; don't silently mark all such actions unkillable or vote-gated.

### 3.2 Procedural frontier — DECIDED

- Any **1 to N** currently available roster members (up to campaign roster size, maximum 4) can host/join a frontier session without the absent players.
- Allows exploration, farming, repeatable procedural missions, obtaining equipment/resources, and leveling characters.
- Frontier discoveries and consequences remain persistent within that environment and campaign.
- Frontier actions **must not independently rewrite the handcrafted world's major shared reputation or storyline**. This is a *boundary of authority*, not a declaration that frontier play has no lasting significance.
- Existing singular shared reputation for the authored campaign remains; a separate numeric frontier reputation is **not decided**.
- **OPEN:** Which frontier-generated economic resources, items, discoveries, or minor effects are allowed to influence the authored world, and how these are represented without circumventing protected story decisions.

### 3.3 Entry, transfers, and location choice

- **WORKING PREFERENCE:** Choose authored world vs. frontier before entering a session. UI and actual load/transfer semantics are not selected.
- The two environments belong to **one campaign history**, with world-bound characters and appropriate progression persisting between them.
- **DECIDED:** Group transitions between cells/locations are coordinated; everyone involved in that session transitions together, and current health values carry over.
- **DECIDED:** A group transition is blocked when any participating player is currently pursued/engaged by enemies, police, or comparable threats.
- **OPEN:** What happens when someone returns to lobby, players are in separate districts, a vehicle is occupied, summons/followers are active, or a protected authored-world roster member disconnects during transition.

## 4. Reputation, NPC agency, story consequences

- **DECIDED:** One shared group reputation belongs to the authored campaign. Actions by **any** player can raise or lower that reputation even if other players are elsewhere.
- **SUPERSEDED:** Per-player reputation, permanent party averaging, and nearby-companion weighting as the primary reputation mechanic.
- **DECIDED:** Meaningful and far-reaching player choices can change NPC relationships, factions, quests, and the world. Redemption/reconciliation can repair many bad relationships, but some outcomes are genuinely irreversible.
- **DECIDED:** Most NPCs are killable, including many important story NPCs. Quest systems adapt to deaths where possible; limited protected/essential exceptions exist. Death can lock or alter a quest and need not be reversible.
- A single numeric reputation does **not** preclude NPC-specific memories, quest flags, consequences, or relationship dialogue. **OPEN:** How these are represented without unintentionally recreating unapproved multiple hidden reputation systems.
- **OPEN:** Attribution for crimes and irreversible emergent actions, shared legal consequences, and which authored decisions count as mandatory democratic votes.

## 5. Group democracy: voting and authority

### 5.1 Core rules — DECIDED

- Any connected roster participant can **propose** a vote for player-initiated saving/loading, waiting, or a group transition.
- Important **branching narrative decisions** are resolved through a group vote rather than decided unilaterally by the active host.
- Host runs authoritative validation/execution of the chosen action, but **hosting does not grant privileged voting power**.
- Permanent roster governance is democratic (§2.2).
- **OPEN:** Voting eligibility during temporary disconnects, whether every roster member or every connected member must cast a ballot for routine actions, and the ballot/quorum threshold for each action type.

### 5.2 Voting mechanics — unselected

- **PROPOSAL from lead:** First-choice and second-choice preferences (a ranked ballot) for multi-option dialogue; on a deadlock, transparently choose at random among tied choices (up to four, at most 25% per choice in a four-way equal tie).
- **OPEN:** Exact ranked-choice counting method, ties, revotes, abstention, AFK behavior, timeout, voting notification, initiator cooldown, cooldown bypass for emergencies, and whether any action requires unanimous consent.
- **PROPOSAL:** Random tiebreaks for *story choices* could be acceptable, but must **not** decide catastrophic administrative actions such as world deletion or irreversible save-history restoration.
- **OPEN:** Whether votes on major authored-world decisions always require all registered members to *vote*, versus only to be present. Full-roster presence for authored-world access is decided; a specific ballot threshold is not.
- **OPEN:** Whether ordinary NPC interactions, quest acceptance, or routine dialogue can occur without a vote. Do not force every shopkeeper exchange to a ballot.

## 6. Quest and reward rules

### 6.1 Eligibility — DECIDED

- **Authored quests:** All registered members are connected (by access rule); on eligible quest completion, **each** receives the **same XP and money** and **their own individually randomized equipment/weapon reward** when applicable, regardless of proximity or direct participation.
- **Frontier quests:** Only players **connected/eligible in that frontier session** receive XP, money, and equipment. **Offline roster members receive nothing.** Among eligible connected members, shared quest completion XP/currency is equal irrespective of separation/participation.
- Skill proficiency earned by actually using a skill accrues **individually**; it is not an automatically equalized shared quest reward.
- Characters are world-bound, and one character becoming stronger than another is permissible. Enemy scaling across unequal levels remains to be designed.

### 6.2 Late join and disconnect edge cases

- **DECIDED:** A frontier participant suffering an unexpected disconnect has approximately **10 minutes** of reconnection/reward-protection grace. A quest that resolves while the participant is protected may retain their reward entitlement; rejoining restores appropriate state.
- **DECIDED:** Authored-world unexpected disconnects use the distinct **15-minute session suspension** policy (§9), rather than continuing the major fight while the full roster is absent.
- **OPEN:** Precise eligibility at the moment a player joins, leaves intentionally, disconnects just as a quest completes, swaps characters, or loses the ability to rejoin. Deliberate departures should not be silently treated as indefinite protected disconnects.
- **OPEN:** Whether an inactive character in the same campaign receives anything when its player selects a different character. No reward duplication between character slots is assumed.

### 6.3 Reward reroll protection

- **DECIDED GOAL:** Prevent trivial save/reload exploitation to repeatedly reroll randomized quest equipment.
- **PROPOSAL from lead:** Resolve loot at or before an important pre-completion step, rather than only on the final hand-in action. By itself this does not stop reloading an earlier checkpoint.
- **RESEARCH OPTION:** Stable, recorded reward identities/seeds by campaign + quest instance + character so that replaying the same quest instance produces the same reward; legitimate new repeatable instances can roll new rewards.
- **OPEN:** The final rule when an authorized full save predates reward generation, when a character changes, or when a quest is truly repeated. No absolute anti-tamper claim for player-controlled saves.

## 7. Combat, difficulty, incapacitation, and wipes

- **DECIDED:** Enemies/NPC opponents scale HP and offensive capability with player count. Scaling should be evaluated for cooperative fun, not merely inflated health pools.
- **OPEN:** Party-wide vs nearby-participant scaling, how to account for level disparity, player joining/leaving during a fight, enemy quantity/AI scaling, and boss-specific policies.
- **DECIDED:** In multiplayer group play, a defeated player becomes **incapacitated** and can be revived. When **every active party member is incapacitated**, the party wipes and returns to an eligible previous save.
- **OPEN:** Exact incapacitation movement, bleedout/timer (if any), revive time, revive items/skills, treatment of a disconnected or absent player, downed player in an occupied vehicle, and which prior save has priority after a wipe.
- **DECIDED:** A cell transition preserves health values, rather than resetting the party to full health.
- **OPEN:** Friendly fire, kill credit/assist assignment, PvE loot drops, player-to-player trading, and physical separation across multiple simultaneously relevant regions.
- Solo gameplay death-and-load rules are to be designed in the broader game design, not silently inferred from multiplayer incapacitation.

## 8. Saving, loading, checkpoints, and waiting

> **Document ownership:** This section is the authoritative *gameplay policy* for group saves/waiting at present. Exact serialization, storage, snapshot atomicity, replication, file formats, and backup technology belong in future technical architecture research. **Do not create an independent SAVE-SYSTEM.md that contradicts these rules.** Split into a dedicated spec only when it becomes operationally large, and then replace this section with a link.

### 8.1 Save categories — DECIDED intent

| Save | Required behavior | Outstanding detail |
|---|---|---|
| **Cell checkpoint** | Entering a new cell/location creates a loadable checkpoint; rolling previous-load history reaches back approximately **two prior loads**. | Exact two-slot vs two-prior-plus-current semantics and what qualifies as a cell entry. |
| **Periodic autosave** | Autosave every **15 minutes** and allow it to serve as a previous-load recovery point. | Whether it shares the same rolling history with cell checkpoints, and which clocks/paused time count. |
| **Quicksave** | A **single** quicksave slot, overwritten by the next quicksave. | Conditions and permissions for creating/loading it. |
| **Full/manual save** | Standalone durable saves available for manual loading and retained unless explicitly deleted. | Limits, confirmation, branch histories, deletion authority. |
| **Recovery snapshot** | Separate special-purpose snapshot for an interrupted authored-world session, including mid-combat resume. **Must not overwrite the above saves.** | Format, retention/cleanup, exactly-once resume and abuse resistance. |

- **DECIDED:** Player-initiated save/load actions in a multiplayer session should be proposed by **any connected participant** and decided by **group vote**, not host fiat.
- **WORKING INTERPRETATION:** Periodic automated autosaves/checkpoints operate as safety mechanisms without interrupting players with a vote; the exact automation/vote policy is **OPEN** and should be ratified before implementation.
- **DECIDED:** A complete multiplayer save must keep the world and participating campaign-bound characters consistent on rollback; a world-only rewind with duplicated character items/XP is not acceptable.
- **OPEN:** Whether manual saves are shared globally across all eligible hosts and how a shared full-save history behaves with rotating hosts.
- **OPEN:** Snapshot consistency boundaries and how an authoritative rollback interacts with a participant who has played a different frontier session since the targeted save.

### 8.2 Waiting and group transitions

- **DECIDED:** The game includes a Skyrim-like **waiting/time advancement** mechanic.
- **DECIDED:** Any connected player can propose waiting or a party transition; the group votes.
- **DECIDED:** Waiting or a group transition is disallowed when **any relevant connected party member** is actively pursued or in combat with enemies/police. **OPEN:** Exact danger-state definitions (alert, wanted, nearby hostile, damage-over-time, etc.).
- **DECIDED:** Group transitions take the relevant group together, and health is preserved.
- **OPEN:** How waiting advances persistent NPC schedules/weather/events, what happens to offline members and frontier vs authored state, and how to avoid exploiting world events with repeated waiting.
- **OPEN:** Vote quorum, cooldown, rejection behavior, emergency overrides, and whether save/load votes are permitted in active combat.

### 8.3 Save-history continuity with portable hosting

- **REQUIRED DESIGN PROBLEM:** If any roster member may host on different days, there must be a way to validate the campaign's latest authoritative revision, synchronize complete world + character history, avoid silent overwrites, and detect forked histories.
- **RESEARCH OPTIONS:** Content-addressed revisions, immutable snapshot IDs, generation counters, signatures/checksums, save exchange over peer connections, replicated backup snapshots, and explicit conflict-resolution UX.
- **Do not claim** that local player-hosted sessions without always-online coordination can absolutely prevent two isolated users from producing divergent progress. Define safe detection and handling instead.

## 9. Unexpected disconnections and exact-state recovery

### 9.1 Authored world — DECIDED

1. If a registered player disconnects unexpectedly, **suspend the authoritative story-world simulation** and freeze the active session, including mid-encounter.
2. Start a **15-minute reconnection window**. The group remains suspended; the disconnected participant can rejoin their campaign-bound character.
3. If they reconnect in time, resynchronize everyone, present a brief restart countdown, and **resume from the frozen game state**.
4. If the window expires, preserve a distinct, consistent **recovery snapshot**, then return connected players to the multiplayer lobby.
5. Once the **entire registered roster** is present again, resume that campaign from the recovery snapshot at the **exact suspended authoritative logical moment**, even halfway through a boss fight—not from the encounter start by default.
6. A later option to **vote to restart the encounter instead** is a **FUTURE IDEA**, not a launch requirement.

**State fidelity requirement:** Preserve enough authoritative information to reconstruct player/entity transforms, health, incapacitation, active projectiles, attack timing and cooldowns, buffs/status, relevant physics state, NPC AI state, encounter phase, quest flags, random state or equivalent, pending events, and other gameplay-critical facts. Purely cosmetic transient effects may be reconstructed rather than byte-identically frozen, provided the observable play state is preserved. Exact serialization architecture is **OPEN**.

**Snapshot timing:** If possible, capture a consistent state at suspension and safeguard it; do not rely solely on the end-of-window write surviving crashes. This is an **engineering recommendation**, not an established implementation.

### 9.2 Frontier — DECIDED grace policy, pending details

- An unexpected disconnect receives approximately **10 minutes** of reconnection/reward eligibility protection.
- Remaining connected participants **may continue** their frontier gameplay; frontier does not need a full-roster pause.
- **OPEN:** What happens to the missing character's avatar and enemy scaling, how to prevent exploitably intentional disconnects, and when eligible rewards become claimed/queued.

### 9.3 Host loss — separate unsolved case

- **DECIDED baseline from previous discussion:** When the *active host* ends their session, other participants disconnect. The roster can reconvene later with a different eligible host if save transfer is implemented.
- An **unexpected host crash** cannot guarantee a snapshot created *after* the crash. Research ongoing durable snapshots, replication to peers, graceful host exit, and recovery from the most recent safely preserved authoritative state.
- Seamless live host migration is **not decided for launch**. Do not imply that rotating hosts between sessions solves a crash while the simulation is running.

## 10. Later expansions — not launch obligations

- Larger shared servers with unrelated players present in the same open world (Destiny-/GTA Online-like social presence) may be revisited after dedicated infrastructure becomes acceptable.
- PvP playlists such as team deathmatch, free-for-all, capture the flag, or other modes may come later.
- A public reusable-engine networking framework may support multiple authority models beyond the first RPG's listen-server preference.
- Sophisticated live host migration, asynchronous off-session voting, and encounter restart-by-vote are candidate enhancements, not currently committed launch features.

## 11. Priority open questions — do **not** silently settle these

| Priority | Decision needed | Why it affects architecture |
|---|---|---|
| **P0** | **Canonical save/host rotation:** How is the latest shared save distributed, authenticated, fork-detected, backed up, and chosen when multiple players host on different days? | Coherent campaign timeline, anti-duplication, rollback. |
| **P0** | **Save/character rewind:** Can a full group vote load an older save after members earned new frontier progression, and does it rewind every character? | Persistence invariants, possible conflicts. |
| **P0** | **Vote policy:** What quorum and approval method for major story, routine transitions/waits/saves, and administrative membership changes? | Democracy must work when players are unavailable or AFK. |
| **P0** | **Disconnected authored session:** What happens on host crash versus guest disconnect, and how to persist/reconstruct all critical mid-combat state? | Recovery technology, correctness. |
| **P1** | **Late joining:** Can a registered member join a frontier mission mid-progress, and when does eligibility begin? | Rewards and quest state. |
| **P1** | **Character slots:** One or many characters per member per campaign, switching timing, shared vs per-character achievements? | Ownership schema and reward rules. |
| **P1** | **Reputation/world events:** Which consequences need voting, and can one member kill a quest-critical NPC in a full-roster session outside dialogue? | Narrative agency and group governance. |
| **P1** | **Loot/economy:** World drops, chests, trading, economic resource transfer frontier→city, item reroll resistance. | Shared versus individual resources. |
| **P1** | **Encounter scaling:** Player count vs nearby active participants and level disparity, including disconnect grace. | Difficulty and progression balance. |
| **P1** | **Pause/wait/transition:** Combat checks, voting timeouts, full-roster disconnect behavior, in-flight/vehicle participants. | Session coordination. |
| **P2** | **Onboarding/access:** Invite/access controls, kicking, privacy, failed network joins, version/mod compatibility, player identity display. | Usable session experience. |
| **P2** | **Presentation:** Voting overlay UX, leaderboards/party UI, reconnection screen, countdown, save history UI. | Player clarity and fairness. |

**These are not blockers to documentation and architecture research.** They must become explicit design decisions before any implementation silently depends on their answers.

## 12. Superseded decisions — keep this history

| Former direction | Current status |
|---|---|
| Characters carry inventories/progression between unrelated multiplayer worlds | **SUPERSEDED:** Characters belong to a single world/campaign; single-player worlds separate. |
| Individual reputation, proximity weighting, reputation averages across friends | **SUPERSEDED:** One authored-world shared campaign reputation; frontier has separately persistent consequences without changing main-story reputation. |
| Only the host can determine major dialogue choices, save/load, wait, or transition | **SUPERSEDED:** Connected players propose; collective voting governs the action. Host merely executes the authoritative result. |
| Every registered roster member must be online for *any* multiplayer play | **SUPERSEDED:** Full roster required for authored world; any subset can use procedural frontier. |
| Only the original designated world host can ever host the campaign | **WORKING PREFERENCE replaced:** Investigate any registered member hosting a later session with the latest valid save. |
| Fixed roster cannot change when someone quits | **SUPERSEDED:** Group-governed retirement/replacement must be possible; voting details open. |
| Every member automatically receives rewards when any campaign member farms solo | **SUPERSEDED for frontier:** Only connected/eligible frontier players receive rewards; offline members get none. |
| Disconnected authored session restarts the fight or reloads an ordinary checkpoint | **SUPERSEDED:** Exact-state resumption from dedicated recovery snapshot is the default. |

## 13. Engineering handoff, examples and acceptance scenarios

**Research-first:** This specification describes player-visible behavior; it does **not** select a network protocol, sync API, physics stack, scripting language, file format, deterministic model, ECS, or backend. The original engine should provide reusable primitives where helpful; game-specific rules remain data/policy above them. Implementation has **not** been authorized.

**Illustrative checks for later requirements/test design:**

1. In a 4-member campaign, 3 can enter frontier but cannot enter authored city; when fourth joins, authored access becomes available.
2. In a 3-member campaign, all **3** suffice to enter the authored world.
3. Solo frontier exploration increases only connected participants' XP/money/loot; the absent roster member receives none; main-story reputation remains unchanged.
4. A mission ends while connected members stand in different regions; all eligible members receive equal quest XP/cash and individual equipment rolls.
5. A participant without hosting authority proposes a major dialogue decision, group transition, or player-initiated save; their proposal can be voted on and executed.
6. Transition fails if **any relevant participant** is pursued; a successful transition preserves health.
7. All but one participant are downed; the remaining participant can revive them. Full-party down triggers rewind to the appropriate earlier campaign save (priority remains open).
8. A player loses connection mid-boss fight. The authored world freezes; on reconnect within 15 minutes it resumes after countdown without an encounter reset.
9. The same incident exceeds 15 minutes; a separate recovery snapshot is created and the group returns to lobby. When all rejoin, the encounter resumes at the suspended logical state and the ordinary save slots remain intact.
10. A campaign revision hosted by one member can later be opened by another *only if* the state is complete and known-valid; contradictory revisions are not silently merged.
11. A randomized quest reward does not trivially change merely by reloading immediately before an ordinary reward hand-in (final mechanism open).
12. Existing code/prototypes are read-only evidence, not authority over any of the above design rules.

## 14. Required follow-up for GLM 5.3

1. Review for conflicts against `VISION.md` v0.5 and `CUSTOM-ENGINE-RESEARCH.md` v0.2. Those older files predate these multiplayer decisions.
2. Propose concise **cross-references and decision-ledger updates**, not parallel, inconsistent descriptions of multiplayer policy.
3. Mark the old portable-character/host-only/all-roster-for-everything rules **SUPERSEDED** wherever they survive.
4. Surface contradictions and P0 open decisions to the human lead; don't decide technologies or gameplay policies without approval.
5. Do not modify implementation, saves, code, assets, or prototype behavior until explicitly authorized.

---

*Source: project-lead decisions in the ongoing design discussions. Dates and classifications record the state of discussion, not independent technical validation. This file intentionally includes open questions so agents do not mistake assistant suggestions for adopted rules.*
