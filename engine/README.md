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

61 tests (33 Stage 0 + 15 Stage 1 + 13 Stage 2: 9 renderer, 4 camera math). The 60-second soak is a measurement, ignored by default:

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

## Tested environments

| Environment | Method | Result |
|---|---|---|
| Linux (dev pod, x86_64, rustc 1.99.1, user-local CMake 3.30.5) | direct build/test | green — 61/61 tests, fmt/clippy clean, headless smoke + 60 s soak + lavapipe offscreen render smoke (correctness only) |
| Windows (windows-latest runner) | CI `engine-ci` | see CI runs |
| Real-window GUI | manual, lead's rig | instructions above; evidence pending the lead's run |

## What is deliberately absent

Physics, ECS, scripting, audio, cooked asset formats, networking, editor, threading/job framework, subsystem registries, render graph, material framework, scene graph — each deferred with a stated trigger (`docs/TECH-ARCHITECTURE.md` §5/§8). No SDL GPU/render APIs are used; Vulkan is reached only through ash (T-12).

Owners: `docs/TECH-ARCHITECTURE.md` (§6 stage records), `docs/DECISIONS.md` (T-01–T-13).
