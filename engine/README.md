# Sektor7 Original Engine — Workspace

> **Status:** Stages 0–4 complete. **Stage 4 (regions & LOD) formally accepted 2026-10-21** (T-15 — independent reviewer PASS at `2e71770` + the lead's physical Windows RTX 5070 Ti acceptance run; record: `docs/STAGE4-WINDOWS-ACCEPTANCE-RECORD-2026-10-21.md`). The Stage 5 persistence & commit pipeline architecture is accepted as the v1.8 reference (T-16) with implementation batch-gated, and the experimental macOS ARM64 port (Mac 2 PASS incl. the physical M1 run) is merged into `main`. **Stage 5 Batch 1 (canonical encoding + full-fidelity spine round-trip) is CLOSED — independent PASS at `2addc19` on the `stage5` branch** (CI green on all three platforms, 199 tests + 1 ignored); the wire format is specified in `docs/STAGE5-B1-WIRE-FORMAT.md`. **Stage 5 Batch 2 (content-addressed revision store + local canonical medium, `engine-persist`) delivered, twice corrected, and finally accepted (HOLD findings A–D: storage validation, suffix absorption, reconciliation verification, lock lifecycle, quarantine safety, then the final D2 namespace-collision corrective) on `stage5` — Batch 2 CLOSED: final re-review PASS at `e36b122`** (CI green on all three platforms; 281 tests + 1 ignored, every historical fingerprint byte-identical; the Windows durability selection gate — custom std provider vs SQLite — is delivered open for the lead's decision at review; implementation record: `docs/STAGE5-B2-STORAGE-RECORD.md`). **Stage 5 Batch 3 (commit pipeline + durable T1 journal + transactional encounter machinery) delivered, four review correctives applied, and accepted — Batch 3 CLOSED 2026-10-22: final independent re-review PASS at `b6e9ca5`** (CI 39813087578 green on all three platforms; 440 tests + 1 ignored, every historical fingerprint byte-identical; six new modules in `engine-persist`; implementation record: `docs/STAGE5-B3-PIPELINE-RECORD.md`). **Stage 5 Batch 4 (application integration — the six-phase SessionCrossing over the real simulation, the four versioned authoritative-state codecs, the structured-key migration, the session snapshot + crash recovery, the encounter and gating demonstrations, the genuine mid-install process-kill proof — both windows) delivered on `stage5` from the B3 closure `b7cd72f` per the lead's formal GO, then corrected per the independent review's four-corrective HOLD (the snapshot-origin interleaved recovery restore, the confirmation/cancellation integration, the fail-closed newest snapshot, the genuine mid-install kill) — stopping for independent Batch 4 PASS/HOLD re-review** (472 tests + 1 ignored, every historical fingerprint byte-identical; the crossing layer in `engine-slice` composing the B2 medium + the B3 pipeline + the real Stage 4 simulation; implementation record: `docs/STAGE5-B4-INTEGRATION-RECORD.md`); Batch 5 remains gated (`docs/TECH-ARCHITECTURE.md` §6; `docs/DECISIONS.md`).

Rust workspace holding the first foundations of the reusable original engine. Scope stays deliberately minimal per stage gate — only what the current authorization ratifies, each mechanism exercised by its own tests (anti-abstraction ledger, `docs/TECH-ARCHITECTURE.md` §8).

## Crates

