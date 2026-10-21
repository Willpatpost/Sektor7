# Sektor7 macOS ARM64 Port — Mac 0 Feasibility Audit and Implementation Plan

> **Status:** PROPOSAL under staged execution — experimental platform-portability research on the isolated `macos-port` branch (base `2e71770`, created from `stage4-review`; `stage4-review` and `main` untouched). Not authorized for merge. **Mac 0 PASS** — independent review of `559b2c2`, 2026-10-20. **Mac 1 GO** — same review; scope is CPU portability + macOS CI only (no Vulkan implementation work), with two qualifications folded in below: §6.3 (Vulkan loader consistency is an empirical Mac 2 verification, not an assumption) and §7 (exact ARM64 fingerprint equality is an acceptance condition, not a design proof). **Mac 1 delivered 2026-10-20 and formally closed PASS** — independent review of `160fac3` and run `39512109794`: all jobs green, complete test inventory and exact Stage 0/3/4 fingerprints on `aarch64-apple-darwin`, no blocking findings (record §13 + closure note). **Mac 2 GO 2026-10-20** (implementation authorized, acceptance pending independent review) — **implemented and delivered as `a6375ea` + `bb4379c`** (record §14): CI green on all three platforms with exact fingerprints and validation-ACTIVE lavapipe smokes exercising the new portability-enumeration flag path; the ash macOS loader-name correction (§6.3) and the environment-research findings (§11 R2) are folded in. The independent review accepted the implementation/CI with three narrow follow-ups — **A** extension-plan correctness, **B** enumeration-failure transparency, **C** README pacing commands — **all delivered in `cc198c7` with run `39519390837` green on all three platforms (§14.1)**. **Physical M1 acceptance NOT performed; Mac 2 is NOT declared PASS** — full PASS remains contingent on physical M1 rendering, active validation, loader compatibility, and the reviewer's final verdict. Mac 3, Stage 5, and any merge remain separately gated.
>
> **Scope guardrails (from the port authorization):** retain Rust, SDL3, ash, Vulkan and the existing rendering architecture; no separate native Metal renderer; no gameplay systems, persistence, networking or ECS; portability-enumeration instance extensions/flags only when supported and required; enable `VK_KHR_portability_subset` on devices that advertise it; verify ash/SDL3 loader consistency; treat validation-layer and device-discovery failures transparently; preserve renderer safety and all Stage 4 contracts. Independent of the pending Stage 4 Windows hardware acceptance; Stage 5 remains unauthorized.
>
> **Date:** 2026-10-20 · **Author context:** prepared for lead review · **Evidence convention:** every claim below is either cited to engine code at the pinned base commit, to the vendored dependency sources actually resolved by `engine/Cargo.lock`, or explicitly tagged **VERIFY (Mac 1/Mac 2)** for empirical confirmation.

---

## 1. Executive summary

**The port is feasible, and the required engine change is small and concentrated.** The engine already sits almost entirely on cross-platform substrates: SDL3 (vendored 3.4.18, CMake static build) for windowing/input/surface, ash 0.38 for Vulkan, pure-Rust naga for GLSL→SPIR-V at build time. The whole engine contains **exactly one** `cfg(target_os …)` conditional (a Linux-only RSS probe that already degrades to `None` elsewhere) and **zero** MSVC/x11-specific code in the render path. Determinism is arch-independent by construction (FNV-1a fingerprints over integers and length-prefixed strings; no libm transcendentals in any fingerprinted path).

The real work is concentrated in three places:

1. **Vulkan portability enumeration (Mac 2, engine-renderer/src/vulkan.rs):** today `pick_physical_device` enumerates without `VK_KHR_portability_enumeration` + `VK_INSTANCE_CREATE_ENUMERATE_PORTABILITY_KHR`, so MoltenVK's device is invisible and the renderer fails transparently with `RendererError::NoDevice` (the designed environment-limitation path). Device creation also enables only `VK_KHR_swapchain`; MoltenVK additionally **requires** `VK_KHR_portability_subset` when advertised. Both changes are supported by the pinned ash 0.38.0+1.3.281 (verified in the vendored source). Estimated **35–60 lines in one file**.
2. **CI (Mac 1, .github/workflows/engine-ci.yml):** an additive `macos-15` (ARM64) job mirroring the CPU gates — fmt/clippy/build/test/fingerprint/selftest/sim smokes — plus adding `macos-port` to the push-branch triggers. The existing ubuntu/windows matrix job stays byte-identical. Estimated **50–90 lines of workflow**.
3. **MoltenVK environment plumbing (Mac 2, physical M1 + CI):** Vulkan SDK for macOS (Khronos loader + MoltenVK ICD + validation layers) on the lead's MacBook Air; SDK installation in CI is a verified-mechanism question (see §11 R2).

**No Cargo.toml, lockfile, simulation, spine, or shell changes are expected.** No new dependencies. Estimated total engine-source delta: under ~100 LOC, all in `vulkan.rs`.

The four genuinely open questions (all empirical, none architectural): MoltenVK's `VK_EXT_headless_surface` support (decides whether the offscreen CI smoke path works on macOS), MoltenVK+SDK install mechanism inside GitHub's macOS runners, paravirtualized-Metal behavior on those runners, and M1 integrated-GPU measurement readings (correctness contracts are expected to hold; the Stage 4 activation-latency budget is CPU-side simulation work).

---

## 2. Audit method (what was read)

Personal first-hand read of the engine at the pinned base — build config and manifests (`engine/Cargo.toml`, `engine/rust-toolchain.toml`, `engine/.cargo/config.toml`, all four crate `Cargo.toml`s, `engine/Cargo.lock`), the render path (`engine-renderer/src/vulkan.rs` init/swapchain/pipeline sections, `renderer.rs`, `config.rs`, `build.rs`), the shell (`engine-shell/src/platform.rs`, `app.rs`, manifest), determinism-critical code (`engine-core/src/fingerprint.rs`, `clock.rs`; `engine-slice/src/math.rs`), the CLI surface (`engine-slice/src/main.rs`), `engine/README.md` (acceptance-plan structure), and `.github/workflows/engine-ci.yml`.

