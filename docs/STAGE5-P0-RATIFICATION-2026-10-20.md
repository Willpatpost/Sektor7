# Sektor7 — Stage 5 Architecture Review: Lead Decisions and Required Corrections

I have reviewed the independent architect's assessment of the complete Stage 5 proposal at `stage5-design@68e42e2`, along with your responses to the six preliminary review questions.

**The overall architectural direction is APPROVED, subject to the corrections and clarifications below.**

I agree with the independent reviewer's recommendations on all six P0 decisions. This message formally establishes those decisions and the requirements for revising the Stage 5 architecture.

This is **not authorization to implement Stage 5**.

Stage 4 remains at reviewed checkpoint `2e7177077656c4cbd97d0d56aa0dd02c64da9fea`, awaiting physical Windows acceptance. The experimental macOS port is being developed separately. Neither effort should be disturbed.

Your next task is to reconcile the architecture with these decisions and deliver a revised design for independent review.

---

# 1. P0 Architectural Decisions — RATIFIED

## P0-1 — Solo T3 Semantics

**Decision: APPROVED WITH EXPLICIT CAMPAIGN-MODE DISTINCTIONS.**

A genuinely standalone, single-owner campaign may use its local file-backed revision store as its canonical medium.

For such a campaign:

- The local store is the authoritative ordering point.
- Successful conditional append, verified persistence, and complete chain validation constitute canonical confirmation for that campaign.
- No external service or peer acknowledgments are required.
- This is a legitimate local-canonical confirmation, not an assertion that the save is replicated or protected against loss of the player's machine.

However, standalone local campaigns and shared cooperative campaigns must remain distinct.

For a shared campaign:

- The campaign's configured shared canonical medium remains authoritative.
- The local store is T1 persistence and potentially a T2 replica.
- Local acknowledgment never substitutes for shared T3 confirmation.
- Playing alone in the procedural frontier does not convert the shared campaign into a standalone campaign.
- If shared canonical authority cannot be verified, authoritative progression must suspend under the existing fail-closed policy.
- Previously confirmed shared progress must remain distinguishable from locally pending progress.

**Additional requirement:** Campaign founding metadata must identify the authority mode and canonical-medium identity.

Do not permit silent transitions between standalone and shared authority modes.

A future conversion from standalone to cooperative must have an explicit, verified migration/founding protocol. Designing its compatibility is appropriate now; implementing the full conversion is not required in Stage 5.

Distinguish `T3-local` from shared-canonical `T3` in diagnostics and technical documentation so users and developers cannot mistake local durability for shared availability.

The common persistence interface should support both modes without pretending their failure guarantees are identical.

## P0-2 — Ledger Growth and Compaction

**Decision: APPROVE FULL-FIDELITY REFERENCE ENCODING, WITH COMPATION DESIGNED NOW.**

Stage 5 may begin with complete execution-ledger serialization.

This is useful because it provides a simple, exact reference representation against which future compacted representations can be tested.

However, indefinite linear ledger growth is not acceptable as the finished engine's long-term storage strategy.

The supplementary Stage 4 test measured approximately 5 MiB of execution-history storage after 6,000 ticks.

Extrapolating that same workload to long campaigns demonstrates that unbounded full-history retention could eventually consume gigabytes.

Therefore:

1. Keep the full ledger as the initial correctness reference.
2. Design the compaction format and its invariants before implementation begins.
3. Preserve sufficient history or equivalent authenticated summaries to verify state continuity.
4. Define how the scheduler's derived state, causal dependencies, executed-event identity, and replay requirements survive compaction.
5. Ensure compaction never reintroduces duplicate execution or permits reused event identities.
6. Treat interrupted compaction as an explicit crash-recovery case.
7. Define how a compacted checkpoint becomes an independently verifiable recovery anchor.
8. Establish measured storage-growth and recovery-cost acceptance criteria.

The proposal's `PrefixHash+Tail` concept is a possible direction, not automatically an approved implementation. It must be justified against the actual Spine invariants.

Do not introduce a compact representation that silently changes historical Stage 0–4 fingerprint semantics.

If compacted history requires a new verification domain, version it explicitly and preserve the existing historical fingerprint contracts.

