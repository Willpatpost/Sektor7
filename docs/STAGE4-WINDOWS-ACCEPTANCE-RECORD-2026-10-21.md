# Sektor7 — Stage 4 Windows acceptance record (2026-10-21)

**Verdict: PASS.** The independent reviewer formally accepted Stage 4 (regions & LOD) at `2e7177077656c4cbd97d0d56aa0dd02c64da9fea`, and the lead's physical Windows rig run satisfied the 8-item acceptance plan in `engine/README.md` ("Stage 4 Windows acceptance"). Verbatim raw terminal evidence: [`STAGE4-WINDOWS-ACCEPTANCE-RAW-2026-10-21.txt`](STAGE4-WINDOWS-ACCEPTANCE-RAW-2026-10-21.txt) (username in paths redacted; no measured value, error line, or output altered or omitted; the original paste is retained unchanged outside the repository).

## Tested configuration

- **Commit / branch:** `2e7177077656c4cbd97d0d56aa0dd02c64da9fea` on `stage4-review`, clean working tree — the frozen Stage 4 checkpoint, unmodified.
- **Date:** October 20–21, 2026.
- **Rig:** the lead's Windows workstation (PowerShell session). Device per every render run: **discrete NVIDIA GeForce RTX 5070 Ti, Vulkan 1.4.351**. The acceptance plan names the rig as RTX 5070 Ti / Ryzen 7 9800X3D (the CPU is not independently logged in the terminal output; the Windows version was not recorded in the session). Surface: 3 swapchain images · 1280x720 · supported present modes fifo/fifo-relaxed/mailbox/immediate/other.
- **Toolchain:** rustc 1.99.1 (58cf8f9c3 2026-10-13) · cargo 1.99.1 (5f94df478 2026-08-27).
- **Session capture:** Start-Transcript/Stop-Transcript to `%TEMP%\Sektor7-Stage4-Windows.txt` on the rig; the paste reproduced in the raw-evidence file is the same session.

## Commands executed (verbatim from the raw evidence)

```powershell
git branch --show-current; git rev-parse HEAD; git status --short
rustc --version; cargo --version
cargo test --workspace
cargo build --release -p engine-slice
.\target\release\engine-slice.exe --sim --presence stationary
.\target\release\engine-slice.exe --sim --presence walk
.\target\release\engine-slice.exe --sim --presence tour        # run twice
.\target\release\engine-slice.exe --render
.\target\release\engine-slice.exe --render --regions --validation on
.\target\release\engine-slice.exe --render --regions --validation on --frames 20 --fps uncapped --simulate-resize
.\target\release\engine-slice.exe --render --regions --validation on --frames 20 --fps uncapped --simulate-resize --replace-pipelines
.\target\release\engine-slice.exe --render --regions --present fifo --fps 240
.\target\release\engine-slice.exe --render --regions --present mailbox --fps 240
.\target\release\engine-slice.exe --render --regions --present immediate --fps uncapped
.\target\release\engine-slice.exe --render --regions --validation on --sim-budget 30
```

## Evidence — four categories, kept distinct

### 1. Automated tests (rig, debug profile)

`cargo test --workspace`: **148 passed · 0 failed · 1 ignored** (the 60 s soak measurement, ignored by design). `cargo build --release -p engine-slice`: succeeded (39.71 s).

### 2. Physical GPU measurements (release profile, RTX 5070 Ti)

**Headless deterministic simulation** — byte-identical with the frozen pod/CI fingerprints (cross-platform and cross-profile determinism):

| scenario | stage4 | held | activations / deactivations | invalid | conservation |
|---|---|---|---|---|---|
| stationary | `da1b2d1a7e5cd49e` | `debd94d1d5adaf80` | 0 / 5 | 0 | true |
| walk | `1df391ad488848bf` | `3fd88e07f7a84fe0` | 5 / 10 (replay 2 · synthesis 3) | 0 | true |
| tour (run 1) | `495ab27b37549e93` | `e6d7019c30efe6c9` | 7 / 11 (replay 3 · synthesis 4) | 0 | true |
| tour (run 2) | identical | identical | identical | 0 | true |

Tour detail: 18,150 executed · reconstruction 16,230 activities · captured 615 · reapplied 125 · queued 500 / held 500.

