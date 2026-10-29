//! The S6-C4 Phase 2 K2-B RETIREMENT PROTOCOL STATE MACHINE — a
//! PURE, test-only model of the revised terminal-retirement design
//! (the K2-A report's fence proposal, corrected per the K2-B
//! directive's questions and the K2-B review's crash-safety
//! corrective). Nothing here touches a provider, a medium, a file,
//! or a database: the model is an in-memory state machine whose
//! EVERY transition re-asserts the protocol's structural
//! invariants, driven through the mandated scenarios — audit
//! acceptance, interrupted deletion, terminal confirmation,
//! identical-hash reintroduction, restart, the ambiguous legacy
//! audit, and the CRASH/RESTART MATRIX across the staged
//! supersession protocol.
//!
//! **The protocol being modeled** (the design, NOT an
//! implementation — K2-C scope if authorized):
//!
//! - An audit is INSTALLED with an immutable deletion manifest (the
//!   units) and its own idempotency key. While any audit is
//!   non-terminal (installed, deleting, or legacy-ambiguous), its
//!   units are RESERVED: any publication or resupply whose closure
//!   intersects the reservation is REFUSED typed (the fence).
//! - Phase B deletes from the manifest, step by step, and may crash
//!   at any point (the durable audit makes the state restartable).
//! - Completion is verified against the whole manifest, and the
//!   TERMINAL COMPLETION RECORD is then appended DURABLY (before
//!   the fence lifts). Terminal records are monotone: once written,
//!   never retracted, and a terminal audit NEVER resumes — not on
//!   restart, not on reintroduction of its former units (the ABA
//!   fix: terminality derives from the RECORD, never from the
//!   objects' presence).
//! - A LEGACY audit (installed before the protocol, carrying no
//!   terminal record) whose units intersect currently-live content
//!   is AMBIGUOUS: presence cannot distinguish an original deletion
//!   remainder from newly committed live content. The ambiguous
//!   state is MAINTENANCE-BLOCKED: destructive resumption is
//!   refused, a conservative fence is NOT permission to delete, and
//!   the only exit is the STAGED supersession migration:
//!
//!   1. **PREPARE** (pure, repeatable, durable-state-free): the
//!      verified complete live-set view — the reconciliation input
//!      — produces the reconciled deletion set (the old units MINUS
//!      the view's required identities). An UNVERIFIED view (the
//!      walk failed, was incomplete, stale, or corrupt) REFUSES the
//!      migration: the model never guesses. A view whose
//!      reconciliation would delete a currently live-required
//!      identity REFUSES (the safety guard).
//!   2. **COMMIT** (the ONE atomic durable transition): installs
//!      the successor audit (its own DISTINCT idempotency key — the
//!      old key plus a monotone supersession discriminator), marks
//!      the old audit SUPERSEDED, and records the supersession
//!      facts — all or nothing. A crash before the commit changes
//!      NOTHING (the old audit stays maintenance-blocked and
//!      fenced); after the commit BOTH facts hold (exactly one
//!      deletion authority, the successor carrying the reconciled
//!      fence). **THE COMMIT'S ATOMICITY IS AN UNPROVEN PRODUCTION
//!      OBLIGATION**: a real provider transaction (filesystem or
//!      SQLite) must establish it; this in-memory model assumes it
//!      and PROVES NOTHING about actual crash durability.
//!   3. The successor proceeds under the new protocol (deleting →
//!      terminal), restart-safe, idempotent under repeated
//!      commits, and refusing conflicting re-supersessions.
//!
//!   The safe invariant throughout: **a legacy audit never loses
//!   its conservative protection until a durable, verifiable
//!   successor has assumed the necessary authority and fencing** —
//!   pre-commit the old audit stays fenced; post-commit the
//!   successor's reconciled set fences exactly the units the
//!   verified view did not protect.
//!
//! - Reintroduction of an identity is permitted exactly when no
//!   NON-TERMINAL audit reserves it; after terminal confirmation it
//!   is a fresh object the retired audit can never touch again.

use std::collections::BTreeSet;

use engine_persist::Hash;

// ------------------------------------------------------------ the model

/// One audit's protocol phase.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuditPhase {
    /// Installed; the fence over its units is active.
    Installed,
    /// Deleting from the manifest (interruptible, restartable).
    Deleting,
    /// The terminal completion record is durably written; the fence
    /// over its units is lifted FOREVER.
    Terminal,
    /// A legacy audit whose units are ambiguous against live
    /// content — maintenance-blocked; only the staged supersession
    /// migration exits.
    LegacyAmbiguous,
    /// A durable supersession record replaced this audit; it is
    /// never resumed (its manifest and key remain immutable).
    Superseded,
}

/// One reclamation audit in the model.
#[derive(Debug, Clone)]
struct Audit {
    id: u64,
    phase: AuditPhase,
    /// The immutable deletion manifest (the reserved units).
    units: BTreeSet<Hash>,
    /// Whether this audit predates the terminal-record protocol.
    legacy: bool,
    /// The audit's idempotency key (immutable once installed).
    key: Vec<u8>,
}

/// The durable supersession record (the atomic commit's facts): the
/// successor's identity and the verified view the reconciliation
/// committed against — the exactness witness the invariants assert.
#[derive(Debug, Clone)]
struct SupersessionRecord {
    successor: u64,
    /// The view's required identities AT COMMIT (what the
    /// reconciliation refused to delete).
    committed_required: BTreeSet<Hash>,
}

/// The migration's reconciliation input — the verified complete
/// live-set view. `Verified` is the ONLY migrating form; every
/// unverified, incomplete, stale, or corrupt view REFUSES.
#[derive(Debug, Clone)]
enum ReconciliationView {
    Verified {
        /// The identities the current retained roots' closures
        /// require (the successful reachability walk's live set).
        required: BTreeSet<Hash>,
    },
    Unverified {
        /// Why the live-set view could not be established (the walk
        /// failed: missing, corrupt, unsupported, or unverifiable
        /// dependencies — or the view is known stale).
        reason: &'static str,
    },
}

impl ReconciliationView {
    /// Builds the verified view from a COMPLETED reachability walk
    /// (the production discipline: the view's required set is the
    /// walk's full live set — the chunks, the roots, the direct
    /// objects, and the walked records — never a hand-picked
    /// subset). A failed walk produces no `LiveSet`, hence no
    /// verified view, hence no migration.
    fn from_walk(live: &engine_persist::dark_reachability::LiveSet) -> Self {
        let mut required = live.chunk_identities.clone();
        required.extend(live.composite_roots.iter().copied());
        required.extend(live.direct_objects.iter().copied());
        required.extend(live.visited_revisions.iter().copied());
        ReconciliationView::Verified { required }
    }
}

