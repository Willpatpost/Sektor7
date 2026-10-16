# Engine Evaluation

> **Status:** Draft v0.1 · 2026-10-16 · Companion to `VISION.md` (read §1 Twin Summits and §9 Architectural Directives first). Decision owner: the project lead. This document does not recommend — it defines how the decision is made, gathers evidence, and leaves the verdict to the proving ground (§5) and scorecard (§6).

## 1. What we are choosing — and what we are not

Per VISION §9, the deterministic, data-oriented simulation core is ours regardless of engine. The engine is chosen as the **hosting platform**: rendering, content/asset pipeline, platform + Steam layer, and the multiplayer transport surface. That makes this choice important but low-risk — the crown jewel carries over either way.

The engine decides:
- How directly we can reach the **graphics summit** (rendering features, photoreal asset compatibility, upscaling)
- How cheaply we can host the **efficiency summit** (runtime overhead, compiled-code extension mechanism, threading)
- How much co-op plumbing arrives for free (replication, servers)
- How fast a solo developer + AI agents can iterate

The engine does **not** decide: simulation design, determinism, world structure, game design. Those live in our documents and our core.

## 2. Candidates

| Candidate | Why it's in the running |
|---|---|
| **Godot 4.x** | The existing prototype line (3.3k LOC, milestone 2). MIT license, zero royalties, forever. Full source access — RCT-style control at the deepest level. Lean runtime. GDExtension hosts the sim core in C++ or **Rust** (godot-rust). RenderingServer / PhysicsServer3D low-level APIs match our data-oriented hosting model exactly. |
| **Unreal Engine 5.x** | The industry photoreal toolchain: Nanite (virtualized geometry — film-scale detail without hand-authored LOD), Lumen (dynamic GI), Virtual Shadow Maps, MetaHuman, path tracing for cinematics. Native C++ — the sim core is first-class. World Partition for open worlds. Iris replication + standard dedicated servers. Built-in scalability tiers + DLSS/FSR/TSR upscaling. |
| **Custom engine — the RCT purist path** | The literal Sawyer route: own every cycle. **Parked, not dismissed.** Our simulation core is already ~80% of a custom engine's job. What parks it as a *first move* is the graphics summit: it discards every existing photoreal toolchain (scans, MetaHuman-class characters, Nanite-scale geometry pipelines) and asks a solo team to out-render two decades of engine development. **Reopen condition:** if the hosting layer of either finalist ever becomes a bottleneck neither can clear, this option comes back. |
| **Unity** | Assessed, not front-run: no Nanite/Lumen-class rendering tech toward the graphics summit, no MIT-class source access toward the efficiency summit, and licensing trust (the 2023 runtime-fee episode) matters for a solo $20 Steam product. It wins neither summit. |

## 3. Criteria

Grouped by source: Graphics Summit (**GS**), Efficiency Summit (**ES**), Co-op (**CO**), Production — solo dev, $20 Steam (**PR**), Asset Pipeline (**AP**).

| # | Criterion | The question it answers |
|---|---|---|
| GS-1 | Rendering ceiling | Dynamic GI, virtualized geometry, virtual shadows, volumetrics, reflections stack, water/wet surfaces, skin/eyes/hair — how high can fidelity go? |
| GS-2 | Upscaling & frame tech | TAA / DLSS / FSR-class reconstruction — the AAA lever for photoreal on modest GPUs |
| GS-3 | Shader & compute control | Custom materials, compute shaders, renderer source access — can we invent technique at the metal? |
| GS-4 | Photoreal asset compatibility | Scans, MetaHuman-class characters, import/LOD/compression pipeline — do AAA assets flow in natively? |
| ES-1 | Extension mechanism | Compiled hot-path hosting (C++/Rust), ABI stability, iteration speed |
| ES-2 | Runtime overhead | What does the engine itself cost on mid-tier hardware before we add anything? |
| ES-3 | Threading & servers | Job systems, headless/server operation |
| ES-4 | Frame pipeline control | Can we bypass per-object overhead (server APIs vs scene-tree costs)? |
| CO-1 | Replication maturity | Built-in netcode quality vs DIY on top of our deterministic core |
| CO-2 | Server story | Listen/dedicated builds, Steam networking integration |
| PR-1 | Solo + AI-agent velocity | Editor weight, iteration speed, scriptability, docs/community |
| PR-2 | Licensing & money | Royalties at $20/unit, source rights |
| PR-3 | Steam + Linux/Deck | Deployment for the mid-tier floor (Deck-class) and desktop |
| AP-1 | Open world & streaming | Partitioning, cell/interior loading, distance management |
| AP-2 | Ecosystem coupling | Which asset sources unlock natively (Fab/UE-licensed scans vs CC0/AI-generated) |