| Crate | Kind | Contents |
|---|---|---|
| `engine-core` | library | Stage 0: `Tick` clock seed; event spine — deterministic `(due, seq)` queue priority, cross-class causal dependencies validated at schedule time, per-tick budget, execution ledger with first-class lateness; safe `processed_through` watermark distinct from `max_executed` (computed live, retreats on late arrivals, `None` when nothing is provably complete); FNV-1a fingerprints (length-prefixed strings, schema v2); level-filtered diagnostics. **Platform-independent — no SDL, no OS dependencies**. Stage 5 B1: canonical wire primitives (`wire.rs`) + full-fidelity spine save/load with the §5.2 integrity gates |
| `engine-persist` | library | Stage 5 B2: content-addressed revision store + local canonical medium — SHA-256 content addressing with structural domain separation (deps: engine-core + `sha2` only); the versioned hash-chained revision record (`revision-record/1`, P1-flagged idempotency-key shape and campaign-id semantics); the `CanonicalMedium` boundary (three append outcomes, platform `SyncTier` on every `Accepted`, lock-held reconciliation of `Unknown`); `LocalFileMedium` implementing the §5.3 nine-step CAS (persistent never-unlinked `HEAD.lock`, insert-once objects, `HEAD.tmp`+rename linearization, quarantine-never-delete orphan protocol, session-scoped trusted cursor with deterministic walk counters) and the 14-boundary crash-window fault seam driven by real-process tests; **Windows durability: process-crash tier only from std — the SQLite selection gate is open at review**. Stage 5 B3: the structured consequential identity (`ConsequenceId`/`consequence-id/1` + the strict reject-never-wrap packed-Stage4 key conversion), the interval-set occurrence coverage with the two-lane `AwardState` and the class-aware rollback (durable promotions survive failures, holes legitimate), the consequential-state envelope (`envelope/1` — the validated uniform per-occurrence unit, class-homogeneous sub-spans, steps never restorable), the durable T1 journal (`journal/1` — hash-chained transaction frames, the four durability states with the session-scoped acknowledgment, one fsync per completed transaction, CancellationRecord supersession), the `CommitPipeline` (the per-key lattice; canonical confirmation = verified chain inclusion AND individual per-key validation of `consequence-batch/1`; stale/Unknown reconciliation with identity preservation), the policy-free `EncounterJournal`, and the two-contract recovery (the pure-function (anchor, journal, confirmed) reconciliation with the four-tier suppression); the failure-first matrix drives the ten §5.4.1 scenarios at the generic level plus real child-process termination at the full stack |
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

281 tests + 1 ignored soak (158 post-consolidation baseline + 41 Stage 5 Batch 1 save tests: 10 wire primitives, 11 round-trip/continuation/golden, 20 corruption; + 82 Stage 5 Batch 2 storage tests: 14 hash/record codec unit, 56 local-medium unit incl. the corruption/divergence/orphan corpora and O(1)-cursor proofs and the review-corrective regressions (suffix-boundary epoch, dependency ancestry, epoch overflow, multi-revision suffix absorption, reconcile-fails-closed, collision-safe quarantine incl. the evidence/metadata namespace collision, HEAD-absent sweep), 12 real-process integration — multi-writer CAS race, killed lock holder, the 24-case crash-window matrix, ambiguous-acknowledment reconcile+retry, cross-process corrupt-history corpus, repeated-crash recovery, bounded waits, the cross-process multi-revision external suffix, and the error-path lock-lifecycle proofs). The 60-second soak is a measurement, ignored by default:

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