/// The staged migration's pure preparation output — the facts the
/// atomic commit will durably establish. NOTHING is mutated at
/// preparation; a crash here changes nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PreparedSupersession {
    /// The legacy-ambiguous audit being superseded.
    old_audit: u64,
    /// The reconciled deletion set (the old units minus the view's
    /// required identities).
    successor_units: BTreeSet<Hash>,
    /// The view's required identities the reconciliation was
    /// computed against (the commit's exactness witness).
    required_at_preparation: BTreeSet<Hash>,
}

/// A typed protocol refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ProtocolError {
    /// The publication or resupply intersects the active
    /// reservation of these non-terminal audits.
    Fenced { by: Vec<u64> },
    /// A terminal, ambiguous, or superseded audit was asked to
    /// delete — it never resumes.
    NotDeletable { audit: u64 },
    /// Only a legacy-ambiguous audit can migrate; only a verified
    /// view can reconcile.
    NotMigratable { audit: u64 },
    /// The live-set view is unverified (missing, incomplete, stale,
    /// or corrupt) — the migration REFUSES rather than guessing.
    MigrationBlocked { reason: &'static str },
    /// A supersession already exists for this audit and the new
    /// attempt conflicts with the committed reconciliation.
    SupersessionConflict { audit: u64 },
    /// The audit id does not exist.
    UnknownAudit { audit: u64 },
    /// A structural invariant failed — the model itself fails
    /// closed (a design defect, not a scenario outcome).
    Invariant(&'static str),
}

/// The pure protocol model. Every mutating operation ends by
/// re-asserting the structural invariants; a violation is a typed
/// `Invariant` failure (fail closed).
#[derive(Debug, Default)]
struct ProtocolModel {
    audits: Vec<Audit>,
    next_id: u64,
    /// The live object identities.
    store: BTreeSet<Hash>,
    /// The audit ids with durably-written terminal records
    /// (monotone — insert only).
    terminal_records: BTreeSet<u64>,
    /// The durable supersession records (the atomic commits' facts;
    /// one per superseded audit, insert only).
    supersession_records: std::collections::BTreeMap<u64, SupersessionRecord>,
    /// The identities the CURRENT retained roots' closures require
    /// (the operator's liveness input — in production, the verified
    /// reachability walk's output; never deleted by any audit).
    live_required: BTreeSet<Hash>,
    /// The operation history (the scenario trace).
    history: Vec<&'static str>,
}

fn identity(seed: u8) -> Hash {
    Hash::from_bytes(&[seed; 32]).unwrap()
}

impl ProtocolModel {
    fn new() -> Self {
        ProtocolModel::default()
    }

    fn audit(&self, id: u64) -> Result<&Audit, ProtocolError> {
        self.audits
            .iter()
            .find(|audit| audit.id == id)
            .ok_or(ProtocolError::UnknownAudit { audit: id })
    }

    /// The structural invariants, re-asserted after every mutating
    /// operation:
    /// - P2/P5: every Terminal audit has a durable terminal record
    ///   (terminality is record-derived, monotone).
    /// - P-supersession: every Superseded audit has a durable
    ///   supersession record; its successor exists; and the
    ///   successor's manifest is EXACTLY the committed
    ///   reconciliation (the old units minus the committed view's
    ///   required set) — never a guessed set.
    /// - The store never loses a live-required identity.
    fn check_invariants(&self) -> Result<(), ProtocolError> {
        for audit in &self.audits {
            if audit.phase == AuditPhase::Terminal && !self.terminal_records.contains(&audit.id) {
                return Err(ProtocolError::Invariant("terminal-without-record"));
            }
            if audit.phase == AuditPhase::Superseded {
                let Some(record) = self.supersession_records.get(&audit.id) else {
                    return Err(ProtocolError::Invariant("superseded-without-record"));
                };
                let Some(successor) = self.audits.iter().find(|a| a.id == record.successor) else {
                    return Err(ProtocolError::Invariant("supersession-without-successor"));
                };
                let expected: BTreeSet<Hash> = audit
                    .units
                    .difference(&record.committed_required)
                    .copied()
                    .collect();
                if successor.units != expected {
                    return Err(ProtocolError::Invariant(
                        "supersession-reconciliation-mismatch",
                    ));
                }
            }
        }
        if !self.store.is_superset(&self.live_required) {
            return Err(ProtocolError::Invariant("live-required-deleted"));
        }
        Ok(())
    }

    fn record(&mut self, step: &'static str) -> Result<(), ProtocolError> {
        self.history.push(step);
        self.check_invariants()
    }

    /// Installs a new audit over the deletion set.
    fn install(&mut self, units: &[Hash], legacy: bool) -> Result<u64, ProtocolError> {
        let id = self.next_id;
        self.next_id += 1;
        let key = if legacy {
            format!("legacy-{id}").into_bytes()
        } else {
            format!("audit-{id}").into_bytes()
        };
        self.audits.push(Audit {
            id,
            phase: AuditPhase::Installed,
            units: units.iter().copied().collect(),
            legacy,
            key,
        });
        self.record("install")?;
        Ok(id)
    }

    /// Publishes (or resupplies) a closure: refused typed iff it
    /// intersects the ACTIVE reservation (the fence — over reuse
    /// AND same-hash new supply alike, content addressing making
    /// them one identity).
    fn publish(&mut self, closure: &[Hash]) -> Result<(), ProtocolError> {
        let intersecting: Vec<u64> = self
            .audits
            .iter()
            .filter(|audit| {
                matches!(
                    audit.phase,
                    AuditPhase::Installed | AuditPhase::Deleting | AuditPhase::LegacyAmbiguous
                ) && closure
                    .iter()
                    .any(|identity| audit.units.contains(identity))
            })
            .map(|audit| audit.id)
            .collect();
        if !intersecting.is_empty() {
            return Err(ProtocolError::Fenced { by: intersecting });
        }
        self.store.extend(closure.iter().copied());
        self.record("publish")?;
        Ok(())
    }

    /// Reintroduces one identity: the same fence rule as a
    /// publication (a resupply of a reserved identity is refused
    /// while ANY non-terminal audit reserves it).
    fn reintroduce(&mut self, identity: Hash) -> Result<(), ProtocolError> {
        self.publish(&[identity])
    }

    /// Moves an installed audit to the deleting phase.
    fn begin_delete(&mut self, audit_id: u64) -> Result<(), ProtocolError> {
        let audit = self
            .audits
            .iter_mut()
            .find(|audit| audit.id == audit_id)
            .ok_or(ProtocolError::UnknownAudit { audit: audit_id })?;
        if audit.phase != AuditPhase::Installed {
            return Err(ProtocolError::NotDeletable { audit: audit_id });
        }
        audit.phase = AuditPhase::Deleting;
        self.record("begin-delete")
    }

    /// Deletes one unit (Phase B, step by step, from the manifest).
    /// Terminal, ambiguous, and superseded audits NEVER delete.
    fn delete_step(&mut self, audit_id: u64, unit: Hash) -> Result<(), ProtocolError> {
        let audit = self
            .audits
            .iter_mut()
            .find(|audit| audit.id == audit_id)
            .ok_or(ProtocolError::UnknownAudit { audit: audit_id })?;
        if audit.phase != AuditPhase::Deleting {
            return Err(ProtocolError::NotDeletable { audit: audit_id });
        }
        if !audit.units.contains(&unit) {
            return Err(ProtocolError::Invariant("delete-outside-manifest"));
        }
        if self.live_required.contains(&unit) {
            return Err(ProtocolError::Invariant("delete-live-required"));
        }
        self.store.remove(&unit);
        self.record("delete-step")
    }

    /// The whole-manifest completion verification: every audited
    /// unit is absent from the store.
    fn complete(&self, audit_id: u64) -> Result<bool, ProtocolError> {
        let audit = self.audit(audit_id)?;
        if audit.phase != AuditPhase::Deleting {
            return Err(ProtocolError::NotDeletable { audit: audit_id });
        }
        Ok(audit.units.iter().all(|unit| !self.store.contains(unit)))
    }

    /// Writes the durable terminal completion record — only after
    /// the completion verification — and lifts the audit's fence
    /// contribution FOREVER. Monotone: the record set only grows.
    fn write_terminal(&mut self, audit_id: u64) -> Result<(), ProtocolError> {
        if !self.complete(audit_id)? {
            return Err(ProtocolError::Invariant("terminal-before-completion"));
        }
        let audit = self
            .audits
            .iter_mut()
            .find(|audit| audit.id == audit_id)
            .unwrap();
        audit.phase = AuditPhase::Terminal;
        self.terminal_records.insert(audit_id);
        self.record("write-terminal")
    }

    /// Restart: the durable state is re-derived from the RECORDS —
    /// terminality from the terminal records, supersession from
    /// the supersession records — never from the objects' presence
    /// (the ABA fix), and never resurrecting a fence a durable
    /// record has retired.
    fn restart(&mut self) -> Result<(), ProtocolError> {
        for audit in &mut self.audits {
            if self.terminal_records.contains(&audit.id) {
                audit.phase = AuditPhase::Terminal;
            } else if self.supersession_records.contains_key(&audit.id) {
                audit.phase = AuditPhase::Superseded;
            }
        }
        self.record("restart")
    }

    /// Detects the legacy-audit ambiguity: a legacy, non-terminal
    /// audit whose units intersect currently-live content (presence
    /// cannot distinguish an original deletion remainder from newly
    /// committed live content) enters the MAINTENANCE-BLOCKED
    /// state.
    fn detect_legacy_ambiguity(&mut self, audit_id: u64) -> Result<bool, ProtocolError> {
        let audit = self
            .audits
            .iter_mut()
            .find(|audit| audit.id == audit_id)
            .ok_or(ProtocolError::UnknownAudit { audit: audit_id })?;
        if !audit.legacy {
            return Err(ProtocolError::NotMigratable { audit: audit_id });
        }
        match audit.phase {
            AuditPhase::Installed | AuditPhase::Deleting => {
                let ambiguous = audit.units.iter().any(|unit| self.store.contains(unit));
                if ambiguous {
                    audit.phase = AuditPhase::LegacyAmbiguous;
                    self.record("detect-legacy-ambiguity")?;
                    Ok(true)
                } else {
                    Ok(false)
                }
            }
            _ => Err(ProtocolError::NotMigratable { audit: audit_id }),
        }
    }

    /// The staged migration, step 1 — PREPARE (pure, repeatable,
    /// durable-state-free): the verified live-set view produces the
    /// reconciled deletion set. An unverified view REFUSES; a
    /// reconciliation that would delete a currently live-required
    /// identity REFUSES (the safety guard). NOTHING is mutated: a
    /// crash before the commit changes nothing and the legacy
    /// audit keeps its conservative fence.
    fn migrate_prepare(
        &self,
        audit_id: u64,
        view: &ReconciliationView,
    ) -> Result<PreparedSupersession, ProtocolError> {
        let audit = self.audit(audit_id)?;
        if audit.phase != AuditPhase::LegacyAmbiguous {
            return Err(ProtocolError::NotMigratable { audit: audit_id });
        }
        let required = match view {
            ReconciliationView::Verified { required } => required,
            ReconciliationView::Unverified { reason } => {
                return Err(ProtocolError::MigrationBlocked { reason });
            }
        };
        let successor_units: BTreeSet<Hash> = audit
            .units
            .iter()
            .filter(|unit| !required.contains(*unit))
            .copied()
            .collect();
        // The safety guard: the reconciliation may never include an
        // identity the CURRENT liveness input protects (a stale or
        // partial view that misses a live identity refuses here).
        if successor_units
            .intersection(&self.live_required)
            .next()
            .is_some()
        {
            return Err(ProtocolError::MigrationBlocked {
                reason: "the reconciliation intersects currently live-required content",
            });
        }
        Ok(PreparedSupersession {
            old_audit: audit_id,
            successor_units,
            required_at_preparation: required.clone(),
        })
    }

    /// The staged migration, step 2 — COMMIT (the ONE atomic durable
    /// transition): installs the successor audit under its OWN
    /// distinct idempotency key, marks the old audit SUPERSEDED,
    /// and records the supersession facts — all or nothing.
    /// **THE ATOMICITY IS AN UNPROVEN PRODUCTION OBLIGATION**: a
    /// real provider transaction must establish it; this model
    /// assumes it and proves nothing about actual crash durability.
    /// Idempotent under a repeated commit of the SAME
    /// reconciliation (the restart rule); a CONFLICTING
    /// re-supersession refuses typed.
    ///
    /// **The R3 correction — the idempotency comparison covers the
    /// COMPLETE reconciliation evidence, never the deletion set
    /// alone:** a repeated commit is idempotent only when the
    /// legacy audit identity, the committed required-identity set,
    /// the reconciled successor units, AND the successor
    /// relationship ALL match the already-committed transition.
    /// Different reconciliation views can produce the SAME
    /// deletion set — those are DIFFERENT claims, and a changed
    /// view can never inherit the authority of a previously
    /// committed view merely because the resulting units happen to
    /// match.
    ///
    /// **The preparation-to-commit freshness check:** the commit
    /// RE-VALIDATES the prepared reconciliation against the CURRENT
    /// live-required set — a preparation whose reconciled units
    /// have become live-required between prepare and commit (the
    /// modeled liveness input changed) is now STALE and REFUSES.
    /// In production this window lives inside the provider's atomic
    /// supersession transaction (the documented unproven
    /// obligation); the model enforces re-validation at commit as
    /// the fail-closed belt — no provider-level concurrency
    /// guarantee is invented here.
    fn migrate_commit(&mut self, prepared: &PreparedSupersession) -> Result<u64, ProtocolError> {
        let old_id = prepared.old_audit;
        // The freshness re-validation (every path — repeated
        // attempts included): the prepared reconciliation must
        // still avoid every CURRENTLY live-required identity.
        if prepared
            .successor_units
            .intersection(&self.live_required)
            .next()
            .is_some()
        {
            return Err(ProtocolError::MigrationBlocked {
                reason: "the prepared reconciliation is stale: a reconciled unit is now live-required",
            });
        }
        if let Some(existing) = self.supersession_records.get(&old_id) {
            // A repeated commit (a restart's re-attempt) is
            // idempotent ONLY on the COMPLETE evidence: the same
            // required set AND the same successor units under the
            // same successor relationship. Anything else —
            // including a different view producing the SAME units —
            // is a conflicting supersession attempt.
            let evidence_matches = existing.committed_required == prepared.required_at_preparation
                && {
                    let existing_successor = self
                        .audits
                        .iter()
                        .find(|audit| audit.id == existing.successor)
                        .expect("the invariants guarantee the successor exists");
                    existing_successor.units == prepared.successor_units
                };
            if evidence_matches {
                return Ok(existing.successor);
            }
            return Err(ProtocolError::SupersessionConflict { audit: old_id });
        }
        let old_audit = self
            .audits
            .iter_mut()
            .find(|audit| audit.id == old_id)
            .ok_or(ProtocolError::UnknownAudit { audit: old_id })?;
        if old_audit.phase != AuditPhase::LegacyAmbiguous {
            return Err(ProtocolError::NotMigratable { audit: old_id });
        }
        // THE atomic durable commit: both facts — the successor's
        // installation AND the old audit's supersession — establish
        // together, with no model-visible intermediate state. The
        // immutable audit manifest and the original idempotency key
        // are never replaced; the successor carries its own
        // distinct key.
        let old_key = old_audit.key.clone();
        old_audit.phase = AuditPhase::Superseded;
        let new_id = self.next_id;
        self.next_id += 1;
        let key = [
            old_key.as_slice(),
            format!("+supersession-{new_id}").as_bytes(),
        ]
        .concat();
        self.audits.push(Audit {
            id: new_id,
            phase: AuditPhase::Installed,
            units: prepared.successor_units.clone(),
            legacy: false,
            key,
        });
        self.supersession_records.insert(
            old_id,
            SupersessionRecord {
                successor: new_id,
                committed_required: prepared.required_at_preparation.clone(),
            },
        );
        self.record("migrate-commit")?;
        Ok(new_id)
    }
}

// --------------------------------------------------- the core scenarios

#[test]
fn the_audit_acceptance_fence_and_completion_scenario() {
    // (a) Audit acceptance: install over {A, B}; a publication
    // touching A is refused typed naming the audit; the deletion
    // runs to completion; the terminal record is written; the fence
    // lifts.
    let mut model = ProtocolModel::new();
    let (a, b) = (identity(1), identity(2));
    model.publish(&[a, b]).unwrap();
    let audit = model.install(&[a, b], false).unwrap();
    assert_eq!(
        model.publish(&[identity(3), a]),
        Err(ProtocolError::Fenced { by: vec![audit] }),
        "the fence refuses a publication intersecting the reservation"
    );
    model.begin_delete(audit).unwrap();
    model.delete_step(audit, a).unwrap();
    model.delete_step(audit, b).unwrap();
    assert!(model.complete(audit).unwrap());
    model.write_terminal(audit).unwrap();
    // The fence is lifted: the former units publish freely.
    model.publish(&[a]).unwrap();
    // And a restart keeps the audit terminal (the record, not
    // presence).
    model.restart().unwrap();
    assert_eq!(model.audit(audit).unwrap().phase, AuditPhase::Terminal);
    assert_eq!(
        model.delete_step(audit, a),
        Err(ProtocolError::NotDeletable { audit }),
        "a terminal audit never resumes"
    );
}

#[test]
fn the_interrupted_deletion_resumes_after_restart_scenario() {
    // (b) Interrupted deletion: the crash lands mid-Phase-B (A
    // deleted, B not); the restart derives the durable phase; the
    // resume completes from the manifest; the fence held throughout.
    let mut model = ProtocolModel::new();
    let (a, b) = (identity(1), identity(2));
    model.publish(&[a, b]).unwrap();
    let audit = model.install(&[a, b], false).unwrap();
    model.begin_delete(audit).unwrap();
    model.delete_step(audit, a).unwrap();
    // The crash: nothing changes (the durable audit IS the state).
    model.restart().unwrap();
    assert_eq!(model.audit(audit).unwrap().phase, AuditPhase::Deleting);
    // The fence still holds over the REMAINING unit.
    assert_eq!(
        model.publish(&[b]),
        Err(ProtocolError::Fenced { by: vec![audit] })
    );
    // The resume completes from the manifest.
    model.delete_step(audit, b).unwrap();
    model.write_terminal(audit).unwrap();
    assert_eq!(model.audit(audit).unwrap().phase, AuditPhase::Terminal);
}

#[test]
fn the_terminal_confirmation_precedes_reintroduction_scenario() {
    // (c) + (d) Terminal confirmation is durable and monotone
    // BEFORE reintroduction is permitted: while the audit is
    // non-terminal, resupplying a reserved identity is refused;
    // after the terminal record, the same identity reintroduces as
    // a fresh object the retired audit can never touch.
    let mut model = ProtocolModel::new();
    let (a, b) = (identity(1), identity(2));
    model.publish(&[a, b]).unwrap();
    let audit = model.install(&[a, b], false).unwrap();
    model.begin_delete(audit).unwrap();
    model.delete_step(audit, a).unwrap();
    // A deleted unit is STILL reserved while the audit is
    // non-terminal — the resupply is refused (the same-hash new
    // supply is the same identity under content addressing).
    assert_eq!(
        model.reintroduce(a),
        Err(ProtocolError::Fenced { by: vec![audit] })
    );
    model.delete_step(audit, b).unwrap();
    model.write_terminal(audit).unwrap();
    // After terminal: the reintroduction succeeds — a fresh object.
    model.reintroduce(a).unwrap();
    assert!(model.store.contains(&a));
    // The ABA case (e): the presence-based probe would now say the
    // audit is pending again (A is present!); the restart derives
    // terminality from the RECORD and never resumes.
    model.restart().unwrap();
    assert_eq!(model.audit(audit).unwrap().phase, AuditPhase::Terminal);
    assert_eq!(
        model.delete_step(audit, a),
        Err(ProtocolError::NotDeletable { audit }),
        "the reintroduced fresh object is untouchable by the terminal audit — the ABA fix"
    );
    // And the fence stays lifted for the fresh object.
    model.publish(&[a, identity(9)]).unwrap();
}

#[test]
fn the_ambiguous_legacy_audit_blocks_and_migrates_scenario() {
    // (f) The ambiguous legacy audit: a pre-protocol audit whose
    // unit X was reintroduced as live content (a retained root
    // requires it). Presence cannot distinguish the remainder from
    // the new supply — the audit enters the maintenance-blocked
    // state; destructive resumption is REFUSED; the STAGED
    // migration prepares from a verified view and commits
    // atomically; the old audit is superseded, immutable, and never
    // resumed; X survives.
    let mut model = ProtocolModel::new();
    let (x, y) = (identity(1), identity(2));
    // The pre-protocol world: the audit deleted Y but not X, then
    // died — no terminal record exists.
    model.publish(&[x, y]).unwrap();
    let legacy = model.install(&[x, y], true).unwrap();
    model.begin_delete(legacy).unwrap();
    model.delete_step(legacy, y).unwrap();
    // X is reintroduced as live content — a retained root requires
    // it (the liveness input).
    model.live_required.insert(x);
    // The ambiguity detection: X is present and reserved — blocked.
    assert!(model.detect_legacy_ambiguity(legacy).unwrap());
    assert_eq!(
        model.audit(legacy).unwrap().phase,
        AuditPhase::LegacyAmbiguous
    );
    // Destructive resumption is refused — the conservative fence is
    // NOT permission to delete.
    assert_eq!(
        model.delete_step(legacy, x),
        Err(ProtocolError::NotDeletable { audit: legacy })
    );
    // The fence still protects X (an ambiguous audit reserves).
    assert_eq!(
        model.reintroduce(x),
        Err(ProtocolError::Fenced { by: vec![legacy] })
    );
    // The staged migration: prepare (pure) from the VERIFIED view
    // {X} — the reconciled set excludes X — then commit (atomic).
    let prepared = model
        .migrate_prepare(
            legacy,
            &ReconciliationView::Verified {
                required: [x].into_iter().collect(),
            },
        )
        .unwrap();
    assert!(!prepared.successor_units.contains(&x));
    assert!(prepared.successor_units.contains(&y));
    // The crash between prepare and commit: NOTHING durable changed
    // — the old audit stays maintenance-blocked and fenced.
    model.restart().unwrap();
    assert_eq!(
        model.audit(legacy).unwrap().phase,
        AuditPhase::LegacyAmbiguous
    );
    let old_key = model.audit(legacy).unwrap().key.clone();
    let old_units = model.audit(legacy).unwrap().units.clone();
    let superseding = model.migrate_commit(&prepared).unwrap();
    let new_audit = model.audit(superseding).unwrap();
    assert!(
        !new_audit.units.contains(&x),
        "X survives the reconciliation"
    );
    assert!(new_audit.units.contains(&y));
    assert_ne!(new_audit.key, old_key, "the supersession key is distinct");
    assert!(!new_audit.legacy);
    // The OLD audit: superseded, its manifest and key IMMUTABLE,
    // never resumed.
    assert_eq!(model.audit(legacy).unwrap().phase, AuditPhase::Superseded);
    assert_eq!(model.audit(legacy).unwrap().units, old_units);
    assert_eq!(model.audit(legacy).unwrap().key, old_key);
    assert_eq!(
        model.delete_step(legacy, x),
        Err(ProtocolError::NotDeletable { audit: legacy })
    );
    // The superseding audit completes over the reconciled remainder
    // (Y is already absent — the deletion is idempotent against the
    // manifest) and reaches terminal; X was never touched.
    model.begin_delete(superseding).unwrap();
    assert!(model.complete(superseding).unwrap());
    model.write_terminal(superseding).unwrap();
    assert!(
        model.store.contains(&x),
        "the live-required identity survives"
    );
}

#[test]
fn the_unverified_view_refuses_the_migration() {
    // The reconciliation discipline: an UNVERIFIED live-set view
    // (the walk failed, or the view is stale) REFUSES the
    // migration — the model never guesses. A view that would
    // delete currently live-required content refuses on the safety
    // guard.
    let mut model = ProtocolModel::new();
    let (x, y) = (identity(1), identity(2));
    model.publish(&[x, y]).unwrap();
    let legacy = model.install(&[x, y], true).unwrap();
    model.live_required.insert(x);
    assert!(model.detect_legacy_ambiguity(legacy).unwrap());
    assert_eq!(
        model.migrate_prepare(
            legacy,
            &ReconciliationView::Unverified {
                reason: "the reachability walk failed: a missing dependency",
            },
        ),
        Err(ProtocolError::MigrationBlocked {
            reason: "the reachability walk failed: a missing dependency",
        })
    );
    // The STALE view: a required set that MISSES the live identity
    // X (an incomplete walk) — the safety guard refuses (the
    // reconciled set would include X).
    assert_eq!(
        model.migrate_prepare(
            legacy,
            &ReconciliationView::Verified {
                required: [y].into_iter().collect(),
            },
        ),
        Err(ProtocolError::MigrationBlocked {
            reason: "the reconciliation intersects currently live-required content",
        })
    );
    // The audit stays maintenance-blocked either way.
    assert_eq!(
        model.audit(legacy).unwrap().phase,
        AuditPhase::LegacyAmbiguous
    );
}

#[test]
fn the_from_walk_view_migration_end_to_end() {
    // The production discipline demonstrated end to end: the
    // verified view is built from a COMPLETED dark reachability
    // walk (a real mock chain with a real composite root), and
    // only that view migrates the ambiguous legacy audit — the
    // walk's full live set is what the reconciliation refuses to
    // delete.
    use engine_persist::composite_manifest::{self, ManifestEntry, SectionKind};
    use engine_persist::composite_reader::{ObjectSource, ObjectSourceError, VerificationMemo};
    use engine_persist::dark_reachability::{ReachabilityLimits, live_set};
    use engine_persist::record::{AuthorityMode, RevisionRecord};
    use std::collections::HashMap;

    struct MapSource {
        objects: HashMap<Hash, Vec<u8>>,
    }
    impl ObjectSource for MapSource {
        fn read_object(&mut self, id: &Hash) -> Result<Vec<u8>, ObjectSourceError> {
            self.objects
                .get(id)
                .cloned()
                .ok_or(ObjectSourceError::Missing(*id))
        }
    }

    // A synthetic /2 composite: the six pre-ledger singletons, the
    // SHARED sealed chunk X, a tail, and the world/director — every
    // object in the mock source.
    let chunk = composite_manifest::encode_chunk_envelope(
        composite_manifest::TAG_LEDGER_RECORDS,
        &[7u8; 4 * 31],
    );
    let chunk_id = chunk.hash();
    let mut source_objects: HashMap<Hash, Vec<u8>> = HashMap::new();
    source_objects.insert(chunk_id, chunk.into_bytes());
    let mut entries: Vec<ManifestEntry> = Vec::new();
    let push_singleton = |entries: &mut Vec<ManifestEntry>,
                          source_objects: &mut HashMap<Hash, Vec<u8>>,
                          tag: &'static str| {
        let object = composite_manifest::encode_chunk_envelope(tag, &[3u8; 11]);
        entries.push(ManifestEntry {
            tag,
            identity: object.hash(),
            payload_length: 11,
            kind: SectionKind::Singleton,
        });
        source_objects.insert(object.hash(), object.into_bytes());
    };
    for tag in [
        composite_manifest::TAG_HEADER,
        composite_manifest::TAG_NEAR,
        composite_manifest::TAG_FAR,
        composite_manifest::TAG_BLOCKED,
        composite_manifest::TAG_HELD,
        composite_manifest::TAG_LEDGER_META,
    ] {
        push_singleton(&mut entries, &mut source_objects, tag);
    }
    entries.push(ManifestEntry {
        tag: composite_manifest::TAG_LEDGER_RECORDS,
        identity: chunk_id,
        payload_length: (4 * 31) as u64,
        kind: SectionKind::LedgerSealed,
    });
    let tail = composite_manifest::encode_chunk_envelope(
        composite_manifest::TAG_LEDGER_RECORDS,
        &[4u8; 2 * 31],
    );
    entries.push(ManifestEntry {
        tag: composite_manifest::TAG_LEDGER_RECORDS,
        identity: tail.hash(),
        payload_length: (2 * 31) as u64,
        kind: SectionKind::LedgerTail,
    });
    source_objects.insert(tail.hash(), tail.into_bytes());
    for tag in [
        composite_manifest::TAG_WORLD,
        composite_manifest::TAG_DIRECTOR,
    ] {
        push_singleton(&mut entries, &mut source_objects, tag);
    }
    let manifest = composite_manifest::CompositeManifest {
        history_kind: composite_manifest::HistoryKind::Full,
        ledger_boundary: 4,
        composed_length: entries.iter().map(|entry| entry.payload_length).sum(),
        entries,
    };
    let root = engine_persist::StoredObject::new(composite_manifest::encode(&manifest)).unwrap();
    let mut source = MapSource {
        objects: source_objects,
    };
    source.objects.insert(root.hash(), root.bytes().to_vec());
    let head = RevisionRecord::founding(AuthorityMode::Standalone, "k2b-retirement-e2e", 0)
        .with_content_refs(vec![root.hash()]);
    source.objects.insert(head.id(), head.canonical_bytes());

    let mut memo = VerificationMemo::new();
    let live = live_set(
        &mut source,
        &[head.id()],
        &ReachabilityLimits::default(),
        &mut memo,
    )
    .unwrap_or_else(|error| panic!("the e2e walk completes: {error:?}"));
    let view = ReconciliationView::from_walk(&live);

    // The ambiguous legacy audit over {X, Y}: X is IN the walk's
    // live set (the retained root's chunk); Y is not.
    let x = chunk_id;
    let y = identity(9);
    let mut model = ProtocolModel::new();
    model.publish(&[x, y]).unwrap();
    let legacy = model.install(&[x, y], true).unwrap();
    assert!(model.detect_legacy_ambiguity(legacy).unwrap());
    let prepared = model.migrate_prepare(legacy, &view).unwrap();
    assert!(
        !prepared.successor_units.contains(&x),
        "the walk-verified live chunk is never in the reconciled set"
    );
    assert!(prepared.successor_units.contains(&y));
    let superseding = model.migrate_commit(&prepared).unwrap();
    model.begin_delete(superseding).unwrap();
    model.delete_step(superseding, y).unwrap();
    model.write_terminal(superseding).unwrap();
    assert!(model.store.contains(&x));
}