Dependency claims were verified against the **pinned, locally vendored sources** rather than recall: `~/.cargo/registry/.../ash-0.38.0+1.3.281/`, `sdl3-sys-0.7.2+SDL-3.4.18/`, and `sdl3-src-3.4.18/SDL/` (the exact SDL sources the static build compiles). Targeted greps over `engine/` for platform conditionals, unordered collections, and libm usage — results cited inline below.

---

## 3. Environmental dependencies

| Requirement | Needed for | Where | Status |
|---|---|---|---|
| Rust 1.99.1, `aarch64-apple-darwin` target | all stages | CI runners (`rustup toolchain install` already in workflow), M1 rig | standard; pinned toolchain file is target-agnostic |
| C compiler + CMake ≥ 3.16 | vendored SDL3 static build | Xcode Command Line Tools (clang) + CMake; both preinstalled on GitHub `macos-15` ARM64 runners | standard (matches existing README build requirements) |
| Vulkan SDK for macOS (Khronos loader `libvulkan.1.dylib`, MoltenVK ICD, `VK_LAYER_KHRONOS_validation`) | Mac 2/3 graphics; optional CI smoke | lead's M1 MacBook Air: LunarG SDK `.pkg`; CI: mechanism to verify (§11 R2) | **VERIFY (Mac 1/2)** for CI; standard for the rig |
| A graphical session | Mac 2 windowed demo, Mac 3 input/lifecycle | M1 rig (real); CI runners have a user session (cocoa driver usable) | rig standard; runner behavior **VERIFY (Mac 1)** |
| Physical Apple M1 MacBook Air | Mac 2 acceptance, Mac 3 verification | lead's hardware | pending the lead's run, mirroring Windows acceptance |

GitHub runner note: pin **`macos-15`** explicitly (ARM64/M1). Do not use `macos-13` (Intel x86_64 — wrong architecture) and avoid `macos-latest` (floating label).

---

## 4. Dependency audit

| Dependency (locked) | macOS ARM64 assessment |
|---|---|
| `sdl3` 0.20 wrapper (`build-from-source-static`, `ash` features) | Pure-Rust wrapper over sdl3-sys; surface creation and instance-extension discovery already flow through it (engine-renderer uses `window.vulkan_instance_extensions()` / `window.vulkan_create_surface(instance.handle())` — SDL boundary, T-09). Compiles wherever sdl3-sys builds. **VERIFY (Mac 1)** = first compile. |
| `sdl3-sys` 0.7.2+SDL-3.4.18 | The `cfg(unix)`-scoped `sdl-unix-console-build` feature (engine-shell/Cargo.toml) maps to SDL's `SDL_UNIX_CONSOLE_BUILD` CMake flag. **Verified inert on macOS:** the vendored SDL 3.4.18 CMake consults that flag only inside `if(UNIX AND NOT (ANDROID OR APPLE OR EMSCRIPTEN OR HAIKU OR RISCOS))` (`SDL/cmake/macros.cmake` ~line 413) — Apple is excluded from the X11/Wayland sanity check, so Cocoa configuration is unaffected. The target-scoped dependency is therefore harmless on macOS; **no change recommended** (minimal-diff discipline). |
| `sdl3-src` 3.4.18 (vendored SDL) | SDL 3.4.18's CMake builds and statically links on macOS ARM64 via Xcode CLT; the Windows-only `CMAKE_TOOLCHAIN_FILE_x86_64_pc_windows_msvc` env in `engine/.cargo/config.toml` is target-suffixed and never read on macOS (by design, per its own comment). No system SDL packages needed — same vendored-native-version discipline as Windows/Linux. |
| `ash` 0.38.0+1.3.281 | Platform-neutral bindings; `Entry::load()` dlopens the loader (`libvulkan.1.dylib` on macOS). **Verified in the pinned source:** `vk::InstanceCreateFlags::ENUMERATE_PORTABILITY_KHR` (`src/vk/extensions.rs:3167`) and the `khr::portability_enumeration` / `khr::portability_subset` extension modules (`src/extensions_generated.rs`) both exist — the Mac 2 changes need no ash upgrade. |
| `libloading` (via ash) | Standard dylib loading; macOS supported. |
| `naga` =29.0.4 (`glsl-in`, `spv-out`) | Pure-Rust build-time GLSL→SPIR-V (`engine-renderer/build.rs`) — no native shader tools on any platform. SPIR-V is consumed by MoltenVK's internal SPIR-V→MSL translation; the shaders are simple (scene/overlay vertex+fragment, no descriptor sets). |
| `bytemuck`, `font8x8` | Platform-neutral (Pod casts; public-domain bitmap data). |
| `engine-core` | Zero dependencies by design (README crate table: "Platform-independent — no SDL, no OS dependencies"). |

**No dependency additions, feature changes, or lockfile changes are required for the port.**

---

## 5. Platform-specific code audit (existing engine)

