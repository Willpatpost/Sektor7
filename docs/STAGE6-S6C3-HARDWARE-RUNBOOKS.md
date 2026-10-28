# STAGE 6 — S6-C3 PHYSICAL HARDWARE SOAK RUNBOOKS

> **Status:** v1 · 2026-10-27 · **S6-C3 implementation artifact —
> PREPARED, NOT EXECUTED.** These runbooks prepare the S6-C4 physical
> measurement campaign. Executing them as the formal Stage 6 hardware
> gate is NOT authorized by S6-C3 and requires the owner's separate
> S6-C4 GO. The `macos-port` development workflow is not modified or
> superseded by anything here.

Both runbooks use the SAME accepted mainline SHA and scenario
version: whatever commit `main` points at when the S6-C4 GO arrives
(the S6-C3 consolidation plus any later accepted mainline), with
`--scenario stage6-v1` (seed `1398032134`, the canonical
`STAGE6_SCENARIO_SEED`). The exact SHA is recorded by the run itself
(build-time provenance in the evidence manifest and the soak summary)
and verified at the rig BEFORE the run (below) — the runtime checkout
is the authoritative record, never a cached build's.

## 0. Common procedure (both environments)

For EVERY run: a FRESH campaign directory and a FRESH evidence
directory (the collector refuses an existing evidence directory by
design — a prior package is never overwritten). The measured window
is 900 seconds; the post-soak drain uses the default 300-second
stabilization deadline unless the environment note says otherwise.

```text
# 0) Verify the exact mainline commit and a clean tree (no blind resets):
git fetch origin
git switch main
git rev-parse HEAD          # RECORD this SHA in the run log
git status --porcelain      # EXPECT: empty (a dirty tree disqualifies the run)

# 1) Build release (the runbooks measure, so release):
cd engine
cargo build --release -p engine-slice

# 2) Headless pre-check (no GPU; proves the lifecycle natively on the rig):
cargo run -q --release -p engine-slice -- \
  --demo-persist <fresh-dir-A> --headless --scenario stage6-v1-short
# EXPECT: exit 0 · "scenario verdict: PASS" · "close: … failure None" ·
#         all cohorts "missing 0 · duplicates 0 (full-ledger)"

# 3) One 900-second VALIDATION-ENABLED run (a correctness-gated measurement):
cargo run --release -p engine-slice -- \
  --demo-persist <fresh-dir-1> \
  --soak-wall-seconds 900 --scenario stage6-v1 \
  --evidence-dir <fresh-evidence-1> --validation on
# EXPECT (exit 0, all of):
#   "the measured window opens — 900s"
#   "cutoff reached true" · coverage 9/9 phases · scripted ticks >= 7800
#     in window (the horizon's final tick is STRUCTURALLY post-cutoff
#     at the exact 900.000 s horizon — reported unreached-by-one; see
#     the runner doc §3a — never a failed gate)
#   drain completed true · time accounting true
#   "scenario: … verdict PASS" · every cohort missing 0 · duplicates 0
#   "close: COMPLETE" · "evidence package: verified complete"
#   "workload gate: PASS" · the soak-summary.json path
# ANY validation error/warning (err/warn must be 0/0) or failed gate
# is a reportable anomaly — the run does not count.

# 4) TWO comparable 900-second VALIDATION-DISABLED measurements:
cargo run --release -p engine-slice -- \
  --demo-persist <fresh-dir-2> \
  --soak-wall-seconds 900 --scenario stage6-v1 \
  --evidence-dir <fresh-evidence-2>
cargo run --release -p engine-slice -- \
  --demo-persist <fresh-dir-3> \
  --soak-wall-seconds 900 --scenario stage6-v1 \
  --evidence-dir <fresh-evidence-3>
# Validation-enabled and validation-disabled runs are DIFFERENT
# configurations, not interchangeable statistical repeats — never
# pooled into one series. The two disabled runs are the comparable
# measurement pair for that environment.

# 5) Fresh-process recovery verification (every run's campaign):
cargo run -q --release -p engine-slice -- \
  --demo-persist <the-run's-campaign-dir> --headless --ticks 30
# EXPECT: exit 0 · "campaign: recovered" · "recovered at tick ~8999"
#         (the horizon's final tick is structurally post-cutoff — see
#         the runner doc §3a) · "closed at tick ~9029" · "conservation
#         true" · the restored fingerprint equals the run's closing
#         stage-4 fingerprint.

# 6) Collect the provenance (per run): the terminal output (tee or
# Start-Transcript), the soak-summary.json, the whole evidence
# directory, the recorded SHA from step 0, and the environment facts
# (OS build, GPU/driver, Vulkan loader version, presentation mode —
# the run's own banner and manifest metadata carry what it can see).
# Hash the evidence directories (e.g. `sha256sum -c` manifests or an
# archive digest) and record the digests in the run log.
```