// ---------------------------------------- the crash/restart matrix (§4)

/// Drives the ambiguous-legacy fixture to the maintenance-blocked
/// state with X live-required and Y already deleted.
fn ambiguous_model() -> (ProtocolModel, u64, Hash, Hash) {
    let mut model = ProtocolModel::new();
    let (x, y) = (identity(1), identity(2));
    model.publish(&[x, y]).unwrap();
    let legacy = model.install(&[x, y], true).unwrap();
    model.begin_delete(legacy).unwrap();
    model.delete_step(legacy, y).unwrap();
    model.live_required.insert(x);
    assert!(model.detect_legacy_ambiguity(legacy).unwrap());
    (model, legacy, x, y)
}

fn verified_view(required: &[Hash]) -> ReconciliationView {
    ReconciliationView::Verified {
        required: required.iter().copied().collect(),
    }
}

#[test]
fn the_crash_matrix_across_the_staged_supersession() {
    // The mandated interruption matrix over the staged supersession
    // protocol — every reachable intermediate state is driven, with
    // the safety invariants asserted after EVERY transition (the
    // model's own check_invariants), and the fence-continuity
    // property checked at each boundary: X stays protected
    // throughout, by the old audit's fence pre-commit and by the
    // successor's reconciled fence (or the liveness rule)
    // post-commit.
    let x_protected = |model: &ProtocolModel, x: Hash| {
        // X is protected iff some non-terminal audit reserves it OR
        // the liveness rule holds it.
        model.audits.iter().any(|audit| {
            matches!(
                audit.phase,
                AuditPhase::Installed | AuditPhase::Deleting | AuditPhase::LegacyAmbiguous
            ) && audit.units.contains(&x)
        }) || model.live_required.contains(&x)
    };

    // (1) Crash BEFORE preparation: the legacy audit stays
    // maintenance-blocked and fenced; nothing changed.
    let (mut model, legacy, x, _) = ambiguous_model();
    model.restart().unwrap();
    assert_eq!(
        model.audit(legacy).unwrap().phase,
        AuditPhase::LegacyAmbiguous
    );
    assert_eq!(
        model.reintroduce(x),
        Err(ProtocolError::Fenced { by: vec![legacy] })
    );
    assert!(x_protected(&model, x));

    // (2) Crash AFTER preparation, BEFORE the durable commit: the
    // preparation is pure — NOTHING durable changed; the restart
    // keeps the old audit blocked and fenced; X protected.
    let (mut model, legacy, x, _) = ambiguous_model();
    let prepared = model.migrate_prepare(legacy, &verified_view(&[x])).unwrap();
    model.restart().unwrap();
    assert_eq!(
        model.audit(legacy).unwrap().phase,
        AuditPhase::LegacyAmbiguous
    );
    assert_eq!(
        model.reintroduce(x),
        Err(ProtocolError::Fenced { by: vec![legacy] })
    );
    assert!(x_protected(&model, x));
    // (3) is NOT a separate model state: the commit is ONE atomic
    // durable transition (the successor's installation and the old
    // audit's supersession establish together — no model-visible
    // window with ambiguous or missing deletion authority). The
    // atomicity itself is the documented UNPROVEN PRODUCTION
    // OBLIGATION (a real provider transaction must establish it).
    // (4) Crash IMMEDIATELY after the commit: the restart derives
    // supersession from the RECORD; exactly one authority (the
    // successor over the reconciled set); the old never resumes.
    let superseding = model.migrate_commit(&prepared).unwrap();
    model.restart().unwrap();
    assert_eq!(model.audit(legacy).unwrap().phase, AuditPhase::Superseded);
    assert_eq!(
        model.audit(superseding).unwrap().phase,
        AuditPhase::Installed
    );
    assert_eq!(
        model.delete_step(legacy, x),
        Err(ProtocolError::NotDeletable { audit: legacy })
    );
    // The successor fences Y (the reconciled remainder), and X is
    // protected by the liveness rule (the reconciled set excluded
    // it).
    assert!(x_protected(&model, x));

    // (5) Restart and REPEATED migration attempts: the repeated
    // commit of the SAME reconciliation is IDEMPOTENT (the restart
    // rule); a CONFLICTING attempt refuses typed.
    let again = model.migrate_commit(&prepared).unwrap();
    assert_eq!(again, superseding, "the repeated commit is idempotent");
    assert_eq!(
        model.migrate_prepare(legacy, &verified_view(&[identity(3)])),
        Err(ProtocolError::NotMigratable { audit: legacy }),
        "preparing a superseded audit refuses"
    );
    let mut conflicting = prepared.clone();
    conflicting.successor_units.insert(identity(4));
    assert_eq!(
        model.migrate_commit(&conflicting),
        Err(ProtocolError::SupersessionConflict { audit: legacy })
    );

    // (6) Crash during the successor's deletion: the restart keeps
    // the Deleting phase; the resume completes from the manifest.
    let (mut model, legacy, x, y) = ambiguous_model();
    let prepared = model.migrate_prepare(legacy, &verified_view(&[x])).unwrap();
    let superseding = model.migrate_commit(&prepared).unwrap();
    model.begin_delete(superseding).unwrap();
    model.restart().unwrap();
    assert_eq!(
        model.audit(superseding).unwrap().phase,
        AuditPhase::Deleting
    );
    // Y is already absent (the legacy audit deleted it) — the
    // successor's deletion is idempotent against the manifest.
    model.delete_step(superseding, y).unwrap();
    assert!(model.complete(superseding).unwrap());

    // (7) Crash after all units are absent, BEFORE terminal: the
    // restart keeps the successor Deleting (the completion is
    // verified but the terminal record is NOT written); the fence
    // holds until the record lands.
    model.restart().unwrap();
    assert_eq!(
        model.audit(superseding).unwrap().phase,
        AuditPhase::Deleting
    );
    assert_eq!(
        model.reintroduce(y),
        Err(ProtocolError::Fenced {
            by: vec![superseding]
        }),
        "the fence holds until the durable terminal record"
    );
    // (8) Crash after the durable terminal confirmation: the
    // restart derives Terminal from the record.
    model.write_terminal(superseding).unwrap();
    model.restart().unwrap();
    assert_eq!(
        model.audit(superseding).unwrap().phase,
        AuditPhase::Terminal
    );
    // (9) Identical-hash reintroduction after terminality: Y
    // resupplies as a fresh object; the retired audits never touch
    // it; a further restart never resurrects either fence.
    model.reintroduce(y).unwrap();
    model.restart().unwrap();
    assert_eq!(
        model.audit(superseding).unwrap().phase,
        AuditPhase::Terminal
    );
    assert_eq!(model.audit(legacy).unwrap().phase, AuditPhase::Superseded);
    assert_eq!(
        model.delete_step(superseding, y),
        Err(ProtocolError::NotDeletable { audit: superseding })
    );
    assert!(model.store.contains(&y) && model.store.contains(&x));

    // (10) A legacy audit with BOTH currently live and genuinely
    // retired identities: covered by the ambiguous scenario and
    // the end-to-end walk test — the reconciled set excludes the
    // live-required identity and retains the genuinely retired one.
    // (11) Failure/unavailability during live-set reconciliation:
    // covered by the_unverified_view_refuses_the_migration — the
    // unverified view refuses, the audit stays blocked.
    // (12) Conflicting/duplicate supersession attempts: covered in
    // (5) — the idempotent repeat and the typed conflict refusal.
}

