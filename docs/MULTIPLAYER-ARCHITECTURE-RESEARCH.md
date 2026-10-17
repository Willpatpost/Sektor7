# MULTIPLAYER-ARCHITECTURE-RESEARCH — Authority, Persistence, and Synchronization

> **Status:** Research study v0.2 · 2026-10-17 · Revised after the three-way review. **v0.1's "Architecture D" (member-replicated canonical storage) was NOT approved by the lead** — this revision analyzes exactly why (§4) and redirects the recommended research direction (§12). Still **UNAPPROVED RESEARCH**: no code exists or is authorized; no transport, coordinator, persistence technology, or service provider is selected; no approved requirement is changed by this file.
>
> **Ownership:** Research and analysis only. [`MULTIPLAYER-SPEC.md`](MULTIPLAYER-SPEC.md) owns player-facing multiplayer policy (referenced, never overridden); [`DECISIONS.md`](DECISIONS.md) is the decision registry; [`CUSTOM-ENGINE-RESEARCH.md`](CUSTOM-ENGINE-RESEARCH.md) owns engine research methodology. This study covers the technical territory the folder map anticipated for a future `SAVE-ARCHITECTURE.md` (serialization, snapshots, distribution, crash consistency) plus campaign authority, sessions, identity, and governance plumbing.
>
> **Inputs:** The lead's 2026-10-17 confirmed requirements — now synchronized into `MULTIPLAYER-SPEC.md` v0.3 and `DECISIONS.md` v0.2 (§0) — and the review directives: solo-frontier availability analysis, authority proof, durability proof, reevaluation of alternatives without assuming third-party services violate the operating-cost constraint, state correctness, an availability matrix, and preservation of the fail-closed policy.
>
> **Evidence key:** **[ESTABLISHED]** = external literature or documented external capability (cited in §16); **[PROJECT EVIDENCE]** = demonstrated in this repository; **[HYPOTHESIS]** = this study's unproven adaptation; **[OPEN]** = unresolved decision or question.

---

## 0. Requirement synchronization record — completed 2026-10-17

All twelve drift flags from v0.1 were synchronized into `MULTIPLAYER-SPEC.md` v0.3 and `DECISIONS.md` v0.2 (M-27–M-35 added; M-11/15/17/18/21/23 modified; supersessions recorded). The v0.1 flag table is preserved below as the drift record. The **emergent solo-durability question** from v0.1 is no longer a side note — the review elevated it to the central technical problem, analyzed in §4–§6.

| # | Confirmed requirement (2026-10-17) | v0.1 documented position | Sync outcome |
|---|---|---|---|
| 1 | Automatic progress-based saving replaces save slots/arbitrary reloads | SPEC §8.1 save categories; M-11/M-17 | **Superseded → M-29** |
| 2 | Encounter-failure transactional rollback | §7 wipe→previous save; M-15 | **Superseded → M-30** |
| 3 | Exact-state restore not required across terminated sessions | M-18 exact-state recovery | **Modified → M-18** (live suspension exact; termination at encounter boundary) |
| 4 | Planned host handoff required | M-21 host-leaving ends session | **Modified → M-31** |
| 5 | One authoritative session per campaign | P0-1 undetermined | **Confirmed → M-28** |
| 6 | Shared canonical storage acknowledgment | P0-1 open | **Confirmed → M-29** (mechanism remains P0 research) |
| 7 | Central hub | §3.3 working preference | **Formalized → M-27** |
| 8 | Admission unanimity | §2.2 open | **Resolved → M-32** |
| 9 | Async governance votes | connected-only voting | **Extended → M-32** |
| 10 | Ranked-choice voting | §5.2 proposal | **Confirmed → M-33** |
| 11 | Time-skip unanimity (connected) | §8.2 group vote | **Modified → M-34** |
| 12 | Retirement archival | §2.2 proposal | **Confirmed → M-32** |

**Fail-closed authority policy is itself ratified (M-35):** no provisional authoritative progression, no automatic divergent-history merging, no silently weakened durability guarantees.

---

## 1. Problem shape, constraints, and corrections from the review

