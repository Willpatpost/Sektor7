# SIMULATION-ARCHITECTURE-RESEARCH — Living-World Simulation for the Original Engine

> **Status:** Research study v0.6 · 2026-10-19 · **UNAPPROVED RESEARCH — the tiered hybrid is the leading simulation architecture hypothesis, not a selected implementation** (three-way review, 2026-10-18). v0.2 incorporated the review corrections: live authoritative state composition (committed + pending + provisional — §3) with asynchronous commit acknowledgment; deterministic replay scoped to represented events vs. labeled plausibility synthesis (§4, §6); memory–reputation decoupling (§5, I8); four-lane event classification with held-event revalidation (§6); predictable-scaling S7 acceptance (§10); project-evidence claims corrected (§3). v0.3 qualifies the Mara example (reputation impact only upon classification; durable deltas are pending until acknowledged) and records the authorized S1/S6-lite experiment (run 2026-10-18 — `experiments/sim-lod-local/`; local harness evidence, not engine implementation). v0.4 records the authorized S4-lite experiment (run 2026-10-18 — `experiments/sim-lod-local/harness_s4.py`; 8/8 checks, fingerprint `bb85bc46a12f4490`) and the lead's durability caveat: the host-loss simulations in these harnesses do **not** establish durability of local emergency recovery copies — that remains a separate persistence concern. v0.5 records the authorized S3-lite experiment (run 2026-10-18 — `experiments/sim-lod-local/harness_s3.py`; all checks passed, fingerprint `d4a58130cdf3f7ff`): long-dormant reactivation under true dormancy, with the replay/synthesis crossover measured in event work (~7.6–12.4 pending events per NPC, density-dependent), never elapsed time; no launch policy selected. v0.6 records the authorized S2-lite experiment (run 2026-10-19 — `experiments/sim-lod-local/harness_s2.py`; all checks passed, fingerprint `f7fce3ee1d7835d1`): event-queue stress under the lead's CORRECTED acceptance criteria — an always-bounded backlog under arbitrary sustained overload is not required or claimed; growth is predictable and measured instead — and supersedes the §10 S2 acceptance row accordingly. No engine simulation code exists or is authorized; no ECS framework, programming language, storage format, or engine backend is selected; no ratified requirement is changed by this file.
>
> **Ownership:** Research and analysis of the engine's **living-world simulation subsystem** — simulation models, level-of-detail (LOD), persistent-NPC state, shared chronology, scheduling, streaming/multiplayer interaction, and experiments. It **defers** to: [`DESIGN-BIBLE.md`](DESIGN-BIBLE.md) (gameplay rules — encounters, NPC mortality, quest systems), [`MULTIPLAYER-SPEC.md`](MULTIPLAYER-SPEC.md) (player-facing multiplayer policy), [`MULTIPLAYER-ARCHITECTURE-RESEARCH.md`](MULTIPLAYER-ARCHITECTURE-RESEARCH.md) (the canonical state taxonomy, commit pipeline, and session authority — consumed here, never redefined), [`CUSTOM-ENGINE-RESEARCH.md`](CUSTOM-ENGINE-RESEARCH.md) (research methodology; this study expands its domain 4), and [`DECISIONS.md`](DECISIONS.md) (the registry — nothing here is ratified).
>
> **Inputs:** the lead's 2026-10-17 research brief (project context and seven research questions) plus the current VISION, DESIGN-BIBLE, MULTIPLAYER-SPEC, DECISIONS, MULTIPLAYER-ARCHITECTURE-RESEARCH, and E5 annex.
>
> **Evidence key:** **[ESTABLISHED-CLASS]** = technique class documented in public game-development or simulation literature (class-level citations only — no claim about any specific title's internals); **[PROJECT EVIDENCE]** = demonstrated in this repository; **[HYPOTHESIS]** = this study's unproven composition; **[ASSUMPTION]** = labeled workload/budget guess pending measurement; **[OPEN]** = unresolved product decision for the lead.

---

## 0. Scope and ownership-map compliance

This study was created after checking the documentation map (VISION §11 / README). No existing document owns simulation architecture research; this file takes that territory as an unapproved research sibling to MULTIPLAYER-ARCHITECTURE-RESEARCH, and registers itself in both maps. It does **not** create a TECH-ARCHITECTURE.md (still future), does not touch gameplay rules (DESIGN-BIBLE), does not touch player-facing policy (MULTIPLAYER-SPEC), and does not modify the persistence/commit/authority model (MULTIPLAYER-ARCHITECTURE-RESEARCH) — it *consumes* that model as the substrate beneath the simulation.

## 1. Project context and requirement traceability

The first game: a visually ambitious persistent open-world RPG — handcrafted city with detailed interiors, NPC schedules, traffic, factions, relationships, encounters, persistent consequences; a procedural frontier; a central hub and one shared world clock; 1–4 co-op players who may occupy distant frontier locations simultaneously; persistent NPC identities including significant permanent deaths; first- and third-person play, advanced traversal, eventual unrestricted personal flight; a **reusable engine** for future games. Both summits stand at full strength: **graphics comparable to leading modern AAA** and **extraordinary computational efficiency** (RCT lineage). Nothing here reduces either; the entire purpose of this architecture is to *afford* both at once.

Ratified anchors this study must respect (never modify):

| Requirement | Consequence for simulation |
|---|---|
| Hundreds of persistent NPCs, always-simulating world (V-03) | Cost must scale with *meaningful change*, not entity count × frame rate |
| One shared campaign clock, central hub (M-27) | A single monotonic time authority beneath all regions |
| One authoritative session; host runs the sim (M-28) | Simulation is single-writer on the host; guests replicate — no co-simulation |
| Progress-based persistence, canonical acknowledgment, encounter transactions (M-29/M-30) | Simulation durable deltas ride the existing commit pipeline; encounter-local state is provisional |
| Frontier consequences must not rewrite authored-city major narrative state (M-07) | **Narrative gating** under clock advance (§6) |
| Shared campaign reputation; NPC-specific memories allowed (M-08, SPEC §4) | NPC memory model must not create a second reputation; reputation-relevant events feed the single shared score (§5, I8) |
| Emergent acts individually actionable; most NPCs killable (M-13, G-08) | Deaths are permanent identity-level events with world consequences |
| Deliberate time-skip = unanimous connected vote (M-34) | Time-skip is an event-queue fast-forward, not a special code path |
| Deterministic/data-oriented/event-driven simulation is E-03 **WORKING HYPOTHESIS** | Determinism is assumed *useful, unproven* — S5 must test it |

## 2. Executive summary

**Recommended research direction [HYPOTHESIS]: a tiered hybrid simulation.** A single **event-driven spine** owns the world clock and all time advancement (the RCT trick: sleeping or far-away things cost nothing until something happens). **Data-oriented batch systems** own continuous near-field simulation (movement, combat-adjacent state, crowds) where cache-friendly throughput is the efficiency summit. **Cognitive facades** — rich per-NPC behavior objects for authored/story NPCs — sit *on top of* the data layer as readers/writers, never as the storage substrate. All durable state lives in a **canonical persistent store** (the multiplayer study's model); every active representation (calendar, components, facade) is a **rebuildable projection** of that store. The host's **live authoritative state** additionally contains pending/unconfirmed deltas and provisional encounter state (§3); LOD transitions capture durable deltas locally and never wait on remote commits. The two hazards this architecture must prove safe are **LOD transitions** (no visible pop, no lost history, no implausible behavior — §4) and **shared-clock narrative gating** (frontier time cannot fire authored-city irreversible events — §6).

## 3. Simulation model comparison (Q1) — and how the models coexist

| Model | What it is best at | Costs | Where it fits here |
|---|---|---|---|
| **Data-oriented ECS** (entities as IDs; components in packed arrays; systems as batch transforms) | Massive throughput on uniform updates — positions, needs, movement, schedule execution; cache locality; predictable memory | Cross-entity relationships (faction↔member graphs) need joins/indices; rich heterogeneous behavior fits poorly in raw arrays; indirection discipline required | **Near-field and mid-field crowd/citizen simulation** — the "hundreds of NPCs" summit. [ESTABLISHED-CLASS] (data-oriented design / ECS literature, e.g., the Overwatch ECS GDC talk as a class exemplar) |
| **Actor / object-oriented** (each NPC an object with behavior and state) | Encapsulated rich behavior; natural for unique authored NPCs; easy heterogeneity | Pointer-chasing, per-object overhead, poor batching — the opposite of the efficiency summit at crowd scale | **Cognitive facades for authored/story NPCs only** — a behavior layer over the data substrate, not the storage |
| **Event-driven / discrete-event** (timestamped event queue; entities wake when events fire) | Dormant cost ≈ zero; natural fit for schedules ("wake 07:00"); **time-skip cost proportional to events, not elapsed time**; deterministic replay | Continuous motion/animation doesn't fit events; event-storm control needed; strict ordering discipline | **The spine: far-field and dormant regions, schedule triggers, economy ticks, world events, and all time advancement** [ESTABLISHED-CLASS] (classic discrete-event simulation) |
| **Pure hybrids of the above as peers** | Flexibility | Without a fixed layering rule, hybrids decay into "everything everywhere" — the failure this architecture must prevent | Not used as peers — used as **layers** (below) |

**How they coexist [HYPOTHESIS]: layering by role, not by entity.** The critical architectural decision is separating **canonical state** from **active representations**:

```text
 Authored content & game policy (DESIGN-BIBLE / MULTIPLAYER-SPEC — owned elsewhere)
─────────────────────────────────────────────────────────────────────────────
 Engine simulation layer [HYPOTHESIS]
 ┌────────────────────────────────────────────────────────────────┐
 │ WORLD CLOCK & EVENT SPINE — one monotonic campaign clock (M-27)│
 │ timestamped event queue; per-tick budgets; narrative gates    │
 └───────┬────────────────────────────────────────────┬───────────┘
         │ drives region cadences                      │ gates irreversible
 ┌───────▼───────────────┐                  ┌──────────▼───────────┐
 │ REGION LOD DIRECTOR   │                  │ NARRATIVE GATE KEEPER│
 │ presence + velocity   │                  │ authored-city major  │
 │ lookahead, hysteresis │                  │ events hold during   │
 └───────┬───────────────┘                  │ frontier-only time   │
         │                                  │ (M-07/M-29)          │
   ┌─────┼──────────────────┐               └──────────┬───────────┘
   ▼     ▼                  ▼                          │
 DORMANT  EVENT-DRIVEN   SIMPLIFIED      FULL DETAIL    │
 store +  calendar of    coarse          ECS batches +  │
 next-evt commitments    components      physics +      │
         (far)           (mid)           facades (near) │
   └─────┴──────┬───────────────┬──────────────┘       │
               │ activation ⇄ deactivation (§4)         │
               ▼                                     held events
 ┌───────────────────────────────────────────────┐  presented on
 │ CANONICAL PERSISTENT STATE STORE              │◄─city re-entry
 │ NPC records · world state · durable deltas    │
 │ via the commit pipeline (M-29/M-30 — owned by │
 │ MULTIPLAYER-ARCHITECTURE-RESEARCH, consumed)  │
 └───────────────────────────────────────────────┘
```

**Proposed interfaces (working labels [HYPOTHESIS]; contracts, not code):**

| Interface | Contract sketch |
|---|---|
| `Clock.advance(dt)` | Single monotonic campaign time; no other component may mutate time |
| `Spine.schedule(at, event, order_key)` | Deterministic event ordering by `(timestamp, order_key)` |
| `RegionDirector.set_presence(players)` | Inputs player positions + velocity lookahead; outputs per-region target LOD with hysteresis |
| `Region.activate()` / `Region.deactivate()` | LOD transitions per §4 contracts |
| `NpcStore.get(id)` / `.commit_durable(deltas)` | Read canonical record; durable deltas ride the existing commit pipeline (never bypassed) |
| `SystemTick.run(region, lod, budget)` | Batch system execution with per-tick time budget; budget overruns are *measured*, never silently dropped |
| `GateKeeper.evaluate(event)` | Returns fire / hold per the four-lane classification (§6), based on session scope |
| `GateKeeper.revalidate(held_event)` | On gate opening: re-check preconditions, trigger context, and classification before presentation — never blind-fire |

**Live authoritative state composition [HYPOTHESIS]:** the host's live authoritative state is the canonical persistent store's **committed data** (last acknowledged revisions, T3) **plus pending/unconfirmed durable deltas** (locally captured, idempotently keyed, awaiting canonical acknowledgment — T1/T2) **plus provisional encounter-transaction state** (the M-30 lane) **plus temporary simulation state** (never restored across terminated sessions). All active representations are projections of this live state. **Safe activation/deactivation without synchronous remote commits:** at deactivation, every durable delta is *captured into the commit pipeline* — a local, idempotent append — and only then is the representation discarded; canonical acknowledgment proceeds asynchronously and **never blocks an LOD transition**. **Pending changes survive LOD deactivation in the active authoritative session:** captured deltas live in the pipeline's local record, not in the discarded representation, and are re-read at reactivation — deactivation never loses a still-unacknowledged change while the session lives. They remain **authoritative-live but not canonical**: pending deltas may shape subsequent simulation, yet they are never misrepresented as T3-confirmed progress (M-29/M-35); if the host is lost, unacknowledged pending work may be lost with it — the durability tier the ratified model already assigns to it. **Reactivation applies each pending change exactly once:** every delta carries an idempotency key, and re-encountering a key already reflected in the live state is a no-op (the applied-key ledger discipline consumed from MULTIPLAYER-ARCHITECTURE-RESEARCH §3). Consequences are never lost *silently*: capture is local and synchronous; confirmation is remote and asynchronous.

**[PROJECT EVIDENCE — scope corrected 2026-10-18]:** the godot prototype offers supporting evidence at the *rendering-submission and world-generation* level only. MultiMesh batching coalesced 1,265 prop instances into batched draw submissions — the actual draw-call count was **not measured** (PROTOTYPE-INVENTORY §3 lists draw calls as needing proof). Deterministic seeded **world generation** (seed 1337) with count-asserting tests demonstrates reproducible *generation*, not deterministic **NPC simulation** (E-03 remains unproven) and not data-oriented agent throughput. Neither demonstrates the tiered architecture; both are supporting evidence within their measured scope.

## 4. Simulation level-of-detail and the transition model (Q2, Deliverable 4)

Four levels (the lead's taxonomy, formalized):

| Level | Representation | Runs when | Cost shape |
|---|---|---|---|
| **Full** | ECS components + physics + animation-adjacent state + cognitive facades | Players present in the region | Per-tick, budgeted; scales with entities *in region* |
| **Simplified** | Coarse components: schedule-space trajectory (between commitment points), decaying needs at coarse rates, statistical movement | Region near players but not observed in detail (or mid-distance) | ~1/10–1/100 of full [ASSUMPTION — S1 measures] |
| **Event-driven** | Calendar of upcoming commitments only; NPCs exist as records + scheduled wake-ups | Far field; city districts with no players | Cost per *event*, not per NPC per tick |
| **Dormant** | Canonical record + `next_event` timestamp; nothing scheduled sooner | Long-unvisited regions; interiors never entered | ≈ storage only |

**Activation (toward full) — the transition contract [HYPOTHESIS]:**
1. **Durable base:** load the **live authoritative record** — last committed state **plus captured-but-unacknowledged pending deltas** (§3), plus any active encounter-transaction state if the region is within one. Pending deltas re-apply exactly once via their idempotency keys.
2. **Catch-up:** fast-forward to *now* by **deterministic replay of the entity's represented history** — recorded events, schedule commitments, and committed deltas from the last committed point. Replay reproduces only what was **actually represented**; it does not invent the micro-events of dormant or simplified spans. This is where determinism pays: the recorded event history replays exactly.
3. **Bounded catch-up [ASSUMPTION]:** if the dormancy window exceeds a replay budget, fall back to **plausibility synthesis** — derive a coherent present state from schedule + travel times + variability seeds ("where *would* Mara be at 14:37?"), and mark the entity's micro-history as synthesized. **Synthesized spans are labeled and never claimed as exact historical record** — exactness is claimed only where a complete recorded event history exists (see §6's determinism scope).
4. **Stitch:** place full-detail state consistent with the coarse trajectory that any simplified-level observer saw (no visible pop); hand off to facades/batches.

**Deactivation (toward dormant):** **capture** all durable deltas into the commit pipeline (I1 below — local idempotent append; canonical acknowledgment is asynchronous and never blocks the transition), stamp `next_event`, discard the representation — it is rebuildable by construction, **and its pending deltas survive in the pipeline's local record for the next activation** (§3).

**The three named risks and their mitigations:**

| Risk | Mitigation |
|---|---|
| Visible inconsistencies (pop, teleporting NPCs) | Stitch step (4); simplified-level trajectories are *constraints* the activated state must satisfy; hysteresis bands prevent LOD thrashing at boundaries |
| Lost history | Only reconstructible state is discarded; everything durable was captured into the commit pipeline before deactivation (auditable invariant I1; acknowledgment is asynchronous) |
| Implausible behavior (NPC frozen in last week's task) | Catch-up replay or plausibility synthesis *always* advances the entity to the current clock; an activation that cannot produce a coherent present state fails closed to a fallback anchor (home/last known location) rather than presenting stale state |

**Transition correctness is the load-bearing claim of this whole study** — S6 (§10) exists to fuzz it.

## 5. Persistent NPCs — conceptual model and lifecycle (Q3, Deliverable 3)

**Conceptual record** (a *conceptual schema* — explicitly **not** a storage format, which remains unselected):

| Facet | Contents | Persist? |
|---|---|---|
| **Identity** | Stable NPC ID (never reused — permanent deaths, M-13/G-08); name; appearance seed; home; faction memberships | **Always** |
| **Schedule** | Recurring + one-off commitments (work, sleep, travel, leisure) with variability seeds | **Always** (it is authored/durable content + committed one-offs) |
| **Relationships** | Typed, weighted edges to NPCs/factions/players + their history | **Always** (world consequences) |
| **Episodic memory** | Witnessed events, player interactions, crimes seen — the inputs to NPC-specific memories and behavior (SPEC §4). **Retention depth is [OPEN]** (§11.3). Not every personal memory must alter the shared reputation: only events classified as **reputation-relevant** feed the single shared campaign reputation (M-08); which classes qualify is game policy **[OPEN]**. NPC-local memories shape dialogue/behavior without creating a second reputation score | **Always** (this is the "NPCs remember you" surface) |
| **Personal state** | Health/injury, possessions, economy position | **Durable when consequential** (injuries, ownership); routine vitals reconstructible |
| **Location** | Last **committed** location | Committed location persists; micro-movement is reconstructible |
| **Routine state** | Current task, hunger, mood, micro-position | **Reconstructible** — derived from schedule + clock on activation |
| **Combat/encounter micro-state** | AI targets, cooldowns, animation phase | **Provisional/temporary** per the M-30 encounter-transaction taxonomy — never persists |

**Rule of thumb (ties the two studies together):** state that shapes *future behavior, dialogue, or world consequences* persists through the commit pipeline; state that only shapes the *next few seconds of motion* is reconstructed. This is the simulation-side reading of M-29/M-30's two-lane model.

**Lifecycle example (Deliverable 3) — "Mara, dockworker":**

| Stage | Simulation level | What happens | What persists |
|---|---|---|---|
| Creation | Authored seed | Identity, home, schedule, faction (dockworkers), 3 relationships seeded | Full record committed |
| A quiet week | Dormant/event | Calendar advances; she wakes only for scheduled commitments (≈ tens of events/week [ASSUMPTION]) | One-offs captured (pending until acknowledged); routine state derived on demand |
| Player enters her district | Full | Activation: catch-up replay to now; she's hauling crates on the pier | Nothing new yet |
| Player steals from her employer **in her view** | Full | Encounter: witnessing → episodic memory entry, relationship delta to the player, a **reputation candidate** recorded for classification (it contributes to the shared campaign reputation, M-08, only if classified reputation-relevant — policy open, §5/I8), faction consequence flag | All four captured as durable deltas — **pending until acknowledged**, not yet confirmed (§3) |
| Players leave | Full → dormant | Facade discarded; deltas captured into the pipeline (pending until acknowledged); `next_event` = end of shift | The memory and reputation changes *must* survive (I1 — pending-live through deactivation, never lost) |
| Next visit, days later | Activation | Catch-up: her schedule adapted (avoidance behavior from the memory), tells dockside rumors referencing the theft | New one-off schedule entries captured (pending until acknowledged) |
| A rival kills her (emergent, M-13) | Full | Death: identity tombstoned — **permanent**; schedule cancelled; relationships produce grief/consequences in others; quests adapt per DESIGN-BIBLE §6 succession patterns | Tombstone + all consequence records committed; she is never reused (I6) |

## 6. Shared chronology (Q4)

**One clock.** The spine owns a single monotonic campaign clock (M-27). Regions advance their *simulations* at whatever LOD their player presence dictates; the clock itself is global and never branches. [HYPOTHESIS]

**Players in different regions:** each region's LOD follows *its* observers; the clock is shared, so a frontier player's 20:00 is the city's 20:00. Cross-region consistency is anchored by the shared clock + canonical store, not by region-local time.

**Clock advance during frontier-only activity (the ratified boundary, M-07/M-29):** while only frontier sessions are active, the authored city runs in **event/simplified mode**. The Gate Keeper classifies city-side activity into four distinct lanes **[HYPOTHESIS]**:

1. **Story milestone gates** — irreversible authored narrative milestones. **Held** (they do not fire, expire, or silently resolve) until a full-roster authored-city session (or a governance action) opens the gate.
2. **Formal voting requirements** — major branching decisions resolved by ranked-choice vote with all registered members participating (M-33). A governance lane, not a simulation gate: the simulation presents the decision point; the ballot resolves it.
3. **Ordinary reversible routines** — schedules ticking, weather, ambient economy drift within bands **[OPEN — see §11]**. These run freely during frontier-only epochs.
4. **Emergent irreversible NPC/world consequences** — permanent NPC deaths, relationship ruptures, property consequences arising from ordinary emergent play (M-13). Within a legitimate session they commit immediately through the durable pipeline and are **not** vote-gated; authored-city-side irreversible consequences of this kind are held with the story milestones during frontier-only epochs rather than silently firing.

**Held events are revalidated before execution, never blindly fired on city re-entry:** when the gate opens, each held event's preconditions, trigger context, and classification are re-checked against the current world state (and governance where applicable) before it presents itself — the world delivers what was pending, after confirming it is still coherent to deliver. This is the mechanism that makes "frontier time must not independently trigger major authored-city narrative events" enforceable rather than aspirational. [HYPOTHESIS]

**Deliberate time-skip (M-34, unanimous connected):** a skip is a **fast-forward of the event queue** — cost proportional to scheduled events, not elapsed time (the event-driven spine's core payoff). The same narrative gates apply: gated authored-city events hold through skips; frontier and routine events process. Catch-up on the next activation uses the same replay machinery as §4. [HYPOTHESIS]

**Determinism (E-03, unproven):** same seed + same inputs (player actions) ⇒ same evolution of **represented** state. This makes catch-up replay faithful for the recorded event/schedule history and would ease testing/replication; it does **not** make plausibility-synthesized micro-history exact — synthesized spans are labeled, never claimed as historical equivalence (§4). S5 tests it; the cross-platform floating-point caveat is flagged in §7.

## 7. Scheduling, parallelism, and determinism (Q5)

- **Fixed-step core tick** for gameplay-relevant simulation (determinism, replay, co-op consistency); render rate decoupled. **[ESTABLISHED-CLASS]**; cadence value [ASSUMPTION — S1].
- **Variable cadences by LOD:** full = every core tick; simplified = 1/10–1/100 ticks; event = on queue; dormant = never. The spine triggers each region's systems at its cadence — systems don't self-schedule.
- **Event queue discipline:** priority queue by `(timestamp, order_key)`; **per-tick event budgets** with measured backlog (event storms must degrade to *deferred*, never to dropped consequences); near-field budgets always reserved (combat never starves behind far-field economy ticks).
- **Parallelism:** batch systems run in parallel over component chunks with **component-ownership discipline** (each system declares the components it writes — no two systems write the same component in one phase) and a **staged read → compute → write** pipeline; cross-entity interactions (relationship updates, combat targeting) resolve in ordered phases or via deterministic merge on `(time, order_key)`. **[ESTABLISHED-CLASS]** (parallel ECS patterns); our composition [HYPOTHESIS] — hazards (write races, ordering nondeterminism, deadlock-free shutdown) are exactly what S1/S5 fuzz.
- **Determinism requirements:** fixed-step core; **per-entity/per-system RNG streams** (no global RNG whose consumption order depends on iteration order — a classic divergence bug); stable iteration order or order-independent aggregation; **[TO-VERIFY]** floating-point reproducibility across Windows/Linux (the cross-platform determinism caveat — may constrain replay verification to per-platform or require fixed-point/compensated techniques in shared paths).
- **Spatial partitioning:** regions + nested interior cells as the primary partition (matches LOD and streaming); fine-grained partitioning (grid/hierarchy) *within* full-detail regions for batch locality and interest management. **[ESTABLISHED-CLASS]**.
- **Profiling surfaces (feed the S-experiments):** per-system CPU per tick; events processed/sec; backlog depth; memory per NPC per LOD; activation/deactivation latency; time-skip cost curves; budget-overrun counts.

## 8. World streaming and multiplayer residencies (Q6)

**Three independent residency axes [HYPOTHESIS]:**

| Axis | Driven by | Question it answers |
|---|---|---|
| **Simulation residency** (LOD level) | Player presence, velocity lookahead, narrative gates | *How deeply does this region simulate?* |
| **Visual residency** (streaming/paging) | Camera(s), LOD rings, impostors | *What is rendered?* |
| **Network replication interest** | Per-player relevance (the multiplayer study's interest management) | *Which players receive which state?* |

They must remain **decoupled**: a teammate's distant region is simulation-full *for them* but visually absent and replication-irrelevant *for you*; a landmark visible from altitude is visual-resident while simulation-dormant. Letting bandwidth concerns drive simulation LOD (or vice versa) couples correctness to network conditions — rejected as a design rule.

- **Interiors:** nested regions in the same partition scheme — full-detail on entry, dormant otherwise; interior state rides the same commit pipeline (bible world model: cells as organization, not implementation).
- **High-speed traversal and unrestricted flight:** LOD rings extend to horizon scale; **velocity-lookahead prefetch** activates regions *ahead* of the motion vector before arrival; extreme ranges fall back to **far-field aggregate models** (city-level statistics rather than per-NPC events) — how *believable* distant city life must be from altitude is a fidelity-vs-cost product decision **[OPEN — §11]**.
- **Multi-region co-op (1–4 players, distant frontiers):** the worst case for naive "full detail everywhere" — and the exact case the tiered architecture exists for. The **host runs the authoritative simulation** (M-28, single session); guests receive replication scoped by interest; nobody co-simulates. Cross-referenced, not redesigned, per the ownership map.
- **Frontier procedural regions:** same machinery — a generated region activates on approach (deterministic generation seed + event replay = catch-up), deactivates on departure, and its consequences commit through the same pipeline, respecting the M-07 boundary.

## 9. Correctness invariants and edge cases (Deliverable 5)

**Invariants (all [HYPOTHESIS] until S-experiments prove them enforceable):**

- **I1 — No lost consequences:** every durable delta is **captured into the commit pipeline** (local idempotent append) *before* its representation is discarded at deactivation; captured pending deltas **survive discard and are re-read on reactivation within the active authoritative session, applied exactly once** (idempotency keys make reapplication a no-op); canonical acknowledgment is asynchronous and never blocks an LOD transition; unacknowledged deltas remain pending, never falsely confirmed as T3.
- **I2 — Dormant cost is storage-shaped:** a dormant NPC costs no per-tick CPU; its next cost is its next event.
- **I3 — No time travel:** activation always yields a state coherent with the shared clock, the schedule, and committed history — never a stale pose.
- **I4 — One clock:** the campaign clock is single, monotonic, and owned by the spine.
- **I5 — Narrative gates hold:** irreversible authored-city events fire only under their gates (M-07/M-29), including during skips and frontier-only epochs.
- **I6 — Death is permanent:** a tombstoned identity is never reused, and its consequences propagate to relationships/quests/factions.
- **I7 — Determinism where claimed:** same seed + inputs ⇒ same state hash, per platform (E-03; FP caveat §7).
- **I8 — Reputation is singular:** reputation-*relevant* events feed exactly one shared campaign reputation (M-08) — no hidden second scores. NPC-specific memories may persist and shape behavior/dialogue without altering the shared reputation; which memory classes are reputation-relevant is game policy **[OPEN]**.

**Edge cases (research list, not exhaustive):**

| Edge case | Hazard | Direction |
|---|---|---|
| NPC dies while dormant (scheduled/faction-consequence death) | Death must apply without an active representation | Death as a *committed event* on the spine, not only as combat outcome |
| Two players approach one dormant region from opposite sides | Activation race / divergent catch-up | Single-writer spine orders activations deterministically (M-28 helps) |
| Time-skip lands mid-activation | Half-activated region meets fast-forward | Activation is transactional vs. the clock: skip waits or restarts activation |
| Schedule references a dead NPC (workplace, relationship) | Implausible behavior | DESIGN-BIBLE §6 succession/substitution patterns — simulation consumes them |
| Player waits outside a dormant interior | Interior must not "pop" on entry | Interiors activate before the door opens (prefetch by proximity + intent) |
| Event backlog during combat | Near-field starvation | Reserved near-field budgets (§7); backlog is *deferred* far-field work |
| Catch-up replay meets player-diverged history | Replay only valid until the last player interaction | Exact replay up to the last recorded interaction (committed or captured-pending); plausibility synthesis after |
| Cross-platform float divergence | Replay/state-hash mismatch between Windows and Linux | [TO-VERIFY] per §7; possibly per-platform determinism or fixed-point shared paths |

## 10. Proposed experiments (Q7, Deliverable 6) — headless, prioritized; S1/S6-lite, S4-lite, S3-lite, and S2-lite run, full set not implemented

All are deterministic, stdlib-style harnesses in the pattern of `experiments/e5-lab-local/` (proven pattern, [PROJECT EVIDENCE]). Budgets below are **[ASSUMPTION]** labels, not hardware targets — no fixed hardware thresholds are set prematurely.

| # | Experiment | Method | Acceptance (binary) |
|---|---|---|---|
| **S1** | **LOD scaling core** | N ∈ {1k, 5k, 20k} NPCs distributed across the four levels; batch systems + event spine; measure CPU/tick, memory/NPC per level, events/sec | Dormant per-NPC per-tick cost ≈ 0; simplified ≪ full (measured ratio reported, not asserted) |
| **S2** | **Event-queue stress** | Schedule-density sweeps; burst injection; per-tick budgets; near-field reservation | **Corrected acceptance (lead, 2026-10-18/19):** stable queue under sustainable load; eventual backlog recovery after bounded bursts; predictable, measurable growth under sustained overload (an always-bounded backlog under arbitrary overload is NOT required — impossible for a finite processor when arrival exceeds capacity); no silent dropping, duplication, or causally invalid execution; no near-field starvation while near work fits its reserved budget; explicit reporting when near-field demand exceeds capacity; overdue events must not silently make authoritative state appear current |
| **S3** | **Reactivation latency** | Dormancy-duration sweep; exact-replay vs plausibility-synthesis crossover | Activation latency under labeled budget; synthesized states pass I3 plausibility checks; exactness claimed only for spans with a complete recorded event history |
| **S4** | **Time-skip cost + gating** | Advance 24 h / 7 d at event LOD with gated authored-city events | Cost ∝ events, not elapsed time; **zero gated events fired** (I5); held events **revalidate then present** correctly on re-entry (never blind-fire) |
| **S5** | **Determinism/replay** | Same seed + scripted inputs across repeated runs | Identical state hashes for **represented** state; per-platform FP behavior characterized |
| **S6** | **Transition fuzzer** | Randomized activate/deactivate/skip interleavings | I1–I3, I6 hold in 100% of runs (I1 in capture semantics — no transition blocks on remote acknowledgment; pending deltas survive deactivate→reactivate cycles and re-apply exactly once); violations reproduced deterministically |
| **S7** (later, with multiplayer line) | **Multi-region co-op profile** | 2–4 simulated player regions; interest scoping | Host-side scaling across separated active regions is **measured and predictable** (characterized curve, no unbounded surprises); sublinearity is *evaluated*, not imposed as a requirement; residency axes stay decoupled |

Priority order: S1 → S6 → S4 → S3 → S2 → S5 → S7 (throughput first because it tests the summit claim; the fuzzer second because transitions are the load-bearing correctness claim). **Status (2026-10-18):** the reduced **S1/S6-lite slice** — 1,000 seeded NPCs across the four LOD levels, deterministic event spine, simulated persistence pipeline (pending/delayed-ack/idempotency/applied-key ledger), seeded transition fuzzer with fault injection (duplicate delivery, dropped acks, event replay), and an independent reference model — was separately authorized by the lead and run; all checks passed (`experiments/sim-lod-local/`, logical-state fingerprint `feeb64371ea9f7ae`). It is local protocol-logic evidence only — not engine implementation and not a production-performance claim. **S4-lite (authorized and run 2026-10-18, `harness_s4.py`):** narrative gate semantics under time advancement — the four event classifications (§6), 24h/7d/30d skips plus a same-elapsed low-workload control, held-milestone survival through 21 frontier days, revalidation on city re-entry (preconditions, dependencies, classification), rejection with fallback supersession, dependency ordering, exactly-once across repeated skips/revalidations/re-entries, and an independent reference model; all 8 checks passed (fingerprint `bb85bc46a12f4490`). Measured cost tracks event workload, not elapsed world time (7-day full-world skip: 87.4 ms / 27,218 events; same-elapsed one-region quiet skip: 13.1 ms / 3,380 events). **S3-lite (authorized and run 2026-10-18, `harness_s3.py`):** long-dormant NPC reactivation under TRUE dormancy (dormant regions process nothing; activation reconstructs) — deterministic replay of represented history vs plausibility synthesis vs an adaptive candidate choosing by estimated event work (labeled threshold — evidence, not a policy selection). Correctness: replay = synthesis = independent reference for presents; the durable lane (deaths, relationships, oneoffs, earned progression) applied exactly once and never written by synthesis; pending preserved across repeated activation/deactivation cycles; synthesis satisfies travel-reachability, bounds, and I3 invariants with synthesized spans labeled; unrecorded deviations are missed by BOTH methods — replay is exact only for sufficiently represented history. Crossover measured in replay WORK (events per NPC): replay cheaper below ~7.6 (quiet) / ~11.2 (normal) / ~12.4 (dense) pending events per NPC, synthesis above; 90 dormant QUIET days (180 events/NPC) cost more to replay than 7 busy days (39 events/NPC) — an elapsed-time rule would choose wrong. Retained trajectory memory is replay's capability cost (e.g., ~6.3 MB for 125 NPCs × 507 events at 90-day dense vs ~51 KB for synthesis). **S2-lite (authorized and run 2026-10-19, `harness_s2.py`):** event spine under normal load, bursts, and sustained overload — per-tick budget with an explicit near-field reservation (research-candidate policy, not approved). Measured: stable at sustainable rates (zero backlog at λ=30 < B=50); at-capacity jitter transients fully drain; sustained-overload growth is predictable (λ=75 → 24.4/tick measured vs 24.6 predicted; λ=150 → 100.4 vs 100.7) with full recovery once the load drops; far bursts to 20× budget drain completely with ZERO near-field lateness (the reservation works); near-field demand beyond its reserve is explicitly reported as near-backlog, never dropped; exactly-once, in-class (due, seq) ordering, and cause-before-effect hold under overload with 27,062 dynamically generated follow-ups; overdue events never look silently current (reads behind a 2,000-event jam are flagged stale-and-pre-write; a write 39 ticks late executes with its lateness recorded). **Notable finding:** NPC schedule SYNCHRONY, not average rate, sets the spine load — shared windows cluster commitment starts to 80/tick and fixed-time shift schedules spike ~200/tick despite a 2.7/tick average, so per-tick budgets must be sized to cluster peaks (or the clusters desynchronized). All checks passed (fingerprint `f7fce3ee1d7835d1`). **Durability caveat (lead, 2026-10-18):** the host-loss simulations in these harnesses do not establish durability of local emergency recovery copies — a separate persistence concern not claimed here. The full S1–S7 experiments remain unimplemented and unauthorized.

## 11. Open questions requiring product decisions (Deliverable 7)

1. **Plausibility vs exactness policy:** how exact must long-dormant activation be (pure plausibility is cheaper; exact replay is verifiable)? Affects feel and S3 budgets.
2. **Far-field fidelity from flight:** how *believable* must distant city life be at horizon scale — per-NPC events, coarse statistics, or visual-only life? Fidelity ambition vs cost; feeds §8.
3. **NPC memory depth:** how much episodic history persists per NPC (storage/complexity vs "the world remembers you" richness), and which memory classes are reputation-relevant versus NPC-local only?
4. **City routine drift during frontier-only time:** schedules only, or bounded economy/faction drift too (within the M-07 boundary)?
5. **Time-skip presentation:** instant fast-forward vs visible accelerated processing (UX, not mechanism).
6. **Determinism strictness:** required for replay/debug/networking (E-03), or best-effort with per-platform scoping (FP caveat)?
7. **Facade richness budget:** how much authored-NPC behavior richness is *required at launch* vs grown later (reusable-engine scope question).

## 12. References (class-level)

- Discrete-event simulation fundamentals (classic DES literature, e.g., Law & Kelton's *Simulation Modeling and Analysis*; event-calculator/queue patterns as used in tools like SimPy) — the event-spine lineage. **[ESTABLISHED-CLASS]**
- Data-oriented design / ECS (e.g., the Overwatch gameplay architecture GDC talk; Flecs/EnTT-class library literature) — batch-component throughput and component-ownership discipline. **[ESTABLISHED-CLASS]**
- Game AI scheduling and offscreen abstraction (e.g., Radiant-AI-class schedule systems; The Sims-class time-budgeted agents) — the LOD-simulation precedent. **[ESTABLISHED-CLASS]**
- Crowd/ambient-life systems in open worlds (GDC-class talks on ambient crowds and traffic) — far-field statistical models. **[ESTABLISHED-CLASS]**
- Spatial partitioning (grids/hi erarchies) and job-parallel iteration patterns — standard systems literature. **[ESTABLISHED-CLASS]**

Class-level only: no claim is made about any specific title's implementation; every composition above remains **[HYPOTHESIS]** until the S-experiments pass. No ECS framework, language, storage format, or backend is selected.

---

*Research-only study, 2026-10-17 (v0.1); corrected 2026-10-18 (v0.2) per the lead's reconciliation directives; revised 2026-10-18 (v0.3) after the three-way review; revised 2026-10-18 (v0.4) recording the S4-lite run; revised 2026-10-18 (v0.5) recording the S3-lite run; revised 2026-10-19 (v0.6) recording the S2-lite run under corrected acceptance criteria — the tiered hybrid is the leading hypothesis, the S1/S6-lite, S4-lite, S3-lite, and S2-lite slices were authorized and run (`experiments/sim-lod-local/`), and the remaining S-experiment set stays unauthorized. Per the lead (2026-10-18/19), no further experimental series begins automatically: the next review assesses whether the evidence base suffices to define the initial reusable simulation interfaces and plan an engine vertical slice. No engine simulation is implemented or selected; all architecture proposals remain unapproved hypotheses. The study defers to DESIGN-BIBLE (gameplay), MULTIPLAYER-SPEC (policy), MULTIPLAYER-ARCHITECTURE-RESEARCH (persistence/authority), and DECISIONS (registry).*
