# MULTIPLAYER SPECIFICATION — Shared Campaigns

> **Status:** Multiplayer gameplay specification v0.2 · 2026-10-16 · Consolidated after the shared-campaign and governance discussions. Game/engine implementation is **not authorized**.
>
> **Ownership:** This file defines **the RPG's multiplayer gameplay policies**: campaign roster and access, host neutrality, votes, shared reputation, reward eligibility, saves/checkpoints, waiting/transitions, incapacitation, disconnect grace and session recovery. [`VISION.md`](VISION.md) defines the ambition; [`DESIGN-BIBLE.md`](DESIGN-BIBLE.md) owns general/solo mechanics; [`CUSTOM-ENGINE-RESEARCH.md`](CUSTOM-ENGINE-RESEARCH.md) owns technical choices; [`DECISIONS.md`](DECISIONS.md) is the decision registry. A future technical `SAVE-ARCHITECTURE.md` may define snapshot implementation without changing player-facing policies here.
>
> **Status rules:** Use [`VISION.md`](VISION.md)'s vocabulary. No prior assistant proposal or example is automatically a project decision.

## 0. At a glance — the experience we are building

- **DECIDED:** The game ships with fully playable **solo** and **1–4 player cooperative** experiences. Multiplayer cannot be deferred until after launch.
- **DECIDED:** The main menu separates **Single-player** and **Multiplayer**. Solo-world/character saves and multiplayer campaigns are separate, not portable between unrelated worlds.
- **DECIDED:** Multiplayer campaigns have a **registered roster** of up to four participants; characters are **bound to that campaign**. Membership and consequential campaign decisions are collectively governed, not owned politically by the current host.
- **DECIDED:** The **authored/story world** is available only with the campaign's **entire registered roster online**. Its consequential story decisions are made democratically.
- **DECIDED:** The **procedural frontier** is available to **any nonempty subset** of registered participants, including one person, for grinding, exploration, and independent advancement.
- **DECIDED:** Frontier consequences persist **within the frontier** but do not independently rewrite the authored world's **major reputation or narrative state**.
- **DECIDED:** The authored world has **one shared campaign reputation**, affected by actions of any participating player, not individually averaged reputation scores.
- **DECIDED:** Quest XP and currency are granted **equally to connected, eligible group members**, irrespective of their physical location or direct participation. Quest equipment rewards are rolled **per eligible character**. In frontier sessions, **offline members receive nothing**.
- **DECIDED:** Connected players can propose group waiting, saving/loading, and transitions for a vote; the host gets **no special voting authority**. Permanent involuntary roster removal requires unanimous approval of the **remaining registered members**, excluding the member proposed for removal. The departing member may retire voluntarily.
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
- **PROPOSED technical requirement (not yet explicitly ratified):** Roster membership and character ownership should use a stable player identity that does not depend on a mutable display name or character name. **OPEN implementation:** Account provider, offline identities, authentication, mapping to platform IDs, privacy and recovery. Steam account IDs are a research possibility, not a hardcoded engine assumption.

### 2.2 Democratic campaign membership — DECIDED

- **DECIDED:** The campaign has a registered roster of up to four participants. Membership is collectively governed; merely operating the host computer confers no unilateral permanent membership authority.
- **DECIDED:** **Involuntary permanent removal requires unanimous approval of all *other* registered members** (the removal target is excluded from that vote). If any eligible remaining member rejects the removal, it fails. The exact implementation of absent-member ballot collection remains **OPEN**; absence is not implicit consent.
- **DECIDED:** A participant may **voluntarily retire** from the campaign; their departure must not permanently lock everyone else out of the authored-world campaign. Exact retirement confirmation, archive and rejoin procedure remains **OPEN**.
- **DECIDED:** Permanent roster lock-in is **SUPERSEDED**. The roster size after retirement governs the future full-roster requirement.
- **DECIDED principle:** The group governs admissions/replacements too. **OPEN:** Whether adding/replacing someone needs unanimity, a different quorum, and what happens to archived characters. Do not silently borrow the removal threshold for admissions.
- **PROPOSAL:** Archive retired characters without permitting a replacement to appropriate their character identity/progression; replacements ordinarily create a campaign-bound character. Reinstatement and slot rules remain **OPEN**.
- **OPEN:** Immediate host powers to kick a disruptive person from the current *session* (distinct from permanent removal); invitations, bans, absences/AFK, unusual 1-member roster cases and voter authentication.

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
- **DECIDED:** Ordinary **emergent gameplay actions remain individually actionable**, including attacking or killing normally killable quest-important NPCs without first triggering a formal party vote. Resulting consequences affect the shared authored campaign. Formal major branching dialogue and designated story decisions still require voting. A small number of protected NPC exceptions remains permitted.
- **OPEN:** Which actions are classified as formal story decisions rather than ordinary emergent play, and how players receive clear warnings before irreversible consequences.

### 3.2 Procedural frontier — DECIDED