| Property | Value | Consequence |
|---|---|---|
| Participants | 1–4 registered members, world-bound campaigns | Tiny scale; protocol simplicity beats sophistication |
| Authoritative writers | Exactly **one session per campaign** (M-28) | Single-writer design; only *fencing* and *selection* are needed, not consensus |
| Hosting | Rotating eligible members; listen-server preferred; **planned handoff required** (M-31) | Authority is transferable state |
| Infrastructure | **No developer-paid recurring infrastructure may be REQUIRED**; research into existing third-party services explicitly approved | An always-available shared medium is *permissible* if it is not developer-funded — this is the review's corrected reading |
| Availability | Subset frontier play incl. **one connected member of a multi-person campaign**; solo play; async ballots | Any mechanism requiring ≥2 online members fails a ratified requirement |
| Durability | Milestones saved only on **shared-canonical-storage acknowledgment** of a complete recoverable revision (M-29) | Tiered durability model (§6) |
| Rollback | Encounter-scoped transactions (M-30); committed progress never arbitrarily rolled back | Two-lane state model (§3) |
| Governance | Democratic, partly asynchronous, unanimity rules (M-32–M-34) | Ballot records must merge without a live host |
| Threat model | Cooperative friends; integrity against accidents/drift/bugs, not adversarial anti-cheat | Signed hash chains suffice |

**Corrections adopted from the review:**

1. **A hash chain proves integrity, not canonicalness.** Two divergent chains can both be internally valid; the chain alone cannot say which is *the* campaign. Canonicalness requires a **selection rule executable by any verifier** — a shared ordering point or an agreement procedure (§5). v0.1 under-specified this; v0.2 makes the selection rule the centerpiece of the authority proof.
2. **Pure peer replication fails the solo case.** With one member online there is no second replica to acknowledge and no witness against a concurrent session elsewhere; under the fail-closed policy that solo play cannot be authoritative — contradicting the ratified subset-play requirement. Formal analysis in §4 (architecture C).
3. **Third-party research is unlocked.** The constraint excludes *developer-paid, recurring* infrastructure as a requirement — not all third-party services. Free-tier or player-visible services are legitimate research candidates, with terms/availability/cost-trajectory as qualification questions (§4 E, §14 E5).

**Project evidence:** none directly (no networking in the prototype). Indirect: deterministic seeded generation and count-asserting smoke harness **[PROJECT EVIDENCE]** (reproducible world state — useful for encounter-boundary reconstruction); the `game.*` hub-coupling anti-pattern motivates strict state-ownership boundaries (PROTOTYPE-INVENTORY §4).

---

## 2. Logical subsystem architecture (Deliverable 1)

```text
 Game-specific policy (RPG rules — configured, not hardcoded)
   destination access · unanimity thresholds · milestone definitions · encounter rules
─────────────────────────────────────────────────────────────────────────────────
 Engine capability layer (reusable across games)
 ┌──────────────┐  ┌──────────────┐  ┌───────────────────────────────┐
 │ Identity     │  │ Governance   │  │ Campaign Authority            │
 │ stable IDs,  │  │ ballot set,  │  │ session lease, epoch fencing, │
 │ ownership    │  │ tallies      │  │ handoff, validation gate      │
 │ binding      │  │ (async-cap.) │  │ single-session enforcement    │
 └──────────────┘  └──────────────┘  └───────────────────────────────┘
 ┌──────────────────────────────┐  ┌────────────────────────────────┐
 │ Simulation Session (host)    │  │ Commit Pipeline                │
 │ authoritative sim; two-lane  │→ │ milestone capture, commit      │
 │ state; encounter transactions│  │ records, idempotency keys,     │
 └──────────────────────────────┘  │ dependency gating, UI states   │
                                   └────────────────────────────────┘
 ┌──────────────────────────────┐  ┌────────────────────────────────┐
 │ Revision Store               │←─│ Canonical Medium + Distribution│
 │ append-only log + snapshots, │  │ exclusive-append point for     │
 │ content addressing, applied- │  │ authority & T3 durability      │
 │ key ledger, verification     │  │ (research: medium or peers);   │
 └──────────────────────────────┘  │ peer sync as optimization      │
                                   └────────────────────────────────┘
 ┌──────────────────────────────────────────────────────────────────┐
 │ Recovery Service — emergency copies, resume validation,          │
 │ encounter-boundary reconstruction, transactional rollback        │
 └──────────────────────────────────────────────────────────────────┘
```

**Revision from v0.1:** the Distribution Service is renamed **Canonical Medium + Distribution**. In the recommended direction (§12, architecture E) the *authoritative append point* is a coordinated medium **[HYPOTHESIS]**; peer-to-peer replication becomes an optimization/caching layer, not the authority mechanism. All other subsystems are unchanged. Interface sketches from v0.1 remain valid; the medium adds `conditional_append(record, expected_head) → accepted/rejected` as the core primitive **[ESTABLISHED]** capability class (versioned stores with compare-and-swap semantics; HTTP conditional writes).

