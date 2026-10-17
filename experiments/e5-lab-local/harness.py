#!/usr/bin/env python3
# E5-Lab LOCAL harness — deterministic simulated canonical medium.
#
# Authorization (project lead, 2026-10-17): "construct and run a narrowly
# scoped, headless, local-only E5-Lab harness using a deterministic simulated
# canonical medium. Test CAS contention, fencing, ambiguous retries, crash
# recovery, and revision validation. Document limitations clearly."
#
# Prohibitions honored: no live R2 connection, no cloud resources, no payment
# credentials, no charges, no network access of any kind. This is protocol
# logic over a simulation — NOT a provider test and NOT engine implementation.
#
# Findings and limitations are recorded in
# docs/E5-CANONICAL-MEDIUM-FEASIBILITY.md (§9).

import copy
import hashlib
import hmac
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
RUN_DATE = "2026-10-17"


def canon(obj):
    return json.dumps(obj, sort_keys=True, separators=(",", ":"))


def rid_of(body):
    return hashlib.sha256(canon(body).encode()).hexdigest()[:16]


class Medium:
    """Simulated conditional-append (CAS) canonical medium.

    Deterministic; in-process; treated as durable across simulated CLIENT
    crashes within a run. Real-provider durability is NOT tested here.

    mode "ok": appends are acknowledged normally.
    mode "ambiguous": attempts alternate deterministically — even attempt
    numbers are applied then the acknowledgment is dropped (caller sees
    "timeout"); odd attempt numbers are dropped entirely (caller also sees
    "timeout"). The caller cannot distinguish the two cases.
    """

    def __init__(self):
        self.head = None
        self.objects = {}
        self.log = []
        self.attempt = 0
        self.mode = "ok"

    def conditional_append(self, record, expected_head):
        self.attempt += 1
        if expected_head != self.head:
            self.log.append([self.attempt, "reject", record["id"]])
            return "rejected"
        if self.mode == "ambiguous":
            if self.attempt % 2 == 0:
                self.objects[record["id"]] = record
                self.head = record["id"]
                self.log.append([self.attempt, "apply+silent", record["id"]])
            else:
                self.log.append([self.attempt, "drop+silent", record["id"]])
            return "timeout"
        self.objects[record["id"]] = record
        self.head = record["id"]
        self.log.append([self.attempt, "accept", record["id"]])
        return "accepted"

    def head_id(self):
        return self.head

    def chain(self):
        out, cur = [], self.head
        while cur is not None:
            out.append(self.objects[cur])
            cur = self.objects[cur]["parent"]
        out.reverse()
        return out


class Writer:
    """Chain-writing client: session epoch, HMAC 'signatures', idempotency keys."""

    def __init__(self, medium, member, secret, epoch=1):
        self.m = medium
        self.member = member
        self.secret = secret.encode()
        self.epoch = epoch

    def make_record(self, kind, key, payload, epoch=None):
        body = {
            "kind": kind,
            "parent": self.m.head_id(),
            "epoch": self.epoch if epoch is None else epoch,
            "host": self.member,
            "idem_keys": [key] if key else [],
            "payload": payload,
        }
        body["sig"] = hmac.new(self.secret, canon(body).encode(), hashlib.sha256).hexdigest()[:16]
        body["id"] = rid_of(body)
        return body

    def open_session(self):
        rec = self.make_record("session-open", None, {"note": "session"})
        return self.m.conditional_append(rec, rec["parent"])

    def commit(self, key, payload):
        """Exactly-once commit with ambiguous-outcome resolution: check the
        chain for the idempotency key first; append conditionally; on timeout
        or rejection, re-read the head and retry with the SAME key."""
        for _ in range(4):
            for rec in self.m.chain():
                if key in rec["idem_keys"]:
                    return ("already-committed", rec["id"])
            rec = self.make_record("commit", key, payload)
            out = self.m.conditional_append(rec, rec["parent"])
            if out == "accepted":
                return ("committed", rec["id"])
        return ("unresolved", None)


def validate_chain(chain, registry):
    """Walk genesis to head. Returns (ok, first_bad_index, reason).
    Checks parent links, recomputed record ids, signatures against the member
    registry, session-epoch discipline (session-open = last+1; every other
    record carries the open session's epoch), and idempotency-key uniqueness."""
    seen_keys = set()
    last_epoch = 0
    prev = None
    for i, rec in enumerate(chain):
        if rec["parent"] != prev:
            return (False, i, "parent-mismatch")
        no_id = {k: v for k, v in rec.items() if k != "id"}
        if rid_of(no_id) != rec["id"]:
            return (False, i, "hash-mismatch")
        if rec["host"] not in registry:
            return (False, i, "unknown-host")
        no_sig = {k: v for k, v in no_id.items() if k != "sig"}
        want = hmac.new(registry[rec["host"]].encode(), canon(no_sig).encode(), hashlib.sha256).hexdigest()[:16]
        if want != rec["sig"]:
            return (False, i, "bad-signature")
        if rec["kind"] == "session-open":
            if rec["epoch"] != last_epoch + 1:
                return (False, i, "epoch-bump")
            last_epoch = rec["epoch"]
        elif rec["epoch"] != last_epoch:
            return (False, i, "stale-epoch")
        for k in rec["idem_keys"]:
            if k in seen_keys:
                return (False, i, "duplicate-idem-key")
            seen_keys.add(k)
        prev = rec["id"]
    return (True, None, None)