**Stage 5 implementation sequencing:** Full-fidelity encoding is acceptable for Batch 1. Actual compaction implementation may be scheduled according to measured evidence, but its architecture must be settled now.

Before final Stage 5 acceptance, provide either a verified bounded-retention implementation or an explicit, measured, independently reviewed justification for deferring a specified part of compaction. Never claim indefinite-campaign storage scalability without evidence.

## P0-3 — Content Hashing

**Decision: APPROVED — SHA-256 using the `sha2` crate.**

Use SHA-256 for snapshot, revision-record, and content-addressed object integrity.

Requirements:

- The dependency belongs in `engine-persist`.
- `engine-core` retains its existing zero-dependency design.
- Stage 0–4 FNV-1a fingerprints remain unchanged.
- Content hashes must be computed over explicitly versioned canonical bytes.
- Every loaded content-addressed object must be verified before it can influence authoritative state.
- Content-addressing must be unambiguous across object types and schemas.

Do not implement an in-house SHA-256 algorithm.

Also preserve the distinction between integrity and authenticity:

**SHA-256 establishes content integrity, not the identity or authorization of the writer.**

Future shared campaigns will require signed authority records and appropriate verification. Stage 5's local signer placeholder must not be represented as production authentication.

## P0-4 — Snapshot Cadence and Retention

**Decision: APPROVED — ADAPTIVE, PROGRESS-AWARE PERSISTENCE.**

Saving remains automatic and progress-based, consistent with the ratified multiplayer gameplay specification.

Do not reinstate manual saves or a fixed 15-minute autosave policy.

Use two complementary mechanisms:

**Milestone commits**

- Meaningful progress produces canonical revision commits.
- Confirmation happens only after the configured medium satisfies its durability and validation contract.
- Pending milestones remain visibly pending.
- Irreversible dependent actions remain gated until confirmation.
- Small changes should not require serializing the complete world every time.

**Recovery snapshots**

Create full recovery anchors based on measured criteria such as:

- Accumulated revision-log size.
- Expected replay/reconstruction work.
- Recovery-time budget.
- Session lifecycle boundaries.
- Meaningful progression checkpoints.
- Storage-growth limits.

The snapshot policy must preserve a consistent world-and-character revision, preventing item, XP, or resource duplication through partial rollback.

Retention must guarantee that a new recovery anchor is fully persisted and verified before dependent older anchors or revisions are eligible for reclamation.

Never delete the only valid recovery path.

Do not invent arbitrary generation counts now. Determine retention thresholds through measurements during Stage 5, then present the proposed numerical defaults for final approval.

## P0-5 — Two-Mode Recovery

**Decision: APPROVED WITH PRECISE DURABILITY GUARANTEES.**

We require clear distinctions among:

1. Confirmed canonical progress.
2. Locally persisted pending progress.
3. In-memory pending progress.
4. Provisional encounter state.
5. Reconstructible routine simulation state.

**Confirmed progress**

Must survive failures covered by the configured canonical medium's durability contract.

A confirmed revision must not be silently rolled back.

Canonical acknowledgment does not guarantee survival of destruction, deletion, or loss of the canonical medium itself. Independent replicas and disaster recovery remain separate concerns.

**Locally persisted pending progress**

A successfully fsynced T1 journal entry should survive process termination and be available for retry/reconciliation, provided the local storage survives intact.

It must remain pending until canonical acknowledgment.

**In-memory pending progress**

Cannot be guaranteed to survive an arbitrary crash before a durable journal write completes.

We must not claim otherwise.

**Provisional encounter state**

Does not require exact recovery after full session termination.

Uncommitted encounter effects roll back according to the previously ratified checkpoint/resource rules.

**Routine simulation state**

May be reconstructed using the existing labeled replay/synthesis mechanisms, without synthesizing consequential progress.

### Session distinction

Preserve the approved gameplay policy:

- During a still-live suspension, resume the exact authoritative logical state.
- After session termination, authored encounters resume from the appropriate encounter boundary.
- A graceful exit may support exact restoration of committed logical state when consistent with the applicable gameplay policy.
- Exact technical serialization of an active session must not silently override the encounter-boundary recovery rules.