**Weights:** proposed in §6 — the project lead amends them. The weighting *is* the decision philosophy, so it belongs to you.

## 4. Evidence per candidate

### 4.1 Godot 4.x

- **Rendering:** Forward+ clustered renderer; SDFGI + VoxelGI (real-time GI), LightmapGI (baked), SSR, volumetric fog, MSAA/TAA, FSR-class upscaling built in; automatic mesh LOD on import; occlusion culling. **No Nanite / Lumen / MetaHuman equivalent** — photoreal geometry at scale means either manual LOD discipline or technique we invent (plausible: our sim core driving massive batched instances through RenderingServer directly).
- **Sim hosting:** GDExtension — sim core in C++ or Rust (gdext is mature); RenderingServer/PhysicsServer3D exist precisely for the create-thousands-of-instances-without-nodes model; WorkerThreadPool for parallel ticks; fixed-timestep loop under our control. The repo already proves the workflow: a 60-check headless test suite runs the whole game from CLI.
- **Netcode:** MultiplayerSynchronizer/Spawner + ENet/WebSocket transports; our deterministic core can implement lockstep or server-authoritative models on top; headless server mode is standard (we use it today for testing).
- **Production:** MIT — $0 and no reporting, ever. Light editor. GDScript iteration is extremely fast for gameplay glue. Source is genuinely ours to modify (the RCT spirit).
- **Open world & assets:** manual chunk streaming — cheap for us because deterministic generation means districts regenerate on load instead of shipping as data; CC0 (PolyHaven) and AI-generated PBR assets fully compatible; **most Fab UE-licensed photoreal scans are NOT usable** — per-asset license verification required.
- **Watch items:** photoreal character/creature story is the weakest flank; renderer maturity at the CP2077 bar is unproven by us; smaller AAA-grade help ecosystem.

### 4.2 Unreal Engine 5.x

- **Rendering:** the summit toolkit. Nanite renders only visible geometry clusters at automatic detail — the single biggest attack on the "photoreal + modest GPU" tension. Lumen dynamic GI (with baked-lightmap fallback for the mid-tier floor); Virtual Shadow Maps; path tracer for cinemics-grade captures; MetaHuman for AAA humans (UE-coupled pipeline); Niagara VFX; water/wet systems.
- **Sim hosting:** the entire engine is C++; the sim core is first-class, not an extension. Fixed-tick subsystems; task-graph parallelism. Determinism of *our* core is unaffected — the rule is simply: authoritative movement lives in our core, never in engine physics.
- **Netcode:** Iris replication framework, prediction/rewind out of the box, dedicated server builds standard, Steam networking mature.
- **Production:** 5% royalty only after $1M lifetime gross — at $20/unit that's 50,000 copies before a cent is owed. Source available to licensees (GitHub), though not MIT-class freedom. Editor is heavy; C++ iteration slower than GDScript (LiveCoding mitigates); Blueprints give rapid prototyping.
- **Open world & assets:** World Partition + data layers built for exactly our district/cell structure. The Fab photoreal ecosystem unlocks natively (Megascans-class scans, MetaHumans).
- **Watch items:** runtime weight on the Deck-class floor; engine complexity for a solo lead; risk of accidentally leaning on engine systems (Chaos physics) that would break determinism — mitigated by the "our core owns authority" rule.

### 4.3 Custom engine & Unity

Parked / not front-run — reasoning in §2. Reopen conditions recorded there.

## 5. Proving ground protocol — the decision experiment

Theory doesn't pick engines. Identical work does. Both finalists build the **same vertical slice**.

**The scene — "One Corner of the City" (~40m × 40m):**
- One street intersection: wet asphalt with puddles, one hero building with interior-lit windows and neon signage, secondary facades, hanging cables, street-level props, lamps
- Weather on: rain + volumetric fog + a lightning flash
- 24 NPCs walking schedules (day/night toggle to prove the cycle) + 2 hovercars on lanes
- Full post chain enabled (tonemap, bloom, DoF, upscaler on)
- Same source assets where licensing allows; otherwise matched-intent assets