---

## 3. State ownership, persistence taxonomy, and state correctness (Deliverable 2)

**Two-lane model** (unchanged from v0.1): a *durable progression lane* (append-only, committed, never arbitrarily rolled back) and an *encounter transaction lane* (provisional, atomically committed or rolled back). Temporary simulation state lives outside both and is deliberately non-restored across terminated sessions.

| Class | Contents | Storage | Lifetime | Restoration semantics |
|---|---|---|---|---|
| **Durable progress** | Character progression, completed milestones, world consequences, frontier history, economy changes | Revision log + snapshots at the canonical medium (§4) | Permanent | Exact; never rolled back |
| **Provisional encounter state** | Pre-encounter ammo/consumables/equipment-condition snapshot, uncommitted loot, encounter-local world changes | Host-local, transaction-scoped | One encounter | Atomic commit on success; **rollback on failure** per M-30 |
| **Temporary simulation state** | Transforms, AI micro-state, projectiles, physics, weather | Host memory | One session | Not restored across terminated sessions (M-18 refinement); exact within a live suspension; reconstructed at the encounter boundary on resume |
| **Governed records** | Signed ballots, roster changes, retirement archives, handoff/lease history | Immutable records merged into the revision chain (and/or the async medium) | Permanent | Union-mergeable; deterministic tallies |
| **Campaign metadata** | Campaign ID, revision head, epoch, roster, ruleset/version, compatibility version | Revision chain head block | Permanent | Validated before any authoritative play (§5) |

**State correctness (review item 5):**

- **Proficiency vs encounter resources.** Use-earned skill proficiency accrues as *individually keyed, idempotent* records in the durable lane as it happens (keys: character + skill + accrual event); item/consumable/condition state stays inside the encounter transaction. On failure the transaction restores pre-encounter values and drops uncommitted loot while every already-recorded proficiency accrual stands. **[HYPOTHESIS]** — E1 must prove no double-counting and no loss across retries and crashes (applied-key ledger).
- **Irreversible authored-world changes** exist only as milestone commits and are **gated until T3 durability** (§6) — an irreversible story beat can never execute on unconfirmed progress (M-29).
- **Idempotent rewards.** Every reward/effect carries a stable key (campaign + milestone instance + character + effect). The applied-key ledger in the revision store makes application exactly-once; retries are safe by construction. **[ESTABLISHED]** pattern (idempotency keys); our ledger composition is **[HYPOTHESIS]**.
- **Mid-commit failure recovery.** The authoritative append point's state is the truth, not any acknowledgment message: if a conditional append succeeded but the ack was lost, a re-read of the head reveals the commit; if it failed, the record is still pending and a retry with the same keys is harmless. A crash between local append and medium append leaves the milestone pending (T1) — visible as pending in the UI, never reported confirmed. **[HYPOTHESIS]** — E1/E2 proof required.

---

## 4. Architecture alternatives — assumptions, guarantees, and the solo case (Deliverable 3, revised)

The binding test from the review: **a 4-person roster with exactly one connected member playing the frontier** must be able to commit milestones that count as saved (M-29) under provably exclusive authority (M-28) — or the architecture fails a ratified requirement.

**A. Full-mesh with majority quorum (Raft-flavored).** *Assumes* a majority of members is reachable. *Guarantees* fork prevention and majority durability when the assumption holds. *Solo case:* 1 of 4 is not a majority — **cannot commit; fails the subset-play requirement. Eliminated.** [ESTABLISHED] mechanism; wrong availability profile.

**B. Host-local with post-session push.** *Assumes* the host machine survives until peers are reachable. *Guarantees* T1 (local disk) durability only; no exclusivity proof; no shared-canonical acknowledgment. *Solo case:* commits are local-pending forever until another member appears — below the M-29 bar. **Retained only as a degraded/offline mode inside E** (progression suspension, emergency copies — fail-closed), never as the authority mechanism.

