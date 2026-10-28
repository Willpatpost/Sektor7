# STAGE 6 — S6-C3 PHYSICAL HARDWARE SOAK RUNBOOKS

> **Status:** v2 · 2026-10-28 · **S6-C3 implementation artifact —
> PREPARED, NOT EXECUTED.** Corrected per the hardware-readiness
> review: the macOS precondition restated against the actual
> mainline source, the Windows instructions made natively executable
> (PowerShell quoting and continuation), the validation-disabled runs
> explicit, and the formal validation-enabled soak's configuration
> gate (requested AND achieved). Executing these as the formal Stage 6
> hardware gate is NOT authorized by S6-C3 and requires the owner's
> separate S6-C4 GO. The `macos-port` development workflow is not
> modified or superseded by anything here.

Both runbooks use the SAME accepted mainline SHA and scenario
version: whatever commit `main` points at when the S6-C4 GO arrives,
with `--scenario stage6-v1` (seed `1398032134`, the canonical
`STAGE6_SCENARIO_SEED`). The exact SHA is recorded by the run itself
(build-time provenance in the evidence manifest and the soak summary)
and verified at the rig BEFORE the run (below) — the runtime checkout
is the authoritative record, never a cached build's.

## 0. The common protocol (shell-agnostic)

For EVERY run: a FRESH campaign directory and a FRESH evidence
directory (the collector refuses an existing evidence directory by
design — a prior package is never overwritten). The measured window
is 900 seconds; the post-soak drain uses the default 300-second
stabilization deadline unless the environment note says otherwise.

The per-environment command blocks below implement, in order:

1. **Verify the exact mainline commit and a clean tree** (no blind
   resets): fetch, switch `main`, `git rev-parse HEAD` (RECORD the
   SHA in the run log), `git status --porcelain` (EXPECT empty — a
   dirty tree disqualifies the run).
2. **Build release** (the runbooks measure): `cargo build --release
   -p engine-slice` from `engine/`.
3. **Headless pre-check** (no GPU; proves the lifecycle natively on
   the rig): the compressed fixture replay — EXPECT exit 0, "scenario
   verdict: PASS", "close: … failure None", every cohort "missing 0 ·
   duplicates 0 (full-ledger)".
4. **One 900-second VALIDATION-ENABLED run** — `--validation on`
   REQUESTED, and the run REQUIRES `validation_active == true`
   (achieved): the integrated soak's configuration gate fails the run
   if the layer was requested but is unavailable on the environment
   (install the Khronos validation layer first; the run records the
   requested/achieved state and the error/warning counts in its
   stdout and soak-summary.json). EXPECT exit 0 with: "the measured
   window opens — 900s" · "cutoff reached true" · coverage 9/9 phases
   · scripted ticks ≥ 7800 in window (the horizon's final tick is
   STRUCTURALLY post-cutoff at the exact 900.000 s horizon — reported
   unreached-by-one; see the runner doc §3a — never a failed gate) ·
   "validation: requested true · active true · err/warn 0/0" · drain
   completed true · time accounting true · "scenario: … verdict
   PASS" · every cohort missing 0 · duplicates 0 · "close: COMPLETE"
   · "evidence package: verified complete" · "workload gate: PASS" ·
   the soak-summary.json path. ANY validation error/warning, a
   requested-but-inactive layer, or a failed gate is a reportable
   anomaly — the run does not count.