**Completed 2026-10-21 — PASS** (independent reviewer acceptance of `2e71770` + the lead's rig run; evidence and measured results: [`docs/STAGE4-WINDOWS-ACCEPTANCE-RECORD-2026-10-21.md`](../docs/STAGE4-WINDOWS-ACCEPTANCE-RECORD-2026-10-21.md) with the verbatim raw terminal output linked there). The plan below remains the reproducible procedure.

1. **Normal graphics regression** — `cargo run -p engine-slice -- --render`: the full Stage 2 checklist still holds (depth ordering, resize, minimize/restore, close/relaunch, `O` overlay, no LNK4098). The default path is untouched by the region system.
2. **Regional graphics + the four LODs** — `cargo run --release -p engine-slice -- --render --regions`: walk (WASD, `Shift` to sprint) from region 0's plate toward region 1's (world +X then +Z, ~75–106 units per hop). Watch plates go green → blue → amber → dark as you leave, and the destination's proxies appear as you approach (velocity lookahead activates it early). A full lap visits all eight plates; every plate should pass through all four states. The overlay's `regions F S E D …` line tracks levels live.
3. **Validation enabled** — `cargo run --release -p engine-slice -- --render --regions --validation on`: zero validation errors/warnings for the whole run, including through region transitions (the exit summary prints the counts; nonzero errors fail the run by design).
4. **Rapid movement + repeated transitions** — sprint back and forth across a boundary: hysteresis prevents oscillation (no per-tick transition thrash); activation/deactivation counters grow only on real crossings; `invalid` stays 0. Re-enter a dormant region repeatedly to exercise activation under backlog.
5. **Minimize/restore + resize** — minimize ~5 s mid-tour: the campaign clock keeps advancing (no pause policy — the sim-time line stays accounted); rendering resumes on restore. Drag-resize: swapchain recreation, `recreations N > 0`, no hangs or artifacts. Headless recreation variants: `cargo run -p engine-slice -- --render --regions --frames 20 --fps 0 --simulate-resize`, and the same with `--replace-pipelines` — a verification scenario that forces every recreation to replace the pipeline state, exercising the surface-format-change lifecycle (the dynamic region buffer must carry across the replacement; resizing alone keeps the surface format and cannot reach that path). The CI regional resize smoke runs the replacement combination with the validation layer active.
6. **Presentation modes + pacing** — repeat 2 with `--present mailbox`, `--present immediate --fps uncapped`, and `--fps 240`: the overlay FPS and cpu/gpu percentiles respond as in Stage 2; the simulation lines stay coherent (conservation, watermark) — note the camera is live input, so logical fingerprints legitimately differ across frame rates (documented; the scripted path is the deterministic one).
7. **Conservation + invalid counters** — every run's exit summary must print `conservation true` and `invalid 0`; anything else is a reportable anomaly.
8. **Activation latency under realistic backlog** — `cargo run --release -p engine-slice -- --render --regions --sim-budget 30`: walk to a dormant region; its reactivation faces real backlog (125 overdue obligations against a 30/tick budget). The exit summary's `activation latency p50/p95/worst` must stay **< 50 ms** — the Stage 4 acceptance budget for a 125-NPC region. That latency is CPU simulation work inside the tick boundary (commit consequences → reconstruct → resume), distinct from GPU render time (the gpu percentiles), presentation blocking (present mode), and frame pacing (the paced-interval percentiles) — four separate measurements, all in the summaries.

## macOS (Apple Silicon) — experimental `macos-port` build and Mac 2 acceptance procedure

> **Status:** Mac 2 **PASS (2026-10-20)** — implementation and CI green on macOS ARM64, and the physical M1 MacBook Air acceptance (Tests A–F below) passed: rendering with active validation 0/0, region visualization (`invalid 0`, conservation true), a 29-recreation resize run, presentation-mode observations (mailbox→FIFO · immediate 225.9 fps), and single-loader confirmation (the SDK's `libvulkan.dylib` and `libvulkan.1.dylib` both resolve to `libvulkan.1.4.363.dylib`). Full evidence and the preserved limitations: `docs/MACOS-PORT-FEASIBILITY.md` §15. The procedure below remains the reproducible run. macOS support is experimental, developed on `macos-port` (branched from the Stage 4 code at `2e71770`) and **merged into `main` 2026-10-21** as part of the lead-authorized branch consolidation; the `macos-port` branch and worktree remain for the deferred Mac 3. Full audit, stage records, and the loader-consistency investigation: `docs/MACOS-PORT-FEASIBILITY.md`.

**One-time environment setup** (nothing here disables SIP or weakens macOS security):

1. Xcode Command Line Tools: `xcode-select --install` (skip if already installed).
2. CMake ≥ 3.16: check `cmake --version`; if absent, `brew install cmake` (or the cmake.org universal installer).
3. Rust, pinned toolchain: install via rustup (`curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`), then from `engine/` run `rustup toolchain install` — it reads `rust-toolchain.toml` (1.99.1). Verify: `uname -m` must print `arm64`; `rustc -vV` must print `host: aarch64-apple-darwin` and `release: 1.99.1`.
4. Vulkan SDK for macOS (Khronos loader + MoltenVK + validation layers): download the current macOS package from the official LunarG SDK page (https://vulkan.lunarg.com/sdk/home) and run its installer. **Verify rather than assume the layout** — record the output of:
   - `ls -li /usr/local/lib/libvulkan*` (wherever the installer placed the loader — `-i` shows whether `libvulkan.1.dylib` and `libvulkan.dylib` are links to the same file or separate copies; ash loads `libvulkan.dylib`, SDL prefers `libvulkan.1.dylib` — see Test F),
   - `vulkaninfo --summary` if the SDK installed it — the device should be Apple silicon via MoltenVK and the instance extensions should list `VK_KHR_portability_enumeration`,
   - `ls /usr/local/share/vulkan/icd.d 2>/dev/null; ls /usr/local/share/vulkan/explicit_layer.d 2>/dev/null` (ICD/layer registration, adjusted to wherever the SDK installed).
5. **Do not place `libMoltenVK.dylib` beside the engine binary** — SDL's Cocoa loader prefers a bundled MoltenVK over the Khronos loader while ash loads the SDK loader directly, which is the split-loader hazard the acceptance explicitly tests for (feasibility doc §6.3).

**Build and run** (from a clone of `macos-port`):

```sh
git clone https://github.com/Willpatpost/Sektor7.git
cd Sektor7 && git checkout macos-port
cd engine
cargo run -p engine-slice -- --render
```

The first build compiles vendored SDL 3.4.18 (a few minutes). Expected: a 1280×720 window with the Stage 2 scene (three overlapping depth-demo cubes), clean exit on Esc/close. **Capture the full exit summary** — it prints GPU name, API version, present mode, swapchain image count, validation activity/counts, and per-tick timing; that summary is the primary acceptance evidence for every test below.

**Mac 2 acceptance checklist (run from `engine/`; `--release` where latency matters):**

- **A — basic Vulkan rendering:** `cargo run -p engine-slice -- --render`. Window opens; instance creation succeeds with the portability-enumeration flag (the macOS loader advertises it); a physical device (Apple GPU via MoltenVK) is selected; the SDL surface, swapchain, and pipeline initialize; the scene renders with correct depth ordering; presentation continues; clean close. Record GPU name, API version, swapchain image count, present mode, and GPU-timestamp support from the summary.
- **B — regions + LOD:** `cargo run -p engine-slice -- --render --regions` — the eight plates (F green · S blue · E amber · D near-black), NPC proxies appearing/disappearing with camera distance (WASD, `Shift` sprint), coherent overlay region lines; rendering stays read-only-derived from simulation state.
- **C — active validation:** `cargo run --release -p engine-slice -- --render --regions --validation on` — the exit summary must report validation **active** with zero errors and zero warnings (a run where the layer is silently absent does not pass; the summary distinguishes active from absent). Record any layer/loader availability limitation separately from application defects.
- **D — recreation paths:** manual resize, minimize/restore, and relaunch, plus the forced-recreation variants (real Cocoa window — no offscreen driver needed): `cargo run -p engine-slice -- --render --regions --frames 20 --fps uncapped --validation on --simulate-resize` and the same with `--replace-pipelines` — no hangs, `recreations N > 0`, the dynamic region buffer carries across replacement, zero validation errors/warnings. (Numeric `--fps` values are clamped to ≥ 1 FPS — `uncapped` is the finite-frame no-pacing form.)
- **E — presentation behavior:** `--present mailbox` and `--present immediate --fps uncapped` (unsupported modes must fall back to FIFO per existing policy — record the resolved mode), and `--fps 240` for a high-refresh pacing observation with CPU/GPU percentiles. These are portability observations, not first-game performance claims.
- **F — SDL3/ash loader compatibility (the Mac 0 review gate):** build first, then run the binary directly so cargo's own process does not pollute the log:

  ```sh
  cargo build -p engine-slice
  DYLD_PRINT_LIBRARIES=1 ./target/debug/engine-slice --render --frames 60 --fps uncapped 2> dyld.log
  grep -iE 'vulkan|molten' dyld.log
  otool -L ./target/debug/engine-slice
  ```

  The binary links no Vulkan library statically (both APIs dlopen at runtime — that is why the dyld log, not `otool`, is the evidence). Report: which library **files** the process actually loaded (`libvulkan.dylib` vs `libvulkan.1.dylib` vs `vulkan.framework/vulkan` vs MoltenVK directly), whether ash's and SDL's choices resolve to the same underlying file (cross-check the `ls -li` output from setup step 4), the ICD in use, and that Test C's validation was active. An unexplained split-loader configuration blocks acceptance.

**Testing on macOS:** `cargo test --workspace --exclude engine-shell` runs everything multi-threaded; the three SDL-touching engine-shell suites must run one test per process (SDL3 on Apple platforms requires every video (re)init in a process to share the thread that first initialized SDL, while the Rust test harness gives each test a fresh thread — the CI job on this branch does this automatically). To run them manually: `cargo test -p engine-shell --test input_dispatch -- --list --format terse`, then invoke each listed test with `--exact <name>`; same for `lifecycle` and `soak`.

## Tested environments

| Environment | Method | Result |
|---|---|---|
| Linux (dev pod, x86_64, rustc 1.99.1, user-local CMake 3.30.5) | direct build/test | green — 158 passed + 1 ignored soak post-consolidation (148 Stage 4 + 10 macOS-portability), fmt/clippy clean, headless smoke + presence-scenario double runs + per-LOD/activation-latency measurement tests + lavapipe offscreen render smokes incl. `--render --regions` (correctness only) |
| Windows (windows-latest runner) | CI `engine-ci` | see CI runs |
| Real-window GUI (RTX 5070 Ti / Ryzen 7 9800X3D, Windows, rustc 1.99.1) | manual, lead's run — Stage 4 acceptance 2026-10-21 | **PASS** — 148+1 tests, release fingerprints byte-identical, validation err/warn 0/0, activation latency worst 0.06 ms vs the < 50 ms budget, conservation true / invalid 0 ([record](../docs/STAGE4-WINDOWS-ACCEPTANCE-RECORD-2026-10-21.md)) |
| macOS (macos-15 ARM64 runner, `macos-port` branch) | CI `engine-ci` on macos-port | Mac 1 CPU gates green; Mac 2 code green — 158 passed + 1 ignored, exact fingerprints on aarch64-apple-darwin |
| macOS (Apple M1 MacBook Air, macOS 26.6.2, Vulkan SDK 1.4.363.0 + MoltenVK 1.4.2) | manual, lead's run — Mac 2 acceptance Tests A–F | **PASS 2026-10-20** — rendering with active validation 0/0 (1,955 frames / 34.7 s), regions (2,475 frames, `invalid 0`, conservation true), 29-recreation resize run, mailbox→FIFO 60.5 / immediate 225.9 / 240-target→FIFO 60.9 fps, single Khronos loader confirmed (feasibility doc §15) |

## What is deliberately absent

Physics, ECS, scripting, audio, cooked asset formats, networking, editor, threading/job framework, subsystem registries, render graph, material framework, scene graph — each deferred with a stated trigger (`docs/TECH-ARCHITECTURE.md` §5/§8). No SDL GPU/render APIs are used; Vulkan is reached only through ash (T-12).

Owners: `docs/TECH-ARCHITECTURE.md` (§6 stage records), `docs/DECISIONS.md` (T-01–T-15).