**C. Pure peer-replicated revision chain (v0.1's "Architecture D").** *Assumes* peers are contactable for acknowledgments and for the unanimous-present selection procedure. *Guarantees* (multi-member online): threshold-acknowledged durability, present-member-unanimous canonical selection, fenced authority. *Solo case — the disproof:* with one member online there is (i) **no second replica**, so the best available durability is the host's own disk (T1), which is not "shared canonical storage"; and (ii) **no witness** against a concurrent session run by an offline member on a diverged chain — exclusivity cannot be verified, only assumed. Under the ratified fail-closed policy (M-35), unprovable exclusivity forbids authoritative progression, so **solo frontier play would be unauthoritative** — contradicting the ratified subset-play requirement. **This is why the lead did not approve D, and this analysis concurs.** C survives only as the peer-optimization layer of E (replica caching, bandwidth sharing), never as the authority or durability mechanism.

**D. Third-party-coordinated / depot-primary.** A single external service is the canonical store *and* the coordination point. *Assumes* the service exists, is reachable, permits game-save use, and stays free. *Guarantees* strong central durability, clean exclusivity (single ordering point), full solo support. *Solo case:* works — session-open and commits go to the service. *Weakness:* everything depends on one provider (availability, terms, account model, longevity); the game inherits the provider's failure modes wholesale. **Viable but monolithic; subsumed as the medium component of E with the provider question made explicit (E5).**

**E. Coordinated canonical medium + peer optimization — recommended research direction.** The campaign's canonical history is an append-only, hash-chained revision log whose **authoritative append point is a shared medium with conditional-write (compare-and-swap on the current head) semantics**. All authority and T3 durability flow through the medium: session-open = conditional append (acquires exclusivity — a second concurrent open fails cleanly and fail-closes); commits = conditional append (the medium write **is** the canonical acknowledgment; no peer-ack threshold is needed). Peer connections remain for gameplay traffic and as a **replica-caching layer** (T2 optimization: local read latency, offline browsing of campaign state, bandwidth offload) — never required for correctness. When the medium is unreachable, gameplay **suspends** with local emergency copies (fail-closed, M-31/M-35) — no local-only authoritative continuation. *Assumes* a **qualifying medium** exists: conditional-write semantics, terms permitting game-save data, adequate availability, **not developer-paid**. Candidate classes to research (none approved): free-tier versioned object storage; WebDAV with ETag conditional writes; git-hosting push semantics (compare-and-swap via non-fast-forward rejection — terms for game-save use questionable); a player-operated always-on machine (permitted by the letter of the constraint — not developer-funded — but a single point of failure and a neutrality concern); Steam-adjacent facilities (Steam Datagram Relay for *transport* is fine; Steam Cloud is per-user and can carry each member's replica cache but is not a shared medium). *Solo case:* the lone member opens a session via medium CAS and commits at T3 — every ratified requirement holds.

| Criterion | A quorum | B host-push | C pure-peer (v0.1 D) | D depot-primary | **E medium + peer opt.** |
|---|---|---|---|---|---|
| Solo frontier commits | **Fails** | T1 only | **Fails (disproof above)** | Works | **Works (T3 via medium)** |
| Exclusive authority proof | Majority | None | Only with peers present | Single point | **Medium CAS, provable incl. solo** |
| Canonicalness selection | Majority vote | Host assertion | Present-member unanimity | The medium's head | **The medium's head** |
| T3 durability | Majority ack | Absent | Peer-threshold ack (unavailable solo) | Provider write | **Medium write** |
| Fork prevention | Yes | No | Detection only | Yes (CAS) | **Yes for committed state** (CAS double-append impossible); emergency-copy divergence still detected at validation |
| Developer-funded infra | $0 | $0 | $0 | $0 if provider is free | **$0 if a qualifying free medium exists (E5)** |
| Provider dependence | None | None | None | **Total** | **Medium for authority/durability; peers optional** |
| Medium outage behavior | n/a | n/a | n/a | Total outage | **Suspend + emergency copies (fail-closed); gameplay traffic unaffected** |
| Complexity | High (consensus) | Low | Medium | Medium | **Medium** |

**The honest precondition:** E is a *research direction*, not an approved architecture. If **no medium qualifies** (E5), the ratified requirement set — solo subset play + T3 acknowledgment + fail-closed exclusivity + zero developer-funded infrastructure — is **not simultaneously satisfiable**, and the lead faces an explicit trilemma: (i) define a solo durability tier below T3 (local-pending, promoted on next contact — a deliberate, lead-owned weakening of M-29 for the solo case only); (ii) accept a medium/provider dependence; (iii) accept paid infrastructure. This study flags the trilemma; it does not choose. **[OPEN]**

---

## 5. Authority proof — issuance, serialization, validation, enforcement (review item 2)

**What authority is:** the right to append the next record to the accepted history. **What canonicalness is:** a selection rule any verifier can execute to identify the accepted history. The hash chain provides integrity (tamper-evidence) for *a* history; it does not choose *between* histories. **[ESTABLISHED]** distinction; v0.1 conflated them — corrected here.

