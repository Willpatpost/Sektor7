# SEKTOR7 — THE SUSTAINED-LOAD SCALE METHODOLOGY (F-12 / F-08)

**Purpose:** the design for the simulation-scale evidence the engine currently
lacks. The existing entity-scale benchmark (`--benchmark-entity-scale`)
proves that **executed event work per tick stays bounded** as population grows
(1k / 5k / 20k entities × three LOD scenarios, double-run determinism). That
is necessary but not sufficient: **a sufficiently overloaded system can appear
fast because it falls behind.** Bounded tick cost is not sustainable world
simulation. This methodology defines the complementary sustained-load
experiments that measure not only execution time but **the work that remains
unfinished**, so we can distinguish efficient sustainable simulation from a
fast scheduler accumulating an unbounded backlog.

Status: **DESIGN READY** (the owner's 2026-11-14 post-audit remediation §6;
the implementation package is separately authorized). Introduced 2026-11-14
on `s6-remediation-c-renderer-docs`.

## 1. What already exists (build on this, do not duplicate)

- **The spine's first-class accounting** — every quantity below is already
  computed and reported per run, never silently discarded:
  outstanding obligations (`pending_count`, near/far split), blocked
  (dependency-gated) count, held (dormant-captured) count, `late_arrivals`
  and `late_executions`, `budget_exhausted_ticks`, per-lane totals
  (executed + backlogged ticks), the safe `processed_through` watermark
  (retreats on late arrivals) vs `max_executed_due`, per-tick execution
  percentiles, and the conservation identity
  `scheduled = executed + pending + blocked + held`.
- **The b6 proof at unit scale** — `s6_burst::b6_sustained_overload_grows_then_drains_with_evidence`
  already demonstrates overload growth and post-overload drain with
  conservation, at small population and short horizon. The sustained-load
  experiment is this proof promoted to a measured, long-horizon, multi-scale
  harness.
- **The entity-scale benchmark** — preserved unchanged as the bounded-cost
  reference; the sustained-load harness reuses its population parameterization
  and its double-run determinism proof.

## 2. The twelve measurement dimensions

| # | Dimension | Source |
|---|---|---|
| 1 | Event demand per logical tick | new: the workload generator's scheduled count per tick |
| 2 | Completed events per logical tick | existing: spine `executed` deltas per tick |
| 3 | Outstanding queue size over time | existing: `pending_count` sampled per tick (near/far split) |
| 4 | Pending growth rate | derived: the slope of #3 across the run window |
| 5 | Oldest pending event age | existing: due-tick of the oldest pending vs the clock |
| 6 | Execution lateness distributions | existing: `late_executions`; NEW: the lateness
  magnitude distribution (ticks-late percentiles), not just the count |
| 7 | Budget exhaustion frequency | existing: `budget_exhausted_ticks` per 1k ticks |
| 8 | Near/far lane fairness and starvation | existing: lane totals; NEW: the near-lane
  p95 wait and any near-obligation age exceeding its cadence budget (the
  starvation-free property the near reserve guarantees — re-proved at scale) |
| 9 | CPU memory as population grows | NEW: resident set sampled at each population step |
| 10 | Cost of scheduling, execution, bookkeeping | existing: per-tick percentiles;
  NEW: the split of tick time into queue admission vs execution vs accounting |
| 11 | Cost of snapshotting / representing world state | existing: the save/round-trip
  benchmarks; NEW: snapshot cost measured at each population scale within the
  sustained run (not just at 1k) |
| 12 | Determinism fingerprints across repeated runs | existing: lane fingerprints —
  the sustained harness double-runs every workload and byte-compares |

Dimensions marked NEW are the harness's additions; everything else is
reporting over existing spine observability.

## 3. The six experiment classes

