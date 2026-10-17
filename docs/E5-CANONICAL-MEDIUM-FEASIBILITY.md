# E5 — CANONICAL-MEDIUM FEASIBILITY (Research Annex)

> **Status:** Research annex v0.3 · 2026-10-17 · **UNAPPROVED RESEARCH.** v0.3 incorporates the lead's independently verified R2 findings (subscription activation; charges beyond free-tier usage; budget alerts are not hard spending caps), the **action-scoped Temporary Credentials** distinction (locally signed; PutObject-without-DeleteObject possible — trusted-signer analysis in §5.1), the directive **not to assume S3-native object versioning**, and the **local simulated-medium harness results** (§9, run 2026-10-17). Retained from v0.2: T3-vs-DR distinction, owner denial/deletion powers, cross-medium authority transition, credential security model, and the authorized real-provider E5-Lab scope with **Cloudflare R2 as first experimental candidate — not a production selection**. **Architecture E is the leading research direction, NOT a selected implementation architecture.** No provider, language, storage format, or production architecture is selected. E1–E4 are not implemented; the real-provider probe remains **gated on flagged prerequisites** (§8).
>
> **Ownership:** Annex to [`MULTIPLAYER-ARCHITECTURE-RESEARCH.md`](MULTIPLAYER-ARCHITECTURE-RESEARCH.md) v0.2 (its §14 E5 is expanded here). [`MULTIPLAYER-SPEC.md`](MULTIPLAYER-SPEC.md) owns player-facing policy; [`DECISIONS.md`](DECISIONS.md) is the decision registry (M-27–M-35 are the ratified requirements this annex must satisfy but never modify).
>
> **Evidence key:** **[ESTABLISHED-CLASS]** = a documented external capability class exists; **[TO-VERIFY]** = a specific per-provider claim that only the E5 lab probe or reading the current provider contract can confirm — this annex deliberately does not assert unverified specifics; **[ASSUMPTION]** = labeled workload/cost assumption pending E3 measurement; **[HYPOTHESIS]** = this study's unproven composition; **[OPEN]** = unresolved decision for the lead.

---

## 1. What the canonical medium must provide (requirement traceability)