**Protocol:**
1. Scripted fixed camera flythrough (identical path) + free gameplay camera, both at 1080p internal resolution
2. Hardware: the mid-tier floor (Steam Deck-class GPU) and the high-end reference rig (Ryzen 7 9800X3D / RTX 5070 Ti)
3. Measure: average and 1%-low frame times, VRAM, build size, load time; screenshot at marked waypoints
4. Judge fidelity side-by-side against CP2077 reference captures of a comparable night street (same waypoints, similar weather)
5. Track honest build effort: hours, asset wrangling, iteration count — solo-dev velocity is a real criterion, not an afterthought
6. Deliverable: `docs/PROVING-GROUND-RESULTS.md` — captures, frame graphs, effort logs, verdict draft

**Success bar:** a finalist must demonstrate a *credible path* to the graphics bar on the high-end rig and a *configurable path* to "smooth as RCT" on the floor. Not perfection on day one — a path.

**Cost note:** the Godot slice has a head start — the current city line already renders most of this scene (facade shader, wet streets, rain, fog, traffic are all built). The experiment is cheap on one side by design; that head start is data, not bias.

## 6. Scorecard (filled after the proving ground)

Each criterion scored 1–5 by the project lead, multiplied by weight.

| Criterion | Weight (proposal — amend) | Godot 4 | UE 5 | Notes |
|---|---|---|---|---|
| GS-1 Rendering ceiling | 3 | | | |
| GS-2 Upscaling & frame tech | 2 | | | |
| GS-3 Shader & compute control | 2 | | | |
| GS-4 Photoreal asset compatibility | 3 | | | |
| ES-1 Extension mechanism | 3 | | | |
| ES-2 Runtime overhead | 2 | | | |
| ES-3 Threading & servers | 2 | | | |
| ES-4 Frame pipeline control | 2 | | | |
| CO-1 Replication maturity | 2 | | | |
| CO-2 Server story | 1 | | | |
| PR-1 Solo + AI-agent velocity | 3 | | | |
| PR-2 Licensing & money | 1 | | | |
| PR-3 Steam + Linux/Deck | 2 | | | |
| AP-1 Open world & streaming | 2 | | | |
| AP-2 Ecosystem coupling | 2 | | | |
| **Total (max 160)** | **32** | | | |

## 7. Risk register

| Candidate | Risk | Severity | Mitigation |
|---|---|---|---|
| Godot | Photoreal ceiling below the bar, characters especially | high | The proving ground tests exactly this; technique-invention budget; characters can be third-party/commissioned regardless of engine |
| Godot | Smaller ecosystem for AAA-grade problems | medium | Our headless-test discipline partially compensates; source access means no dead ends |
| UE5 | Deck-class floor performance | medium | Scalability tiers + upscalers; Nanite's per-cluster rendering is floor-friendly |
| UE5 | Solo velocity under engine weight | medium | Blueprints, LiveCoding, AI-agent assistance |
| UE5 | Royalty / licensing surprise | low | 5% only past $1M gross; source access included for licensees |
| Both | Accidentally leaning on engine physics, breaking determinism | medium | Hard rule: all authoritative movement lives in our sim core; engine physics is cosmetic near-field only |

## 8. What carries over regardless of winner

- The simulation core design (future `TECH-ARCHITECTURE.md`)
- The design bible, world layout, asset specifications
- The deterministic world-generation approach
- The Godot prototype's gameplay knowledge — full systems inventory, controls scheme, test discipline

## 9. Process from here

1. **Project lead reviews and amends criteria + weights** ← you are here
2. Confirm the candidate list (Unity stays out? custom stays parked?)
3. Build the proving-ground slice in Godot (head start: current city line is ~80% of the scene)
4. Build the identical slice in UE5
5. Run the protocol on both hardware tiers; fill the scorecard
6. Decision recorded here; `VISION.md` §8.1 updated to *decided*

---

*Related: `VISION.md` (foundation) · future: `ASSETS.md` — sourcing couples tightly to this choice (see AP-2), `TECH-ARCHITECTURE.md`*
