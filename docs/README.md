# Project Documentation — Working Title SEKTOR 7

> **Status:** Folder index and documentation governance v0.3 · 2026-10-19 · registers `TECH-ARCHITECTURE.md` (unapproved engine-consolidation proposal).
>
> **Authority and scope:** The human project lead makes final commitments. This folder records the vision, game mechanics, shared-campaign rules, original-engine research framework and prior prototype evidence. **This document set does not authorize implementing or modifying the game/engine.**
>
> **Provenance note (refreshed 2026-10-18):** This folder contains **eleven** Markdown files. The 2026-10-16 handoff package comprised seven files consolidated from four ChatGPT-attached sources (`VISION`, `DESIGN-BIBLE`, `MULTIPLAYER-SPEC`, `CUSTOM-ENGINE-RESEARCH`); the three unapproved research documents were added afterwards, and `TECH-ARCHITECTURE.md` — the engine-consolidation proposal for the architecture gate — was added 2026-10-19. The reconstructed `PROTOTYPE-INVENTORY.md` was reconciled on 2026-10-17 against the recovered first-hand original — every figure matched; its §7 preserves the commands and raw results — so the earlier "not a verified verbatim copy" warning is resolved. That reconciliation record and the git-stash original remain worth preserving (stash disposition pending the lead). Back up the repository `docs/` directory and compare any still-unprovided originals before destructive replacement.

## Recommended reading order

1. [VISION.md](VISION.md) — shared goals, the Twin Summits, original reusable engine decision, high-level launch commitments and collaboration agreement.
2. [DESIGN-BIBLE.md](DESIGN-BIBLE.md) — first RPG gameplay: cameras, systemic combat, traversal, classless progression, quests, NPC agency and unresolved solo mechanics.
3. [MULTIPLAYER-SPEC.md](MULTIPLAYER-SPEC.md) — world-bound shared campaigns, roster democracy, authored/frontier access, group reputation, rewards, saves, waiting, disconnect recovery and open edge cases.
4. [CUSTOM-ENGINE-RESEARCH.md](CUSTOM-ENGINE-RESEARCH.md) — subsystem research, benchmarks, phased experiments, technical questions and approval protocol.
5. [DECISIONS.md](DECISIONS.md) — canonical register of decisions and open questions; consult this before proposing changes.
6. [PROTOTYPE-INVENTORY.md](PROTOTYPE-INVENTORY.md) — Stage 0 Godot findings, reconstructed and reconciled 2026-10-17 against the recovered first-hand original (§7; all figures matched).
7. [MULTIPLAYER-ARCHITECTURE-RESEARCH.md](MULTIPLAYER-ARCHITECTURE-RESEARCH.md) — **unapproved research:** co-op authority, persistence, synchronization; architecture-E direction, durability tiers, E1–E5 experiments.
8. [E5-CANONICAL-MEDIUM-FEASIBILITY.md](E5-CANONICAL-MEDIUM-FEASIBILITY.md) — **unapproved E5 research annex:** canonical-medium feasibility, provider matrix, deployment alternatives, cost models, local harness results.
9. [SIMULATION-ARCHITECTURE-RESEARCH.md](SIMULATION-ARCHITECTURE-RESEARCH.md) — **unapproved research:** living-world simulation architecture, LOD transitions, persistent NPCs, chronology, S-experiments.
10. [TECH-ARCHITECTURE.md](TECH-ARCHITECTURE.md) — **unapproved consolidation proposal:** engine subsystems and boundaries, state ownership and the time contract, technology decision map (shortlists, no selections), and the Slice-0 vertical-slice plan.

## Ownership rules

| File | Authoritative for | Defers to |
|---|---|---|
| `VISION.md` | Product ambition, engine direction, launch-level commitments, anti-scope-creep / technical-honesty charter | Detailed rules and technology decisions |
| `DESIGN-BIBLE.md` | Gamewide and solo RPG mechanics | Multiplayer-specific policies |
| `MULTIPLAYER-SPEC.md` | Multiplayer player-facing rules, governance, rewards, saving, waiting and recovery | Technical persistence and network implementation |
| `CUSTOM-ENGINE-RESEARCH.md` | Research methodology, subsystem questions, staged experiments, evidence standards | Game-design policy and lead's decisions |
| `DECISIONS.md` | Status/decision index, change log, priorities | Full wording in owning docs |
| `MULTIPLAYER-ARCHITECTURE-RESEARCH.md` | Unapproved research: authority/persistence/synchronization architectures and experiments | MULTIPLAYER-SPEC policy, DECISIONS registry, lead approval |
| `E5-CANONICAL-MEDIUM-FEASIBILITY.md` | Unapproved E5 research annex: medium feasibility, deployment alternatives, cost models | MULTIPLAYER-ARCHITECTURE-RESEARCH, DECISIONS registry, lead approval |
| `SIMULATION-ARCHITECTURE-RESEARCH.md` | Unapproved research: living-world simulation architecture and experiments | DESIGN-BIBLE gameplay rules, MULTIPLAYER-SPEC policy, MULTIPLAYER-ARCHITECTURE-RESEARCH state taxonomy, DECISIONS registry, lead approval |
| `TECH-ARCHITECTURE.md` | Unapproved proposal: engine subsystems, boundaries, contracts; technology decision map; vertical-slice plan | VISION summits, research-study hypotheses, game-policy owners, DECISIONS registry, lead approval |
| `PROTOTYPE-INVENTORY.md` | Reported existing-experiment evidence and source limitations | Technical validation and new tests |
| `README.md` | Folder map, maintenance rules | All substantive owners above |

