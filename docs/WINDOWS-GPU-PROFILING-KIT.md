# SEKTOR7 — THE OWNER'S WINDOWS GPU PROFILING KIT
## The real-rig evidence procedure (RTX 5070 Ti / 9800X3D) — F-06's hardware baseline

**The standing rule (the audit's own words): CI's llvipe runners are software
rasterization and do NOT represent an RTX 5070 Ti. No CI number in this repo is
GPU evidence. The only authoritative graphical performance evidence is THIS kit's
runs on YOUR rig.** The pod's `--benchmark-render` numbers are the same class —
CPU-side A/B evidence only.

## 0. One-time setup (PowerShell, from the repo root on main)

```powershell
git checkout main
cd engine
rustc -vV          # expect 1.99.1, host: x86_64-pc-windows-msvc
cargo build --release
nvidia-smi         # the driver + GPU identity (save the output)
vulkaninfo --summary   # the loader, the 5070 Ti device, the validation layers
```

Optional but recommended for frame pacing: install **PresentMon** (Microsoft,
GitHub) — it captures per-frame present durations from outside the app, which is
the unbiased frame-time authority.

## 1. The measured runs (each ~2–5 minutes; the demo exits itself with --frames)

The four comparison axes (run each ONCE, save the console output — the demo
prints its run summary with cpu-work p50/p95/p99 and gpu p95):

```powershell
# The overlay-off clean baseline at each frame rate
cargo run --release -p engine-slice -- --render --regions --frames 600 --fps 60 --no-overlay
cargo run --release -p engine-slice -- --render --regions --frames 1440 --fps 144 --no-overlay
cargo run --release -p engine-slice -- --render --regions --frames 2400 --fps 240 --no-overlay
cargo run --release -p engine-slice -- --render --regions --frames 2400 --fps 0 --no-overlay   # uncapped

# The overlay-on comparison (the same four rates, overlay enabled — the default)
cargo run --release -p engine-slice -- --render --regions --frames 600 --fps 60
cargo run --release -p engine-slice -- --render --regions --frames 1440 --fps 144
cargo run --release -p engine-slice -- --render --regions --frames 2400 --fps 240
cargo run --release -p engine-slice -- --render --regions --frames 2400 --fps 0

# The validation-layer correctness run (one, overlay on — the CI gate equivalent on real hardware)
cargo run --release -p engine-slice -- --render --regions --frames 600 --fps 60 --validation on

# The region CPU benchmark on YOUR CPU (the same A/B the CI canary prints, on the 9800X3D)
cargo run --release -p engine-slice -- --benchmark-render
```

With PresentMon capturing (`PresentMon.exe -output_file run.csv` during each
run), you also get the unbiased present durations, 1%-low FPS, and the
frame-pacing distribution.

## 2. The persisted-campaign graphical run (the integrated engine, not just the render demo)

The Stage 6 integrated soak with rendering — the REAL workload (the persistence
engine + the renderer together), bounded:

```powershell
cargo run --release -p engine-slice -- --demo-persist C:\s7-rig-evidence\soak --soak-wall-seconds 900 --scenario stage6-v1-short --evidence-dir C:\s7-rig-evidence\evidence --validation on
```

(Adjust the paths; 900s is the runbook's full window — the CI uses 40s only
because runners are billed. The evidence package lands in --evidence-dir; the
recovery relaunch afterwards is part of the runbook contract.)

## 3. What to send back (the evidence bundle)

For each run: the console output (the run summary lines), the PresentMon CSV if
captured, and the §0 environment outputs once. The numbers that matter:
**cpu work p50/p95/p99, gpu p95, presented FPS, 1%-low FPS, present-mode
behavior on recreation, and the overlay on/off delta at each rate.**

## 4. Interpretation guides (honest, per the audit)

- The **overlay on/off delta** on your rig decides whether the F-05 throttle
  (refreshing the overlay text at a throttled rate) is worth implementing — on
  the pod it costs ~0.10–0.15 ms/frame CPU (≤~4.7% of a 240 FPS budget), which
  your 9800X3D will shrink further; if your measured delta is negligible at
  your target rate, the throttle stays unnecessary.
- The **uncapped run** shows the engine's ceiling on your hardware — the
  frame-time distribution there is the baseline future renderer work (F-04's
  streaming design, F-08's dense-world milestone) measures against.
- The **validation-on run** is correctness evidence (the same gates the CI
  enforces) — it is NOT a performance number (validation layers add overhead by
  design).
- The **`--benchmark-render` A/B on your CPU** should show the same shape as
  the pod's (the scratch form's tighter p95) — if it does not, that is a real
  finding.