| Capability | Why (ratified requirement / study §) |
|---|---|
| **Atomic conditional append** (compare-and-swap on the current head) | Exclusive session authority incl. solo (M-28; study §5) |
| **Durable persistence on acknowledgment** | Milestones saved only on shared-canonical acknowledgment (M-29; study §6, T3) |
| **Shared read access for all roster members** | Rotating hosts; rejoin after absence; validation gate (M-28, study §5) |
| **Reachable by a single member alone** | Subset/solo frontier play — the R4 case (spec §3.2; study §4) |
| **Not developer-paid recurring** | Infrastructure constraint (lead's 2026-10-17 corrected reading: research into existing services approved; no developer-paid recurring infrastructure may be *required*) |
| **Terms permitting game-save data** + **export/migration path** | Commercial viability; exit strategy (lead's authorization item 3) |
| **Scoped, revocable credentials** | So storage access cannot bypass governance (lead's authorization item 6; §5 below) |

## 2. Provider-feasibility matrix

Candidate **classes** — no provider is selected; every specific claim below requires the E5 lab probe against a current contract.

| Candidate class | Atomic conditional writes | Durability | Availability | Shared access | Free-tier fit [TO-VERIFY] | Terms risk | Credential scoping | Exit path | Verdict |
|---|---|---|---|---|---|---|---|---|---|
| **S3-compatible free-tier object storage** (e.g., Cloudflare R2, Backblaze B2, Storj, IDrive e2) | S3 conditional-write primitives exist as a protocol family (If-None-Match create-only; If-Match update) **[ESTABLISHED-CLASS]** — per-provider support and semantics **[TO-VERIFY]** | Provider-managed redundant storage; SLA-dependent **[TO-VERIFY]** | High (commercial CDN-grade networks) | Via bucket/prefix credentials shared by the roster | Typically 5–25 GB free; some with zero egress **[TO-VERIFY]** | Generally permissive for arbitrary app data on business/dev accounts **[TO-VERIFY]** | IAM-style tokens; granularity per provider **[TO-VERIFY]** (R2: API tokens are bucket-scoped with no action split, but **action-scoped Temporary Credentials** — locally signed, incl. PutObject-without-DeleteObject — exist; lead-verified 2026-10-17, §5.1) | Object export = trivially portable bytes | **Strongest candidate class** — probe first |
| **Git-hosting push-CAS** (GitHub/GitLab free private repos) | Push is a true CAS on refs (non-fast-forward rejection) **[ESTABLISHED-CLASS]** | Strong (hosting-grade) | High | Repo collaborator access; per-member identities | Generous free private repos **[TO-VERIFY]** | **ToS gray-to-high:** repo hosting terms historically discourage non-code/general-purpose data storage **[TO-VERIFY]**; API rate limits | Per-repo collaborators; fine-grained tokens **[TO-VERIFY]** | Clone = full export | **Experiment-only candidate** — excellent for the E5-Lab probe and prototypes; terms risk for production |
| **Consumer cloud drives / WebDAV** (Dropbox, Google Drive, Nextcloud) | ETag/If-Match exists on some WebDAV surfaces; consumer-API conditional semantics inconsistent **[TO-VERIFY]** | Good | High | OAuth per-member or shared folder | Consumer free tiers are large but **terms commonly restrict non-interactive arbitrary storage** **[TO-VERIFY]** | **High** | OAuth token complexity; scoping varies | Export tools exist | **Weak candidate** — terms and consistency are the concerns |
| **Player-operated always-on storage** (a member's NAS/mini-PC running WebDAV or an S3-compatible server) | Depends on the chosen server software (S3-compatible servers and WebDAV servers with ETag support exist) **[TO-VERIFY per software**] | Owner's hardware/RAID | Owner's uptime; home bandwidth (commits are KB-scale; snapshots MB-scale — plausible) | Full owner control; roster credentials | $0 by construction | No third-party terms | Server-configurable (write-only, per-prefix) **[TO-VERIFY]** | Trivial (it's plain files) | **Viable for technical groups**; product friction for general players; trust asymmetry scoped to one campaign |
| **Developer-managed free-tier compute/KV** (Workers KV / Durable Objects, Supabase, small-PaaS allowances) | KV stores often lack native CAS; actor-style systems (single-threaded objects) provide a serialization point **[TO-VERIFY]** | Provider-managed | High | Via the dev's application | Free tiers exist but write quotas can be tight at aggregate scale **[TO-VERIFY]** | Permissive **[TO-VERIFY]** | Dev-defined | Custom export | **Conditional candidate** — only as an *optional, non-required* convenience (see §3) |
| **Steam-adjacent facilities** | None provide shared conditional storage: Steam Cloud is per-user (replica cache only); SDR is transport-only **[ESTABLISHED-CLASS]** | — | — | — | — | — | — | — | **Excluded as a medium** (retained: SDR as transport; Steam Cloud as personal replica sync **[TO-VERIFY]**) |

## 3. Deployment alternatives — storage-account arrangements (authorization item 4)

| Dimension | **Developer-managed** (one dev account serves all campaigns) | **Player-managed** (the campaign's members provide storage) | **Hybrid — per-campaign governed medium** |
|---|---|---|---|
| Constraint compliance (no dev-paid infra *required*) | Only while the free tier suffices — aggregate growth converts it into developer-paid (violates if required) **[ASSUMPTION]** | **By construction** | **By construction** if the default is player-managed |
| Cost to developer | $0 at small scale; grows with total campaigns | $0 | $0 |
| Aggregate-scale risk | **High** — 1,000 campaigns ≈ 30–60 GB/year exceeds typical free tiers (§6) | None (per-campaign ≈ ≤1 GB/year) | None |
| Availability | Provider SLA | Owning member's uptime or chosen provider | Per-campaign choice |
| Trust asymmetry | **Developer is a universal superuser** — one blast radius across *all* campaigns; dev becomes de-facto operator | Owning member is superuser for *one* campaign | As player-managed; **rotation is a governed operation** — the roster votes which member's account is canonical; migration is a separate cross-medium authority transition (§4.1), not merely a host handoff |
| Player UX friction | **Zero** (best for a Steam-purveyed audience) | **High** — provisioning accounts or running storage | Medium — guided setup, presets, or "bring your own" |
| Migration | Needed at scale-out | Not needed | Built-in (governed rotation) **[HYPOTHESIS]** |
| Governance fit | Weak (dev outside the democratic model) | Good | **Best** — medium ownership becomes campaign-configured state |

**Research framing [HYPOTHESIS, not a decision]:** the constraint-native and economics-native arrangement is **per-campaign player-managed media** (each campaign's data is tiny — §6 — and fits any free tier or a member's own hardware), with the engine treating the medium as **pluggable per-campaign configuration**, and governed rotation/migration as the protocol that keeps ownership aligned with the roster. A developer-managed convenience default is only acceptable if it is genuinely *optional* (terms allow, capacity holds, and campaigns can leave) — whether that is desirable at launch is **[OPEN]** for the lead.

## 4. T3 durability — exact guarantees and failure model (authorization item 5)

**T3-confirmed (exact statement):** a revision is T3-confirmed when (1) the medium accepted it via a **successful conditional append** — this instant is the linearization point that orders all accepted history; (2) the provider's **persistence acknowledgment** covers the object (durability is per-provider design/SLA — redundant storage targets are typical but **[TO-VERIFY]** per candidate); and (3) the revision is **chain-complete** — parent-reachable from a snapshot, content-verifiable, replayable. All three conjuncts are required; none alone suffices.

**T3 versus disaster recovery (lead clarification, 2026-10-17):** T3 is *canonical acknowledgment* — it linearizes, persists (per the provider's acknowledgment), and completes the revision **at the medium**. It does **not** by itself guarantee survival of provider-side deletion, account termination, or owner action: a T3-confirmed revision whose medium is deleted is gone **unless an independent recovery copy exists**. Survival of those failures is the role of the **additional peer-replica disaster-recovery layer** (and migration) — separate from, and on top of, T3, never a redefinition of it. The always-on-replica recommendation below is therefore a disaster-recovery recommendation; M-29's acknowledgment standard remains T3 alone.

**Failure model — what T3 tolerates, and what it does not:**

| Failure | T3 status | Behavior / recovery |
|---|---|---|
| Medium outage **at commit time** | Milestone stays **pending** | Fail-closed (M-35): retry with same idempotency keys on recovery; never falsely confirmed |
| Medium outage **at session open** | — | Cannot open; fail-closed |
| Provider data loss (object loss/corruption) | Affected revisions lose T3 | **Detected** by chain verification (hash chain); recover the prefix from any peer replica; governed re-append **[HYPOTHESIS]** |
| Account termination / credential revocation | Availability loss, not integrity loss | Migrate the chain from any surviving replica to a new medium; re-validate |
| **Stale reads** (client-side) | No effect on correctness | CAS resolves ordering (a stale writer simply fails); freshness *reads* need read-your-writes consistency — S3-family strong consistency is documented post-2020 **[TO-VERIFY per provider]** |
| Split-brain at the medium | — | **Impossible by construction** (single CAS ordering point) — this is the core of Architecture E |
| Deletion/jamming by a credential holder | Integrity preserved; availability damaged | §5 mitigations; fail-closed suspension |
| Account owner deletes everything | Integrity lost *at the medium* **unless an independent replica exists** | **Cannot be fenced at the medium** — T3 does not cover owner deletion (see the T3-vs-DR distinction above); recovery only via peer replicas + migration; a documented per-arrangement trust asymmetry (§3, §5 R7) |

**Key recommendation [HYPOTHESIS]:** the engine should **always maintain peer replicas of committed revisions** (T2 backup alongside the T3 canonical). At §6's data scale this is effectively free, and it converts a provider catastrophe (loss, termination, mass deletion) from *data loss* into a *migration event*. T3 remains the acknowledgment standard (M-29); replicas are recovery depth, not a substitute.

### 4.1 Cross-medium authority transition — a separate problem, not a host handoff (lead clarification, 2026-10-17)

Moving a campaign's canonical medium is an **authority transition between ordering points**, fundamentally different from a live host handoff (which transfers a lease *within* one medium, fenced by that medium's single CAS point). During a migration, two media could each serve as a valid conditional-append point, so the protocol must guarantee **exactly one medium is canonical at any time**. Candidate shape **[HYPOTHESIS]**: (1) a *terminal record* appended at the old medium — "this campaign now lives at M2, chain head rN" — closing the old ordering point; (2) a *founding record* at M2 whose parent is that same old head, opening the new ordering point; (3) validation rules that accept the old medium only up to its terminal record and the new medium only from its founding record; (4) peer replicas re-anchored and re-verified after the transition. Degenerate cases make this strictly harder than a handoff: the old medium may be **unreachable or already deleted** when the decision is made, so the transition must also be constructible from a peer replica plus **governed resolution** (never auto-merge — M-35). Open questions **[OPEN]**: who initiates migration (a governance vote is the natural candidate); how concurrent writes during the transition window are arbitrated; how members holding only the old medium's credentials discover the migration. Protocol design is a future research item and is **not** part of the E5-Lab probe.

## 5. Authentication, authorization, and conditional writes (authorization item 6)

**The layering principle — storage credentials are not authority:**

```text
Storage credentials  ⊂  Protocol authority  ⊂  Governance legitimacy
(byte-write access)     (valid signed records)  (what the roster voted)
```

- **R1 — Signatures:** every chain record is individually signed by the *acting member* (session-open by the host, handoff by departing host + successor, commits by the host-of-record, ballots by each voter). The medium can order bytes; it **cannot invent authority** — a record without a valid signature is rejected by every verifier.
- **R2 — Session-open validity is protocol-checked, not medium-checked:** a session-open record is valid only if its protocol preconditions hold (prior lease closed or expired; host eligibility). Every client verifies these rules; the medium's CAS only serializes. A member who holds storage credentials and writes their own session-open **competes via CAS like anyone else** — against a live valid lease, verifiers reject the result; the residual case (fake closure) requires forged signatures, which they cannot produce.
- **R3 — Handoff** requires both the departing host's and the successor's signatures.
- **R4 — Ballot outcomes are computed, never asserted:** tallies derive from the signed ballot record set (study §9); no head-write can fabricate a vote outcome. Writing to storage therefore **cannot bypass campaign governance**.
- **R5 — Residual storage-layer attacks are denial-of-service shaped:** head-jamming (appending garbage to move the head) and deletion. Mitigations: verifiers fall back to the **last valid prefix** and route to governed resolution (never auto-merge — M-35); **action-scoped credentials** — R2 API tokens are bucket-scoped with no action split, but R2's **Temporary Credentials** (lead-verified 2026-10-17) are minted by **local signing with a parent token** and support **action scoping, including PutObject without DeleteObject** (trusted-signer implications in §5.1); **S3-native object versioning is NOT assumed available** (lead directive 2026-10-17 — do not rely on it); peer replicas as recovery depth (§4).
- **R6 — Credential rotation/revocation is itself a governance operation** (roster vote), so removing a retired or removed member's write access follows the democratic rules rather than storage-admin fiat — protocol support is **[OPEN]**.
- **R7 — The honest asymmetry (lead clarification, 2026-10-17):** an *account owner* (developer in developer-managed; the owning member in player-managed) can always **deny access to or delete** what the account holds. **Signed records protect authenticity — they prove who wrote what and that records were not forged — but they cannot prevent an owner from denying access or deleting stored data**; no protocol layer fences the account owner at the medium. Mitigations are recovery-shaped, never preventive: peer replicas (§4 DR layer), migration (§4.1), governed rotation, and product-UX disclosure to rosters **[OPEN]**.

### 5.1 Credential security model — working proposal [HYPOTHESIS]

| Credential tier | Holder | Can | Cannot | Provisioned by |
|---|---|---|---|---|
| **Owner/admin** | The account owner (per the §3 arrangement) | Everything: mint/revoke tokens, delete data, manage billing | — | The owner (account creation) |
| **Member scoped write token** | Each roster member | Conditional append (object write) within the campaign's bucket | Administer buckets, touch other buckets, billing | The owner, or an escrowed minting procedure (below) |
| **Read-only token** | Members, prospective hosts, validation | Read and verify the chain | Write | The owner or escrow |

**Rules:** (a) members normally operate with **action-scoped temporary credentials where available** (R2: PutObject-without-DeleteObject via locally signed tokens — lead-verified 2026-10-17) or bucket-scoped object-write tokens; deletion-resistance relies on that action scoping plus the peer-replica DR layer (§4) — **S3-native object versioning is NOT assumed available** — and disposable-bucket hygiene during experiments. (b) Tokens are per-campaign, bucket-scoped, revocable, and never committed to the repository — session environment only. (c) **The offline-owner problem (lead clarification, 2026-10-17):** if only the account owner can mint or revoke scoped tokens, the owner's absence blocks onboarding new members, rotating a compromised token, or migrating media. Candidate mitigations, all **[HYPOTHESIS]**: (i) *pre-provisioned token batches* — the owner mints a reserve of scoped tokens ahead of time, activated on governance vote, with an accepted revocation lag; (ii) *escrowed minting rights* — a break-glass minting credential split across the roster (secret-sharing, n-of-m reconstruction on governance vote), so token lifecycle never depends on one person being online; (iii) *provider-native delegation via locally signed temporary credentials* — R2's Temporary Credentials are minted by **signing locally with the parent token's secret key** (lead-verified 2026-10-17: action scoping incl. PutObject-without-DeleteObject is supported). This materially changes the offline-owner analysis: **minting requires no Cloudflare API call and no account-owner presence — only the parent token.** Escrowing the parent token across the roster (secret-sharing, n-of-m on governance vote) would therefore let a governed quorum mint action-scoped member credentials while the owner is offline. **Trusted-signer implications [TO-VERIFY in probe]:** the parent token becomes the minting trust root — its holder can mint arbitrary action-scoped credentials within the parent's scope until the parent is revoked at the account level; whether revoking the parent token invalidates already-minted locally signed children is unverified; parent rotation remains an account-level (owner-online) operation, so any escrow must pair with a rotation policy. Choosing among these mitigations is **[OPEN]** for the lead. The E5-Lab probe needs only a single bucket-scoped Object Read & Write token, so this model does not gate the experiment.

**Conclusion:** valid storage credentials grant byte-write only. Governance and session authority require signatures satisfying protocol rules that encode the ratified governance model; what credentials *can* do is deny service, and the fail-closed policy answers denial with suspension — never with bypass.

## 6. Modeled costs and quotas — explicitly labeled assumptions (authorization item 7)

**Every figure in this section is an [ASSUMPTION] pending E3 measurement; none is measured.**

Workload assumptions (per 4-member campaign):

| Quantity | Value [ASSUMPTION] | Basis of the guess |
|---|---|---|
| Sessions | 12/month × 2.5 h | Typical committed co-op group |
| Milestone commits | 40/session → ~480/month | Quest completions, skill-ups, world events |
| Commit record size | 8 KB average (2–20 KB range) | Compact deltas + idempotency keys |
| Snapshots | 4/month × 5 MB (1–10 MB range) | Periodic full canonical state |
| Ballots | Dozens/month, <1 KB each | Ranked-choice + unanimity votes |
| Storage growth | **~25–50 MB/month → ~300–600 MB/year** | Commits + snapshots + chain metadata |
| Reads | Head-reads on open/reconnect (~10–50/member/month); join pulls 10–50 MB | Validation gate + rejoin |
| Object operations | ~500–1,000 writes/month; ~2,000–10,000 reads/month | Commits + head updates + ballots |

**Per-campaign fit:** ≤1 GB/year and ≤10k ops/month sits **20–100× inside** every typical free tier (≥5–10 GB, ≥1M-class operation quotas **[TO-VERIFY]**) — per-campaign data is *small* by cloud-storage standards.

**Aggregate (developer-managed scenario) [ASSUMPTION]:** 100 campaigns ≈ 3–6 GB year-one (marginal fit on 10 GB free tiers); **1,000 campaigns ≈ 30–60 GB — exceeds typical free tiers**, at which point a developer-managed medium becomes developer-paid and *required* — a constraint violation. This is the quantitative case behind §3's preference for per-campaign arrangements.

**Player-operated bandwidth [ASSUMPTION]:** commits are KB-scale (fine on home upload links); snapshots are 1–10 MB occasionally (plausible); join pulls 10–50 MB (acceptable). E3 must measure real payload sizes before any of this is treated as fact.

## 7. Critical risks register (ranked)

1. **Medium qualification failure** → the study's trilemma reaches the lead (solo-tier weakening / medium dependence / paid infra). Everything else is downstream of this.
2. **Terms-of-service risk** — game-save data on free/consumer tiers (consumer drives historically restrict non-interactive arbitrary storage; repo hosting discourages non-code storage) **[TO-VERIFY]**.
3. **Player friction** — asking a Steam-purveyed audience to provision storage accounts or run hardware is a real product risk; mitigations: guided presets, provider recipes, the optional dev-managed convenience (if terms allow), player-operated appliances for technical groups.
4. **Free-tier drift** — quotas and pricing change over time; the exit/migration path and per-campaign portability are mandatory features, not nice-to-haves.
5. **Solo availability dependence** — solo sessions hard-depend on the medium's reachability; player-operated media have uptime limits (fail-closed makes this safe but inconvenient).
6. **Credential lifecycle** — 4-way sharing, scoping, rotation UX (R5–R7).
7. **Consistency surprises** — non-S3-class media with eventual consistency or weak ETags **[TO-VERIFY]**.
8. **Account-owner superpowers / blast radius** per arrangement (§3, §5 R7).
9. **Rate limits** on API-driven media (git hosts in particular).
10. **Single-region outages** on otherwise-strong providers.

## 8. Authorized smallest experiment — "E5-Lab CAS probe" (authorized 2026-10-17; **gated on flagged prerequisites — NOT yet run**)

The lead authorized a single, isolated E5-Lab CAS probe on 2026-10-17 with **Cloudflare R2 as the first experimental candidate — explicitly not a selected production provider**. The experiment stays headless and provider-adapter-focused: no gameplay code, no real multiplayer integration, no production architecture implementation.

**Authorized scope:** (1) atomic conditional-append behavior (create-only and update CAS on the head); (2) concurrent-writer contention — exactly one winner per round; (3) stale-epoch / authority-fencing behavior — stale conditional tokens rejected; (4) retry idempotency and ambiguous request outcomes — timeout-after-write resolved by head re-read, no duplicate effects; (5) crash recovery of the latest acknowledged revision; (6) credential permission boundaries — what the scoped token can and cannot reach; (7) read-after-write behavior, latency, request counts, and a 24-hour persistence check.

**Safety rules (ratified with the authorization):** disposable data only; accidental-charge prevention; credentials never committed to the repository — session environment only.

**Prerequisite verification (2026-10-17; source: Cloudflare's published documentation — developers.cloudflare.com/r2/pricing and /r2/api/tokens):**

- **[VERIFIED — flag: account-owner action + paid activation]** "You must purchase R2 before you can generate an API token" (R2 API tokens documentation). Enabling R2 requires a Cloudflare account with billing/payment information on file — even though usage within the free tier incurs no charge.
- **[VERIFIED]** Free tier: **10 GB-month storage, 1M Class A operations, 10M Class B operations per month; egress free.** The probe's planned workload (thousands of operations, well under 100 MB) sits far inside these limits — expected charge **$0**.
- **[VERIFIED — lead independently confirmed 2026-10-17]** R2 requires subscription activation and **can incur charges beyond free-tier usage; budget alerts are not hard spending caps** (completing the documentation finding that beyond-limit usage is billed, service does not stop, and billing rounds up). Charge safety therefore rests on the probe's bounded operation counts plus a token scoped to one disposable bucket. Helpful detail: `DeleteObject`, `DeleteBucket`, and `AbortMultipartUpload` are free operations, and unauthorized (401) requests are not billed.
- **[VERIFIED — design finding, refined 2026-10-17]** R2 **API-token** permissions are four fixed levels (Admin Read & Write, Admin Read only, **Object Read & Write**, Object Read only) with **bucket-level scoping only — no action split** at the token level: an object-write API token can also delete objects in its buckets. **Lead-verified refinement:** R2's **Temporary Credentials** — minted by locally signing with a parent token's secret — support **action scoping, including PutObject without DeleteObject**, plus prefix scoping; this restores a write-without-delete path for members at the cost of a trusted signer (the parent-token holder) — implications in §5.1(c). **S3-native object versioning is not assumed available** (lead directive 2026-10-17).
- **[VERIFIED — local environment]** No Cloudflare/R2/AWS credentials exist in the working environment; `curl` and `python3` are available for a provider-adapter harness (no AWS CLI/rclone/mc — the adapter will speak the S3-compatible HTTP API directly; S3 conditional-write behavior on the R2 endpoint is itself a probe item).

**Flags requiring account-owner action before the probe proceeds (per the lead's prerequisite gate):**
1. Create/own a Cloudflare account and **enable R2** — requires payment information on file (the paid-activation flag; expected probe cost $0 within free limits).
2. Create a **disposable bucket** for probe data.
3. Mint an **R2 API token with Object Read & Write scoped to that bucket only**, and provide its Access Key ID, Secret Access Key, and account ID (for the endpoint) via the session environment.

**Status: NOT RUN.** Per the authorization's prerequisite gate, the probe halts here pending the flagged account-owner actions. No measured results exist; nothing in this section is evidence.

## 9. Local simulated-medium harness — authorized and run (2026-10-17)

**Authorization (lead, 2026-10-17):** "construct and run a narrowly scoped, headless, local-only E5-Lab harness using a deterministic simulated canonical medium. Test CAS contention, fencing, ambiguous retries, crash recovery, and revision validation. Document limitations clearly." Prohibitions honored: **no live R2 connection, no cloud resources, no payment credentials, no charges, no network** — this is protocol logic over a simulation, not a provider test and not engine implementation.

**Artifacts:** `experiments/e5-lab-local/harness.py` (Python 3.12, standard library only, deterministic — a simulated conditional-append medium, HMAC-signed hash-chained records with idempotency keys and session epochs) and `experiments/e5-lab-local/results.json` (machine-readable results). Run: `python3 experiments/e5-lab-local/harness.py` — exit 0, 2026-10-17.

**Results — all checks passed; results fingerprint `527220de7c202777`, identical across two in-process runs:**

| Test | Result | Measured outcome |
|---|---|---|
| T1 CAS contention | **PASS** | 100 rounds × 2 simultaneous appends at the same head: 100 accepted / 100 rejected; no round produced two winners; losing records never entered the chain (length 101) |
| T2a Stale-parent fencing | **PASS** | 20/20 appends with a stale expected-head rejected by CAS; chain unchanged |
| T2b Stale-epoch fencing | **PASS** | the epoch-blind medium accepts a fresh-parent/stale-epoch record; chain validation rejects it ("stale-epoch" at index 6); the last-valid-prefix fallback returns the pre-injection head |
| T3 Ambiguous retries | **PASS** | 50/50 idempotency keys committed exactly once under apply-then-drop-ack / drop-then-timeout ambiguity; resolution by chain re-read; chain valid |
| T4 Crash recovery | **PASS** | writer destroyed after an unacknowledged accepted append; the restarted writer re-read the chain, found its key already committed (no duplicate), opened a valid successor session (epoch 2), and completed all 30 keys — no gaps, no duplicates |
| T5 Revision validation | **PASS** | good chains validate (hash-chain integrity, parent links, signatures vs member registry, session-epoch discipline, idem-key uniqueness); 5/5 injected faults (tampered payload, forged signature, duplicate idem-key, stale epoch, parent mismatch) rejected with the correct reasons |
| T0 Determinism | **PASS** | identical results fingerprints across two full suite runs |

**Limitations (explicit):** the simulated medium is in-process and deterministic — it models the conditional-append *contract*, not a real provider. Not tested here: real CAS semantics on a provider endpoint (If-Match/If-None-Match behavior and error codes), durability and persistence SLAs, network latency/partition/timeout distributions, authentication and credential boundaries, read-after-write consistency, quota/billing behavior, and true process-kill crash recovery (client crashes are simulated by discarding client state within one process; the medium's "durability" is in-memory). Concurrency is cooperative round-robin, not parallel threads. These results are **[PROJECT EVIDENCE — local simulation]**: they validate Architecture E's *protocol logic* only.

**Guarantees still dependent on real-provider testing (the gated R2 probe, §8):**
1. Real conditional-write semantics and error behavior on the R2 endpoint.
2. Persistence/durability of acknowledged revisions (the 24-hour re-read check).
3. Read-after-write consistency, latency, and request-count profiles.
4. Credential permission boundaries — bucket scoping in practice; action-scoped Temporary Credentials incl. PutObject-without-DeleteObject; whether revoking a parent token invalidates locally signed children.
5. Quota/billing behavior under the probe's bounded workload.

### 9.1 T2b post-hoc analysis — head usability and rollback safety (2026-10-17)

The T2b result: an epoch-blind medium (like any real S3-compatible store) **accepted** a fresh-parent/stale-epoch record before chain validation rejected it. Three questions, answered:

**Can the canonical head become unusable?** Distinguish two heads. The **storage head** (what the medium's CAS linearizes on) stays writable but comes to point at a protocol-invalid record; the **protocol head** (the last-valid prefix's tip) freezes at the pre-jam record. New milestones cannot be committed at that medium while the jam sits on the storage spine — the only CAS-extendable chain runs *through* the invalid record, which validation rejects. This is **availability damage (fail-closed suspension), not integrity damage**: no valid record is lost, and validation still identifies exactly where validity ends. Jamming cannot be fully prevented at the medium — any credential holder with object-write access can do it (action-scoped PutObject-only credentials prevent deletion but not jamming).

**Can it cause an unsafe rollback?** Only if acknowledgment ignores validation. If T3-confirmation were granted on storage-level CAS success alone, records appended after the jam could be "confirmed" and then discarded by the last-valid-prefix fallback — an unsafe rollback of acknowledged milestones, violating M-29's committed-progress guarantee. If confirmation additionally requires the record to lie on a **fully-valid chain** (validate-after-append), post-jam records can never confirm, so the fallback discards only never-confirmed (pending) work — which fail-closed permits. The benign ordering case is worth noting: a valid record appended *just before* a jam survives, because it belongs to the last-valid prefix; the hazard is exclusively records appended *after* the jam.

**Prevention and recovery Architecture E requires [HYPOTHESIS — protocol-design requirements, not ratified]:**
1. **Validate-after-append:** T3-confirmed ⇔ conditional append succeeded **and** the record lies on the last-valid prefix of the re-read chain. Closes the unsafe-rollback window; the harness's resolution logic already behaves this way, but the requirement must be explicit in the protocol.
2. **Validate-before-extend:** writers refuse to append onto any chain that fails full validation (fail-closed at the writer).
3. **Quarantine + governed recovery (M-35):** persistent jamming is a governance event — identify the credential holder, revoke via the R6 governance path, then either conditionally **re-anchor the storage head to the last-valid prefix tip** (a CAS'd repair write) or **migrate media** (§4.1) if re-anchoring is contested. Never auto-merge.
4. **Logical-parent continuation (research candidate only):** allow valid records to chain "over" jammed spines via logical parents that skip quarantined records, enabling in-place continuation without re-anchoring. Needs its own analysis and experiment before any adoption.

None of these is implemented; recording them here keeps the T2b finding from being mistaken for a solved problem.

## 10. Open items for the lead

1. Candidate order — satisfied 2026-10-17: Cloudflare R2 (S3-compatible class) authorized as the first experimental candidate (§8); a second candidate is chosen only if R2 is disqualified.
2. Whether an optional developer-managed convenience medium is acceptable at launch at all (§3).
3. The trilemma response, pre-decided or deferred, if the probe disqualifies candidates.
4. Credential-model choice (R6, §5.1): pre-provisioned batches vs. escrowed minting vs. provider-native delegation — including who provisions restricted credentials when the canonical account owner is offline.
5. Depth of ToS review (mechanical checks vs. formal legal review) before any production commitment.
6. Cross-medium migration protocol design (§4.1) — a future research item distinct from live host handoff.
7. R2-specific follow-ups if the real-provider probe proceeds: Temporary Credentials revocation semantics (does revoking the parent token invalidate locally signed children?), combined action+prefix scoping behavior, and S3 conditional-write (If-Match / If-None-Match) behavior on the R2 endpoint. (S3-native object versioning is not assumed available — lead directive 2026-10-17.)
8. T2b-derived protocol requirements (§9.1): ratify validate-after-append as part of the T3 acknowledgment definition when the architecture work formalizes it, and decide whether logical-parent continuation warrants a future experiment or quarantine + re-anchor/migration suffices.

---

*Research-only annex, 2026-10-17 (v0.3). Architecture E is the leading research direction and is NOT selected; no provider, language, format, or production architecture is chosen. E1–E4 remain unimplemented. The local simulated-medium harness (§9) is authorized and run — protocol logic only. The real-provider E5-Lab CAS probe with Cloudflare R2 remains authorized but gated on the flagged account-owner prerequisites (§8). The next approval gate is real-provider experimental validation.*