#[test]
fn the_supersession_key_discipline_scenario() {
    // The idempotency-key discipline and the ambiguity's exclusive
    // scope: the migration's superseding key is distinct from every
    // existing key; a NEW-protocol audit can never go ambiguous —
    // the fence refuses the resupply that would create the
    // ambiguity, so the ambiguous state is exclusively the LEGACY
    // transition's artifact (the pre-protocol audits carry no
    // terminal record and predate the fence).
    let mut model = ProtocolModel::new();
    let (x, y, z) = (identity(1), identity(2), identity(3));
    model.publish(&[x, y, z]).unwrap();
    let legacy = model.install(&[x, y], true).unwrap();
    let fresh = model.install(&[z], false).unwrap();
    model.live_required.insert(x);
    assert!(model.detect_legacy_ambiguity(legacy).unwrap());
    let prepared = model.migrate_prepare(legacy, &verified_view(&[x])).unwrap();
    let superseding = model.migrate_commit(&prepared).unwrap();
    // Every audit's key is distinct — the supersession
    // discriminator makes a collision unrepresentable.
    let keys: Vec<Vec<u8>> = model.audits.iter().map(|audit| audit.key.clone()).collect();
    let mut sorted = keys.clone();
    sorted.sort();
    sorted.dedup();
    assert_eq!(sorted.len(), keys.len());
    assert_ne!(
        model.audit(superseding).unwrap().key,
        model.audit(legacy).unwrap().key
    );
    // A new-protocol audit never migrates and never ambiguates —
    // the typed refusals.
    assert_eq!(
        model.detect_legacy_ambiguity(fresh),
        Err(ProtocolError::NotMigratable { audit: fresh })
    );
    assert_eq!(
        model.migrate_prepare(fresh, &verified_view(&[])),
        Err(ProtocolError::NotMigratable { audit: fresh })
    );
    // The fence prevents the resupply that would create a new
    // protocol audit's ambiguity: Y stays reserved by the
    // superseding audit while it is non-terminal.
    assert_eq!(
        model.reintroduce(y),
        Err(ProtocolError::Fenced {
            by: vec![superseding]
        })
    );
    // The superseding audit completes over the reconciled remainder
    // and terminalizes; X (live-required) was never touched.
    model.begin_delete(superseding).unwrap();
    model.delete_step(superseding, y).unwrap();
    model.write_terminal(superseding).unwrap();
    assert!(model.store.contains(&x));
}