def last_valid_prefix(chain, registry):
    ok, i, reason = validate_chain(chain, registry)
    if ok:
        return chain, None, None
    prefix = chain[:i]
    ok2, _, _ = validate_chain(prefix, registry)
    return (prefix if ok2 else None), i, reason


def run_suite():
    registry = {"alice": "s3cr3t-alice", "bob": "s3cr3t-bob"}
    tests = []

    # T1 — CAS contention: two writers aim at the same head every round.
    m = Medium()
    alice = Writer(m, "alice", registry["alice"])
    bob = Writer(m, "bob", registry["bob"])
    alice.open_session()
    heads = [m.head_id()]
    rounds_ok = True
    for i in range(100):
        a = alice.make_record("commit", "a%d" % i, {"round": i, "writer": "alice"})
        b = bob.make_record("commit", "b%d" % i, {"round": i, "writer": "bob"})
        out_a = m.conditional_append(a, a["parent"])
        out_b = m.conditional_append(b, b["parent"])
        if not (out_a == "accepted" and out_b == "rejected"):
            rounds_ok = False
        heads.append(m.head_id())
    ok, _, _ = validate_chain(m.chain(), registry)
    chain_len = len(m.chain())
    bob_landed = [r for r in m.chain() if any(k.startswith("b") for k in r["idem_keys"])]
    t1_pass = rounds_ok and ok and chain_len == 101 and not bob_landed
    tests.append({"name": "T1 CAS contention", "pass": t1_pass,
                  "detail": "100 rounds x 2 simultaneous appends at the same head: "
                            "100 accepted / 100 rejected; no round produced two winners; "
                            "losing records never entered the chain (length %d)" % chain_len})

    # T2a — stale-parent fencing (same medium as T1).
    old_head = heads[5]
    stale_rejected = 0
    for i in range(20):
        rec = alice.make_record("commit", "stale-p%d" % i, {"probe": i})
        if m.conditional_append(rec, old_head) == "rejected":
            stale_rejected += 1
    t2a_pass = stale_rejected == 20 and len(m.chain()) == 101
    tests.append({"name": "T2a stale-parent fencing", "pass": t2a_pass,
                  "detail": "%d/20 appends with a stale expected-head were rejected by "
                            "CAS; chain unchanged" % stale_rejected})

    # T2b — stale-epoch fencing (fresh medium; the medium is epoch-blind).
    m2 = Medium()
    a2 = Writer(m2, "alice", registry["alice"])
    a2.open_session()
    for i in range(5):
        a2.commit("v%d" % i, {"i": i})
    pre_head = m2.head_id()
    stale = a2.make_record("commit", "stale-epoch-key", {"forced": True}, epoch=0)
    out = m2.conditional_append(stale, stale["parent"])  # accepted: parent is fresh
    okv, idx, reason = validate_chain(m2.chain(), registry)
    prefix, _pidx, _preason = last_valid_prefix(m2.chain(), registry)
    t2b_pass = (out == "accepted" and okv is False and reason == "stale-epoch"
                and idx == 6 and prefix is not None and prefix[-1]["id"] == pre_head)
    tests.append({"name": "T2b stale-epoch fencing", "pass": t2b_pass,
                  "detail": "epoch-blind medium accepts a fresh-parent/stale-epoch record; "
                            "chain validation rejects it ('%s' at index %s); last-valid-prefix "
                            "fallback returns the pre-injection head" % (reason, idx)})

    # T3 — ambiguous retries (fresh medium, ambiguous acknowledgment mode).
    m3 = Medium()
    a3 = Writer(m3, "alice", registry["alice"])
    a3.open_session()
    m3.mode = "ambiguous"
    resolved = 0
    for i in range(50):
        status, _ = a3.commit("k%d" % i, {"i": i})
        if status in ("committed", "already-committed"):
            resolved += 1
    ok3, _r3i, _r3r = validate_chain(m3.chain(), registry)
    keys3 = [k for rec in m3.chain() for k in rec["idem_keys"]]
    t3_pass = (resolved == 50 and ok3
               and sorted(keys3) == sorted("k%d" % i for i in range(50)))
    tests.append({"name": "T3 ambiguous retries", "pass": t3_pass,
                  "detail": "50/50 idempotency keys committed exactly once under "
                            "apply-then-drop-ack / drop-then-timeout ambiguity; resolution "
                            "by chain re-read; chain valid"})

    # T4 — crash recovery (fresh medium).
    m4 = Medium()
    a4 = Writer(m4, "alice", registry["alice"])
    a4.open_session()
    for i in range(1, 15):
        a4.commit("c%d" % i, {"i": i})
    rec15 = a4.make_record("commit", "c15", {"i": 15})
    crash_out = m4.conditional_append(rec15, rec15["parent"])  # lands...
    del a4  # ...client dies before recording the acknowledgment
    a4b = Writer(m4, "alice", registry["alice"], epoch=2)
    sess_out = a4b.open_session()
    res15 = a4b.commit("c15", {"i": 15})  # must find c15 already committed
    for i in range(16, 31):
        a4b.commit("c%d" % i, {"i": i})
    ok4, _r4i, _r4r = validate_chain(m4.chain(), registry)
    keys4 = [k for rec in m4.chain() for k in rec["idem_keys"]]
    t4_pass = (crash_out == "accepted" and sess_out == "accepted"
               and res15 == ("already-committed", rec15["id"]) and ok4
               and sorted(keys4) == sorted("c%d" % i for i in range(1, 31)))
    tests.append({"name": "T4 crash recovery", "pass": t4_pass,
                  "detail": "writer destroyed after an unacknowledged accepted append; "
                            "restarted writer re-read the chain, found its key already "
                            "committed (no duplicate), opened a valid successor session "
                            "(epoch 2), and completed all 30 keys — no gaps, no duplicates"})

    # T5 — revision validation (fresh medium + injected faults on copies).
    m5 = Medium()
    a5 = Writer(m5, "alice", registry["alice"])
    a5.open_session()
    for i in range(5):
        a5.commit("w%d" % i, {"i": i})
    good = m5.chain()
    okg, _gi, _gr = validate_chain(good, registry)

    faults = []
    c = copy.deepcopy(good)
    c[2]["payload"] = {"evil": True}
    faults.append(("tampered-payload", validate_chain(c, registry),
                   (False, 2, "hash-mismatch")))
    c = copy.deepcopy(good)
    last = c[-1]
    last["sig"] = "forged-signature"
    last["id"] = rid_of({k: v for k, v in last.items() if k != "id"})
    faults.append(("forged-signature", validate_chain(c, registry),
                   (False, 5, "bad-signature")))
    dup = a5.make_record("commit", "w3", {"dup": True})
    faults.append(("duplicate-idem-key", validate_chain(good + [dup], registry),
                   (False, 6, "duplicate-idem-key")))
    stale5 = a5.make_record("commit", "fresh-key", {"x": 1}, epoch=0)
    faults.append(("stale-epoch", validate_chain(good + [stale5], registry),
                   (False, 6, "stale-epoch")))
    c = copy.deepcopy(good)
    c[2]["parent"] = "bogus-parent"
    faults.append(("parent-mismatch", validate_chain(c, registry),
                   (False, 2, "parent-mismatch")))
    t5_pass = okg and all(got == want for _n, got, want in faults)
    tests.append({"name": "T5 revision validation", "pass": t5_pass,
                  "detail": "good chain validates; 5/5 injected faults (tampered payload, "
                            "forged signature, duplicate idem-key, stale epoch, parent "
                            "mismatch) rejected with the correct reasons"})

    stats = {"t1_chain_length": chain_len, "t3_keys": 50, "t4_keys": 30, "t5_faults": 5}
    return {"tests": tests, "stats": stats}