Stage 5 must also prove that a world revision and its associated player progression cannot be independently rewound or advanced.

**Do not promise that every unconfirmed consequence survives every possible crash.**

The actual requirement is that persistence transitions never discard consequential state unless the discarded information has a verified recoverable representation, and that confirmed progress is never falsely represented or silently lost through recovery logic.

## P0-6 — Background I/O

**Decision: APPROVED — MEASURE FIRST, SYNCHRONOUS INITIAL IMPLEMENTATION.**

Stage 5 may initially perform persistence operations synchronously.

Do not introduce background workers purely because asynchronous I/O sounds architecturally attractive.

However, design the ownership boundaries so durable writes, snapshot encoding, and validation can later move to a dedicated I/O worker without compromising canonical ordering.

Requirements:

- Simulation order must remain deterministic.
- Save completion must not advance the authoritative campaign clock.
- Acknowledgments must be associated with specific revisions and idempotency keys.
- Completion order cannot override revision order.
- Pending status remains visible while writes are outstanding.
- Authoritative gameplay may continue during pending confirmation only while exclusive authority remains verified.
- Irreversible dependent actions remain gated.
- Loss of verified authority suspends authoritative progression.

Measure snapshot, journal, commit, and recovery latency.

Do not evaluate only against a 16.7 ms frame budget. Our engine targets high-refresh operation, so include the implications for 60, 144, and 240 FPS where relevant.

If measurements demonstrate unacceptable frame stalls, return with a proposed asynchronous architecture and evidence. Do not introduce threading without review.

---

# 2. Required Correction A — Local CAS and Filesystem Crash Safety

Your acknowledgement that atomic rename is not compare-and-swap is correct.

However, the proposed `HEAD.lock` stale-lock recovery remains unsafe.

A lock file's existence and the observed HEAD revision cannot reliably distinguish a crashed writer from a slow, paused, or otherwise still-active writer.

Deleting such a lock could allow simultaneous writers to enter the supposed exclusive section.

**Required architecture change: Use OS-managed exclusive file locking rather than create-new lock-file ownership with manual stale-lock deletion.**

Rust 1.99.1 provides standard-library file locking through `std::fs::File::lock()` and related APIs.

The design should use a persistent lock file, with OS-managed lock ownership and automatic release when the relevant process/file handles terminate.

Do not unlink and recreate the active lock pathname as part of ordinary lock release.

The local CAS protocol must define:

1. Acquisition of exclusive cross-process coordination.
2. Full validation of the current storage HEAD.
3. Comparison with the caller's expected revision.
4. Rejection of stale parents without modifying authoritative HEAD.
5. Durable preparation of the new immutable revision and referenced objects.
6. Atomic publication of the replacement HEAD.
7. Appropriate filesystem synchronization.
8. Lock release only after the operation reaches a defined outcome.
9. Reconciliation of ambiguous results after crashes or I/O errors.

The linearization point must be explicitly identified and justified.

Test competing independent processes, not only multiple handles inside one process.

### Crash consistency

Define the exact write/synchronization order for:

- Revision object creation.
- Snapshot object creation.
- Journal append.
- Temporary HEAD creation.
- HEAD replacement.
- Directory metadata synchronization.
- Recovery after interruption at every boundary.

Pay particular attention to differences between Unix and Windows durability semantics.

If standard-library filesystem operations cannot establish the required guarantee on a supported platform, identify the limitation and propose an appropriate platform-specific implementation or transactional storage provider.

Do not silently downgrade the guarantee.

### SQLite comparison

Reevaluate SQLite as a possible implementation of the local provider's transaction layer.

This does not mean abandoning canonical revision bytes or adopting SQLite as the canonical wire format.

The comparison should distinguish:

- Logical data format.
- Local transaction implementation.
- Portability.
- Crash consistency.
- Locking semantics.
- Maintenance burden.
- Runtime and packaging overhead.

A custom file-backed implementation is acceptable if it is simpler and demonstrably correct, but rejecting an embedded database solely because it requires another internal storage representation is insufficient.

No database selection is being changed by this message. Return with a justified recommendation.

---

# 3. Required Correction B — Durable CaptureLedger and Idempotency

Approve the fundamental **durable-before-discard** design.