- **Exactly one `cfg(target_os …)` in the entire engine:** `engine-shell/src/app.rs` `resident_set_kib()` — Linux `/proc/self/status` read; the `#[cfg(not(target_os = "linux"))]` twin already returns `None`, so macOS soak reporting degrades transparently today. An optional macOS `mach task_info` implementation is *out of scope* (not required by any contract; soak RSS lines are measurements, not gates).
- **`cfg(unix)` manifest scoping** (engine-shell's sdl3-sys console-build dep): inert on macOS with vendored-source evidence (§4).
- **Window/surface initialization:** SDL3 wrapper only — no `SDL_WINDOW_VULKAN`-style flags, no platform window handles, no Win32/X11 calls anywhere in the engine (grep over `engine/` finds no `win32`/`x11` symbols). SDL3 dropped the SDL2-era Vulkan window flag; surface creation is `SDL_CreateVulkanSurface` through the wrapper with the ash instance handle.
- **Swapchain extent is capabilities-driven:** `clamp_extent` prefers the surface's `current_extent` and only falls back to the window size on the `u32::MAX` sentinel; MoltenVK reports a definite extent from the CAMetalLayer, so Retina scaling is handled by construction (and the window is created without a high-pixel-density request, so point and pixel sizes coincide).
- **Paths/FS:** only `OUT_DIR`-relative shader outputs and engine-relative CMake file references; no OS path assumptions.
- **Timing:** `std::time::Instant` for pacing/measurements; simulation time is the u64 `Tick` logical clock (`engine-core/src/clock.rs`) — OS time never enters simulation logic.
- **Unordered collections:** `HashSet` appears only in shell input state and test fixtures — never in fingerprinted state (fingerprint.rs documents the ordered-collection rule; sim/spine state uses ordered collections).

---

## 6. Vulkan portability gap analysis (the Mac 2 engine changes)

### 6.1 Change 1 — portability enumeration (instance creation, `VulkanContext::new`)

Current behavior: instance extensions come from SDL's driver-required list (`VK_KHR_surface` + `VK_EXT_metal_surface` on Cocoa; `VK_EXT_headless_surface` on the offscreen driver) plus optional `VK_EXT_debug_utils`; `enumerate_physical_devices` therefore returns an empty list under a conformant loader with only MoltenVK installed → `RendererError::NoDevice`, the designed transparent failure.

Required change (per the authorization's "use portability-enumeration instance extensions/flags when supported and required"):
1. After the SDL extension query, enumerate the loader's instance extensions via `entry.enumerate_instance_extension_properties()`.
2. If `VK_KHR_portability_enumeration` is among them, append it to the enabled set and add `vk::InstanceCreateFlags::ENUMERATE_PORTABILITY_KHR` to the `InstanceCreateInfo`.
3. Leave everything else (validation try/fallback ladder, partial-init cleanup order) untouched. On Windows/Linux/ICD-conformant environments the extension is simply not in the list and behavior is bit-identical to today — the change is **conditional and inert on existing platforms**, preserving Stage 2–4 contracts and the lavapipe CI smokes unchanged.

### 6.2 Change 2 — `VK_KHR_portability_subset` (device creation)

Current behavior: device creation enables only `VK_KHR_swapchain`. MoltenVK always advertises `VK_KHR_portability_subset`, and the Vulkan spec *requires* enabling it when advertised (`VUID-VkDeviceCreateInfo-pProperties-04451`) — without it, `vkCreateDevice` fails on MoltenVK.

Required change: `pick_physical_device` already enumerates device extensions (it checks for swapchain support); extend it to also report whether the chosen device advertises `VK_KHR_portability_subset`, and include `khr::portability_subset::NAME` in the device-extension list when it does. Inert on devices that don't advertise it (NVIDIA/lavapipe paths unchanged).

### 6.3 Loader consistency (ash ↔ SDL3) — unresolved by design analysis; verified empirically at Mac 2

The engine deliberately holds **both** loader handles open (`Entry::load()` and an explicit `SDL_Vulkan_LoadLibrary(null)` before the extension query — the existing comment explains the offscreen-driver dependency). On macOS:

- ash dlopens **`libvulkan.dylib`** — its fixed macOS library name in the pinned vendored source (`ash-0.38.0+1.3.281/src/entry.rs`, cfg macos/ios; **corrected at the Mac 2 preflight — the Mac 0 claim of `libvulkan.1.dylib` was wrong**) — independently of SDL.
- SDL's Cocoa backend searches, in order: `@executable_path/../Frameworks/libMoltenVK.dylib`, **`vulkan.framework/vulkan`**, `libvulkan.1.dylib`, `libvulkan.dylib`, `MoltenVK.framework/MoltenVK`, `libMoltenVK.dylib` (vendored `SDL_cocoavulkan.m`), with `Cocoa_Vulkan_LibraryPathIsUsable` filtering `@executable_path` entries to ones that exist on disk.
- **Qualification (independent Mac 0 review, 2026-10-20; ash name corrected at the Mac 2 preflight):** loader consistency is **not established** by the SDK installation providing the libraries. SDL may resolve `vulkan.framework/vulkan` ahead of `libvulkan.1.dylib`, while ash loads `libvulkan.dylib` — a *different name string* than either of SDL's Vulkan-loader choices. Whether an SDK install's `libvulkan.1.dylib` and `libvulkan.dylib` are links to the same underlying file (single loader) or distinct copies (split loader) is layout-dependent and is exactly what Test F resolves on the M1. If ash and SDL resolve *different* files, SDL's proc addresses and ash's instance dispatch come from different loaders — the split-loader hazard. The original Mac 0 assertion that a system SDK install necessarily makes both resolve the same dylib was an assumption and is withdrawn.
- **Mac 2 acceptance procedure (no Vulkan work in Mac 1):** on the M1 rig, establish the actually resolved loader path on each side — e.g. `DYLD_PRINT_LIBRARIES=1` on a demo run, or comparing the `vkGetInstanceProcAddr` pointer SDL exposes (`SDL_Vulkan_GetVkGetInstanceProcAddr`) against ash's entry — then prove functional compatibility: an ash-created instance accepts an SDL-created surface, and `--validation on` reports the Khronos layer active through both chains. If the resolved paths differ, reconcile via environment (SDK layout / SDL's explicit library-path argument), not by engine code changes.
- **Operational rule for Mac 2/3 (documented, not coded): do not place a `libMoltenVK.dylib` next to the engine binary.** Bundling one would make SDL bypass the Khronos loader while ash keeps it — the same split-loader hazard (existence-filter evidence in the vendored source).

### 6.4 Feature-compatibility table (engine requirements vs MoltenVK expectation)

| Engine requirement (cite) | MoltenVK on Apple-silicon expectation | Status |
|---|---|---|
| Vulkan 1.1 instance API (`make_api_version(0,1,1,0)`) | MoltenVK advertises ≥1.2/1.3 on M1 | supported; exact `api_version` string lands in render stats — **VERIFY (Mac 2)** |
| `VK_KHR_surface` + `VK_EXT_metal_surface` (SDL Cocoa list) | implemented | supported |
| `VK_EXT_debug_utils` + `VK_LAYER_KHRONOS_validation` | SDK ships both; engine already degrades transparently when absent | supported via SDK install |
| Queue: graphics + present on one family | single graphics+compute family with present support | supported |
| `VK_KHR_swapchain`; FIFO present (Auto/FIFO default; Mailbox/Immediate fall back to FIFO — `resolve_present_mode`) | FIFO guaranteed; IMMEDIATE typically available; MAILBOX not | supported (FIFO path) |
| Surface format BGRA8_UNORM + SRGB_NONLINEAR preferred, first-available fallback (`pick_surface_format`) | BGRA8 exposed on Apple silicon | supported |
| Depth D32_SFLOAT → D24 → D16 (`pick_depth_format`, capability-checked) | Depth32Float native | supported |
| Image count `(min+1).max(2)` clamped to max (`SwapchainState::build`) | min 2 typical; engine clamps either way | supported — **VERIFY (Mac 2)** actual count in stats |
| Extent from `current_extent` with sentinel fallback; zero-extent deferral (minimize) | definite current extent reported | supported |
| Classic render pass, 2 subpass-external dependency, push constants (64 B mat4), vertex/index buffers, no descriptor sets | all core Vulkan 1.0–1.1 concepts | supported |
| Host-visible persistently mapped overlay/region buffers + DEVICE_LOCAL depth | unified memory satisfies both property sets trivially | supported |
| 2 frames in flight; classic semaphores/fences; per-image present semaphores; `device_wait_idle` before destroy/recreate; `oldSwapchain` recreation flow | all supported | supported |
| GPU timestamps (queue-family `timestamp_valid_bits` gate; skipped at 0) | Apple-silicon counter support varies by OS/driver | transparent either way — **VERIFY (Mac 2)** readings |
| Dynamic rendering / bindless / timeline semaphores / sync2 | **not used by the engine** (verified across the render path) | n/a |

The engine's Vulkan usage is deliberately classical (Stage 2 authorization shape), which is precisely the subset MoltenVK implements most solidly. No engine requirement was found that MoltenVK is known to lack.

### 6.5 Explicitly not changing

No Metal path, no SDL GPU/render API usage (T-12 stands), no present-mode policy change, no new pipelines/layouts/buffers, no simulation or spine changes, no Cargo features. The two changes above are additive conditionals that are no-ops on every currently-supported environment.

---

## 7. Determinism and fingerprints

- **Fingerprint mechanism is architecture-independent by construction:** FNV-1a 64-bit over `u8/u32/u64` writes and length-prefixed strings, little-endian byte order, no `std` hashers, no unordered iteration (engine-core/src/fingerprint.rs). Cross-platform byte-identity is already proven in CI between MSVC and GNU toolchains on x86_64 (the pinned greps in engine-ci.yml).
- **No libm transcendentals in any fingerprinted path:** the only `.sin/.cos/.tan` uses in the engine are the render-camera helpers (engine-slice/src/math.rs — perspective/rotation matrices), which feed push-constant bytes, never logical state; camera input is documented as cadence-dependent and outside the deterministic contracts. Simulation math is basic IEEE-754 arithmetic (add/mul/div, casts) — exactly specified and identical on aarch64 (Rust compiles without FP contraction; NEON's IEEE-compliance includes subnormals). Counts (`executed 45717`) and hex fingerprints are integers.
- **Conclusion + gate (qualified by the independent Mac 0 review):** design analysis identifies no divergence source, but exact ARM64 fingerprint equality is an **empirical acceptance condition of Mac 1**, not something design already proves — the gate is the pinned CI greps (`engine=6504123a47cab66f`, `state=937bd3b6f56f07e0`, `executed 45717`, `engine-lanes=5591d3eb05643e78`, `state-lanes=9d90cb3bbd45b032`, and per-scenario `stage4=da1b2d1a7e5cd49e` / `1df391ad488848bf` / `495ab27b37549e93`) passing on `aarch64-apple-darwin`. If any value differs on macOS, the response is to **investigate the actual cause** (architecture, libm, formatting) and report it — never to modify historical fingerprints or weaken the gate to obtain a green build. A mismatch stops the stage.

---

## 8. CI design (Mac 1) — additive only

Current state: `engine-ci.yml` runs one `build-and-test` matrix (`ubuntu-latest`, `windows-latest`) — fmt, clippy `-D warnings`, build, `cargo test --workspace`, fingerprint selftest double-run with pinned greps, Stage 3 headless smoke with pinned greps, Stage 4 presence-scenario double-runs, and a **Linux-only** lavapipe offscreen Vulkan smoke (Windows runners expose no Vulkan loader — reported honestly, never silently skipped).

Proposed macOS additions (no existing step, matrix entry, or gate modified):
1. Add `macos-port` to `on.push.branches` (currently `[main, stage4-review]`) so the branch's own pushes verify. The `paths:` filter already covers `engine/**` and the workflow file.
2. New job `macos-arm64` on `runs-on: macos-15` (ARM64), separate from the existing matrix job: install pinned toolchain → **explicit architecture gate** (`uname -m` = `arm64`, `rustc -vV` host = `aarch64-apple-darwin` — the runner label is verified, not trusted) → fmt → clippy → build → test → fingerprint selftest double-run + pinned greps → Stage 3 smoke + pinned greps → Stage 4 presence double-runs **with pinned per-scenario values** (stationary `da1b2d1a7e5cd49e`, walk `1df391ad488848bf`, tour `495ab27b37549e93` — the Mac 1 authorization extends pinning to Stage 4). No GPU, no Vulkan SDK required. Bash steps avoid bash-4 constructs because `shell: bash` on macOS runners resolves to the system bash 3.2.
3. Vulkan smoke on macOS: **staged.** The Linux-only smoke stays Linux-only. A macOS MoltenVK offscreen (or cocoa-driver) smoke is added only after R1/R2 below are verified empirically, initially as its own step in the `macos` job — never as a `continue-on-error` (a silently-failing gate weakens the gates; the project's honesty convention applies: macOS CI Vulkan evidence would be labeled VM/paravirtualized-GPU correctness evidence, exactly as lavapipe runs are labeled software-rasterizer correctness evidence — physical M1 evidence stays with the lead's run).
4. Windows/Linux gates: untouched, byte-identical.
5. Runner resources (Mac 1 review constraint): GitHub's standard `macos-15` runner is an M1 with ~7 GB RAM and limited disk. If parallel compilation exceeds those resources, reduce Cargo/CMake build parallelism — never engine semantics — and `continue-on-error` is prohibited (concealment, not a gate). A green CPU-portability run is **not** graphical Vulkan/MoltenVK acceptance.
6. Test-runner note (Mac 1 correctives, runs 39510072079 / 39510757458): (a) all shell checks added for macOS use herestring `grep -Fq <<< "$var"` forms — BSD `grep -q` exits on the first match, so piping a live producer under `pipefail` can SIGPIPE-fail a matching check (the architecture gate self-terminated this way before the fix); (b) SDL 3.4.18 on Apple platforms asserts every video (re)init happens on the thread that first initialized SDL (vendored `SDL/src/SDL.c`, `#ifdef SDL_PLATFORM_APPLE` — no such assert elsewhere, hence green ubuntu/windows under the default multi-threaded libtest harness), while libtest runs each test on a fresh thread — even with `--test-threads=1`, verified empirically — so engine-shell's repeated-init lifecycle suites aborted by SDL's own assertion. CI-level correction only: the macOS job runs all SDL-free suites with plain `cargo test` and executes the three SDL-touching engine-shell suites (`input_dispatch`, `lifecycle`, `soak`) one test per process (`--exact`), restoring the one-thread-per-process invariant the real application already satisfies. No engine or test changes; every test still runs exactly once; ubuntu/windows jobs untouched.

---

## 9. Implementation plan (Mac 1 → Mac 3)

**Mac 1 — CPU portability (CI-first, zero engine changes expected).**
Add the `macos-15` job + branch trigger (§8). Run. Expected outcomes: build/fmt/clippy/test green; fingerprint + presence greps identical to Win/Linux. Possible (small) findings: platform-specific clippy/formatting noise — fix minimally on `macos-port`. If `sdl3-sys`'s vendored build hits a macOS CMake wrinkle (static-lib linking convention), resolve within the existing vendored-static strategy (no system-SDL switch). Stretch item (optional, non-gating): prove the CI MoltenVK SDK install mechanism and run one offscreen `--render --frames 20 --fps 0` as evidence-gathering for R1/R2.
**Gate to Mac 2:** all pinned greps green on macOS ARM64 — Stage 0, Stage 3, and the Stage 4 per-scenario values — with zero fingerprint drift; existing ubuntu/windows jobs unchanged and green; runner architecture confirmed `arm64` / `aarch64-apple-darwin`.

**Mac 2 — Graphical portability (engine change + physical M1).**
Implement §6.1/§6.2 in `vulkan.rs` (conditional, inert on Win/Linux — re-run the full CI matrix to prove inertness, plus the lavapipe smokes unchanged). On the lead's M1 MacBook Air (after SDK install): `--render` full Stage 2 checklist, `--render --regions` Stage 4 demo (plates/proxies/LOD colors), `--validation on` zero errors/warnings, `--simulate-resize` + `--replace-pipelines` headless recreation checks, loader-consistency observation (§6.3), stats capture (api_version, swapchain image count, present mode, timestamp availability). README gains the macOS acceptance section mirroring the Windows one.
**Gate to Mac 3:** Stage 4 region demo renders correctly under active validation on M1; recreation paths exercise; no contract regression on existing platforms (CI green everywhere).

**Mac 3 — Compatibility verification.**
On M1: repeated regional transitions (tour), rapid boundary crossings (hysteresis/`invalid 0`), minimize/restore + resize with region geometry live, input (WASD/relative-mouse/focus-loss clearing), conservation identity in every summary, activation-latency budget under `--sim-budget 30` (< 50 ms — CPU-side work; spine p50/p95 on the dev pod are ~0.09/0.19 ms, so the budget has two orders of margin even before M1 speeds), relaunch stability. Results compared against the Windows/Linux evidence table and recorded (README tested-environments table + this doc's successor record).
**Exit:** comprehensive report; merge decision remains the lead's, per the no-merge-without-review constraint.

---

## 10. Estimate of required code changes

| File | Change | Est. size |
|---|---|---|
| `engine/crates/engine-renderer/src/vulkan.rs` | portability-enumeration discovery + conditional instance flag; `portability_subset` detection + conditional device extension | **35–60 LOC**, one file |
| `.github/workflows/engine-ci.yml` | `macos-port` branch trigger + new additive `macos-15` job | **50–90 LOC** workflow |
| `.github/workflows/engine-ci.yml` (Mac 2, conditional) | macOS Vulkan smoke step + SDK install (only after R1/R2 verified) | +30–60 LOC |
| `engine/README.md` (Mac 2/3) | macOS build/acceptance sections + tested-environments row | docs |
| `docs/MACOS-PORT-FEASIBILITY.md` | this document | docs |
| Cargo manifests / lockfile / engine-core / engine-shell / engine-slice | **no changes** | 0 |

Total engine-source delta below ~100 LOC, concentrated in one file whose invariants manifest (ownership/lifetime/sync/destruction) is untouched — both changes are pure additions to existing create-info assembly, well inside the reviewed Stage 2/4 structure.

---

## 11. Risks and unknowns (each with its resolution stage)

- **R1 — `VK_EXT_headless_surface` on MoltenVK.** SDL's offscreen driver advertises/uses it (vendored `SDL_offscreenvulkan.c`, optional mode); whether MoltenVK implements it decides if the offscreen CI smoke path works on macOS as-is. Resolution: Mac 1 stretch / Mac 2 (fallback: cocoa-driver windowed smoke on runners, or macOS Vulkan CI deferred to physical hardware — Linux lavapipe coverage remains the correctness gate either way).
- **R2 — Vulkan runtime installation on macOS (advanced at Mac 2, not closed).** The Mac 2 research probe (run `39517277409`, macos-15) established: the runner has **no preinstalled Vulkan components** (no loader, no `VULKAN_SDK`, no `vulkaninfo`); Homebrew 6.0.22 provides bottled **`molten-vk` (1.4.2)**, **`vulkan-loader` (1.4.357.0)**, and **`spirv-tools` (1.4.357.0)** formulae (verified by `brew info` on the runner — an earlier pod-side check that reported them absent was a methodology error: the GitHub contents API was queried without the `.rb` filename extension); and both LunarG `latest/mac/vulkan-sdk.tar.gz` and `.dmg` URLs are live (HTTP 200, verified from both the pod and the runner). Remaining unverified: whether `brew install molten-vk vulkan-loader` yields a complete working stack (loader + ICD discovery + validation layers — no `vulkan-validationlayers` formula was probed), MoltenVK-on-VM Metal behavior (R3), and `VK_EXT_headless_surface` on MoltenVK 1.4.2 (R1). No guessed mechanisms enter any required gate; a future authorized install-and-smoke experiment is the natural continuation.
- **R3 — Paravirtualized Metal on macOS runners.** GitHub ARM runners are VMs with paravirtualized GPUs; Metal (hence MoltenVK) is commonly usable, but this is empirical. Resolution: same stage as R1/R2.
- **R4 — MoltenVK swapchain particulars** (min/max image count, IMMEDIATE availability, timestamp valid bits). Engine already clamps/falls back/skips by design; verify actual stats at Mac 2. Measurement differences, not correctness risks.
- **R5 — M1 MacBook Air integrated-GPU performance.** Correctness contracts don't depend on throughput; the Stage 4 activation-latency budget is CPU simulation work with large margin. Mac 3 records honest measurements labeled as such (not first-game performance claims, per the standing instrument caveat).
- **R6 — macOS GUI quirks** (relative-mouse capture, focus events, dock minimize behavior, menu bar). SDL3 Cocoa implements all; Mac 3 verifies against the existing shell contracts (focus-loss clearing, minimize deferral, sticky quit).
- **R7 — Environment error transparency.** Already designed in: `NoLoader`/`NoDevice`/validation-absent degrade with honest messages; macOS inherits these paths unchanged (the current `NoDevice` message under bare macOS without the Mac 2 change is the expected transparent failure, verified by design review).
- **R8 — Float formatting in greppable output.** CI greps pin only integer counts and hex fingerprints; Rust float formatting is platform-stable. No risk identified; covered empirically by the Mac 1 gates.

No risk identified in: dependencies (§4), determinism (§7), or renderer architecture (§6.4) — the port touches the smallest possible surface the Stage 2–4 authorizations created.

---

## 12. Evidence appendix (key citations)

- Instance/device creation, extension assembly, validation fallback: `engine/crates/engine-renderer/src/vulkan.rs` (`VulkanContext::new`, `pick_physical_device`, device-create block; `RendererError` variants).
- Present-mode/depth/format/image-count/extent policies: same file (`pick_surface_format`, `pick_depth_format`, `SwapchainState::build`, `clamp_extent`) + `engine-renderer/src/config.rs` (`resolve_present_mode`).
- Loader duality: `vulkan.rs` `Entry::load()` + `SDL_Vulkan_LoadLibrary(null)` comment; SDL Cocoa search order + existence filter: vendored `SDL/src/video/cocoa/SDL_cocoavulkan.m`; offscreen headless-surface optionality: vendored `SDL/src/video/offscreen/SDL_offscreenvulkan.c`.
- ash portability API: vendored `ash-0.38.0+1.3.281/src/vk/extensions.rs` (`ENUMERATE_PORTABILITY_KHR`), `src/extensions_generated.rs` (`khr::portability_enumeration`, `khr::portability_subset`).
- Console-build inertness on Apple: vendored `SDL/cmake/macros.cmake` (`if(UNIX AND NOT (ANDROID OR APPLE OR EMSCRIPTEN OR HAIKU OR RISCOS))`).
- Single platform conditional: `engine/crates/engine-shell/src/app.rs` (`resident_set_kib`); `cfg(unix)` manifest scoping: `engine/crates/engine-shell/Cargo.toml`.
- Fingerprint/determinism: `engine/crates/engine-core/src/fingerprint.rs`; `engine-core/src/clock.rs`; grep over `engine/` — transcendentals only in `engine-slice/src/math.rs` (render camera).
- CI gates and pinned values: `.github/workflows/engine-ci.yml`; acceptance-plan structure to mirror: `engine/README.md`.

## 13. Mac 1 record (2026-10-20) — CPU portability delivered; CI green on macOS ARM64

**Scope delivered (per the Mac 1 GO):** `macos-port` push trigger; additive `macos-15` ARM64 job mirroring the CPU gates with an explicit architecture gate and pinned Stage 0/3/4 fingerprints; the two Mac 0 review qualifications folded into §6.3/§7; herestring `grep -Fq` forms per the review follow-up. **No engine source, test, manifest, or lockfile changes**; the ubuntu/windows matrix job is byte-identical to the reviewed workflow in every run below.

**Commits (`macos-port`, base `2e71770`):**
- `473df26` — Mac 1 implementation (job + trigger + doc qualifications).
- `8b9a5dd` — corrective 1: architecture-gate shell fix (BSD `grep -q` early-exit + `pipefail` SIGPIPE self-terminated a correctly-matching host; runner `arm64` and host `aarch64-apple-darwin` were both confirmed in that run's log).
- `13ee359` — corrective 2: herestring grep forms throughout the macOS job + SDL Apple video-thread handling (one test per process for the three SDL-touching engine-shell suites).
- This commit — Mac 1 record.

**CI evidence (workflow `engine-ci`):**
- **`39512109794` (commit `13ee359`) — all three jobs green** (`ubuntu-latest`, `windows-latest`, `macos-15 ARM64`; macOS job 3 min 22 s): https://github.com/Willpatpost/Sektor7/actions/runs/39512109794
- Honest failure trail: `39510072079` (`473df26` — macOS gate shell bug; ubuntu/windows green) and `39510757458` (`8b9a5dd` — SDL Apple video-thread assertion; ubuntu/windows green; fmt/clippy/build already green on macOS with 79 tests green before the abort).

**Runner architecture (gated, not assumed):** `uname -m` = `arm64`; `rustc -vV` host = `aarch64-apple-darwin` (rustc 1.99.1, 58cf8f9c3 2026-10-13); pinned toolchain installed from `rust-toolchain.toml`.

**macOS ARM64 results (run 39512109794):**
- **Format check, Clippy (`-D warnings`), workspace build: green.** The vendored SDL 3.4.18 CMake static build compiled cleanly with no macOS-specific flags; the 7 GB M1 runner needed no parallelism reduction.
- **Tests: 148 passed + 1 ignored — identical to ubuntu/windows.** 133 passed via the multi-threaded SDL-free run (`cargo test --workspace --exclude engine-shell`); 15 passed via one-test-per-process for the SDL-touching suites (`input_dispatch` 13, `lifecycle` 2); the 60 s soak measurement stays ignored by design (its per-process invocation reports `0 passed; 1 ignored`). SDL3's Apple-only video-thread assertion (§8.6) is satisfied by construction in the per-process runs.
- **Stage 0 (double-run identical):** `engine=6504123a47cab66f`, `state=937bd3b6f56f07e0` — exact historical values.
- **Stage 3, 300 ticks (double-run identical):** `executed 45717`, `engine-lanes=5591d3eb05643e78`, `state-lanes=9d90cb3bbd45b032` — exact historical values.
- **Stage 4 presence (double-run identical per scenario):** `stage4=da1b2d1a7e5cd49e` (stationary), `stage4=1df391ad488848bf` (walk), `stage4=495ab27b37549e93` (tour) — exact historical values. **The review's acceptance condition B is met empirically: ARM64 output is byte-identical to the pinned cross-platform values.**

**macOS-specific corrections required:** two, both CI-workflow-level and outside engine source — (1) the `grep -q`/`pipefail` gate bug; (2) the SDL-on-Apple video-thread invariant vs the multi-threaded libtest harness (one test per process for the SDL suites; ubuntu/windows keep the default harness — the assertion is Apple-only in the vendored SDL source). Zero engine, test, dependency, or gate changes.

**Remaining blockers for Mac 2 (Mac 2 is not authorized):**
1. Independent review of this Mac 1 record (Mac 1 was on HOLD until the review gates passed; the green run above satisfies the stated gates).
2. Vulkan loader resolved-path verification on the M1 rig (§6.3 — empirical; SDL may resolve `vulkan.framework/vulkan` before `libvulkan.1.dylib`).
3. The §6.1/§6.2 engine changes (portability enumeration + `VK_KHR_portability_subset`) — not yet made; no Vulkan work was done in Mac 1.
4. MoltenVK/SDK environment on the lead's M1 MacBook Air (R2); CI MoltenVK smoke feasibility (R1/R3) remains staged and non-gating.

A green CPU-portability run is **not** graphical Vulkan/MoltenVK acceptance (§8.5).

### Mac 1 closure — independent review PASS (2026-10-20)

Independent review of checkpoint `160fac3`, the complete commit diff since Mac 0, and run `39512109794` (including the macOS execution logs): **Mac 1 PASS, formally closed.** All three CI jobs green; native ARM64 architecture and pinned toolchain confirmed; formatting, Clippy, build, and the complete current test inventory passed; Stage 0/3/4 historical fingerprints reproduced exactly on ARM64; SDL3 Apple video-thread test isolation accepted; no engine source, manifest, dependency, or historical fingerprint changes; Windows/Linux workflow gates unchanged and green. No blocking findings.

**Non-blocking maintenance note (recorded per the review):** future engine-shell test suites must be incorporated into the macOS test-isolation step — the one-test-per-process suite list in `engine-ci.yml` currently names `input_dispatch`, `lifecycle`, `soak`, and a new suite is excluded by `cargo test --workspace --exclude engine-shell`, so one absent from the list would silently skip macOS coverage. To be hardened (dynamic suite enumeration or an explicit checklist rule) at the next authorized workflow touch; until then, adding an engine-shell suite requires updating the list in the same change.

**Mac 2 remains ON HOLD pending explicit authorization.** No Vulkan implementation, no repository branch changes, no assumed MoltenVK/SDL loader compatibility. The original Mac 2 scope and acceptance requirements (§6, §9) remain in force; Mac 3, Stage 5, and any merge stay separately gated.

*Mac 1 record closed. No engine source was changed at any point in Mac 1; the `macos-port` branch carries the audit document, the CI additions, this record, and the closure note.*

## 14. Mac 2 implementation record (2026-10-20) — portability changes delivered; automated verification green; physical M1 pending

**Authorization:** Mac 2 GO (implementation authorized, acceptance pending independent review), authorized base `27ff9fb`. Preflight verified the branch clean at that commit and the engine byte-identical to the audited base `2e71770` (the only intervening changes were CI and docs).

**Implementation (commit `a6375ea`):**
- **Change A — portability enumeration** (`engine-renderer/src/vulkan.rs`, `VulkanContext::new`): a pure `InstanceExtensionPlan` assembles the instance-extension list — SDL-required extensions first (order preserved, no duplicates), `VK_KHR_portability_enumeration` appended when the loader advertises it (enumerated via `entry.enumerate_instance_extension_properties(None)`; enumeration failure is a transparent environment limitation — an empty advertised set preserves pre-Mac-2 behavior exactly), validation `VK_EXT_debug_utils` last. The validation fallback removes debug-utils **by name** (`without_validation`), structurally eliminating the Mac 0 `pop()` ordering hazard the authorization §4 flagged; `create_flags()` derives `ENUMERATE_PORTABILITY_KHR` from the *same* plan in both creation attempts, so flag and extension cannot diverge and a missing validation layer can never hide portability devices.
- **Change B — portability subset** (same file, `pick_physical_device` + device creation): a pure `device_extension_capabilities` scan keeps `VK_KHR_swapchain` mandatory for candidacy and records the selected device's `VK_KHR_portability_subset`; the result rides the existing selection tuple (no second selection path, no capability framework); `select_device_extensions` builds the device-extension list — swapchain always, portability subset only when advertised (required by `VUID-VkDeviceCreateInfo-pProperties-04451`; MoltenVK device creation fails without it). Inert on devices that do not advertise it.
- **Tests:** 7 new pure policy tests in `vulkan::policy_tests` covering the authorization §7 minimum list: advertised→enabled+flag, absent→existing behavior, SDL preservation without duplicates, fallback keeps portability while dropping only debug-utils, validation-off adds nothing, subset enable/omit, swapchain stays mandatory.
- **Docs/tooling:** `engine/README.md` gains the macOS build + Mac 2 M1 acceptance procedure (environment setup with verification commands, Tests A–F incl. the `DYLD_PRINT_LIBRARIES` loader-evidence gate, the no-bundled-MoltenVK rule, SDL test-isolation instructions); optional research workflow `.github/workflows/macos-molvk-research.yml` (§8 evidence-gathering, non-gating, self-triggering on its own file because `workflow_dispatch` cannot resolve branch-only workflows — fix landed as `bb4379c`).
- **Diff surface:** `vulkan.rs` (+337/−25 — policy helpers, wiring, tests), `README.md` (+47), the new workflow file. **No simulation, spine, shell, manifest, or lockfile changes; no dependency upgrades; no engine file other than `vulkan.rs` was touched.**

**Automated verification:**
- **Dev pod (x86_64):** fmt clean; clippy `-D warnings` clean; build green; **155 passed + 1 ignored** (148 existing + 7 new); Stage 0/3/4 fingerprints exact, double-run identical; all five lavapipe offscreen render smokes green **with the flag path active** — the pod's loader (Ubuntu 24.04 `libvulkan1` 1.3.275.0-1build1, the exact package CI installs) was probed directly and advertises `VK_KHR_portability_enumeration`; the `--validation on` runs exercised the transparent fallback (no layer on the pod — the loader's own message was recorded and the runs proceeded and exited cleanly).
- **CI run `39516476270` (commit `a6375ea`): all three jobs green** — https://github.com/Willpatpost/Sektor7/actions/runs/39516476270. Ubuntu: the five lavapipe smokes **validation-ACTIVE, `err/warn 0/0` each, swapchain `recreations` 0/2 as scripted** — the inertness gate for change A on a conformant ICD, with the flag path exercised (the runner log confirms `libvulkan1 1.3.275.0-1build1`, the same package the pod probe showed advertising the extension). Windows: green. macos-15 ARM64: fmt/clippy/build green, **155 passed + 1 ignored** (identical totals to the pod; engine-renderer 24 in the SDL-free run), all pinned fingerprints exact on `aarch64-apple-darwin`, the SDL one-test-per-process isolation unchanged.
- **Gate preservation:** the ubuntu/windows `engine-ci` jobs are byte-identical to the reviewed workflow (engine-ci was not modified in this stage); no `continue-on-error`; no fingerprint changes anywhere; the research workflow is separate, non-gating, and triggers only on its own file or manual dispatch.

**Loader investigation (authorization §6):** vendored-source facts re-verified at the Mac 2 preflight — SDL's Cocoa search order and existence filter (`SDL_cocoavulkan.m`), and **ash's macOS `LIB_PATH = "libvulkan.dylib"`** (`entry.rs`) — the latter correcting the Mac 0 §6.3 claim (§6.3 above is updated). The ash/SDL name-string difference (`libvulkan.dylib` vs SDL's preferred `libvulkan.1.dylib` / possible `vulkan.framework/vulkan`) sharpens the split-loader question rather than settling it: consistency depends on whether those names resolve to the same underlying file on the actual SDK install. **Established from the pod/runner:** the loader's extension advertisement (probe), the runner's empty Vulkan environment, and the installation mechanisms (brew formulae exist and are bottled; LunarG URLs live). **Not established without the M1:** the actually resolved loader files, ICD selection, and function-level compatibility — Test F in `engine/README.md` is the procedure (`DYLD_PRINT_LIBRARIES` on the directly-invoked binary + `ls -li` cross-check + `vulkaninfo`), with the split-loader case remaining the Mac 2 stop condition per the authorization. No speculative workaround was implemented.

**Research probe (run `39517277409`, optional §8 evidence-gathering):** macos-15-arm64, macOS 15.7.9 — no preinstalled Vulkan components; Homebrew 6.0.22 with bottled `molten-vk` 1.4.2 / `vulkan-loader` 1.4.357.0 / `spirv-tools` 1.4.357.0 (all not installed); LunarG `tar.gz` and `dmg` URLs HTTP 200 from the runner; `system_profiler` surfaced no GPU/Metal lines (R3 — Metal behavior in the runner VM remains unverified). Findings folded into R2 above; R1 (headless surface on MoltenVK 1.4.2) remains open.

**Physical M1 acceptance: NOT PERFORMED.** The reproducible procedure (environment setup with verification commands, Tests A–F, evidence capture) is in `engine/README.md`; nothing is recorded as verified that was not run. CI compilation and software-ICD correctness are not graphical acceptance.

**Status:** implementation complete ✓ · automated tests and CI green on all three platforms ✓ · Vulkan runtime environment on macOS hardware NOT yet verified · physical M1 graphics NOT performed · independent review PENDING. **Mac 2 is not declared PASS by this record.**

*Mac 2 stops at the independent review checkpoint. Mac 3, Stage 5, and any merge remain unauthorized.*

### Mac 2 review follow-ups A–C — corrective record (2026-10-20, commit `cc198c7`)

The independent review of `c58ad16` accepted the implementation and CI and required three narrow corrections; all three are delivered in `cc198c7` and verified end-to-end (physical acceptance remains HOLD pending the M1 run; PASS is not declared):

- **A — extension-plan correctness:** `InstanceExtensionPlan` no longer stores an extension list — the list is **derived** from components: SDL's required entries (now deduplicated, first occurrence first, via `dedup_keep_first`), the portability-enumeration decision, and validation-**added** debug-utils state. `without_validation` clears only the validation-added component, so an SDL-required extension (including an SDL-required `VK_EXT_debug_utils`) can never be removed by the fallback, and no input can produce a duplicate entry — both review properties are structural, not list surgery. Focused tests added: SDL-input deduplication; fallback preserves an SDL-required debug-utils, unduplicated.
- **B — failure transparency:** the `vkEnumerateInstanceExtensionProperties` Err branch no longer disappears into `unwrap_or_default` — it queues an explicit warning through the renderer's existing validation-diagnostics channel (`queue_environment_message`: "…failed (…) — `VK_KHR_portability_enumeration` not enabled: environment limitation, not an application error"), which every run summary drains; behavior is otherwise unchanged (empty advertised set → pre-Mac-2 instance creation). Focused test added: the queue receives the message as a warning. Healthy loaders still report `err/warn 0/0` — enumeration succeeds on the pod and on CI, so the new warning never fires in a healthy environment.
- **C — README smoke commands:** the macOS section's finite-frame commands now use `--fps uncapped` (numeric `--fps` clamps to ≥ 1 FPS, so `--fps 0` was pacing Tests D/F at 1 FPS); the clamp is documented inline. The pre-existing Stage 4 CI and Windows-acceptance commands are untouched — reviewed gates — and the global frame-pacing policy is unchanged.

**Verification:** local fmt/clippy (`-D warnings`)/build green; **158 passed + 1 ignored** (155 + 3 new tests); Stage 0/3/4 fingerprints exact, double-run identical; all five lavapipe render smokes green with the flag path active. **CI run `39519390837` (commit `cc198c7`): all three jobs green** — https://github.com/Willpatpost/Sektor7/actions/runs/39519390837 (macos-15 ARM64: 158 passed + 1 ignored, engine-renderer 27, all pinned fingerprints exact; ubuntu: the five validation-ACTIVE lavapipe smokes `err/warn 0/0`; windows green; engine-ci jobs byte-identical).

*The follow-up corrections are submitted for independent review. Mac 3, Stage 5, and any merge remain unauthorized.*
