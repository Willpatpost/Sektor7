# E5 — CANONICAL-MEDIUM FEASIBILITY (Research Annex)

> **Status:** Research annex v0.2 · 2026-10-17 · **UNAPPROVED RESEARCH.** Incorporates the lead's 2026-10-17 review clarifications (T3 vs disaster recovery; owner denial/deletion powers; cross-medium authority transition; credential security model) and records the authorized E5-Lab experiment scope with **Cloudflare R2 as the first experimental candidate — not a selected production provider**. **Architecture E is the leading research direction, NOT a selected implementation architecture.** No provider, programming language, storage format, or production architecture is selected by this file. E1–E4 are not implemented; the E5-Lab probe is authorized but **gated on flagged prerequisites** (§8).
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
| **S3-compatible free-tier object storage** (e.g., Cloudflare R2, Backblaze B2, Storj, IDrive e2) | S3 conditional-write primitives exist as a protocol family (If-None-Match create-only; If-Match update) **[ESTABLISHED-CLASS]** — per-provider support and semantics **[TO-VERIFY]** | Provider-managed redundant storage; SLA-dependent **[TO-VERIFY]** | High (commercial CDN-grade networks) | Via bucket/prefix credentials shared by the roster | Typically 5–25 GB free; some with zero egress **[TO-VERIFY]** | Generally permissive for arbitrary app data on business/dev accounts **[TO-VERIFY]** | IAM-style tokens; granularity per provider **[TO-VERIFY]** (R2, verified 2026-10-17: bucket-level scoping, **no** write-without-delete split — §8) | Object export = trivially portable bytes | **Strongest candidate class** — probe first |
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
- **R5 — Residual storage-layer attacks are denial-of-service shaped:** head-jamming (appending garbage to move the head) and deletion. Mitigations: verifiers fall back to the **last valid prefix** and route to governed resolution (never auto-merge — M-35); **write-without-delete scoped credentials and object versioning where supported [TO-VERIFY per provider]** (R2, verified 2026-10-17 — §8: token-level write-without-delete is **not** offered; bucket-level scoping only, with prefix-scoped short-lived Temporary Credentials as a separate feature); peer replicas as recovery depth (§4).
- **R6 — Credential rotation/revocation is itself a governance operation** (roster vote), so removing a retired or removed member's write access follows the democratic rules rather than storage-admin fiat — protocol support is **[OPEN]**.
- **R7 — The honest asymmetry (lead clarification, 2026-10-17):** an *account owner* (developer in developer-managed; the owning member in player-managed) can always **deny access to or delete** what the account holds. **Signed records protect authenticity — they prove who wrote what and that records were not forged — but they cannot prevent an owner from denying access or deleting stored data**; no protocol layer fences the account owner at the medium. Mitigations are recovery-shaped, never preventive: peer replicas (§4 DR layer), migration (§4.1), governed rotation, and product-UX disclosure to rosters **[OPEN]**.

### 5.1 Credential security model — working proposal [HYPOTHESIS]

| Credential tier | Holder | Can | Cannot | Provisioned by |
|---|---|---|---|---|
| **Owner/admin** | The account owner (per the §3 arrangement) | Everything: mint/revoke tokens, delete data, manage billing | — | The owner (account creation) |
| **Member scoped write token** | Each roster member | Conditional append (object write) within the campaign's bucket | Administer buckets, touch other buckets, billing | The owner, or an escrowed minting procedure (below) |
| **Read-only token** | Members, prospective hosts, validation | Read and verify the chain | Write | The owner or escrow |

