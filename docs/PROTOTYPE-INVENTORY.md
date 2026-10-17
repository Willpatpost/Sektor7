# PROTOTYPE INVENTORY — Stage 0 Godot Report

> **Status:** Reconstructed audit record v0.1 · 2026-10-16 · **REPORTED FINDINGS, NOT INDEPENDENT VERIFICATION.**
>
> **Ownership:** This file records the Stage 0 read-only inventory reported by GLM 5.3. It is a record of an **EXISTING EXPERIMENT**, not a technical selection or permission to modify code. [`VISION.md`](VISION.md) owns the project goals; [`CUSTOM-ENGINE-RESEARCH.md`](CUSTOM-ENGINE-RESEARCH.md) owns evaluation methods; [`DECISIONS.md`](DECISIONS.md) records decisions.
>
> **Provenance and limitation:** The project lead supplied GLM's audit summary in conversation, not the original repository files, commands, captured logs, or the subsequently created `PROTOTYPE-INVENTORY.md` in GLM's repository. The exact details below are **attributed to GLM's report** and cannot be independently reproduced from the four Markdown files attached in this handoff. Replace/reconcile this summary with the original audit evidence if available. Do not label these numbers independently verified by ChatGPT.

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

1. Compare this reconstructed summary with GLM's original `docs/PROTOTYPE-INVENTORY.md`; prefer directly cited raw artifacts and exact commands when available.
2. Attach or point to versioned test invocation, relevant commit hash, and terminal output demonstrating 60 passing checks.
3. Preserve original license/attribution files with any moved assets and recheck rights for any non-CC0 dependency.
4. Capture representative visuals and optimized performance baseline only when an experiment is explicitly approved.
5. Update this record, `DECISIONS.md`, and `CUSTOM-ENGINE-RESEARCH.md` if newly verified evidence changes any reported numbers.

*This file exists to avoid losing GLM's useful report when replacing the documentation directory. It must not be mistaken for the missing original audit document.*
