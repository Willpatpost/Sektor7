# Sektor7 Original Engine — Workspace

> **Status:** Stage 0 (workspace) and Stage 1 (SDL3 application shell) complete — lead-authorized 2026-10-19. Stage 2 (Vulkan presentation) is **not begun** and requires explicit approval (`docs/TECH-ARCHITECTURE.md` §6; `docs/DECISIONS.md` T-08/T-10).

Rust workspace holding the first foundations of the reusable original engine. Scope stays deliberately minimal per stage gate — only what the current authorization ratifies, each mechanism exercised by its own tests (anti-abstraction ledger, `docs/TECH-ARCHITECTURE.md` §8).

## Crates

| Crate | Kind | Contents |
|---|---|---|
| `engine-core` | library | Stage 0: `Tick` clock seed; event spine — deterministic `(due, seq)` queue priority, cross-class causal dependencies validated at schedule time, per-tick budget, execution ledger with first-class lateness; safe `processed_through` watermark distinct from `max_executed` (computed live, retreats on late arrivals, `None` when nothing is provably complete); FNV-1a fingerprints (length-prefixed strings, schema v2); level-filtered diagnostics. **Platform-independent — no SDL, no OS dependencies** |
| `engine-shell` | library | Stage 1: SDL3 application shell — `Platform` (driver selection via SDL3's safe hint interface, incl. the dummy mode for headless environments — zero project-owned unsafe code), `ShellApp` (real window + OS event pump + CPU-paced loop with frame statistics; focus diagnostics through the shared engine-core logger), input-intent state machine (WASD axes, mouse-look deltas under relative capture, key/button edges, focus-loss clearing of held keys and stale deltas, resize, sticky quit) |
| `engine-slice` | library + binary | Stage 0 stdin CLI + fingerprint self-test; Stage 1 modes: `--window` (real GUI), `--window --dummy` (headless smoke), `--soak <secs>` (loop soak; add `--real` for a real-window soak) |

## SDL3 binding and native-library strategy (Stage 1 decision, DECISIONS T-09)

- **Binding:** `sdl3` 0.20.0 (vhspace/sdl3-rs, MIT) over `sdl3-sys` 0.7.2+SDL-3.4.18 (maia-s/sdl3-sys-rs, Zlib).
- **Native:** vendored **SDL 3.4.18** source (`sdl3-src` 3.4.18), compiled from source with CMake and **linked statically** (`build-from-source-static`) — identical native version on every platform, no system SDL packages, no DLL redistribution. SDL3 is zlib-licensed, so static linking is unproblematic.
- On Unix hosts lacking X11/Wayland development libraries (headless CI, the dev pod), a target-scoped `sdl-unix-console-build` feature permits the build; where desktop dev packages exist, desktop backends are still autodetected. Windows builds are unaffected.
- **Build requirements:** a C compiler and CMake ≥ 3.16 on the PATH (CI runners have both; the dev pod uses a user-local CMake 3.30.5).
- The wrapper's optional satellite crates (sdl3-image/mixer/ttf) are **locked in Cargo.lock but not enabled or compiled**.
- **MSVC runtime alignment (Stage 1 corrective pass, LNK4098):** SDL's CMake Debug configuration selects MSVC's debug CRT (/MDd — MSVCRTD) while Rust links the release dynamic CRT; `engine/build/msvc-sdl3-crt.cmake` forces the release dynamic CRT (/MD) for the SDL build in every configuration, delivered via the target-suffixed env var in `engine/.cargo/config.toml` (inert on non-Windows targets). **Run cargo from `engine/`** so the config is discovered.

## Build and test

```sh
cargo build --workspace
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
```

90 tests (33 Stage 0 + 15 Stage 1 + 21 Stage 2 + 21 Stage 3: 9 spine lanes, 8 simulation determinism, 4 frame timing). The 60-second soak is a measurement, ignored by default:

```sh
cargo test -p engine-shell --test soak -- --ignored --nocapture
```

Determinism discipline: Stage 1 left `engine-core` untouched — Stage 0 fingerprints are unchanged (`engine=6504123a47cab66f`, `state=937bd3b6f56f07e0`).

## Manual GUI testing (graphical session required — the lead's rig)

**Windows:** install Rust via rustup, clone the repository, then:

```sh
cd engine
cargo run -p engine-slice -- --window
```

The first build compiles vendored SDL 3.4.18 (a few minutes). A 1280×720 window opens: **Sektor7 original engine — Stage 1 shell**. Verify:

1. **WASD** — the periodic frame lines show `move=(...)` changing (+1 forward for W, −1 for S, 0 with both, ±1 strafe for D/A).
2. **Left click** — toggles relative mouse capture (`relative mouse capture: true`; the cursor is captured), and mouse movement produces nonzero `look=(...)` deltas; click again to release.
3. **Resize** the window — a `window resized: WxH` line prints.
4. **Alt+Tab** away and back — `diagnostics: keyboard focus lost/regained` lines print, and held input is cleared (no stuck keys).
5. **Esc** or the window **close button** — exits cleanly with the loop summary (frames, avg/p50/p95/p99/max frame interval, resident memory); the summary reports `quit: Callback` for Esc and `quit: Requested` for the close button.
6. The build output should contain **no LNK4098** warnings (corrective-pass finding 2 — the SDL CRT-alignment toolchain applies automatically when cargo runs from `engine/`).
7. Optional real-window soak: `cargo run -p engine-slice -- --soak 60 --real`.

**Linux (desktop with X11/Wayland dev packages):** the same commands; desktop backends are autodetected. Without them, `--window --dummy` is the headless smoke only.

## Stage 2 render demo (Vulkan; hardware evidence needs a graphical session)

**Windows reference rig (RTX 5070 Ti):** after the Stage 1 setup above:

```sh
cargo run -p engine-slice -- --render
cargo run -p engine-slice -- --render --present mailbox
cargo run -p engine-slice -- --render --present immediate --fps uncapped
cargo run -p engine-slice -- --render --fps 240
```

Verify: **depth ordering** (the three overlapping depth-demo cubes intersect correctly from every angle); **resize** (swapchain recreation, no hangs or artifacts); **minimize/restore** (frames skip while minimized; rendering resumes); **close** (Esc or the close button) exits cleanly with the run summary — presented frames, CPU P50/P95/P99, GPU timestamp percentiles, present mode, swapchain recreations, validation counts; **relaunch** repeatedly; **O** toggles the overlay; the build output shows **no LNK4098**. `--validation on` forces validation layers (absent layers are an environment limitation, reported honestly); `--no-overlay` disables the readout. The exit summary carries the instrument caveat: readings of this simple scene are **not first-game performance claims** — the 240+ FPS target is a long-term objective (DECISIONS T-11), and present-mode/uncapped runs exist to measure presentation behavior on real hardware. Pod/CI lavapipe runs are correctness evidence only.

**Windows note — "An Application Control policy has blocked this file" (os error 4551):** Smart App Control blocks locally built unsigned executables. Confirm with `Get-ItemProperty HKLM:\SYSTEM\CurrentControlSet\Control\CI\Policy -Name VerifiedAndReputablePolicyState` (1/2 = evaluation/enforcement) and the block events in the `Microsoft-Windows-CodeIntegrity/Operational` log (3076/3077, naming policy `VerifiedAndReputableDesktop`); the fix is Settings → Privacy & security → Windows Security → App & browser control → Smart App Control settings → **Off** (permanent for the install — the standard choice for a dev machine, which compiles unsigned binaries by definition). Headless recreation verification: `cargo run -p engine-slice -- --render --frames 20 --fps 0 --simulate-resize` (the run summary reports swapchain recreations).

## Stage 3 headless simulation (`--sim`)

Deterministic, no GPU or window required — every run prints only logical state, so repeated runs with identical arguments print identical output:

```powershell
cargo run -p engine-slice -- --sim --ticks 300 --cadence 60
cargo run -p engine-slice -- --sim --ticks 300 --cadence 240
cargo run -p engine-slice -- --sim --ticks 300 --cadence irregular
cargo run -p engine-slice -- --sim --ticks 300 --seed 4242
cargo run -p engine-slice -- --sim --ticks 300 --budget 140 --near-reserve 70
cargo run -p engine-slice -- --sim --ticks 300 --budget 30
cargo run -p engine-slice -- --sim --ticks 300 --budget 30 --near-reserve 10
```

Expected observations: identical output for repeated identical commands (double-run discipline); `cadence` variants and `irregular` (which includes a 2 s stall — twenty due ticks, exceeding the 8-tick per-frame catch-up bound, so the retained obligation drains across the following frames) all reach the same campaign tick with the same fingerprints — render cadence does not change logical results; the 140/70 run shows the near-field reservation working (near backlogged ticks 0 while far backlogs ~293 of 300); the budget-30 runs show honest overload (every tick budget-exhausted, ~9k late executions, progress slowed ~5× — chained schedules self-throttle, so lateness and exhausted ticks, not queue growth, are the overload signal). The near reservation with a reduced budget: the **default** scales proportionally to preserve the documented near share (60/200 → 9 of 30, keeping both lanes under pressure instead of degenerating into a near-only split), while an **explicit** `--near-reserve` is honored as given, clamped to the budget — the config line in the report prints the effective reservation and its provenance either way. The graphical demo runs the same simulation beside rendering:

```powershell
cargo run -p engine-slice -- --render
cargo run -p engine-slice -- --render --fps 240
cargo run -p engine-slice -- --render --sim-budget 30
cargo run -p engine-slice -- --render --sim-budget 30 --sim-near-reserve 25
```

The overlay and the exit summary carry the simulation lines (campaign tick and rate, outstanding catch-up work, events/sec, pending/blocked near/far, lateness, budget exhaustion, the `max_executed_due` / `processed_through` watermark pair, and the lane-domain fingerprints at close); the exit summary additionally reports the **effective configuration** (budget, near reservation with provenance, catch-up bound), the **no-discard time accounting** (`ticks due = campaign + outstanding` over the total advanced simulation time), and **per-tick** time percentiles (one sample per logical tick, catch-up batches included). Elapsed simulation time is never silently discarded: rendering stalls and minimized periods accrue in full — the campaign clock keeps advancing while the application runs, with no pause policy — and only camera movement is capped against post-stall view jumps. The 10 Hz simulation rate is a default for this demonstration workload, configurable via `--rate` — not an engine-wide rate for future systems.

## Stage 4 regions & LOD (`--presence`, `--render --regions`)

Deterministic headless scripted presence drives the RegionDirector — presence is a function of the **logical tick** (frame cadence cannot change transition history), and every scenario prints only logical state, so repeated runs are identical:

```powershell
cargo run -p engine-slice -- --sim --presence stationary
cargo run -p engine-slice -- --sim --presence walk
cargo run -p engine-slice -- --sim --presence tour
```

Expected observations: the `--- regions ---` report — per-region LOD levels, activation/deactivation counts, reconstruction activities and captured/reapplied consequences, queued/blocked/held with the conservation identity, and the `stage4`/`held` fingerprints. `stationary` (at region 0's center) demotes five regions to Dormant — 625 held obligations, zero executions for them, their NPC steps frozen; `tour` cycles all eight regions through deactivate/reactivate with conservation true throughout. Double-run identical output is the determinism gate; the test suite additionally proves cross-cadence equivalence (headless vs 60 Hz vs 240 Hz vs an irregular stall pattern reach identical `stage4-state` fingerprints — fewer executions at lower LOD is the measured work reduction; total CPU-time claims are reported separately, never inferred from execution counts alone).

The graphical mode makes the **camera the live presence source** (frame-sampled external input — genuinely cadence-dependent, with no cross-cadence equivalence claim for camera input, unlike the scripted path):

```powershell
cargo run -p engine-slice -- --render --regions
```

Eight area plates colored by LOD (F green · S blue · E amber · D near-black) mark the region ring; NPC render proxies (small boxes) appear for Full/Simplified regions only and drop on demotion; the camera starts on region 0's plate. The overlay adds region lines (levels, transitions, invalid, reconstruction, activation latency p50/p95); the exit summary prints the full region report including worst-case activation latency and the stage4/held fingerprints. Rendering stays strictly downstream of authoritative simulation state: proxies are derived read-only from the represented world each frame, and the dynamic geometry rides a fenced per-frame-slot persistently mapped buffer — the overlay's validated pattern — drawn through the existing scene pipeline (no new pipeline, layout, or graphics system; the buffer allocates lazily, so the default `--render` path pays nothing).

### Stage 4 Windows acceptance (RTX 5070 Ti / Ryzen 7 9800X3D)

CI's lavapipe regional smokes are **correctness evidence only**; the acceptance below establishes real-GPU behavior and the activation-latency budget. Run from `engine/`; use `--release` wherever latency matters.

1. **Normal graphics regression** — `cargo run -p engine-slice -- --render`: the full Stage 2 checklist still holds (depth ordering, resize, minimize/restore, close/relaunch, `O` overlay, no LNK4098). The default path is untouched by the region system.
2. **Regional graphics + the four LODs** — `cargo run --release -p engine-slice -- --render --regions`: walk (WASD, `Shift` to sprint) from region 0's plate toward region 1's (world +X then +Z, ~75–106 units per hop). Watch plates go green → blue → amber → dark as you leave, and the destination's proxies appear as you approach (velocity lookahead activates it early). A full lap visits all eight plates; every plate should pass through all four states. The overlay's `regions F S E D …` line tracks levels live.
3. **Validation enabled** — `cargo run --release -p engine-slice -- --render --regions --validation on`: zero validation errors/warnings for the whole run, including through region transitions (the exit summary prints the counts; nonzero errors fail the run by design).
4. **Rapid movement + repeated transitions** — sprint back and forth across a boundary: hysteresis prevents oscillation (no per-tick transition thrash); activation/deactivation counters grow only on real crossings; `invalid` stays 0. Re-enter a dormant region repeatedly to exercise activation under backlog.
5. **Minimize/restore + resize** — minimize ~5 s mid-tour: the campaign clock keeps advancing (no pause policy — the sim-time line stays accounted); rendering resumes on restore. Drag-resize: swapchain recreation, `recreations N > 0`, no hangs or artifacts. Headless recreation variants: `cargo run -p engine-slice -- --render --regions --frames 20 --fps 0 --simulate-resize`, and the same with `--replace-pipelines` — a verification scenario that forces every recreation to replace the pipeline state, exercising the surface-format-change lifecycle (the dynamic region buffer must carry across the replacement; resizing alone keeps the surface format and cannot reach that path). The CI regional resize smoke runs the replacement combination with the validation layer active.
6. **Presentation modes + pacing** — repeat 2 with `--present mailbox`, `--present immediate --fps uncapped`, and `--fps 240`: the overlay FPS and cpu/gpu percentiles respond as in Stage 2; the simulation lines stay coherent (conservation, watermark) — note the camera is live input, so logical fingerprints legitimately differ across frame rates (documented; the scripted path is the deterministic one).
7. **Conservation + invalid counters** — every run's exit summary must print `conservation true` and `invalid 0`; anything else is a reportable anomaly.
8. **Activation latency under realistic backlog** — `cargo run --release -p engine-slice -- --render --regions --sim-budget 30`: walk to a dormant region; its reactivation faces real backlog (125 overdue obligations against a 30/tick budget). The exit summary's `activation latency p50/p95/worst` must stay **< 50 ms** — the Stage 4 acceptance budget for a 125-NPC region. That latency is CPU simulation work inside the tick boundary (commit consequences → reconstruct → resume), distinct from GPU render time (the gpu percentiles), presentation blocking (present mode), and frame pacing (the paced-interval percentiles) — four separate measurements, all in the summaries.

## Tested environments

| Environment | Method | Result |
|---|---|---|
| Linux (dev pod, x86_64, rustc 1.99.1, user-local CMake 3.30.5) | direct build/test | green — 148 passed + 1 ignored soak, fmt/clippy clean, headless smoke + presence-scenario double runs + per-LOD/activation-latency measurement tests + lavapipe offscreen render smokes incl. `--render --regions` (correctness only) |
| Windows (windows-latest runner) | CI `engine-ci` | see CI runs |
| Real-window GUI | manual, lead's rig | instructions above (Stage 2 checklist + the Stage 4 acceptance plan); evidence pending the lead's run |

## What is deliberately absent

Physics, ECS, scripting, audio, cooked asset formats, networking, editor, threading/job framework, subsystem registries, render graph, material framework, scene graph — each deferred with a stated trigger (`docs/TECH-ARCHITECTURE.md` §5/§8). No SDL GPU/render APIs are used; Vulkan is reached only through ash (T-12).

Owners: `docs/TECH-ARCHITECTURE.md` (§6 stage records), `docs/DECISIONS.md` (T-01–T-15).