**Rules:** (a) members normally operate with object-write tokens scoped to the campaign bucket (per R5) — noting the R2 finding (§8) that token-level write-without-delete splits are not universally available, deletion-resistance relies on versioning where offered, the peer-replica DR layer (§4), and disposable-bucket hygiene during experiments. (b) Tokens are per-campaign, bucket-scoped, revocable, and never committed to the repository — session environment only. (c) **The offline-owner problem (lead clarification, 2026-10-17):** if only the account owner can mint or revoke scoped tokens, the owner's absence blocks onboarding new members, rotating a compromised token, or migrating media. Candidate mitigations, all **[HYPOTHESIS]**: (i) *pre-provisioned token batches* — the owner mints a reserve of scoped tokens ahead of time, activated on governance vote, with an accepted revocation lag; (ii) *escrowed minting rights* — a break-glass minting credential split across the roster (secret-sharing, n-of-m reconstruction on governance vote), so token lifecycle never depends on one person being online; (iii) *provider-native delegation* (e.g., R2's short-lived, prefix-scoped Temporary Credentials derived from a parent token) **[TO-VERIFY in probe]**. Choosing among these is **[OPEN]** for the lead. The E5-Lab probe needs only a single bucket-scoped Object Read & Write token, so this model does not gate the experiment.

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
- **[VERIFIED — flag: billing behavior]** Usage beyond free limits is **billed** (service does not stop) and rounds up to the next billing unit; no spend-cap setting is documented on the pricing page. Charge safety therefore rests on the probe's bounded operation counts plus a token scoped to one disposable bucket. Helpful detail: `DeleteObject`, `DeleteBucket`, and `AbortMultipartUpload` are free operations, and unauthorized (401) requests are not billed.
- **[VERIFIED — design finding]** R2 API-token permissions are four fixed levels (Admin Read & Write, Admin Read only, **Object Read & Write**, Object Read only) with **bucket-level scoping only — no prefix scoping and no write-without-delete split** at the token level. This corrects §5 R5's assumption for R2: an object-write token can also delete objects in its buckets. A separate **Temporary Credentials** feature (short-lived, prefix-scoped, derived from a parent token) exists and is relevant to the §5.1 credential model **[TO-VERIFY in probe]**.
- **[VERIFIED — local environment]** No Cloudflare/R2/AWS credentials exist in the working environment; `curl` and `python3` are available for a provider-adapter harness (no AWS CLI/rclone/mc — the adapter will speak the S3-compatible HTTP API directly; S3 conditional-write behavior on the R2 endpoint is itself a probe item).

**Flags requiring account-owner action before the probe proceeds (per the lead's prerequisite gate):**
1. Create/own a Cloudflare account and **enable R2** — requires payment information on file (the paid-activation flag; expected probe cost $0 within free limits).
2. Create a **disposable bucket** for probe data.
3. Mint an **R2 API token with Object Read & Write scoped to that bucket only**, and provide its Access Key ID, Secret Access Key, and account ID (for the endpoint) via the session environment.

**Status: NOT RUN.** Per the authorization's prerequisite gate, the probe halts here pending the flagged account-owner actions. No measured results exist; nothing in this section is evidence.

## 9. Open items for the lead

1. Candidate order — satisfied 2026-10-17: Cloudflare R2 (S3-compatible class) authorized as the first experimental candidate (§8); a second candidate is chosen only if R2 is disqualified.
2. Whether an optional developer-managed convenience medium is acceptable at launch at all (§3).
3. The trilemma response, pre-decided or deferred, if the probe disqualifies candidates.
4. Credential-model choice (R6, §5.1): pre-provisioned batches vs. escrowed minting vs. provider-native delegation — including who provisions restricted credentials when the canonical account owner is offline.
5. Depth of ToS review (mechanical checks vs. formal legal review) before any production commitment.
6. Cross-medium migration protocol design (§4.1) — a future research item distinct from live host handoff.
7. R2-specific follow-ups if the probe proceeds: object-versioning availability, Temporary Credentials semantics for prefix scoping, and S3 conditional-write (If-Match / If-None-Match) behavior on the R2 endpoint.

---

*Research-only annex, 2026-10-17 (v0.2). Architecture E is the leading research direction and is NOT selected; no provider, language, format, or production architecture is chosen. E1–E4 remain unimplemented; the E5-Lab CAS probe is authorized (2026-10-17) with Cloudflare R2 as first experimental candidate but is gated on the flagged account-owner prerequisites (§8). The next approval gate is experimental validation.*