However, the transaction and identity contracts must be stronger.

## Transactional deactivation

A region deactivation must have a recoverable all-or-nothing boundary.

The design must account for mutations to:

- Region state.
- Spine held-event state.
- Region checkpoints.
- Consequential scratch.
- The in-memory CaptureLedger.
- The T1 persistence journal.

The expected sequence is:

1. Validate the proposed transition without irreversible mutation.
2. Determine all consequential captures and their stable identities.
3. Persist the necessary recovery records to T1.
4. Verify the durable-write result.
5. Only then perform destructive or reconstructive state transitions.
6. On failure, retain or restore a coherent pre-transition state.

Do not make `capture()` return a success-shaped receipt before required persistence has succeeded.

Define typed failure results and explain which side effects are permitted at each phase.

Test failures after every individual operation, not merely before or after the complete transition.

## Applied-key authority

The confirmed canonical revision chain is the authoritative source of which consequential changes are canonically committed.

A separately persisted `applied` file may act as a cache or auxiliary recovery index, but cannot independently establish canonical confirmation.

Distinguish:

- Keys captured in memory.
- Keys durably journaled at T1.
- Keys applied to still-pending live simulation state.
- Keys represented in confirmed canonical revisions.

An effect may legitimately exist in the authorized live world while its canonical commit is pending.

That must not be conflated with durability.

On restart, the system must reconcile pending state against the confirmed chain and journal before deciding whether to reapply an effect.

A crash between journal, application, and HEAD publication must never cause duplication, silent loss, or false confirmation.

## Monotonic consequence identities

I agree with your discovery that recovery can reuse episode-based consequence keys.

However, deriving the next counter only from the snapshot and highest applied-key episode is insufficient.

Recovery must consider **all issued durable identities**, including:

- Confirmed revisions.
- Pending T1 journal entries.
- Restored checkpoint metadata.
- Relevant persisted allocation frontiers.

Use a monotonic high-watermark policy that cannot reuse an identity previously issued into durable state.

Retries of an existing operation must retain their original idempotency keys; genuinely new operations must receive fresh identities.

The current packed key design also needs explicit bounds and overflow checks.

Audit the region/episode/NPC bit allocation and ensure no values can overlap or wrap silently. If the existing format is too restrictive for future engine scale, propose a versioned structured identity format now.

Do not change historical Stage 4 fingerprint behavior casually.

A new Stage 5 identity encoding must be introduced with explicit versioning and compatibility tests.

---

# 4. Required Correction C — Compaction, Retention, and Recovery Anchors

The revised architecture must contain an actual compaction design, not merely reserve a future format tag.

Describe:

- Which execution-history information must remain directly available.
- Which historical information can be replaced by authenticated summaries.
- How the Spine reconstructs its known/executed/dependency invariants.
- How identity and sequence allocation remain monotonic.
- How historical cause/effect relationships remain verifiable.
- How recovery anchors establish trustworthy continuation state.
- How older snapshots and revisions become eligible for deletion.
- How active readers and writers interact with compaction.
- How interrupted compaction is recovered.
- How the engine detects incomplete or inconsistent compaction.
- What additional storage is temporarily required during compaction.

A checkpoint is not a valid recovery anchor merely because it has a matching hash.

It must also represent a semantically complete, internally consistent authoritative state.

Compaction must never silently remove the only data required to reconstruct a confirmed campaign revision.

Preserve the full-fidelity reference representation so compacted recovery can be compared against the original logical history.

---

# 5. Additional Required Clarifications

## A. Canonical serialization and independent verification

I accept your proposed independent verification strategy.

Retain:

- Save → load → re-encode byte-for-byte equality.
- Save-invisibility continuation tests.
- SHA-256 object verification.
- Strict typed decoding and semantic integrity gates.
- Differential corruption testing.
- An independent test-only reference decoder.

Additionally, specify that architecture-dependent Rust types such as `usize` must have a fixed-width canonical wire representation.

Do not serialize native-memory layout or platform-dependent integer widths.

Define maximum accepted lengths and counts, reject resource-exhausting or malformed inputs, and require unique ordered map/set entries.

Document domain separation, unknown-version handling, format evolution, and the treatment of floating-point configuration values.

