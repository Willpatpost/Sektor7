# PROTOTYPE INVENTORY — Stage 0 Godot Report

> **Status:** Reconstructed audit record v0.2 · 2026-10-16, reconciled 2026-10-17 · **Findings verified against the original in-repo audit — see §7.**
>
> **Ownership:** This file records the Stage 0 read-only inventory reported by GLM 5.3. It is a record of an **EXISTING EXPERIMENT**, not a technical selection or permission to modify code. [`VISION.md`](VISION.md) owns the project goals; [`CUSTOM-ENGINE-RESEARCH.md`](CUSTOM-ENGINE-RESEARCH.md) owns evaluation methods; [`DECISIONS.md`](DECISIONS.md) records decisions.
>
> **Provenance and limitation:** The project lead supplied GLM's audit summary in conversation, not the original repository files, commands, captured logs, or the subsequently created `PROTOTYPE-INVENTORY.md` in GLM's repository. The exact details below are **attributed to GLM's report** and cannot be independently reproduced from the four Markdown files attached in this handoff. Replace/reconcile this summary with the original audit evidence if available. Do not label these numbers independently verified by ChatGPT.
>
> **Reconciliation (2026-10-17, GLM 5.3):** The original audit evidence exists in this repository: the Stage 0 verification was executed directly here on 2026-10-16 (commands and raw results in §7), and the original first-hand `PROTOTYPE-INVENTORY.md` is preserved in the pre-sync git stash (`stash@{0}^3`). The reconstruction below was cross-checked against the original — **every figure matches; no numbers changed.** The findings are GLM-verified in-repo; ChatGPT's reconstruction text is preserved unmodified.

## 1. Reported inventory

| Item | GLM 5.3 reported finding | Provenance |
|---|---|---|
| Location | `godot/` | Stage 0 report |
| Engine/version | Godot **4.7.2**, Forward+ | Stage 0 report |
| GDScript inventory | **16 `.gd` files, 4,273 lines** | 3,901 source + 372 test lines, per report |
| Headless checks | **60/60 passing** | Reported test run, raw transcript not attached |
| Project import | Exit 0; no `SCRIPT ERROR` | Reported import, raw logs not attached |
| Project-local add-ons | No add-ons, GDExtensions or autoloads; procedural PCM audio | Reported inspection |
| Repository state | `godot/` working tree untouched; import cache gitignored | Reported `git status`; raw output not attached |
| Asset provenance | Four PolyHaven CC0 PBR sets, eleven CC0 props, one CC0 HDRI, and DejaVu font under separately permissive Bitstream Vera terms | GLM reported inspecting directory `CREDITS.md` files; source licenses should be retained for commercial reuse |

The older documentation's approximate **3.3k LOC** estimate is obsolete according to GLM's subsequent count. The older **60 headless checks** claim reportedly matches the audit exactly.

## 2. Reported functionality

GLM states that its headless test drive verified the following features. Because only the summary was supplied, distinctions between static inspection, logic testing and rendered visual tests cannot be independently confirmed here:

- Deterministic seed **1337**, **8×8 grid of 50 m cells**.
- **156 colliders**, **1,265 MultiMesh-batched props**, **five authored landmarks**.
- Character movement, jumping, combat and hitscan.
- Hovercar flight.
- Four-job quest chain; dialogue, shop, upgrades.
- Five perspectives/POVs.
- Menus, settings, rebinding persistence and gamepad support.
- Procedural/generated aspects of the existing city and PCM audio.

**Not reported as retested:** New high-fidelity visual captures. GLM said a software-rendered/llvmpipe capture kit exists, but no visual comparison was performed during this pass. The reported fontconfig warning was considered environmental and benign; raw logs are not included.

## 3. Transferable patterns — engineering evidence, not design mandates

| Pattern GLM identified | Potential lesson | What still needs proof |
|---|---|---|
| MultiMesh queue/flush batching | Group same-mesh objects to reduce rendering submissions | Actual draw calls and GPU costs across equivalent workloads |
| Decoupled AABB `colliders` registry | Reuse one spatial description across scattering, cable placement and collision queries | Data layout, scalability and cache behavior in our runtime |
| Registry-defined asset loading and graceful fallbacks | Explicit asset dependencies, deterministic startup, understandable failures | Asset schema and load pipeline choices in our engine |
| SceneTree-driven smoke harness | CLI-friendly tests and automated regression checks | Engine-agnostic test harness requirements for later benchmarks |
| In-shader procedural windows (per-building uniforms drive window grids from world position) | Window detail without window geometry — a material/geometry budget lever | Visual quality vs. cost on matched workloads |
| Determinism discipline (fixed seeds 1337/77, rng-free authored landmarks, generated-count assertions in tests) | Reproducible world generation and benchmarkable cities | Cross-platform determinism and replay behavior |

