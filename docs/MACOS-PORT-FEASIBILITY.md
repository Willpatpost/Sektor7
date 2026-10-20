# Sektor7 macOS ARM64 Port — Mac 0 Feasibility Audit and Implementation Plan

> **Status:** PROPOSAL under staged execution — experimental platform-portability research on the isolated `macos-port` branch (base `2e71770`, created from `stage4-review`; `stage4-review` and `main` untouched). Not authorized for merge. **Mac 0 PASS** — independent review of `559b2c2`, 2026-10-20. **Mac 1 GO** — same review; scope is CPU portability + macOS CI only (no Vulkan implementation work), with two qualifications folded in below: §6.3 (Vulkan loader consistency is an empirical Mac 2 verification, not an assumption) and §7 (exact ARM64 fingerprint equality is an acceptance condition, not a design proof). Mac 2/3 remain gated on independent review after Mac 1 delivery.
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

- ash dlopens `libvulkan.1.dylib` (its fixed macOS library name), independently of SDL.
- SDL's Cocoa backend searches, in order: `@executable_path/../Frameworks/libMoltenVK.dylib`, **`vulkan.framework/vulkan`**, `libvulkan.1.dylib`, `libvulkan.dylib`, `MoltenVK.framework/MoltenVK`, `libMoltenVK.dylib` (vendored `SDL_cocoavulkan.m`), with `Cocoa_Vulkan_LibraryPathIsUsable` filtering `@executable_path` entries to ones that exist on disk.
- **Qualification (independent Mac 0 review, 2026-10-20):** loader consistency is **not established** by the SDK installation providing both libraries. SDL may resolve `vulkan.framework/vulkan` ahead of `libvulkan.1.dylib` depending on the SDK's install layout and the dlopen search configuration, while ash always loads `libvulkan.1.dylib` on its own. If the two resolve *different* library files, SDL's proc addresses and ash's instance dispatch come from different loaders — the split-loader hazard. The original Mac 0 assertion that a system SDK install necessarily makes both resolve the same dylib was an assumption and is withdrawn.
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
- **R2 — Vulkan SDK install inside GitHub macOS runners.** Candidate mechanisms: LunarG SDK `.dmg` + `hdiutil` + `sudo installer` (runners have sudo), or a Homebrew-provided path if one exists at implementation time; cacheable via `actions/cache`. Resolution: Mac 1 stretch — verified mechanism or explicit deferral, no guessed URLs in required gates.
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

*Mac 0 stops here for review. No engine source was changed; the `macos-port` branch carries only this document.*