Optional bounded watchdog wrapper (the §6 external-watchdog
discipline — pathological hangs outside application control): wrap
step 3/4 with a process-level timeout comfortably above
900 s + drain + close (e.g. 1200 s). An externally terminated run's
close status is honestly unknown — the campaign recovers from
whatever durable state exists; it is never counted as a completed
measurement.

## 1. Windows 11 · RTX 5070 Ti (direct Vulkan)

The reference rig (the Checkpoint-1 physical acceptance machine).
Notes: PowerShell; `Start-Transcript` for the terminal record;
Vulkan 1.4 via the NVIDIA driver — no SDK needed unless validation
layers are desired system-wide (the engine degrades transparently
and reports `validation_active` honestly; the validation-ENABLED run
REQUIRES the Khronos validation layer installed and active — verify
the banner says so, or the run's validation gate is vacuous and the
run does not count as the validation-enabled configuration). Fresh
directories under `$env:TEMP` per run. An external RSS sampler may
run alongside (the engine records `rss_kib: null` on Windows by
design — whole-process memory observations come from the sampler,
labeled as such, never mixed into the engine's structural
measurements).

## 2. macOS ARM64 · Apple M1 (MoltenVK)

**Precondition (stated honestly):** mainline today exposes no
MoltenVK device — the Vulkan portability-enumeration changes live on
the owner's `macos-port` track (Mac 2: `VK_KHR_portability_enumeration`
+ `VK_KHR_portability_subset`, closed PASS on the physical M1). This
runbook becomes executable when that accepted work reaches the
mainline the S6-C4 GO names; until then a macOS mainline run fails
transparently at device discovery (`NoDevice`) — the designed
environment-limitation path, never a silent skip. The `macos-port`
workflow itself is not modified or superseded here.

When the precondition holds: the LunarG Vulkan SDK for macOS
(Khronos loader + MoltenVK ICD + the validation layer) installed;
the single-loader discipline verified (ash's `libvulkan.dylib` and
SDL's resolved loader must be the same loader — the Mac 2 record's
Test F procedure; never place a `libMoltenVK.dylib` beside the
binary). Fresh directories under `$TMPDIR`. Report M1 readings
separately by platform — never pooled with the RTX 5070 Ti series.
MoltenVK FIFO present is the expected path; `present_mode` in the
banner and manifest records what actually ran.

## 3. What the runs produce (the S6-C4 inputs)

Per environment: one validation-enabled 900-second package (the
correctness-gated measurement) and two validation-disabled 900-second
packages (the comparable pair). Each package: the five s6-evidence/2
streams + manifest (frame intervals with CPU phases and GPU
timestamps where supported, per-tick spine/lane counters and the
fixed-step debt, the persistence latency families, per-transition
region durations at both scopes, the memory cadence), the campaign's
`soak-summary.json` (the measured window, cutoff state, in-window
coverage, drain, terminal cohort accounting, verdicts, provenance),
and the terminal output. Development-pod and CI numbers are never
presented as hardware results; lavapipe is correctness smoke only.
A complete package is never confused with a passing performance
gate — the measurement acceptance is S6-C4's own review.
