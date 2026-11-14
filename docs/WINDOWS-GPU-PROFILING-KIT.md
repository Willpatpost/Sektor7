# SEKTOR7 — THE OWNER'S WINDOWS GPU PROFILING KIT
## The real-rig evidence procedure (RTX 5070 Ti / 9800X3D) — F-06's hardware baseline, corrected per F-09

**The standing rule (the audit's own words): CI's llvipe runners are software
rasterization and do NOT represent an RTX 5070 Ti. No CI number in this repo is
GPU evidence. The only authoritative graphical performance evidence is THIS kit's
runs on YOUR rig.** The pod's `--benchmark-render` numbers are the same class —
CPU-side A/B evidence only.

**F-09 correction (post-audit remediation):** this kit's earlier revision
documented the "uncapped" runs as `--fps 0` — but the CLI clamps numeric values
to 1..=1000, so those commands silently requested **1 FPS**. Numeric `--fps 0`
is now REFUSED by the binary (a visible error directing to `--fps uncapped`,
regression-tested), and the uncapped commands below use the parser-supported
literal plus an explicit presentation mode. The renderer's run summary now also
prints and verifies the **actual resolved present mode** and self-identifies any
vsync-resolved uncapped run as `presentation-limited` — a FIFO-limited result is
never misread as the engine's unrestricted ceiling.

## 0. One-time setup (PowerShell, from the repo root on main)

```powershell
git checkout main
cd engine
rustc -vV          # expect 1.99.1, host: x86_64-pc-windows-msvc
cargo build --release
nvidia-smi         # the driver + GPU identity (save the output)
vulkaninfo --summary   # the loader, the 5070 Ti device, the validation layers
Get-CimInstance Win32_Processor | Select-Object Name, NumberOfCores, NumberOfLogicalProcessors, MaxClockSpeed
                    # CPU identity (the 9800X3D)
Get-CimInstance Win32_VideoController | Select-Object Name, CurrentRefreshRate, CurrentHorizontalResolution, CurrentVerticalResolution
                    # display: refresh + resolution — what FIFO-class presentation bounds throughput by
```

Optional but recommended for frame pacing: install **PresentMon** (Microsoft,
GitHub) — it captures per-frame present durations from outside the app, which is
the unbiased frame-time authority (avg FPS, 1%-low FPS, and the full presented
frame-time distribution).

## 1. Four different quantities — never conflate them

Every run summary below reports four distinct measurements. Keep them separate
when reading results (yours or anyone's):

1. **Application pacing** — the `--fps` target the loop aims for. The summary's
   `paced frame interval` percentiles measure the loop cadence INCLUDING the
   pacing sleep; `--fps uncapped` means no pacing sleep at all.
2. **Presentation mode** — the **actual resolved Vulkan present mode** the
   summary prints (the startup line also shows the supported set). `fifo`,
   `fifo-relaxed`, and `mailbox` are vsync-class: presented throughput is
   bounded by the display refresh captured in §0. Only `immediate` is not
   refresh-bounded. When a run requested uncapped pacing or
   `--present immediate` but resolved to a vsync-class mode, the summary prints
   a `presentation-limited` line — that run is NOT a ceiling reading.
3. **GPU execution time** — the `gpu render (timestamps)` p50/p95/p99: the
   measured GPU work per frame, independent of pacing and presentation.
4. **Actually presented frames** — the `presented N frames over X s (Y fps
   avg)` line, and PresentMon's external count (the authority for avg FPS,
   1%-low FPS, and the frame-time distribution).

A run is an **unrestricted-ceiling attempt** only when pacing was `uncapped`
AND the resolved present mode was `immediate`. Anything less is bounded by an
identifiable factor — report the factor, not just the number.

## 2. The measured runs (each ~2–5 minutes; the demo exits itself with --frames)

All throughput runs below pass `--present immediate` so the pacing target is
actually reachable above the display refresh (with the default FIFO, a 144 or
240 FPS-paced run is bounded to the display refresh — the engine labels such
runs `presentation-limited`; check that line in every saved output). Run the
whole matrix **at least twice** and report both runs or the spread — a single
run is not variance evidence.

```powershell
# The overlay-off clean baseline at each frame rate
cargo run --release -p engine-slice -- --render --regions --present immediate --frames 600 --fps 60 --no-overlay
cargo run --release -p engine-slice -- --render --regions --present immediate --frames 1440 --fps 144 --no-overlay
cargo run --release -p engine-slice -- --render --regions --present immediate --frames 2400 --fps 240 --no-overlay
cargo run --release -p engine-slice -- --render --regions --present immediate --frames 2400 --fps uncapped --no-overlay   # the ceiling attempt

# The overlay-on comparison (the same four rates, overlay enabled — the default)
cargo run --release -p engine-slice -- --render --regions --present immediate --frames 600 --fps 60
cargo run --release -p engine-slice -- --render --regions --present immediate --frames 1440 --fps 144
cargo run --release -p engine-slice -- --render --regions --present immediate --frames 2400 --fps 240
cargo run --release -p engine-slice -- --render --regions --present immediate --frames 2400 --fps uncapped

# The validation-layer correctness run (one, overlay on — the CI gate equivalent on real hardware)
cargo run --release -p engine-slice -- --render --regions --frames 600 --fps 60 --validation on

# The region CPU benchmark on YOUR CPU (the same A/B the CI canary prints, on the 9800X3D)
cargo run --release -p engine-slice -- --benchmark-render
```

With PresentMon capturing (`PresentMon.exe -output_file run.csv` during each
run), you also get the unbiased present durations, avg FPS, 1%-low FPS, and the
frame-pacing distribution. **Warmup policy:** the first run after a build
includes one-time driver/pipeline warmup — either report the second matrix
pass, or exclude the first seconds from the PresentMon CSV, and state which
you did in the bundle.

After the uncapped runs, capture the limiting-factor evidence:

```powershell
nvidia-smi -q -d PERFORMANCE,TEMPERATURE   # clocks, thermal limits, power caps
```

Record whether vsync, the presentation mode, driver caps, display behavior, or
thermal conditions limited each run (the summary's present-mode and
presentation-limited lines answer the vsync part per run).

## 3. The persisted-campaign graphical run (the integrated engine, not just the render demo)

The Stage 6 integrated soak with rendering — the REAL workload (the persistence
engine + the renderer together), bounded:

```powershell
cargo run --release -p engine-slice -- --demo-persist C:\s7-rig-evidence\soak --soak-wall-seconds 900 --scenario stage6-v1-short --evidence-dir C:\s7-rig-evidence\evidence --validation on
```

(Adjust the paths; 900s is the runbook's full window — the CI uses 40s only
because runners are billed. The evidence package lands in --evidence-dir; the
recovery relaunch afterwards is part of the runbook contract. This is a
correctness/stability run on the default FIFO presentation — the resolved
present mode is recorded in the evidence metadata, and the run makes no
throughput-ceiling claims.)

## 4. What to send back (the evidence bundle)

For each run: the console output (the run summary lines, INCLUDING the
present-mode and presentation/presentation-limited lines), the PresentMon CSV
if captured, and the §0 environment outputs once. The numbers that matter:
**cpu work p50/p95/p99, gpu p50/p95/p99, presented FPS (avg + 1%-low from
PresentMon), the resolved present mode per run, present-mode behavior on
recreation, and the overlay on/off delta at each rate — plus the repeated-run
spread and the §2 limiting-factor capture.**

## 5. Interpretation guides (honest, per the audit)

- **The uncapped run with `immediate` resolved** approaches the engine's
  presentation-unbounded ceiling on your hardware — still bounded by CPU work
  and GPU execution time, which IS the ceiling. The frame-time distribution
  there is the baseline future renderer work (F-04's streaming design, F-08's
  dense-world milestone) measures against.
- **A `presentation-limited` uncapped run** (immediate unsupported on this
  surface/driver — the FIFO fallback) is a vsync-bound reading, NOT the
  ceiling. Report it as presentation-limited; a truly unconstrained ceiling
  may be unavailable on a given device, and the honest report says so rather
  than relabeling the bounded number.
- The **overlay on/off delta** on your rig decides whether the F-05 throttle
  (refreshing the overlay text at a throttled rate) is worth implementing — on
  the pod it costs ~0.10–0.15 ms/frame CPU (≤~4.7% of a 240 FPS budget), which
  your 9800X3D will shrink further; if your measured delta is negligible at
  your target rate, the throttle stays unnecessary.
- The **validation-on run** is correctness evidence (the same gates the CI
  enforces) — it is NOT a performance number (validation layers add overhead by
  design).
- The **`--benchmark-render` A/B on your CPU** should show the same shape as
  the pod's (the scratch form's tighter p95) — if it does not, that is a real
  finding.