**Render runs** (all 1280x720 · 3 swapchain images; "validation" = that run's layer state; every run printed `validation err/warn 0/0`):

| run | frames / time | fps avg | cpu p50/p95/p99 ms | gpu p50/p95/p99 ms | recreations | validation |
|---|---|---|---|---|---|---|
| `--render` (Stage 2 baseline) | 1304 / 30.2 s | 43.1 | 0.52 / 0.90 / 4.19 | 0.04 / 0.06 / 0.07 | 3 | off |
| `--render --regions --validation on` | 2467 / 77.6 s | 31.8 | 0.22 / 0.49 / 0.54 | 0.01 / 0.01 / 0.01 | 0 | **on** |
| forced resize (20 frames, uncapped) | 20 / 0.1 s | 278.3 | 0.88 / 14.55 / 27.70 | 0.05 / 0.31 / 0.31 | 1 | **on** |
| forced resize + `--replace-pipelines` | 20 / 0.1 s | 232.5 | 0.69 / 21.59 / 35.03 | 0.05 / 0.07 / 0.07 | 1 | **on** |
| `--present fifo --fps 240` | 1327 / 11.5 s | 115.9 | 0.64 / 1.04 / 1.14 | 0.05 / 0.08 / 0.09 | 0 | off |
| `--present mailbox --fps 240` | 914 / 4.1 s | 224.0 | 0.68 / 1.04 / 1.14 | 0.05 / 0.05 / 0.06 | 0 | off |
| `--present immediate --fps uncapped` | 7373 / 4.2 s | 1744.3 | 0.54 / 0.62 / 0.72 | 0.05 / 0.06 / 0.08 | 0 | off |
| `--validation on --sim-budget 30` | 2492 / 51.4 s | 48.4 | 0.88 / 1.19 / 1.41 | 0.05 / 0.09 / 0.09 | 0 | **on** |

Simulation lines held everywhere: `ticks due = campaign + outstanding` accounted with outstanding 0 in every run; per-tick spine execution and region-director tick-boundary timing reported as separate labeled measurements (77.6 s regional run: spine p50 0.000 / p95 0.032 ms · director p50 0.000 / p95 0.001 ms; budget-30 run: spine p50 0.013 / p95 0.028 · director p50 0.001 / p95 0.001, 514 samples each). Camera presence is live input, so render-run lane fingerprints legitimately differ across runs and frame rates (documented; the scripted headless path above is the deterministic one). The two forced-recreation runs reached identical region state (`stage4 cbfa6d4b61e0c7c4` · `held 5ae1d16610435b8c`, conservation true, invalid 0), the replacement variant proving the region-scene buffer carried across the forced pipeline replacement on real GPU.

**Regional activation under constrained scheduling** (`--sim-budget 30`, near reserve 9 default-scaled from 60): honest overload — 168 budget-exhausted ticks, 3,108 late executions, watermark pair `max_executed_due` 514 / `processed_through` 289, 4 activations / 7 deactivations, invalid 0, conservation true, 29,354 reconstructed activities, 125 captured / 0 reapplied in this particular run (exactly-once reapplication is exercised by the headless tour above and the test suite). **Activation latency p50 0.05 ms · p95 0.06 ms · worst 0.06 ms — against the < 50 ms Stage 4 acceptance budget for a 125-NPC region.** The unconstrained regional run measured p50 0.04 / p95 0.06 / worst 0.06 ms.

### 3. User-confirmed visual observations (the lead's attestation)

The lead confirmed the expected four distinct LOD region appearances in distance order and that moving/rendered scene elements were visible; the automated resize and pipeline-replacement checks passed on screen. **Evidence sources: the textual log and the user attestation — no independently recorded screenshot or video exists, and not every visual behavior was independently instrumented.**

### 4. Independent reviewer acceptance

The independent reviewer formally accepted Stage 4 at `2e7177077656c4cbd97d0d56aa0dd02c64da9fea` (the frozen checkpoint, unmodified). The reviewer's archival instructions — keep automated tests, physical GPU measurements, user-confirmed observations, and reviewer acceptance distinct; no invented GitHub Actions run ID; no unsupported benchmarks; no claim that every visual behavior was independently instrumented — are followed by this record.

## Limitations and untested conditions

- The rig run is local hardware evidence; it has **no GitHub Actions run ID** (none is claimed).
- Run durations and interaction differed (the baseline run includes window interaction); the fps averages are **not controlled apples-to-apples performance comparisons**, and all readings are of the small Stage 4 demonstration scene — not first-game performance claims.
- The Stage 2 baseline `--render` run had the validation layer **off** (its summary still printed err/warn 0/0); validation-ACTIVE evidence is the `--validation on` runs.
- The CPU (Ryzen 7 9800X3D) is known from the acceptance plan's rig naming, not independently logged in the terminal output; the Windows version was not recorded in the session.
- Minimize/restore and overlay interactions are covered by the accounted sim-time lines in every run summary and the lead's on-screen observation, but were not separately instrumented in this session's log; boundary-sprint/hysteresis behavior is evidenced by the transition counters (no thrash; invalid 0 across all runs) and the lead's observation rather than a dedicated recorded sprint.
- The budget-30 run captured 125 consequences with 0 reapplied in that particular run; reapplication under repeated deactivate/reactivate cycles is proven by the headless tour (615 captured / 125 reapplied) and the test suite.

## Post-acceptance state (2026-10-21 branch consolidation)

The same day the lead authorized the branch consolidation: `main` fast-forwarded through `2e71770` (Stage 4) and `9d4bfd9` (the accepted Stage 5 v1.8 architecture docs — every reviewed SHA preserved linearly), and `macos-port` merged as `dda67fa` (Mac 2 closed PASS incl. the physical M1 run; conflict-free, disjoint file sets). The merged tree was verified before the merge commit: fmt/clippy `-D warnings`/build clean, **158 passed + 1 ignored** (Stage 4's 148 + the macOS-portability policy tests' 10), every frozen fingerprint byte-identical, and the offscreen regional render smokes (plain + forced pipeline replacement) clean on the dev pod. CI on the consolidated `main` runs all three platforms (ubuntu incl. the validation-ACTIVE regional smokes, windows-latest, macos-15 ARM64). Stage 5 implementation remains batch-gated (T-16); Batch 1 begins only on the lead's explicit authorization.