A stable format specification must exist before permanently versioning the save format.

Historical 64-bit FNV fingerprints remain regression instruments, not cryptographic integrity proofs.

## B. Canonical-medium result semantics

The interface must account for uncertainty.

A remote conditional append may:

- Succeed and be acknowledged.
- Definitively reject a stale parent.
- Fail before execution.
- Succeed but lose its acknowledgment.
- Time out with an unknown result.
- Become temporarily unverifiable.

An `Accepted | Rejected`-only interface is insufficient for the eventual networked provider.

The API needs an explicit unknown-outcome/reconciliation path.

An unknown outcome must never be interpreted automatically as either success or failure.

Retain the original idempotency key during reconciliation, re-read and verify canonical history where possible, and fail closed where authority or confirmation cannot be established.

The local implementation should exercise this behavior using fault injection.

**The networked provider being a future drop-in is an architectural objective, not a proven equivalence.**

Different providers may expose materially different consistency, acknowledgment, and failure semantics.

## C. Recovery from corrupt canonical history

Be precise about last-valid-prefix recovery.

Finding a valid prefix does not automatically authorize rolling authoritative state back to that prefix.

The system must distinguish:

- Unconfirmed operations that may be safely discarded.
- Previously confirmed operations whose durability can still be established.
- Corruption that makes prior confirmation impossible to verify.
- Genuine divergent histories.

Never silently discard previously confirmed milestones.

If the system cannot verify a recovery path consistent with committed state, it must fail closed, preserve evidence, and require an appropriate recovery procedure.

An independent replica or previously verified backup may provide a recovery route, but missing confirmed records cannot simply be declared nonexistent.

## D. Encounter recovery consistency

Keep encounter-specific gameplay policy outside the generic persistence mechanism.

The `EncounterJournal` may provide reusable transactional machinery, but it must not decide which resources or skill gains survive failure.

Preserve existing rules governing:

- Pre-encounter ammunition and consumable restoration.
- Equipment condition restoration.
- Removal of uncommitted loot.
- Preservation of approved use-earned skill progression.
- Previously committed mission stages and progress.
- Encounter-boundary recovery after termination.

The example encounter should demonstrate these rules without becoming a production gameplay subsystem.

## E. Stage 5 scope discipline

The Stage 5 architectural plan should remain focused on persistence and the commit pipeline.

I agree with including a minimal encounter demonstration sufficient to prove rollback and durability behavior.

Do not expand the implementation into general NPC movement, broader gameplay, asset streaming, physics, networking, or unrelated systems.

The richer systems work mentioned elsewhere in the roadmap should be brought back as a separate scope question, not silently included.

## F. E5-Lab evidence

Your explanation of the simulated E5-Lab results is accepted.

Retain an explicit distinction between:

**Already demonstrated:** protocol-model behavior against an in-memory simulated medium.

**Not yet demonstrated:** real third-party CAS semantics, persistence acknowledgment guarantees, network partitions, credentials, read-after-write consistency, quotas, and provider suitability.

**New Stage 5 obligations:** actual filesystem durability, process-level contention, crash injection, local journal recovery, and correct local CAS.

Do not present the simulated results as proof of the production storage provider.

Cloudflare R2 remains an experimental candidate, not an approved canonical-medium provider. The real-provider probe remains gated on its previously identified prerequisites.

---

# 6. Revised Implementation and Acceptance Structure

I support retaining the five-batch plan, with the following clarified acceptance expectations.

## Batch 1 — Canonical Encoding and Spine Round-Trip

Required:

- Fixed-width, versioned binary encoding.
- Strict decoder.
- Full-fidelity save and load.
- Derived-state reconstruction.
- Integrity and conservation gates.
- Exact event identity and scheduling semantics.
- Cross-process and cross-platform byte equality.
- Save-invisibility continuation tests.
- Historical Stage 0–4 fingerprints unchanged.

No filesystem authority implementation yet.

## Batch 2 — Revision Store and Local Canonical Medium

Required:

- Immutable content-addressed records.
- SHA-256 verification.
- True cross-process CAS.
- OS-managed locking or a justified alternative.
- Crash-consistent HEAD publication.
- Fault-injection matrix.
- Real child-process termination tests.
- No acknowledged-commit loss within the documented storage failure model.
- Unknown-outcome reconciliation.
- Complete chain validation.

This is a particularly important independent review gate.

## Batch 3 — Commit Pipeline and Encounter Journal

Required:

- Explicit logical/pending/T1/confirmed distinctions.
- T1 journal crash recovery.
- No false confirmation.
- Idempotent retries.
- Reconciliation of ambiguous commits.
- Applied-key state correctly grounded in canonical revisions and recoverable pending state.
- Transactional encounter commit/rollback.

## Batch 4 — Application Integration

Required:

- NpcWorld and RegionDirector codecs.
- Durable-before-discard transitions.
- Monotonic consequence identity recovery.
- Checkpoint and held-obligation integrity.
- Pending/confirmed UI demonstration.
- Irreversible-action gating demonstration.
- Correct encounter-boundary behavior.
- Conservation across crashes, recovery, and regional transitions.

## Batch 5 — Lifecycle, CI, Performance, and Documentation

Required:

- End-to-end save/restart/recovery tests.
- Actual process-kill tests.
- Cross-platform CI.
- Corruption and divergence detection.
- Measured snapshot, journal, commit, and recovery latency.
- Quantified execution-ledger growth and compaction readiness.
- Retention and recovery-cost recommendations.
- Evidence on the Windows physical rig.
- A formal acceptance report.

Maintain the independent review gate after every implementation batch.

No batch may begin merely because the previous batch's author reports success.

---

# 7. Documentation and Decision Tracking

The original proposal is preserved at:

`stage5-design@68e42e21eb27ef8cf0e83d9664292e3568dc2f3f`

Prepare an updated **Stage 5 architecture proposal v1.1** on the same isolated documentation branch.

The revision should:

1. Record all six P0 decisions as lead-ratified architectural directions.
2. Incorporate the required corrections from this message.
3. Clearly separate settled decisions from remaining P1 choices.
4. Resolve contradictions between original proposal sections.
5. Explain any necessary deviations from the five-batch plan.
6. Trace requirements to specific acceptance tests.
7. Include a compact design-decision and correction log.
8. State every remaining limitation honestly.

Preserve the original v1.0 in Git history and make the v1.1 changes reviewable as a separate commit.

Do not silently modify the established governing contracts.

If the revised design requires a change to a previously ratified policy, identify that conflict and return it to me for an explicit decision.

Do not ratify open P1 items on my behalf.

For P1 questions, provide recommended defaults and alternatives, with rationale. I will decide them before their relevant implementation gates.

---

# 8. Development Restrictions

The following remain mandatory:

- `stage4-review` stays frozen at `2e71770`.
- The Stage 4 physical Windows hardware acceptance is still pending.
- `main` must not be modified.
- `macos-port` must not be modified by this development stream.
- Stage 5 source-code implementation is not authorized.
- No Stage 5 Batch 1 work may begin.
- No merges are authorized.
- No new dependencies may be added to the engine yet.
- Documentation-only changes may occur on `stage5-design` as described above.

The independent reviewer will evaluate the revised architecture.

**After the revised design passes review and Stage 4 receives final acceptance, I will separately authorize Stage 5 Batch 1.**

---

# 9. Next Deliverable

Produce:

**Sektor7 — Stage 5 Persistence and Commit Pipeline Architecture v1.1**

Include the revised design and a short review cover report stating:

- Full commit SHA.
- Files changed.
- How each of the six P0 decisions was incorporated.
- How the local CAS locking defect was resolved.
- How durable consequence application and monotonic identities are handled.
- The proposed compaction and retention model.
- The explicit recovery guarantees.
- Remaining P1 questions and your recommendations.
- Any genuine unresolved blockers before implementation.

Do not implement code, perform merges, or start the next stage.

**Stop after the revised architecture is committed and pushed for independent review.**

The goal is not the smallest document or the quickest implementation.

The goal is a sound, maintainable, cross-platform persistence foundation that preserves our existing simulation guarantees and can safely support long-lived solo and cooperative campaigns without a future architectural rewrite.

Proceed with the Stage 5 v1.1 architecture revision only.