Each class is a demand profile over the same populations (1k / 5k / 20k
reference points; additional populations only when a class's result makes
them meaningful). Demand is parameterized as events-per-tick scheduled
against the fixed execution budget (the default 200, plus a reduced-budget
variant per class to expose the budget's role explicitly).

1. **Low demand** — demand well under budget. Expect: zero backlog growth,
   zero budget exhaustion, lateness ~0. Proves the baseline is actually
   comfortable, not merely passing.
2. **Near saturation** — demand approaching budget. Expect: intermittent
   exhaustion, bounded oscillating backlog, nonzero but stable lateness.
   The interesting class: the transition signature.
3. **Overload** — demand exceeding budget. Expect: monotone backlog growth
   with the measured growth rate ≈ (demand − budget); the run must
   demonstrate the growth, not hide it.
4. **Recovery** — overload then demand falls. Expect: the drain rate, the
   drain time to zero, and conservation across the whole arc. (b6 promoted
   to scale.)
5. **Mixed LOD** — a small active (near) population at high cadence while
   distant populations receive deliberate low-frequency updates. The
   critical distinction class (§5): the far lanes' low execution rate is
   BY DESIGN; the harness must show near-lane starvation-freedom while far
   lanes execute at their designed cadence.
6. **Changing presence** — regions transition between LOD states over time
   (the presence scenarios applied at scale). Expect: activation/deactivation
   latency and the reconstructed-work cost measured against the backlog the
   transitions themselves create.

**Horizon:** every class runs long enough to identify the backlog SHAPE —
stabilizes, drains, oscillates, or grows indefinitely — which means a
simulated-time horizon ≥ 10× the drain time observed in class 4 at that
population (the harness reports the horizon it used; no fixed-wall-clock
cap masquerading as a logical horizon).

## 4. The run matrix

Populations {1k, 5k, 20k} × classes {1..6} × LOD scenarios {the entity-scale
benchmark's three} × double-run = 108 measured runs, plus the reduced-budget
variant for classes 2–4. All headless (no GPU), deterministic, seeded; the
pod is the reference environment with the rig as the future cross-check.

## 5. The backlog-vs-deliberate-LOD distinction (the operational rule)

- A **far-lane obligation executing at its designed low cadence is not
  backlog.** The far lanes' scheduling contract is deliberately low-rate;
  their pending count at steady state is the designed pipeline depth, and
  their obligations are not late relative to their own (sparse) due ticks.
- A **near-lane obligation pending beyond its due tick, or any obligation
  whose age exceeds its lane's cadence budget, IS backlog.**
- The harness reports them separately, always: near-pending age vs near
  cadence, far-pending age vs far cadence, and never sums them into one
  "queue size" without the split. A workload is **sustainable** iff no
  lane's pending-age distribution grows without bound over the horizon.

## 6. The fidelity-contract honesty rules

- Budget-limiting alone is never reported as efficiency. The report's
  "fidelity level" field states, per workload, what the budget actually
  bought: which classes executed at which cadence, and what was deferred.
- Delayed or skipped behavior is never called equivalent to full simulation
  without proving the specific gameplay contract (the NPC routine/LOD
  contracts exist for the region system; anything beyond them is labeled
  UNPROVEN, not assumed).
- No RollerCoaster-Tycoon-level efficiency claims from this methodology —
  the claim vocabulary is exactly: sustainable / saturated / overloaded /
  recovering, per lane, at the measured population.

## 7. The per-workload report (the eight required fields)

Computational cost (per-tick percentiles + the dimension-10 split) · logical
progress (campaign clock, watermark) · fidelity level (§6) · backlog (per
lane, age distributions, shape classification) · latency (lateness count and
magnitude percentiles) · memory (resident set, snapshot cost) · determinism
(double-run fingerprint equality) · failure/saturation behavior (exhaustion
frequency, the growth/drain rates, any conservation violation = a hard
failure).

## 8. Acceptance criteria for the implementation package

1. A `--benchmark-sustained-load` harness (engine-slice, the benchmark
   modules) executing the §4 matrix with the §2 dimensions and §7 report,
   observational only (no CI thresholds — the audit's rule).
2. A regression proving the classifier: a synthetic workload with a KNOWN
   growth rate must be classified overloaded with the measured rate within
   a stated tolerance; a known-drain workload classified recovering; the
   low-demand class proven zero-backlog (deterministic, no timing).
3. A regression proving the §5 split: a mixed-LOD workload where the far
   lanes' steady pending is classified designed-cadence while an injected
   near-lane excess is classified backlog.
4. The double-run determinism proof over every workload.
5. The report delivered as evidence (pod reference run) and interpreted
   per §6 — including the honest statement of whatever remains unproven.

**Explicitly not claimed by this design:** any sustainability property of
populations beyond those measured; any equivalence between budgeted
low-cadence execution and full-rate simulation; any real-time (wall-clock)
guarantee — the spine owns logical time only.