**Prevent contradictory copies:** A high-level file may summarize a decision in one sentence and link to its owner. It must not redefine the owner's thresholds or mechanisms. If a future subsystem gets a dedicated implementation specification, record its boundary explicitly. For example, `MULTIPLAYER-SPEC.md` owns what saves/waits mean to players; a possible future `SAVE-ARCHITECTURE.md` would own serialization, distributed snapshot replication and crash consistency.

## Shared status vocabulary

- **VISION COMMITMENT:** Committed destination or launch requirement.
- **DECIDED:** Explicitly accepted game-design or process requirement.
- **WORKING PREFERENCE:** Favored option, not yet a selected implementation.
- **WORKING HYPOTHESIS:** Technical idea requiring evidence.
- **PROPOSAL:** Suggestion not approved by the lead.
- **OPEN DECISION:** Question deliberately unresolved.
- **EXISTING EXPERIMENT:** Prior observation, audit or prototype evidence; not authority over the new design.
- **SUPERSEDED:** Earlier design replaced by a later human decision.

Examples and speculative implementation mechanisms must carry the correct tag. When an assistant recommendation seems logically necessary, still label it a research/technical proposal until the lead ratifies it. Date all meaningful changes and do not silently erase earlier decisions.

## Current hard boundaries

- **Original reusable engine selected as project direction**; exact engine technology remains open.
- **First game:** large open-world futuristic RPG with authored city and separate procedural frontier; both first- and third-person; highly systemic combat; classless and ultimately broad-mastery progression; major consequences and selected NPC mortality.
- **Launch multiplayer:** functional for 1–4 players, with separate solo saves and world-bound co-op characters, **no developer-funded dedicated servers**; player-hosted listen-server currently preferred. All registered members required for authored story; subsets may play frontier.
- **Persistence and recovery (multiplayer):** automatic progress-based persistence with shared-canonical-storage acknowledgment (no manual save slots or arbitrary-state reloads); encounter-failure transactional rollback; planned host handoff required; unexpected-disconnect suspension (15 min authored / ~10 min frontier grace); fail-closed authority.
- **Democratic governance:** Host has no extra story vote. Involuntary removal requires unanimity among the remaining registered members; permanent admission requires unanimity among existing members; deliberate time skipping requires unanimity of currently connected participants; major narrative decisions use ranked-choice voting; governance ballots may be cast asynchronously. Remaining ballot mechanics (counting method, ties, deadlines, absent-member collection) remain open.
- **Working process:** Windows first, Linux second. No specific hardware specs define the research ceiling; performance benchmarks are required. No gameplay/engine code work until the lead explicitly approves it.

## Inputs, audit transparency, and next handoff

The four provided sources were `VISION(3).md` (v0.5), `DESIGN-BIBLE(1).md` (v0.1), `MULTIPLAYER-SPEC(1).md` (v0.1), and `CUSTOM-ENGINE-RESEARCH(1).md` (v0.2). This package integrates the lead's subsequent explicit multiplayer roster decision and earlier conversation decisions not yet fully incorporated into those four files. The reconstructed `PROTOTYPE-INVENTORY.md` has since been reconciled against the recovered original (2026-10-17, §7 — all figures matched). Review any still-unprovided originals against this package if their contents differ.

**Recommended handoff to GLM:** Read all eleven files; compare with the repository backup; report discrepancies instead of automatically overwriting authorizations, implementations, licenses or unprovided evidence. Keep a clean diff, preserve any raw audit/test provenance, and obtain confirmation before executing any code modifications or Stage 1 research spikes.

**Suggested future files, only as work warrants:** `WORLD-DESIGN.md`, `PERFORMANCE-BENCHMARKS.md`, `TECHNOLOGY-RESEARCH.md`, `ENGINE-ROADMAP.md`, `ASSETS.md`, `PRODUCTION-PLAN.md`. The anticipated `SAVE-ARCHITECTURE.md` territory is covered by the unapproved research study `MULTIPLAYER-ARCHITECTURE-RESEARCH.md` while under review. Do not create empty scaffolds simply to increase document count.