## 4. Reported anti-patterns / migration hazards

- Large `game.*` hub coupling and ambiguous state ownership: risky for democratic co-op and distributed campaign state.
- Per-building `ShaderMaterial` instances where shared/batched material data might be more efficient.
- Per-frame node/light churn with uncertain CPU overhead.
- One `CharacterBody3D` per enemy rather than a separately evaluated data-oriented crowd representation.

These are **reported observations**, not proof that any proposed replacement is inherently superior. Maintain baseline and quality-equivalent measurements before drawing conclusions.

## 5. Disposition and restrictions

**DECIDED:** Preserve the prototype as a **read-only calibration/reference experiment**. It does not determine the engine's rendering API, language, ECS, simulation model or editor. The retired `browser/` line is not production.

**AUTHORIZED IN PAST:** Stage 0 read-only audit. **NOT AUTHORIZED BY THIS DOCUMENT:** Refactors, code migrations, edits to game logic, asset changes, new research implementation, or destructive repo cleanup. Full resets remain possible only after the human lead approves them.

## 6. Suggested reconciliation when original GLM inventory is available

*(Executed 2026-10-17 — see §7. The original was recovered from this repository's pre-sync stash; all figures matched.)*

1. Compare this reconstructed summary with GLM's original `docs/PROTOTYPE-INVENTORY.md`; prefer directly cited raw artifacts and exact commands when available.
2. Attach or point to versioned test invocation, relevant commit hash, and terminal output demonstrating 60 passing checks.
3. Preserve original license/attribution files with any moved assets and recheck rights for any non-CC0 dependency.
4. Capture representative visuals and optimized performance baseline only when an experiment is explicitly approved.
5. Update this record, `DECISIONS.md`, and `CUSTOM-ENGINE-RESEARCH.md` if newly verified evidence changes any reported numbers.

## 7. Verification record and reconciliation (GLM 5.3, 2026-10-17)

**Where and when the audit ran:** directly in this repository on 2026-10-16, against the `godot/` tree as committed at `2aed2a9`. The audit was read-only: only the gitignored `.godot` import cache was touched; `git status --porcelain -- godot` was empty before and after.

**Verification commands and raw results (2026-10-16):**

1. `~/.local/bin/godot4 --headless --path godot --import` — exit 0; no `SCRIPT ERROR` lines. (The fontconfig warnings are environmental: this machine lacks libfontconfig; the game ships DejaVu Sans Bold and registers a ThemeDB fallback font.)
2. `~/.local/bin/godot4 --headless --path godot -s res://tests/smoke.gd` — **`60 passed, 0 failed`**, exit 0.
3. `wc -l godot/src/*.gd godot/tests/*.gd` — **4,273 total** (3,901 source + 372 test), 16 `.gd` files.
4. `git status --porcelain -- godot` — empty (prototype untouched).

**Method:** full first-hand read of all 16 GDScript files, `main.tscn` (a single empty `Node3D` + script), `project.godot` (Godot 4.7.2, Forward+, MSAA 2×; no addons, GDExtensions, or autoloads), and every asset `CREDITS.md`. Logic-level verification only — no visual captures were re-run this pass (a software-Vulkan render kit exists for that when an experiment is approved).

**Composition facts from the first-hand read (supplementing §2):** 14 source scripts + 2 test scripts; deterministic seeds **1337** (city) and **77** (enemies); authored landmarks are rng-free; hovercars use a thrust/drag/vertical-thruster flight model; enemy AI uses line-of-sight raycasts with burst fire; audio is fully procedural PCM.

**Cross-check result:** every figure in §§1–2 matches the original first-hand report — LOC, check count, collider/prop/landmark counts, seeds, asset inventory, and license terms. Per §6.5, no numeric updates are required in `DECISIONS.md` or `CUSTOM-ENGINE-RESEARCH.md`. The original file remains preserved at git stash `stash@{0}` (untracked commit `stash@{0}^3`) pending the lead's confirmation of this reconciliation.

---

*Reconstructed 2026-10-16 to avoid losing GLM's report when replacing the documentation directory; reconciled against the original in-repo audit 2026-10-17. §§1–6 preserve the reconstruction's structure; §7 is the first-hand verification record.*