// ------------------- the R3 and freshness final-review regressions

#[test]
fn the_r3_idempotency_compares_the_complete_reconciliation_evidence() {
    // R3: a repeated commit is idempotent ONLY on the complete
    // evidence — the same required set AND the same successor units
    // under the same successor relationship. Different
    // reconciliation views producing the SAME deletion set are
    // DIFFERENT claims: the changed view can never inherit the
    // committed view's authority.
    let (mut model, legacy, x, y) = ambiguous_model();
    // View 1: required {X} → the reconciled successor units {Y}.
    let prepared_first = model.migrate_prepare(legacy, &verified_view(&[x])).unwrap();
    assert_eq!(
        prepared_first.successor_units,
        [y].into_iter().collect::<BTreeSet<Hash>>()
    );
    let superseding = model.migrate_commit(&prepared_first).unwrap();

    // (1) The EXACT repeat of the same prepared supersession is
    // idempotent.
    assert_eq!(model.migrate_commit(&prepared_first).unwrap(), superseding);

    // (4) The repeat after a process-model restart is idempotent
    // for identical evidence only.
    model.restart().unwrap();
    assert_eq!(model.migrate_commit(&prepared_first).unwrap(), superseding);

    // (2) A DIFFERENT required set producing DIFFERENT successor
    // units — conflict.
    let mut different_units = prepared_first.clone();
    different_units.required_at_preparation = [x, identity(3)].into_iter().collect();
    different_units.successor_units = [y, identity(4)].into_iter().collect();
    assert_eq!(
        model.migrate_commit(&different_units),
        Err(ProtocolError::SupersessionConflict { audit: legacy })
    );

    // (3) THE R3 CORE: a different required set producing the
    // IDENTICAL successor units {Y} — still a conflict (a different
    // reconciliation claim, never authority inheritance).
    let mut same_units = prepared_first.clone();
    same_units.required_at_preparation = [x, identity(5)].into_iter().collect();
    assert_ne!(
        same_units.required_at_preparation, prepared_first.required_at_preparation,
        "the adversarial fixture carries a different required set"
    );
    assert_eq!(
        same_units.successor_units, prepared_first.successor_units,
        "the adversarial fixture produces the same deletion set"
    );
    assert_eq!(
        model.migrate_commit(&same_units),
        Err(ProtocolError::SupersessionConflict { audit: legacy }),
        "the same deletion set under a different view is a conflict, never idempotent"
    );

    // (6) + (7) The rejected conflict mutated NOTHING: the original
    // audit, the successor, the supersession record, and the
    // active reservations are all unchanged.
    assert_eq!(model.audit(legacy).unwrap().phase, AuditPhase::Superseded);
    assert_eq!(
        model.audit(superseding).unwrap().units,
        [y].into_iter().collect::<BTreeSet<Hash>>()
    );
    let record = model.supersession_records.get(&legacy).unwrap();
    assert_eq!(record.successor, superseding);
    assert_eq!(
        record.committed_required,
        [x].into_iter().collect::<BTreeSet<Hash>>()
    );
    assert_eq!(model.audits.len(), 2, "no phantom successor was installed");
    // The reservation behavior is unchanged: Y stays fenced by the
    // successor.
    assert_eq!(
        model.reintroduce(y),
        Err(ProtocolError::Fenced {
            by: vec![superseding]
        })
    );

    // (5) The repeat after TERMINAL confirmation never resurrects
    // deletion authority: the exact repeat still returns the
    // successor's identity, the successor stays Terminal (its
    // record), and deletion through it still refuses.
    model.begin_delete(superseding).unwrap();
    model.delete_step(superseding, y).unwrap();
    model.write_terminal(superseding).unwrap();
    assert_eq!(model.migrate_commit(&prepared_first).unwrap(), superseding);
    model.restart().unwrap();
    assert_eq!(
        model.audit(superseding).unwrap().phase,
        AuditPhase::Terminal
    );
    assert_eq!(
        model.delete_step(superseding, y),
        Err(ProtocolError::NotDeletable { audit: superseding })
    );
}