- Any **1 to N** currently available roster members (up to campaign roster size, maximum 4) can host/join a frontier session without the absent players.
- Allows exploration, farming, repeatable procedural missions, obtaining equipment/resources, and leveling characters.
- Frontier discoveries and consequences remain persistent within that environment and campaign.
- Frontier actions **must not independently rewrite the handcrafted world's major shared reputation or storyline**. This is a *boundary of authority*, not a declaration that frontier play has no lasting significance.
- Existing singular shared reputation for the authored campaign remains; a separate numeric frontier reputation is **not decided**.
- **DECIDED:** Frontier-earned **character XP/skills/perks, money, equipment and inventory** follow eligible world-bound characters into authored-world play. Frontier local discoveries and consequences persist in the frontier but do not independently change the major authored story or shared reputation.
- **OPEN:** Whether frontier-generated minor economic changes beyond player-owned money/items affect the authored world’s wider economy, and how to ensure no transaction bypasses protected narrative outcomes.

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
- **DECIDED:** Individual emergent acts can change the shared authored reputation and world even when other connected companions disagree; those ordinary acts are not pre-voted.
- **OPEN:** Crime attribution, legal/wanted responses, warnings, and the boundary between ordinary emergent actions and major formal voting decisions.

## 5. Group democracy: voting and authority

### 5.1 Core rules — DECIDED

- Any connected roster participant can **propose** a vote for player-initiated saving/loading, waiting, or a group transition.
- Important **branching narrative decisions** are resolved through a group vote rather than decided unilaterally by the active host.
- Host runs authoritative validation/execution of the chosen action, but **hosting does not grant privileged voting power**.
- Permanent roster governance is democratic (§2.2); involuntary removal requires unanimity among remaining registered members, and voluntary retirement is available. Admissions/replacements remain governed by the group with their exact quorum **OPEN**.
- **OPEN:** Voting eligibility during temporary disconnects, how many connected members must vote for routine actions, and quorum/threshold for formal story choices and roster admissions. **Not open:** Involuntary removal requires the unanimous votes of remaining registered members; neither the target nor the host gets special voting privilege.

### 5.2 Voting mechanics — unselected

- **PROPOSAL from lead:** First-choice and second-choice preferences (a ranked ballot) for multi-option dialogue; on a deadlock, transparently choose at random among tied choices (up to four, at most 25% per choice in a four-way equal tie).
- **OPEN:** Exact ranked-choice counting method, ties, revotes, abstention, AFK behavior, timeout, voting notification, initiator cooldown and emergency overrides. Membership removal unanimity is already decided; do not treat that particular threshold as open.
- **PROPOSAL:** Random tiebreaks for *story choices* could be acceptable, but must **not** decide catastrophic administrative actions such as world deletion or irreversible save-history restoration.
- **OPEN:** Whether votes on major authored-world decisions always require all registered members to *vote*, versus only to be present. Full-roster presence for authored-world access is decided; a specific ballot threshold is not.
- **DECIDED principle:** Ordinary player-initiated gameplay and emergent acts are individually actionable; votes apply to significant formal narrative branches and shared campaign operations, not every shopkeeper exchange. The exact list of vote-gated quest decisions remains **OPEN**.

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

> **Document ownership:** This section is the authoritative *gameplay policy* for group saves/waiting at present. Exact serialization, storage, snapshot atomicity, replication, file formats, and backup technology belong in future technical architecture research. **Do not create a competing player-facing save-policy document.** A future dedicated technical `SAVE-ARCHITECTURE.md` may own serialization, snapshots, integrity and distributed persistence; it must reference rather than override these gameplay policies. If this policy section grows unwieldy, move it explicitly and update the links.

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
| **P0** | **Vote policy:** What quorum/method for major story, routine transitions/waits/saves and *admissions*? Involuntary removal unanimity is already decided; voluntary retirement is allowed. | Democracy must work when players are unavailable or AFK. |
| **P0** | **Disconnected authored session:** What happens on host crash versus guest disconnect, and how to persist/reconstruct all critical mid-combat state? | Recovery technology, correctness. |
| **P1** | **Late joining:** Can a registered member join a frontier mission mid-progress, and when does eligibility begin? | Rewards and quest state. |
| **P1** | **Character slots:** One or many characters per member per campaign, switching timing, shared vs per-character achievements? | Ownership schema and reward rules. |
| **P1** | **Reputation/world events:** How are ordinary emergent actions distinguished from vote-gated *formal* story choices? Individual attacks on killable quest NPCs are permitted. | Narrative agency and group governance. |
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
| Fixed roster cannot change when someone quits | **SUPERSEDED:** Voluntary retirement allowed; involuntary removal requires unanimous approval of other registered members. Admission/replacement threshold open. |
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

## 14. Maintainer and AI-agent handoff

- Use this document as the source of truth for multiplayer gameplay behavior and keep cross-document summaries brief. See [`DECISIONS.md`](DECISIONS.md) for the canonical registry.
- Do not reopen ratified positions—including the full-roster authored-world rule, frontier partial-roster play, shared reputation, 15-minute exact-state authored recovery, ten-minute frontier grace, and unanimous remaining-member removal—without the project lead's direction.
- Important open research: save distribution and conflict detection for rotating hosts, snapshot/rewind correctness, host-crash recovery, vote details, and session security. Do not invent working solutions or silently select a networking technology.
- Design/test proposals must distinguish game-specific policy from reusable engine primitives. No code/saves/assets modifications are authorized by this document.

---

*Source: project-lead decisions in the ongoing design discussions. Dates and classifications record the state of discussion, not independent technical validation. This file intentionally includes open questions so agents do not mistake assistant suggestions for adopted rules.*
