//! The S6-C4 Phase 2 K2-B RETIREMENT PROTOCOL STATE MACHINE — a
//! PURE, test-only model of the revised terminal-retirement design
//! (the K2-A report's fence proposal, corrected per the K2-B
//! directive's two open questions). Nothing here touches a
//! provider, a medium, a file, or a database: the model is an
//! in-memory state machine whose EVERY transition re-asserts the
//! protocol's structural invariants, driven through the six
//! mandated scenarios — audit acceptance, interrupted deletion,
//! terminal confirmation, identical-hash reintroduction, restart,
//! and the ambiguous legacy audit.
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
//!   the only exit is a VERIFIED NON-DESTRUCTIVE MIGRATION that
//!   computes the reconciled deletion set (the old units minus the
//!   identities the CURRENT retained roots require) and publishes
//!   it as a NEW superseding audit under a DISTINCT idempotency key
//!   — the old audit's manifest and key stay immutable; the old
//!   audit is never resumed.
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
    /// content — maintenance-blocked; only migration exits.
    LegacyAmbiguous,
    /// A newer superseding audit replaced this one; this audit is
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

/// A typed protocol refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
enum ProtocolError {
    /// The publication or resupply intersects the active
    /// reservation of these non-terminal audits.
    Fenced { by: Vec<u64> },
    /// A terminal, ambiguous, or superseded audit was asked to
    /// delete — it never resumes.
    NotDeletable { audit: u64 },
    /// Only a legacy-ambiguous audit can migrate.
    NotMigratable { audit: u64 },
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
    /// The identities the CURRENT retained roots' closures require
    /// (the migration's liveness input — never deleted).
    live_required: BTreeSet<Hash>,
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
    /// - P4 (structurally, in `publish`): the reservation — the
    ///   union of every non-terminal, non-superseded audit's units
    ///   — is the ONLY fence; terminal and superseded audits
    ///   contribute nothing.
    /// - The store never loses a live-required identity.
    fn check_invariants(&self) -> Result<(), ProtocolError> {
        for audit in &self.audits {
            if audit.phase == AuditPhase::Terminal && !self.terminal_records.contains(&audit.id) {
                return Err(ProtocolError::Invariant("terminal-without-record"));
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

    /// Restart: the durable state is re-derived from the records —
    /// terminality from the TERMINAL RECORDS, never from the
    /// objects' presence (the ABA fix). A restart can only CONFIRM
    /// phases already durable; it never resurrects a fence.
    fn restart(&mut self) -> Result<(), ProtocolError> {
        for audit in &mut self.audits {
            if self.terminal_records.contains(&audit.id) {
                audit.phase = AuditPhase::Terminal;
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

    /// The verified NON-DESTRUCTIVE migration of a legacy-ambiguous
    /// audit: the reconciled deletion set (the old units MINUS the
    /// identities the current retained roots require) is published
    /// as a NEW superseding audit under a DISTINCT idempotency key;
    /// the old audit's manifest and key remain IMMUTABLE and the
    /// old audit is never resumed.
    fn migrate(&mut self, audit_id: u64) -> Result<u64, ProtocolError> {
        let (old_units, old_key) = {
            let audit = self
                .audits
                .iter_mut()
                .find(|audit| audit.id == audit_id)
                .ok_or(ProtocolError::UnknownAudit { audit: audit_id })?;
            if audit.phase != AuditPhase::LegacyAmbiguous {
                return Err(ProtocolError::NotMigratable { audit: audit_id });
            }
            audit.phase = AuditPhase::Superseded;
            (audit.units.clone(), audit.key.clone())
        };
        // The reconciliation: live-required identities survive.
        let reconciled: Vec<Hash> = old_units
            .iter()
            .filter(|unit| !self.live_required.contains(*unit))
            .copied()
            .collect();
        // The superseding audit's DISTINCT idempotency key: the old
        // key plus the supersession discriminator (a fresh durable
        // key — the old key stays consumed and immutable).
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
            units: reconciled.iter().copied().collect(),
            legacy: false,
            key,
        });
        self.record("migrate")?;
        Ok(new_id)
    }
}

// --------------------------------------------------- the six scenarios

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
    // state; destructive resumption is REFUSED (a conservative
    // fence is NOT permission to delete); the verified
    // non-destructive migration publishes the reconciled set under
    // a NEW audit with a DISTINCT key; the old audit is superseded,
    // immutable, and never resumed; X survives.
    let mut model = ProtocolModel::new();
    let (x, y) = (identity(1), identity(2));
    // The pre-protocol world: the audit deleted Y but not X, then
    // died — no terminal record exists.
    model.publish(&[x, y]).unwrap();
    let legacy = model.install(&[x, y], true).unwrap();
    model.begin_delete(legacy).unwrap();
    model.delete_step(legacy, y).unwrap();
    // X is reintroduced as live content — a retained root requires
    // it (the migration's liveness input).
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
    // The migration: the reconciled set excludes the live-required
    // X; the superseding audit carries a DISTINCT key.
    let old_key = model.audit(legacy).unwrap().key.clone();
    let old_units = model.audit(legacy).unwrap().units.clone();
    let superseding = model.migrate(legacy).unwrap();
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
    let superseding = model.migrate(legacy).unwrap();
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
        model.migrate(fresh),
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