5. **TWO comparable 900-second VALIDATION-DISABLED measurements** —
   `--validation off` EXPLICIT in both (never relying on the release
   build's `auto` behavior). Validation-enabled and validation-
   disabled runs are DIFFERENT configurations, not interchangeable
   statistical repeats — never pooled into one series. The two
   disabled runs are the comparable measurement pair for that
   environment.
6. **Fresh-process recovery verification** (every run's campaign):
   relaunch the same campaign directory headlessly for 30 ticks —
   EXPECT exit 0 · "campaign: recovered" · "recovered at tick ~8999"
   (the horizon's final tick is structurally post-cutoff) · "closed
   at tick ~9029" · "conservation true" · the restored fingerprint
   equal to the run's closing stage-4 fingerprint.
7. **Collect the provenance** (per run): the terminal output (tee or
   Start-Transcript), the soak-summary.json (which itself names the
   evidence directory, the schema version, and the verified status),
   the whole evidence directory, the recorded SHA, and the
   environment facts (OS build, GPU/driver, Vulkan loader version,
   presentation mode — the run's own banner and manifest metadata
   carry what it can see). Hash the evidence directories and record
   the digests in the run log.

Optional bounded watchdog wrapper (the §6 external-watchdog
discipline — pathological hangs outside application control): wrap
steps 4–5 with a process-level timeout comfortably above 900 s +
drain + close (e.g. 1200 s). An externally terminated run's close
status is honestly unknown — the campaign recovers from whatever
durable state exists; it is never counted as a completed measurement.

## 0.1 Windows 11 — PowerShell (native commands)

PowerShell continuation is the backtick (`` ` ``) at end of line —
never a shell backslash. Paths use `$env:TEMP`.

```powershell
# 0) Verify the exact mainline commit and a clean tree:
git fetch origin
git switch main
git rev-parse HEAD        # RECORD this SHA in the run log
git status --porcelain    # EXPECT: empty

# 1) Build release:
cd engine
cargo build --release -p engine-slice

# 2) Headless pre-check:
cargo run -q --release -p engine-slice -- --demo-persist "$env:TEMP\s7-pre" --headless --scenario stage6-v1-short

# 3) The validation-enabled 900-second run:
cargo run --release -p engine-slice -- `
  --demo-persist "$env:TEMP\s7-soak-1" `
  --soak-wall-seconds 900 `
  --scenario stage6-v1 `
  --evidence-dir "$env:TEMP\s7-soak-1-evidence" `
  --validation on

# 4) The two validation-DISABLED measurements (--validation off EXPLICIT):
cargo run --release -p engine-slice -- `
  --demo-persist "$env:TEMP\s7-soak-2" `
  --soak-wall-seconds 900 `
  --scenario stage6-v1 `
  --evidence-dir "$env:TEMP\s7-soak-2-evidence" `
  --validation off
cargo run --release -p engine-slice -- `
  --demo-persist "$env:TEMP\s7-soak-3" `
  --soak-wall-seconds 900 `
  --scenario stage6-v1 `
  --evidence-dir "$env:TEMP\s7-soak-3-evidence" `
  --validation off

# 5) Fresh-process recovery (each run's campaign):
cargo run -q --release -p engine-slice -- --demo-persist "$env:TEMP\s7-soak-1" --headless --ticks 30

# 6) The terminal record:
Start-Transcript -Path "$env:TEMP\s7-soak-transcript.txt"
# (re-run a step with the transcript capturing, or wrap the whole
#  session; Stop-Transcript when done)
```

## 0.2 Linux / macOS — bash (native commands)

Bash continuation is the backslash (`\`) at end of line.

```sh
# 0) Verify the exact mainline commit and a clean tree:
git fetch origin
git switch main
git rev-parse HEAD        # RECORD this SHA in the run log
git status --porcelain    # EXPECT: empty

# 1) Build release:
cd engine
cargo build --release -p engine-slice

# 2) Headless pre-check:
cargo run -q --release -p engine-slice -- \
  --demo-persist "$TMPDIR/s7-pre" --headless --scenario stage6-v1-short

# 3) The validation-enabled 900-second run:
cargo run --release -p engine-slice -- \
  --demo-persist "$TMPDIR/s7-soak-1" \
  --soak-wall-seconds 900 \
  --scenario stage6-v1 \
  --evidence-dir "$TMPDIR/s7-soak-1-evidence" \
  --validation on

# 4) The two validation-DISABLED measurements (--validation off EXPLICIT):
cargo run --release -p engine-slice -- \
  --demo-persist "$TMPDIR/s7-soak-2" \
  --soak-wall-seconds 900 \
  --scenario stage6-v1 \
  --evidence-dir "$TMPDIR/s7-soak-2-evidence" \
  --validation off
cargo run --release -p engine-slice -- \
  --demo-persist "$TMPDIR/s7-soak-3" \
  --soak-wall-seconds 900 \
  --scenario stage6-v1 \
  --evidence-dir "$TMPDIR/s7-soak-3-evidence" \
  --validation off

# 5) Fresh-process recovery (each run's campaign):
cargo run -q --release -p engine-slice -- \
  --demo-persist "$TMPDIR/s7-soak-1" --headless --ticks 30

# 6) The terminal record:
cargo run --release -p engine-slice -- <the step-3 command> 2>&1 | tee "$TMPDIR/s7-soak-transcript.txt"
```

## 1. Windows 11 · RTX 5070 Ti (direct Vulkan)

The reference rig (the Checkpoint-1 physical acceptance machine).
Vulkan 1.4 via the NVIDIA driver. The validation-ENABLED run requires
the Khronos validation layer installed and actually active — the
soak's configuration gate fails the run if the layer was requested
(`--validation on`) but unavailable; verify the run's "validation:
requested true · active true" line. An external RSS sampler may run
alongside (the engine records `rss_kib: null` on Windows by design —
whole-process memory observations come from the sampler, labeled as
such, never mixed into the engine's structural measurements).

## 2. macOS ARM64 · Apple M1 (MoltenVK)

**Source-level portability support EXISTS on the accepted mainline**
(`engine-renderer/src/vulkan.rs`: the `VK_KHR_portability_enumeration`
instance-extension decision with the `ENUMERATE_PORTABILITY_KHR`
flag, and `VK_KHR_portability_subset` enabled when the chosen device
advertises it — the Mac 2 implementation, verified in the mainline
source). What remains for the M1 run are the ENVIRONMENT
preconditions and the physical verification:

- The LunarG Vulkan SDK for macOS installed (the Khronos loader,
  the MoltenVK ICD, and the validation layer — the validation-ENABLED
  run's configuration gate requires the layer to be actually active).
- The single-loader discipline verified (ash's `libvulkan.dylib` and
  SDL's resolved loader must be the same loader — the Mac 2 record's
  Test F procedure; never place a `libMoltenVK.dylib` beside the
  binary).
- **Physical M1 graphical verification is still required** — the
  mainline source support has not yet been exercised on the physical
  machine for the soak.

Report M1 readings separately by platform — never pooled with the
RTX 5070 Ti series. MoltenVK FIFO present is the expected path;
`present_mode` in the banner and manifest records what actually ran.
The owner's `macos-port` development workflow is not modified or
superseded by this runbook.

## 3. What the runs produce (the S6-C4 inputs)

Per environment: one validation-enabled 900-second package (the
correctness-gated measurement) and two validation-disabled
900-second packages (the comparable pair). Each package: the five
s6-evidence/2 streams + manifest (frame intervals with CPU phases and
GPU timestamps where supported, per-tick spine/lane counters and the
fixed-step debt, the persistence latency families, per-transition
region durations at both scopes, the memory cadence), the campaign's
`soak-summary.json` (`s6-soak-summary/1` — which names the evidence
directory, the schema version, the verified status, the validation
requested/achieved state with its counts, and the missing-to-horizon
progress), and the terminal output. Development-pod and CI numbers
are never presented as hardware results; lavapipe is correctness
smoke only. A complete package is never confused with a passing
performance gate — the measurement acceptance is S6-C4's own review,
and the workload-completeness coverage threshold is a
workload-CORRECTNESS threshold, NOT evidence of sustained 10 Hz
performance throughout the window (the S6-C4 performance tolerance
is a separate owner decision).
