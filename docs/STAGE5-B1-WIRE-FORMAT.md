# Sektor7 — Stage 5 Batch 1 canonical wire format (save/1)

> **Status:** Wire-format specification for the Stage 5 Batch 1 canonical encoding, written before the save-format version froze (B1 authorization §7) and describing the **implemented** format exactly — `engine/crates/engine-core/src/wire.rs` (primitives) and the spine save/load in `engine/crates/engine-core/src/spine.rs`. The governing contract is the accepted Stage 5 architecture v1.8, §5.1 (canonical wire format) and §5.2 (save schema + gates) in [`STAGE5-PERSISTENCE-ARCHITECTURE-PROPOSAL.md`](STAGE5-PERSISTENCE-ARCHITECTURE-PROPOSAL.md). Nothing here is speculative future functionality: every rule in this document is implemented and tested in this batch.

## 1. Scope and identity

A **save** is a self-contained byte sequence encoding one spine and its world at a stable tick boundary (between ticks). This batch ships the **full-fidelity reference encoding** — the correctness oracle for later batches; the file-backed medium, content addressing, SHA-256, the commit pipeline, and compaction are explicitly out of scope (B1 authorization §9).

The format's outer identity is the pair (domain tag `sektor7/save/1`, format version `1`). A decoder that encounters any other version rejects the input with both versions named — readers never guess across versions, and no migration mechanism exists in save/1 (the ratified P1-2 deferral: migration is designed only when a shipped schema requires it).

## 2. Primitives and byte ordering

All multi-byte values are **little-endian**.

| Wire type | Encoding |
|---|---|
| `u8` | 1 byte |
| `u32` | 4 bytes, little-endian |
| `u64` / `Tick` | 8 bytes, little-endian (`Tick` is a u64 newtype) |
| byte array | `u64` length prefix + bytes |
| UTF-8 string | `u64` length prefix + bytes (the Fingerprinter's discipline: embedded NULs are safe; no split aliases a concatenation) |
| enum | `u8` discriminant; only the defined values are valid |
| collection | `u64` count prefix + elements |

**No `usize` or architecture-dependent width appears anywhere on the wire.** Native counts convert through a checked `usize → u64` boundary (a hypothetical wider target fails loudly instead of truncating). There is no native-memory-layout serialization, no pointer-width or platform-endianness dependence, and no hash-map iteration order anywhere: bytes are identical across platforms, compilers, and processes.

**Enum discriminants:** lane `Field` — 0 = Near, 1 = Far; schedule kind — 0 = `AtLogicalTime`, 1 = `ExecutionRelative`; the ledger's lateness flag — 0 = false, 1 = true. `EventClass` (a u16 newtype in memory) is **widened to u32** on the wire (lossless; a value above `u16::MAX` is rejected as a limit violation, never truncated).

## 3. Sections and domain tags

Every section opens with a versioned domain-tag string, so a byte stream's schema is unambiguous at each boundary:

| Section | Tag | Owner |
|---|---|---|
| Save header | `sektor7/save/1` | the save format |
| Spine | `engine-core/spine-save/1` | engine-core |
| World | the `SaveWorld` implementation's `SAVE_DOMAIN` (e.g. `engine-core-test/save-world-save/1` for the batch's test worlds, `engine-slice/save-demo-world-save/1` for the demo) | the application |

Domain tags are exact-match validated (`SaveError::Domain` names both tags). Future SHA-256 content addressing (B2, `engine-persist`) will hash `domain-tag || canonical bytes` per the architecture's §5.1 — the tags above are the separation seams. FNV-1a remains the regression instrument in this batch; it is never a cryptographic integrity proof.

## 4. Save layout

```text
save           := header spine-section world-section
header         := tag("sektor7/save/1") u32(version = 1)

spine-section  := tag("engine-core/spine-save/1")
                  Tick(clock)  u64(next_seq)
                  u32(budget)  u32(near_reserve)
                  u64(stats.scheduled)  u64(stats.executed)
                  u64(stats.late_arrivals)  u64(stats.late_executions)
                  u64(stats.blocked_transitions)  u64(stats.invalid_dropped)
                  u64(stats.budget_exhausted_ticks)
                  lane(near)  lane(far)  seq-section(blocked)  seq-section(held)  ledger

lane           := u64(count)  event-entry × count        # canonical (due, seq) ascending
seq-section    := u64(count)  event-entry × count        # canonical seq ascending
ledger         := u64(count)  record-entry × count       # execution (append) order

world-section  := tag(<world save domain>)  bytes(<world canonical bytes>)
```

The world section is **nested and length-isolated**: the world encodes itself into its own buffer (its own domain tag inside, its own discipline), and the save embeds those bytes under one length prefix. A world-level corruption therefore cannot desynchronize the spine framing, and the world decoder must consume exactly its own bytes (`Trailing` otherwise).