def fingerprint(results):
    return hashlib.sha256(canon(results).encode()).hexdigest()[:16]


def main():
    r1 = run_suite()
    r2 = run_suite()
    fp1, fp2 = fingerprint(r1), fingerprint(r2)
    deterministic = fp1 == fp2
    results = dict(r1)
    results["meta"] = {
        "run_date": RUN_DATE,
        "medium": "deterministic in-process simulation (conditional-append contract)",
        "python": sys.version.split()[0],
        "authorization": "lead, 2026-10-17 — local-only, headless; no live providers, "
                         "no cloud resources, no credentials, no charges, no network",
    }
    results["determinism"] = {
        "pass": deterministic,
        "fingerprint": fp1,
        "note": "sha256 of canonical suite results (tests+stats), compared across "
                "two full in-process runs",
    }
    all_pass = deterministic and all(t["pass"] for t in results["tests"])

    print("E5-Lab LOCAL harness — deterministic simulated canonical medium")
    print("authorization: lead 2026-10-17 (local-only, headless; no providers, "
          "no network, no credentials, no charges)")
    print("")
    for t in results["tests"]:
        print("[%s] %s — %s" % ("PASS" if t["pass"] else "FAIL", t["name"], t["detail"]))
    print("[%s] T0 determinism — identical results fingerprints across two full "
          "suite runs: %s" % ("PASS" if deterministic else "FAIL", fp1))
    print("")
    print("overall: %s" % ("ALL CHECKS PASSED" if all_pass else "FAILURES PRESENT"))

    out_path = os.path.join(HERE, "results.json")
    with open(out_path, "w") as f:
        json.dump(results, f, indent=2, sort_keys=True)
    print("results written: %s" % out_path)
    return 0 if all_pass else 1


if __name__ == "__main__":
    sys.exit(main())