#[test]
fn the_preparation_to_commit_freshness_check_refuses_stale_reconciliations() {
    // The targeted freshness check: a reconciliation VERIFIED at
    // preparation can go stale before the commit — the modeled
    // live-required set grows to include a reconciled unit between
    // the two steps. The commit REFUSES the now-stale preparation
    // (fail-closed); the legacy audit keeps its conservative fence;
    // a FRESH preparation under the updated view commits cleanly.
    // The production counterpart of this window lives inside the
    // provider's atomic supersession transaction — the documented
    // unproven obligation; the model invents no provider-level
    // concurrency guarantee, only the commit-time re-validation.
    // The realistic staleness shape: the pre-protocol audit crashed
    // BEFORE any deletion — both units remain present, X's presence
    // against the reservation makes it ambiguous — and the
    // preparation's view {X} is INCOMPLETE: another retained root's
    // requirement for Y is discovered only after the preparation.
    let mut model = ProtocolModel::new();
    let (x, y) = (identity(1), identity(2));
    model.publish(&[x, y]).unwrap();
    let legacy = model.install(&[x, y], true).unwrap();
    model.live_required.insert(x);
    assert!(model.detect_legacy_ambiguity(legacy).unwrap());
    assert_eq!(
        model.audit(legacy).unwrap().phase,
        AuditPhase::LegacyAmbiguous
    );
    let prepared = model.migrate_prepare(legacy, &verified_view(&[x])).unwrap();
    assert_eq!(
        prepared.successor_units,
        [y].into_iter().collect::<BTreeSet<Hash>>()
    );

    // The liveness change: Y (present all along) joins the
    // live-required set — the prepared reconciliation would now
    // delete live content.
    model.live_required.insert(y);

    // The stale commit REFUSES.
    assert_eq!(
        model.migrate_commit(&prepared),
        Err(ProtocolError::MigrationBlocked {
            reason: "the prepared reconciliation is stale: a reconciled unit is now live-required",
        })
    );
    // The legacy audit keeps its conservative protection: still
    // maintenance-blocked, still fencing both units.
    assert_eq!(
        model.audit(legacy).unwrap().phase,
        AuditPhase::LegacyAmbiguous
    );
    assert_eq!(
        model.reintroduce(x),
        Err(ProtocolError::Fenced { by: vec![legacy] })
    );
    assert_eq!(
        model.reintroduce(y),
        Err(ProtocolError::Fenced { by: vec![legacy] })
    );
    assert!(!model.supersession_records.contains_key(&legacy));

    // The recovery path: a FRESH preparation under the UPDATED view
    // {X, Y} reconciles to NOTHING and commits cleanly (an empty
    // successor deletion set — the audit is fully superseded with
    // nothing left to delete).
    let fresh = model
        .migrate_prepare(legacy, &verified_view(&[x, y]))
        .unwrap();
    assert!(fresh.successor_units.is_empty());
    let superseding = model.migrate_commit(&fresh).unwrap();
    model.begin_delete(superseding).unwrap();
    assert!(model.complete(superseding).unwrap());
    model.write_terminal(superseding).unwrap();
    assert!(model.store.contains(&x) && model.store.contains(&y));
}