A save's decode must end with **zero bytes remaining** (`Trailing` otherwise) — trailing data is corruption, never noise.

## 5. Event and record encodings

```text
event-entry    := u64(seq)  u32(class)  u8(field)  u8(kind)
                 [ u64(due)                       # kind 0 (AtLogicalTime)
                 | u64(origin)  u64(delay) ]      # kind 1 (ExecutionRelative)
                 u64(payload)
                 u64(dep-count)  u64(seq) × dep-count

record-entry   := u64(seq)  u32(class)  u8(field)
                 u64(intended_due)  u64(executed_tick)
                 u8(kind)  u8(late)
```

A queued entry's **due is not stored separately** — it is derived from the schedule exactly as the live spine derives it (`ExecutionRelative` due = `origin.saturating_add(delay)`), so the wire never carries a redundant copy. The ledger record's `late` flag **is** stored (it is part of the authoritative record) and is **cross-checked at load** against its derivation (`executed_tick > intended_due`) — a mismatch is clock-coherence corruption.

## 6. Collections, canonical order, and the dependency exception

- **Lanes** (near, far): entries in strictly ascending `(due, seq)` order with unique identities. The live queues are binary heaps whose internal layout is rebuild-order-dependent; the encoder sorts into canonical order and the decoder enforces it, so heap layout never reaches the wire (the existing `suspend_from_queue` invisibility argument).
- **Blocked and held**: strictly ascending seq (the live `BTreeMap` canonical order).
- **Ledger**: the authoritative execution (append) order, with unique identities.
- **Dependencies (`depends_on`)**: **stored-order faithful** — the one documented exception to canonical ordering. Dependencies are semantically a set (the live spine accepts duplicates and unsorted lists; waiter registration deduplicates), but the fingerprints hash the stored vector, so the wire preserves order and duplication exactly. Round-trip fidelity — not canonicalization — is the invariant here, and a duplicated-dependency round-trip is tested.

## 7. Documented maxima and allocation safety

| Bound | Value |
|---|---|
| Byte-array / string length | 2³² |
| Collection count | 2³² |
| Domain-tag length | 2¹⁶ |
| Per-entry minimum (event entry) | 38 bytes (seq 8 + class 4 + field 1 + kind 1 + due 8 + payload 8 + dep-count 8) |
| Per-entry minimum (ledger record) | 31 bytes |
| Per-element minimum (dependency) | 8 bytes |

Every length and count is validated **before any allocation may trust it**: against the documented maximum above, **and** against the bytes actually remaining in the input divided by the section's per-element minimum (no element is smaller than one byte). A corrupted length field is rejected (`Limit`), never used to reserve memory — there is no path from malformed input to unbounded allocation.

## 8. Authoritative, derived, and observational data

- **Authoritative (on the wire):** clock, `next_seq`, budget, near reserve, every `Stats` counter, both lane queues, the blocked set, the held set, and the full execution ledger.
- **Derived (rebuilt at load, never serialized):** `known` (the dense allocation range `[0, next_seq)`), `executed` (the complement of unfinished), `waiting_on` (rebuilt through the live `unmet_dependencies` / `register_waiters` paths, so the structures are identical by construction), `held_index` (the `(due, seq)` index), and `max_executed_index` (recomputed from the canonical ledger — value-identical to the incrementally maintained index). Waiter-vector registration order may differ from the original history; it is behaviorally invisible (queues are `(due, seq)`-keyed and no fingerprint domain hashes `waiting_on`) and proven so by the continuation-invisibility tests.
- **Observational (not logical state, not serialized):** `LaneReport`, `LaneTotals`, `SuspensionStats`, `Diagnostics`. A restored spine resumes these at defaults; they never affect scheduling, accounting, or fingerprints. (This is the architecture's field-by-field canonical/derived/observational verification. If observational continuity across a save/load is ever wanted — e.g. cumulative lane totals in a resumed session's reports — that is a format-visible change requiring a version bump and explicit authorization.)

## 9. Load-time integrity gates (fail-closed, typed)

Every gate rejects with a typed `SaveError` — malformed input is never repaired, never best-effort, and never yields a success-shaped value. Gate names appear in the error's `Integrity` payload:

1. **Domain/schema match** — header, spine, and world tags (`Domain`).
2. **Version** — exactly 1; both versions named in the error (`Version`).
3. **Configuration bounds** — `budget ≥ 1` (`spine-save/budget-zero`), `near_reserve ≤ budget` (`spine-save/reserve-exceeds-budget`). The live constructors normalize these; a save holding unnormalized values is corrupt, not quietly re-normalized.
4. **Section canonical order and uniqueness** — during the section reads (`Order`, `Duplicate`).
5. **Identity uniqueness, frontier, dependencies** — every unfinished identity appears at most once across all sections (`spine-save/identities`), lives below `next_seq` (`spine-save/frontier`), and every dependency references `[0, next_seq)` (`spine-save/dependency-frontier`) — the dense-allocation consequence of the live `known` gate.
6. **Dense allocation** — `stats.scheduled == next_seq` (`spine-save/dense-allocation`).
7. **Structural conservation** — `stats.executed == next_seq − unfinished_count` (`spine-save/structural-conservation`): allocation is dense and cancellation-free, so executed is exactly the complement of unfinished — together with gate 5 this makes the identity-set equality a pigeonhole fact (no missing, duplicated, or fabricated obligations).
8. **Stated conservation** — `scheduled == executed + queued + blocked + held`, each section counted once (`spine-save/conservation`; the sums are checked arithmetic, and `Overflow` is the defensive checked-arithmetic class — under save/1's invariants it cannot fire once the preceding gates hold; it guards future format evolution).
9. **Ledger count** — `stats.executed == ledger length` (`spine-save/ledger-count`).
10. **Per-record coherence** — seq below the frontier; not an unfinished identity (`spine-save/ledger-overlaps-unfinished`); `intended_due ≤ executed_tick ≤ clock`; the stored lateness flag equals its derivation (`spine-save/clock-coherence`).
11. **Counter bounds** — `late_executions ≤ executed`, `late_arrivals ≤ scheduled`, `blocked_transitions ≤ scheduled`, `budget_exhausted_ticks ≤ clock` (`spine-save/counter-bounds`) — each structurally implied by live behavior.
12. **Blocked coherence** — every blocked obligation has at least one unmet dependency (`spine-save/blocked-without-unmet-deps`); the live release path would have returned a fully-resolved obligation to its queue.
13. **No trailing bytes** — the whole save and the nested world section must each be consumed exactly (`Trailing`).

The remaining decode-failure classes are primitive-level: `Truncated`, `Trailing`, `Domain`, `Version`, `Enum` (invalid discriminant), `Utf8`, `Limit` (a length/count beyond a documented maximum or the remaining input), `Order`, `Duplicate`, `Overflow`, and `Integrity(gate)`.

**The clock is initialized from the verified save** — the one sanctioned deviation from tick-zero construction (the architecture's documented P3 exception), meaningful only after every gate above has passed; an incoherent save is rejected rather than time-traveled.

## 10. Versioning policy

Save/1 is this document's format. **Any additive change requires a version bump**; readers reject unknown versions with both versions named and never guess across them. Migration is a ratified future mechanism (architecture P1-2: deferred until a shipped schema requires it — none exists yet). Historical fingerprint domains (`engine-core/spine/2`, `state/2`, `spine-lanes/1`, `state-lanes/1`, `spine-held/1`, and the Stage 4 application domains) are untouched by this format and remain byte-identical; persistence-specific state that later needs fingerprint coverage gets **new versioned domains**, never edits to historical ones.

## 11. Golden fixtures and compatibility tests

- **The pinned golden fixture:** the fixed golden scenario's complete 2,167-byte save is pinned as hex in `engine/crates/engine-core/tests/save_roundtrip.rs` (`golden_save_bytes_are_pinned`). The test runs in CI on Ubuntu, Windows, and macOS ARM64 — the pinned value **is** the cross-platform byte-equality proof.
- **Cross-process determinism:** `engine-slice --save-selftest` builds its fixed scenario, asserts the restored re-encoding is byte-identical in-process, and prints `save-len=1209` and `save-fnv=c6c1b46c7d21843b` (FNV-1a over the canonical bytes). CI double-runs the mode per platform and greps both pinned values.
- **The independent reference codec:** `engine/crates/engine-core/tests/common/mod.rs` implements this layout with its own cursor — no production `Decoder` — asserting the spec's structure and canonical properties while parsing, reproducing the production bytes exactly on re-serialization, and providing the offset map the corruption tests mutate.
- **Behavioral compatibility:** the continuation-invisibility tests (same file) prove a restored spine continued with identical inputs is indistinguishable from the never-saved control — scheduling decisions, executions, statistics, both watermarks, all five fingerprint domains, and byte-identical re-encoding — across saturation, cross-lane causality resolved after the restore boundary, held reactivation with overdue work, late arrivals, repeated save/load cycles, and multiple seeds and configurations.
- **The corruption corpus:** `engine/crates/engine-core/tests/save_corruption.rs` exercises every failure class of §9 against real production bytes — truncation at every byte boundary, domain/version/enum mutations, oversized counts and lengths, noncanonical and duplicate identities, contradictory statistics, fabricated frontiers, clock and blocked incoherence, and world-section trailing data.