**Under architecture E [HYPOTHESIS]:**

- **Issuance.** A session opens by conditionally appending a *session-open* record `{campaign head, host identity, epoch = (highest epoch on the accepted chain) + 1, roster snapshot}` to the medium. The CAS succeeds only if the expected head is still the head — i.e., no other session opened or committed first.
- **Serialization.** The medium's single ordering point (conditional append) serializes all session-opens, handoffs, and commits. There is no distributed race: two concurrent opens resolve deterministically — first CAS wins; the loser's append is rejected, it re-reads the head, sees the winning session's epoch, and **fail-closes** (cannot start authoritative play). Exclusive authority is therefore *provable against everyone who can reach the medium*; members who cannot reach it are excluded from authoritative play by policy (fail-closed), never silently accommodated.
- **Validation.** Any verifier walks the chain from the medium's head: hash-chain integrity, signature checks, epoch monotonicity, parent-consistency (each record's parent is the prior accepted record). Unverified or outdated data cannot start authoritative play (M-28) — the gate is exactly this walk plus a fresh head read.
- **Enforcement.** Every commit carries `(parent revision, epoch)`. The medium enforces structurally (a stale-epoch or stale-parent record cannot be appended — CAS fails); every peer-side verifier rejects such records on sight even if delivered by other channels. This is the fencing-token discipline **[ESTABLISHED]** (Kleppmann; Raft terms) with the medium as the token mint.
- **Planned handoff (M-31).** Nominate → accept → conditionally append a *handoff* record (same head, epoch+1). Exactly one valid continuer at every step; the predecessor's epoch is fenced off the moment the record lands.
- **Authority loss.** Heartbeat/lease expiry or medium unreachability → progression suspends, local emergency copy preserved, resumption requires renewed validation (M-31). **[OPEN]:** heartbeat cadence and lease thresholds.

**Under pure-peer (C) — why it fails as authority:** the selection rule is "unanimous agreement of present members," which is *unexecutable* when the relevant members are offline; canonicalness is then merely assumed. Detection-after-the-fact (head comparison on next contact) is retained in E anyway as the fork check for **emergency copies** (local data appended off-medium during an outage must re-validate before use).

---

## 6. Durability proof — tiers and tolerated failures (review item 3)

