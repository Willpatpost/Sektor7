# SEKTOR7 — THE GPU CAPACITY & SYNCHRONIZATION ANALYSIS (F-03 / F-04)

**Purpose:** the measured-hypothesis plan for the renderer's fixed vertex
capacities (F-03) and its upload/synchronization strategy (F-04), per the
owner's 2026-11-14 post-audit remediation §8. This is analysis and design —
**no renderer refactor is begun under this directive.** Every fact below was
read from the source at the `s6-remediation-c-renderer-docs` tree
(engine-renderer @ `5f642af`'s parent state) and spot-verified against the
cited lines.

Status: **DESIGN READY** (the implementation package is separately
authorized). Introduced 2026-11-14.

## 1. The verified inventory (facts, with citations)

- **Vertex sizes:** `SceneVertex` 24 B (`scene.rs:10-16`), `OverlayVertex`
  24 B (`overlay.rs:9-14`). `FRAMES_IN_FLIGHT = 2` (`config.rs:88`).
- **Static scene** (the cube field + demonstrators): one device-local VBO
  (844 verts, 20,256 B) + IBO (5,064 B), uploaded init-time via a transient
  HOST_VISIBLE staging buffer + one-shot copy + fence
  (`create_device_local_buffer_with_data`, `vulkan.rs:1566-1715`); the
  staging resources are destroyed in the same call. No overflow risk —
  VBO/IBO/index-count derive from the same `build_scene()` vectors.
- **Dynamic region-scene buffer** (NPC proxies + area plates): HOST_VISIBLE
  \| HOST_COHERENT, persistently mapped, lazily allocated on the first
  non-empty region frame; one buffer with two disjoint per-slot regions
  (`ensure_region_capacity`, `vulkan.rs:1206-1225`; slot-offset binding at
  `vulkan.rs:2229-2231`).
- **Dynamic overlay buffer:** same pattern, always allocated
  (`vulkan.rs:1178-1188`).
- **View-projection:** a 64-byte push constant (`vulkan.rs:1321-1325`,
  written per frame at `2207-2213`). No uniform buffers, no descriptor sets,
  anywhere in the crate.
- **Depth:** one DEVICE_LOCAL image + memory per swapchain image,
  recreated with the swapchain (`vulkan.rs:888-935`).
- **Synchronization:** per-slot in-flight fence + acquire semaphore
  (fences created SIGNALED; `vulkan.rs:1857-1866`); the CPU blocks at the
  slot fence wait (2 s timeout, `vulkan.rs:2039-2042`) and at the per-image
  fence guard before `reset_fences` (the 240-pacing-against-FIFO wedge fix,
  `vulkan.rs:2104-2124`); acquire with a 100 ms timeout; submit waits only
  `image_available` at `COLOR_ATTACHMENT_OUTPUT`; present waits the
  per-image present semaphore (indexed by swapchain image, grow-only).
  **No `queue_wait_idle` anywhere**; `device_wait_idle` only at swapchain
  recreation and renderer drop. The init-time staging wait is the crate's
  only unbounded fence wait (`u64::MAX`, `vulkan.rs:1679`).
- **Dynamic upload path:** persistently-mapped `copy_nonoverlapping` memcpy
  into the slot's region, after that slot's fence wait
  (`write_region_scene`/`write_overlay`, `vulkan.rs:1495-1519`); HOST_COHERENT
  with **zero flush/invalidate calls**; the GPU reads dynamic geometry
  directly from host-visible memory. **No device-local path for dynamic
  data exists.**
- **Swapchain:** image request `(min_image_count + 1).max(FRAMES_IN_FLIGHT)`
  capped by max; four recreation triggers (suboptimal flag, proactive
  resize, acquire/present OUT_OF_DATE); recreation always
  `device_wait_idle`s first, rebuilds swapchain/views/framebuffers/depth,
  retains command buffers/semaphores/fences/timestamps and the entire
  pipeline+buffer state — unless the surface format changed or
  `--replace-pipelines` is set (then a full replacement `PipelineState` is
  prepared before activation, including the region buffer if the outgoing
  state had one — Stage 4 final-review finding A, `vulkan.rs:2380-2416`).
- **GPU timestamps:** a 4-query pool (2 per slot), TOP_OF_PIPE/
  BOTTOM_OF_PIPE writes per recorded frame, read on the slot's next use
  after the fence proves resolution, masked to `timestamp_valid_bits`
  (`vulkan.rs:2129-2137, 2181-1189, 2331-2357`); p50/p95/p99 over a
  600-sample window.

## 2. Classification (the directive's four categories)

### 2.1 Current correctness risks (confirmed, not speculative)

1. **The region-scene buffer silently truncates oversized geometry, twice,
   with no diagnostic.** The capacity is the fixed formula
   `((8 + 1000) * 36 + 512) * FRAMES_IN_FLIGHT` = 73,600 verts total /
   36,800 per slot (`scene.rs:308-318` — "all eight regions Full … 1000 NPC
   proxy boxes … plus margin"): a hard-coded world-scale assumption (8
   regions, 1000 NPCs). Overflow is clamped by `min()` at the byte copy
   (`vulkan.rs:1513`) AND at the draw count (`vulkan.rs:2228`) — both
   silent; neither returns an error, sets a counter, nor trips the
   validation layer. The clamp is on raw vertex count, so an overflow can
   cut a 36-vertex proxy box **mid-primitive**.
2. **The overlay's glyph cap does not bound vertices below buffer
   capacity.** `MAX_GLYPHS = 6_000` (`overlay.rs:21`) vs a per-slot vertex
   capacity of 300,000 (`vulkan.rs:1178`): a font8x8 glyph is up to 64 set
   pixels × 6 verts = 384 verts (the 'A' test proves 28 px → 168 verts,
   `overlay.rs:94-104`), so worst case 6,000 × 384 = 2,304,000 verts —
   7.7× the slot capacity — and even average-density text at the glyph cap
   exceeds it (1,008,000). The capacity docstring itself hedges ("typical
   stats text uses ~1–2%"). The same double silent `min()` applies
   (`vulkan.rs:1495, 2237`).
3. Minor: the per-image fence guard silently skips when the image index is
   out of table range (`vulkan.rs:2104`) — defended in practice by the
   resize-on-recreation discipline; and the init-time staging fence wait is
   unbounded (`vulkan.rs:1679`) — a wedged init submission would hang
   renderer construction (vs. the frame path's 2 s bound).

**The honest overall statement:** at the CURRENT demo scale (8 regions,
1000 NPCs, the fixed stats overlay) the capacities are adequate and no
truncation occurs — every observed run's geometry fits. The risk is that
ANY future growth (a denser region, a longer overlay, a larger NPC count)
truncates invisibly: the failure mode is missing geometry with zero
evidence, which is the worst kind for a rendering bug report.

### 2.2 Current measurable performance bottlenecks (measure before touching)

1. **Dynamic geometry is read by the GPU from host-visible memory every
   frame** (the persistently-mapped coherent buffers). The cost is
   PCIe-bandwidth + caching dependent and is exactly what the GPU-kit
   matrix measures (gpu p50/p95/p99 region-on vs region-off). At the
   current scene sizes (≤ 883 KB/slot region + the overlay in-use
   fraction) this is expected to be negligible on the 5070 Ti — the
   hypothesis to CONFIRM, not assume.
2. **`device_wait_idle` at every swapchain recreation** — a full pipeline
   drain on each resize/format change; visible as a latency spike in the
   resize smoke, bounded by the recreation frequency. Irrelevant at steady
   state.
3. **The per-frame memcpy CPU cost** — already measured
   (`--benchmark-render`: the F-02/F-05 numbers; region build p95 0.035 ms,
   overlay rebuild 0.090–0.131 ms on the pod). Not currently a bottleneck
   at 240 FPS budgets.
4. **FIFO presentation bounding the loop** — not a renderer defect (the
   F-09 kit now specifies `--present immediate` and labels
   presentation-limited runs); listed because it masqueraded as one.

### 2.3 Future capacity requirements (what invalidates the current design)

- **Region capacity** scales linearly in (regions + NPCs): the formula
  serves (8, 1000). A 20k-entity world (the entity-scale benchmark's top
  reference point) needs ~ (8 + 20,000) × 36 ≈ 720k verts/slot ≈ 17.3 MB
  host-visible per slot — 20× today's allocation, and beyond the F-12/F-08
  question of whether 20k PROXIES would ever be drawn (LOD exists so they
  would not — only Full/Simplified regions draw proxies; the capacity
  formula's "all eight regions Full" worst case is itself the scaling
  cliff).
- **Overlay capacity** scales with text volume × glyph density; the stats
  overlay is bounded by its content, but any future debug-drawing use
  (lines, boxes) multiplies it.
- **Depth memory** scales with extent × swapchain images (4K recreation
  ≈ 33 MB × images on the 5070 Ti — trivial there, relevant on iGPUs).

### 2.4 Architectural opportunities NOT yet justified by measured workloads

- A streaming allocator / sub-allocated memory heap (vs one
  `vkAllocateMemory` per buffer): justified only by allocation-count
  pressure that does not exist (≤ 6 persistent allocations).
- Device-local dynamic buffers with a staging ring (async upload): justified
  only if §2.2-1 measures a real cost at target frame budgets on the rig.
- Descriptor/uniform-buffer migration off push constants: justified only by
  exceeding the 128-byte push-constant limit (64 used).
- More frames in flight: justified only by measured acquire blocking under
  `immediate` at high frame rates.

## 3. The measured-hypothesis plan

| # | Hypothesis | Measurement that tests it | Where |
|---|---|---|---|
| H1 | Host-visible dynamic reads are negligible at current scale on real hardware | The GPU-kit matrix's region-on vs region-off gpu p50/p95/p99 delta | the kit (awaiting the rig) |
| H2 | The overlay's in-use vertex fraction stays < 10% of slot capacity for the real stats text | Instrument the overlay build to report `verts.len()` vs capacity (a print, not a gate) | the sustained-load/render harness (next package) |
| H3 | The truncation paths are currently unreachable at demo scale | The visibility counter (§4 step 1) reading zero across the full CI battery + soak | the next package |
| H4 | Recreation drains cost < one frame at 60 FPS on the rig | The resize smoke's recreation-timing readout vs the frame budget | the kit's §2 matrix |
| H5 | 2 frames-in-flight saturates `immediate` presentation on the 5070 Ti | The uncapped run's presented fps vs cpu-work p50 (if presented ≈ 1/cpu-p50, slots are not the limiter) | the kit |

H1/H4/H5 are rig-gated (the standing rule: no CI/pod number is GPU
evidence); H2/H3 are harness-gated and implementable without hardware.

## 4. The proposed next-package work (F-03 policy; design + acceptance tests)

**Step 1 — make truncation visible (the narrow corrective; fail-visible
over fail-silent, the engine's own discipline):**
- `RenderStats` gains `region_truncated_vertices` / `overlay_truncated_
  vertices` counters, incremented where the two `min()` clamps sit today
  (`vulkan.rs:1513/2228` region, `1495/2237` overlay); the run summary
  prints them (zero in every current run — the proof of H3); a nonzero
  value is REPORTED, not fatal (a typed refusal is step 2's policy
  decision).
- Acceptance tests: a pure unit test of the clamp-decision function
  (supplied vs capacity → the truncated delta); a lavapipe smoke variant
  driving an oversized synthetic scene asserting the counter is nonzero
  and reported (the b-series test-seam precedent); the full battery
  re-pinned for the added tests.

**Step 2 — the capacity policy (the F-03 decision, for separate
authorization):** derive the region capacity from the authoritative
world-scale constants (engine-core's `REGION_COUNT` / `NPC_COUNT` — already
the single source of truth since the V2W F4 move) instead of the literal
`(8 + 1000)`, and REFUSE TYPED at the `render_frame` boundary when a
frame's geometry exceeds capacity (consistent with the engine's fail-closed
discipline everywhere else) rather than growing allocation on demand.
Overlay: bound `MAX_GLYPHS` by the vertex budget (capacity ÷ worst-case
glyph verts) or refuse typed the same way. The acceptance test: the
oversized-geometry regression flips from counter-nonzero to typed-refusal.

**Step 3 — F-04 (only with rig evidence):** if H1 measures a real cost at
target budgets, the staging-ring/device-local design is proposed as its own
package with its own acceptance tests; otherwise it stays in §2.4 as
unjustified. **No major renderer refactor is part of this directive.**

## 5. What this analysis does NOT claim

- No claim that the current renderer is inadequate for its present
  workload — every measurement so far says it is comfortable.
- No claim that any §2.4 opportunity improves anything — each is gated on
  a measurement that has not been taken (H1/H4/H5 need the owner's rig).
- No silent-truncation fix is implemented under this directive — the risk
  is documented (§2.1) with the narrow corrective specified (§4) for the
  next authorized package, per §8's instruction.