| Tier | Definition | Tolerates | Does NOT tolerate |
|---|---|---|---|
| **T1 — local disk** | Record written to the host machine's disk | Client crash; process restart | Disk/machine loss; any other machine's failure; is not "shared canonical storage" |
| **T2 — replicated** | Record verifiably held on ≥2 independent member machines | Single machine loss (incl. the host's) | Two machine losses; unverifiable freshness until replicas communicate; not universally accessible |
| **T3 — canonical shared-storage confirmation** | The authoritative append point (the medium) has accepted a complete, recoverable revision | Any set of member-machine failures short of the medium itself; provides freshness + exclusivity + recoverability to **any** member, including solo and offline-rejoining ones | Medium outage at write time (the milestone stays pending — fail-closed, never falsely confirmed) |

**What "acknowledgment" means per architecture:** under E, the medium's conditional-append success **is** the T3 acknowledgment — peer acks are not required for durability (they are T2 optimization). Under C (v0.1), acknowledgment was a peer-replica threshold — which is *unavailable* in the solo case, the disproof of §4. Under B, nothing beyond T1 exists.

**M-29 mapping:** "a milestone counts as saved only after shared canonical storage acknowledges a complete, recoverable revision" = **T3**. The UI's pending/confirmed distinction is exactly T1/T2-pending vs T3-confirmed; irreversible actions gated on unconfirmed milestones are gated on T3. **[HYPOTHESIS]** until E1/E2 prove the protocol.

---

## 7. Failure and recovery analysis (Deliverable 4, expanded)

| Scenario | Mechanism (architecture E) | Verdict |
|---|---|---|
| **Solo frontier session (1 of 4 online)** | Medium CAS session-open; commits at T3 | **Proceeds authoritatively** — the ratified case |
| **Concurrent host attempts** | Second session-open CAS fails; loser re-reads and fail-closes | **Prevented** for committed state (v0.1 could only detect) |
| **Stale snapshot / outdated host** | Validation gate (head read + chain walk + epoch check) rejects | Fail-closed ✓ |
| **Mid-commit crash** | Idempotency keys + applied-key ledger; medium state is truth; re-read head on recovery | No double-apply; pending never shown confirmed — E1 proof |
| **Medium outage during play** | Progression suspends; local emergency copies; no local-only authoritative continuation | Fail-closed ✓ (M-31/M-35) |
| **Host↔medium partition** | Same as outage from the host's view | Fail-closed ✓ |
| **Planned host handoff** | §5 protocol; single continuer by construction | E2 proof |
| **Lease/authority expiry** | Heartbeat timeout → suspend → emergency copy → renewed validation | Thresholds **[OPEN]** |
| **Emergency-copy divergence** (two members hold different local copies from an outage) | Validation gate compares heads; divergent histories **never auto-merge**; resolution is governed | Policy **[OPEN]** (candidates: longest valid chain + present-member unanimity; loser archived) |
| **Guest disconnect mid-encounter** | Spec policy unchanged (15-minute live suspension; exact resume within the live session) | Confirmed input |
| **Host crash mid-encounter** | Session ends (no migration at launch); emergency copies; resume at encounter boundary from last T3 revision | Whether encounter-failure rollback applies **[OPEN]** (natural reading: yes) |
| **Async ballot while target offline** | Ballots ride the medium as immutable records; grow-only merge | E4 proof |
| **Rejoin after long absence** | Pull chain from medium; validate; resume via hub | E1/E3 proof |

---

## 8. Commit and dependency model (Deliverable 5)

**Commit record** (unchanged schema from v0.1): `{campaign, parent_rev, epoch, milestones[], deltas[], idempotency_keys[], dependencies[], host_signature}`.

**Protocol under E [HYPOTHESIS]:**

```text
1  Simulation reaches a milestone
2  Commit Pipeline builds CommitRecord (parent = current head, epoch = lease epoch)
3  → local append; status PENDING (T1); UI shows pending progress
4  → conditional-append to the canonical medium (CAS on head)
5  → accepted ⇒ revision DURABLE (T3); UI confirms; gates release
   rejected ⇒ head moved (concurrent activity) ⇒ re-validate, re-base, retry (same keys)
6  (optional, asynchronous) peer replicas pull the revision for T2 caching
failure at any step ⇒ retry with the SAME idempotency keys ⇒ applied-key
ledger ⇒ no duplicated rewards; a lost ack is resolved by re-reading the head
```

**Gating:** irreversible actions depend on `status == T3`. The list of gated actions is game policy for MULTIPLAYER-SPEC **[OPEN]**. **Snapshots vs deltas:** periodic content-addressed snapshots bound replay; cadence is an E3 measurement question. **[ESTABLISHED]** model (Git; WAL checkpointing).

---

## 9. Governance and identity plumbing

Unchanged from v0.1: ballots are signed immutable records; the ballot set is a grow-only union (**[ESTABLISHED]** CRDT family) with deterministic tallies regardless of arrival order; unanimity and ranked-choice rules are tally policies on top (M-32–M-34). Under E, the medium doubles as the **async ballot depot** (a member casts while others are offline; tallies recompute deterministically as ballots arrive). Stable member identity remains **[OPEN]** (platform-agnostic interface; provider mapping — e.g., Steam IDs — is a research candidate, not a selection; M-24 stays PROPOSAL).

---

## 10. Reusable engine boundaries (Deliverable 6)

| Engine capability (game-agnostic) | This RPG's configured policy |
|---|---|
| Revision store, content addressing, verification | Milestone definitions; delta schema |
| **Canonical-medium interface (conditional append) + pluggable providers** | Medium choice; provider terms acceptance |
| Fencing/epoch service, lease + handoff primitives | Handoff eligibility; hosting entitlement |
| Commit pipeline, idempotency, dependency gating | Which actions are irreversible/gated |
| Peer replica sync (T2 optimization) | Replica policy; bandwidth budgets |
| Ballot aggregation (grow-only, deterministic tallies) | Unanimity/participation rules per ballot kind |
| Encounter transaction + rollback framework | Encounter definitions; restore-on-failure lists |
| Identity interface + ownership binding | Provider mapping; character-slot rules |
| Recovery: emergency copies, validation, reconstruction | Resume UX; divergent-copy resolution policy |

---

## 11. Availability matrix (review item 6) — new test scenarios

Proceed = authoritative play possible; tier = durability level achieved.

| Scenario | A quorum | B host-push | C pure-peer | D depot-primary | **E medium+peer** |
|---|---|---|---|---|---|
| R1: 4/4 online, authored world | Proceed, majority T | Proceed, T1 | Proceed, peer-ack | Proceed, T3 | **Proceed, T3** |
| R2: 4/3 online, frontier | Proceed (3 = majority of 4? no — 3 of 4 IS a majority) T | Proceed, T1 | Proceed | Proceed, T3 | **Proceed, T3** |
| R3: 4/2 online, frontier | **Blocked** (2 < majority) | Proceed, T1 | Proceed | Proceed, T3 | **Proceed, T3** |
| R4: 4/1 online, frontier (the review case) | **Blocked** | T1 only | **Blocked by fail-closed** (no exclusivity proof) | Proceed, T3 | **Proceed, T3** |
| R5: 1-member campaign, solo | Blocked (no majority of 1? — 1 of 1 IS a majority; but B/C still fail) | T1 only | Proceed trivially (single member = unanimity) | Proceed, T3 | **Proceed, T3** |
| R6: one member's machine permanently lost | Recovers if majority intact | Progress on that machine lost | Recovers if another replica exists | Recovers from depot | **Recovers from medium** |
| R7: two members' machines lost | **Fails** if majority broken | Lost | Recovers only if a replica survives | Recovers | **Recovers** |
| R8: simultaneous hosting attempts | One leader elected | **Both run — divergence** | Both run; divergence detected later | **One wins (CAS); other fail-closes** | **One wins (CAS); other fail-closes** |
| R9: medium/service outage | n/a | Proceed (T1) | Proceed (peer mode) | **Total stop** | **Suspend + emergency copies (fail-closed); resume on recovery** |
| R10: host↔medium partition | n/a | Proceed | Proceed | Stop for that host | **Suspend for that host; peers see session stall** |
| R11: async ballot, target offline | n/a | n/a | Delayed until contact | Works via depot | **Works via medium** |
| R12: rejoin after long absence | Sync via majority | Pull from host | Pull from peers | Pull from depot | **Pull from medium; validate; resume via hub** |

(R5 nuance: a 1-member campaign makes peer-unanimity trivially satisfiable — C's failure is specific to *multi-person* campaigns playing solo, which is exactly the ratified frontier-subset case R4.)

---

## 12. Evaluation and recommended research direction (Deliverable 7)

- **A (quorum):** eliminated — fails R3/R4 availability.
- **B (host-push):** retained only as E's fail-closed degraded mode (never authoritative).
- **C (pure peer, v0.1's D):** **rejected — the review's disproof stands** (R4: no exclusivity proof, no T3 solo). Peer replication survives as E's optimization layer.
- **D (depot-primary):** viable but monolithic — total dependence on one provider including gameplay-adjacent flows; subsumed into E.
- **E (coordinated canonical medium + peer optimization):** satisfies every ratified requirement including the solo case; its dependence is scoped to authority + durability, with fail-closed degradation; its precondition — **a qualifying non-developer-funded medium** — is precisely quantifiable by E5.

**Recommended research direction [HYPOTHESIS, unapproved]:** pursue **E**. Priority order: (1) E5 medium characterization (the precondition — if it fails, the §4 trilemma goes to the lead before any further architecture work); (2) E1/E2 protocol proofs against a medium simulator; (3) E3 sizing. A numeric scorecard is deferred until experiments produce measured evidence.

---

## 13. Fail-closed policy (review item 7) — preserved and explicit

1. **No authoritative progression without verified freshness and exclusivity** — both proven against the canonical medium at session open and maintained by lease/heartbeat.
2. **Medium unreachable ⇒ suspend**, preserve local emergency copies, never continue authoritatively on local-only state.
3. **Divergent histories are never automatically merged** — detection at the validation gate routes to governed resolution; losing branches are archived, not destroyed.
4. **Durability claims always reflect the true tier** — the UI never shows T3-confirmed before the medium accepts; solo/degraded modes may not relabel T1 as saved.
5. **Solo play under any mechanism that cannot prove exclusivity is blocked from authoritative campaign advancement** — this is the rule that disqualifies pure-peer (§4 C) and that E satisfies via medium CAS.

These rules restate ratified policy (M-35, M-28, M-31); they constrain every architecture in this study.

---

## 14. Proposed experiments (Deliverable 8) — revised 2026-10-17; NOT implemented, NOT authorized

**E1 — Revision-chain core + medium simulation.** *Goal:* prove log/snapshot/idempotency/CAS semantics. *Method:* headless harness implementing CommitRecord append, hash chain, applied-key ledger, and a **medium simulator with conditional-append**; scripted 4-peer scenarios: commits, solo-member sessions (R4), offline rejoin, duplicate delivery, mid-commit crash, lost-ack recovery via head re-read. *Acceptance:* zero duplicated effects in 100% of injected duplicate/retry cases; solo sessions reach T3 exactly like multi-member sessions; two-lane proficiency/item split commits and rolls back correctly.

**E2 — Fencing, handoff, and contention.** *Goal:* prove single-continuer and exclusivity. *Method:* scripted planned handoffs; concurrent session-opens against the medium simulator (R8); stale-epoch and stale-parent injection. *Acceptance:* exactly one valid epoch per handoff; 100% of concurrent opens produce one winner and one clean fail-close; 100% of stale records rejected.

**E3 — Latency and size characterization.** *Goal:* size the budget. *Method:* representative milestone payloads through the medium path (loopback + a real conditional-write round-trip) and peer paths; snapshot/delta sizing. *Acceptance:* measured tables; proposed snapshot cadence; no invented thresholds.

**E4 — Governance ballot aggregation.** *Goal:* deterministic async tallies. *Method:* randomized ballot orders, duplicates, late votes, mixed unanimity/ranked kinds. *Acceptance:* identical tallies in 100% of orderings; duplicates ignored.

**E5 — Canonical-medium characterization (expanded; the E precondition).** *Goal:* determine whether a qualifying medium exists. *Method:* laboratory evaluation of candidate service classes: conditional-write/versioning semantics (ETag/If-Match or equivalent), terms of service for game-save data, availability history, account/credential model, free-tier limits and cost trajectory, and an exit/migration path (export format, portability). Include player-operated options with their availability/neutrality caveats. *Acceptance:* a capability report per candidate; any candidate failing semantics or terms is disqualified; **if no candidate qualifies, the §4 trilemma is reported to the lead immediately.** No provider is approved by running this probe.

---

## 15. Risks and unresolved decisions

**Risks (highest first):**
1. **Medium qualification (E5)** — the entire E direction rests on a qualifying non-developer-funded medium existing with acceptable terms and availability.
2. **The trilemma** — if E5 fails, the lead must choose among solo-tier weakening, provider dependence, or paid infrastructure (§4).
3. **Emergency-copy divergence UX** — off-medium local copies from outages can diverge; resolution is governed and possibly contentious.
4. **Two-lane correctness** — proficiency accrual must never double-count or lose; E1 gates.
5. **Schema evolution** — canonical format changes across game versions must not invalidate old chains; migration policy unwritten.
6. **Complexity creep** — medium + fencing + transactions + governance for a 1–4 player game; simplifications that preserve ratified requirements should win.

**Unresolved decisions (for the lead):**
1. E5 medium pursuit and acceptance criteria (terms, availability floor).
2. The trilemma response if no medium qualifies.
3. Divergent emergency-copy resolution policy.
4. Host-crash-mid-encounter: does encounter-failure rollback apply?
5. Lease/heartbeat thresholds.
6. Identity provider mapping (M-24 remains PROPOSAL).
7. The gated-irreversible-actions list (game policy for MULTIPLAYER-SPEC).

---

## 16. References

- Ongaro & Ousterhout, *In Search of an Understandable Consensus Algorithm* (Raft), 2014 — term/epoch fencing; why majority quorum fails low-participation availability.
- Kleppmann, *Designing Data-Intensive Applications*, 2017, and *How to do distributed locking*, 2016 — fencing tokens, single-writer discipline, idempotency.
- Gray & Cheriton, *Leases*, 1989; Burrows, *The Chubby Lock Service*, 2006 — lease semantics.
- Shapiro et al., *Conflict-free Replicated Data Types*, 2011 — grow-only sets for ballot aggregation.
- Git object model (git-scm.com) — hash-chained immutable history + snapshots.
- SQLite WAL documentation — durable append + periodic checkpointing.
- HTTP conditional requests / ETag semantics (RFC 7232 lineage) — the conditional-write (compare-and-swap) capability class the canonical medium requires.
- Valve Steamworks documentation — Steam Networking Sockets / Steam Datagram Relay (relayed transport, no developer-funded servers); Steam Cloud (per-user storage; not a shared medium).

All citations mark **[ESTABLISHED]** concepts; this study's compositions remain **[HYPOTHESIS]** until E1–E5 pass. No provider or technology is selected.

---

*Revised 2026-10-17 after three-way review. Research-only: no code, no technology selection, no approved-requirement changes. The recommended direction (architecture E) is an unapproved hypothesis whose precondition (E5) is explicitly quantified; the §4 trilemma is flagged for the lead, not decided.*
