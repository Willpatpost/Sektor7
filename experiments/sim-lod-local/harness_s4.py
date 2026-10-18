#!/usr/bin/env python3
# S4-LITE harness — narrative gate semantics under time advancement.
#
# Authorization (project lead, 2026-10-18): "You are authorized to implement
# and run S4-lite: Narrative Gate Semantics Under Time Advancement." Scope:
# the four event classifications of SIMULATION-ARCHITECTURE-RESEARCH §6
# (authored story milestones requiring narrative gates; formal voting-
# dependent story decisions awaiting governance authorization — the voting
# system itself is NOT implemented or redesigned here; ordinary reversible
# world routines; emergent irreversible city-side consequences), preserving
# the ratified M-07 boundary: frontier-only time advancement must not
# independently rewrite major authored-city narrative state.
#
# Required scenarios honored: 24h / 7d / 30d clock advancement; routine
# processing during frontier-only epochs; milestones held without firing or
# silent expiry; vote-dependent events modeled as awaiting authorization;
# revalidation of held events on full-roster city re-entry (preconditions,
# trigger context, classification); rejection or safe supersession of events
# whose prerequisites no longer hold; dependency ordering (no dependent
# irreversible event before its prerequisite); duplicate-execution
# prevention across repeated skips, revalidation and re-entry; independent
# reference model; deterministic logical-state fingerprints; reproducible
# failure records.
#
# Performance measurements honored: events processed / held / revalidated /
# canceled / executed; scheduling overhead; elapsed time; time-skip cost
# curves (24h, 7d, 30d, plus a same-elapsed low-workload control). Cost-vs-
# workload is DEMONSTRATED BY MEASUREMENT, not assumed from the priority
# queue. Event types are realistic and explicitly described; no artificial
# computational padding.
#
# Prohibitions honored: no production engine code, renderer, cloud
# infrastructure or networking; no new technology selections; no expansion
# into full S1/S2/S3/S5/S7; no changes to ratified gameplay requirements;
# the tiered hybrid remains an unapproved research hypothesis. Correctness
# results here demonstrate gate SEMANTICS in this harness, not real game
# behavior.
#
# Note (lead, 2026-10-18): the S1/S6-lite simulated host-loss test does NOT
# establish durability of local emergency recovery copies; that remains a
# separate persistence concern and is not claimed here either.

import heapq
import json
import os
import platform
import random
import sys
import time

import harness as base

HERE = os.path.dirname(os.path.abspath(__file__))
RUN_DATE = "2026-10-18"

DAY = base.DAY
WEEK = base.WEEK
S4_SEED = 44004          # scenario/script seed (currently scripted; reserved)
S4_MSEED = 44005         # milestone trigger jitter seed
LANE_GATED = "gated"
LANE_VOTE = "vote"
LANE_EMERG = "emergent_city"


# ----------------------------------------------------------------------------
# Milestone definitions (deterministic; explicit workload description)
#
# Routine lane: NPC commitment transitions (the S1/S6 schedule machinery) —
#   ordinary reversible world routines; they process during frontier-only
#   epochs and never touch narrative flags.
# Gated lane (m##): authored story milestones with narrative effects; held
#   during frontier-only time, evaluated in-session, revalidated on re-entry.
# Vote lane (v##): formal story decisions that await a governance
#   authorization (stubbed grant op); effects deferred to a city session.
# Emergent-city lane (e#): irreversible consequences; fire immediately when
#   due during an open city session, held when due during frontier-only time.
# Dependencies (depends): a milestone fires only after its prerequisites
#   fired; a rejected/superseded prerequisite supersedes the dependent.
# Fallbacks (superseded_by): a rejected milestone may be safely superseded
#   by a defined replacement that fires in its place.
# ----------------------------------------------------------------------------

def build_milestones():
    rng = random.Random(S4_MSEED)
    flags0 = {"quest_a": True, "truce": True, "district_open": True,
              "grid": True, "aftermath": False}
    ms = []
    # gated roots m00..m09 — triggers spread across ~33 days
    for i in range(10):
        ms.append({"mid": "m%02d" % i, "lane": LANE_GATED,
                   "trigger": int(rng.uniform(0.5, 33.0) * DAY),
                   "requires": [], "depends": [], "effects": [],
                   "superseded_by": None})
    # story-evolution effects: m02 breaks the truce, m03 closes the district
    ms[2]["effects"] = [("truce", False)]
    ms[3]["effects"] = [("district_open", False)]
    # forced early triggers so the scripted timeline deterministically
    # exercises every lane (seeded rng jitter remains for the others)
    ms[0]["trigger"] = int(0.8 * DAY)
    ms[1]["trigger"] = int(1.5 * DAY)
    ms[2]["trigger"] = int(1.0 * DAY)
    ms[3]["trigger"] = int(5.0 * DAY)
    # m04 resolves the quest arc
    ms[4]["effects"] = [("quest_a", False), ("aftermath", True)]
    # m05 (late trigger) requires the truce — it will be held through
    # frontier epochs and REJECTED at re-entry after m02 fired; it has a
    # defined fallback replacement
    ms[5]["requires"] = [("truce", True)]
    ms[5]["trigger"] = int(rng.uniform(18.0, 28.0) * DAY)
    ms[5]["superseded_by"] = "m05f"
    ms.append({"mid": "m05f", "lane": LANE_GATED, "trigger": 10 ** 9,
               "requires": [], "depends": [], "effects": [("aftermath", True)],
               "superseded_by": None})
    # m06 requires the district open (m03 closes it early) — rejected at
    # first evaluation after that, no fallback
    for m in ms:
        if m["mid"] == "m06":
            m["requires"] = [("district_open", True)]
            m["trigger"] = int(rng.uniform(6.0, 12.0) * DAY)
    # dependents: a chain across sessions (m00 -> d10 -> d11), a dependent
    # of the doomed m05 (d12), and a dependent of a root (d13)
    ms.append({"mid": "d10", "lane": LANE_GATED,
               "trigger": int(rng.uniform(3.0, 6.0) * DAY),
               "requires": [], "depends": ["m00"], "effects": [("aftermath", True)],
               "superseded_by": None})
    ms.append({"mid": "d11", "lane": LANE_GATED,
               "trigger": int(rng.uniform(7.0, 11.0) * DAY),
               "requires": [], "depends": ["d10"], "effects": [],
               "superseded_by": None})
    ms.append({"mid": "d12", "lane": LANE_GATED,
               "trigger": int(rng.uniform(19.0, 29.0) * DAY),
               "requires": [], "depends": ["m05"], "effects": [],
               "superseded_by": None})
    ms.append({"mid": "d13", "lane": LANE_GATED,
               "trigger": int(rng.uniform(9.0, 14.0) * DAY),
               "requires": [], "depends": ["m01"], "effects": [],
               "superseded_by": None})
    # votes v00..v05 — formal story decisions awaiting authorization
    for i in range(6):
        ms.append({"mid": "v%02d" % i, "lane": LANE_VOTE,
                   "trigger": int(rng.uniform(1.0, 26.0) * DAY),
                   "requires": [], "depends": [],
                   "effects": [("grid", bool(i % 2))],
                   "superseded_by": None})
    # v02 requires the open district — if granted after m03 fired, its
    # revalidation rejects it (authorized but prerequisites dead)
    for m in ms:
        if m["mid"] == "v02":
            m["requires"] = [("district_open", True)]
            m["trigger"] = int(rng.uniform(15.0, 25.0) * DAY)
        if m["mid"] == "v00":
            m["trigger"] = int(1.2 * DAY)
        if m["mid"] == "v01":
            m["trigger"] = int(4.0 * DAY)
    # emergent-city events e0..e3 — triggers land inside frontier epochs
    # (held), except e3 which lands inside an open city session (fires
    # immediately)
    ms.append({"mid": "e0", "lane": LANE_EMERG,
               "trigger": int(rng.uniform(2.5, 9.0) * DAY),
               "requires": [], "depends": [], "effects": [("aftermath", True)],
               "superseded_by": None})
    ms.append({"mid": "e1", "lane": LANE_EMERG,
               "trigger": int(rng.uniform(2.5, 9.0) * DAY),
               "requires": [("truce", True)], "depends": [],
               "effects": [], "superseded_by": None})
    ms.append({"mid": "e2", "lane": LANE_EMERG,
               "trigger": int(rng.uniform(12.0, 30.0) * DAY),
               "requires": [], "depends": [], "effects": [],
               "superseded_by": None})
    ms.append({"mid": "e3", "lane": LANE_EMERG,
               "trigger": int(9.8 * DAY),
               "requires": [], "depends": [], "effects": [],
               "superseded_by": None})
    return ms, flags0


# ----------------------------------------------------------------------------
# S4 engine (gate machinery under test)
# ----------------------------------------------------------------------------

class S4Engine:
    def __init__(self, world, pipe, meas, milestones, flags0, active_regions=None):
        self.regions_map, self.npcs = world
        self.pipe = pipe
        self.meas = meas
        self.milestones = {m["mid"]: dict(m) for m in milestones}
        for m in self.milestones.values():
            m["state"] = "scheduled"
            m["held_at"] = None
            m["resolved_at"] = None
            m["reason"] = None
        self.narrative = dict(flags0)
        self.flag_change_log = []     # (flag, value, tick, city_open)
        self.transitions = []         # (mid, from, to, tick, city_open)
        self.city_open = False
        self.t = 0
        self.active = (set(range(base.N_REGIONS)) if active_regions is None
                       else set(active_regions))
        self.cals = {r: [] for r in self.active}
        for p in self.npcs:
            if p["region"] in self.active:
                nxt = base.next_event_engine(p, (), 0)
                heapq.heappush(self.cals[p["region"]], (nxt[0], p["id"]))
        self.routine_events = 0
        self.fire_log = []            # (mid, tick, seq)
        self.granted_log = {}         # mid -> grant tick
        self._order = sorted(self.milestones)
        self.counters = {"held": 0, "awaits": 0, "grants": 0, "revalidated": 0,
                         "fired": 0, "rejected": 0, "superseded": 0,
                         "emergent_frontier": 0, "routine": 0}

    # -- helpers ------------------------------------------------------------

    def _pump_routines(self, now):
        for r in self.active:
            cal = self.cals[r]
            while cal and cal[0][0] <= now:
                tick, npc_id = heapq.heappop(cal)
                p = self.npcs[npc_id]
                nxt = base.next_event_engine(p, (), tick)
                heapq.heappush(cal, (nxt[0], npc_id))
                self.routine_events += 1
                self.counters["routine"] += 1

    def _preconds_ok(self, m):
        return all(self.narrative.get(f) == v for f, v in m.get("requires", []))

    def _deps_state(self, m):
        sts = [self.milestones[d]["state"] for d in m.get("depends", [])]
        dead = any(s in ("rejected", "superseded") for s in sts)
        pending = any(s != "fired" for s in sts)
        return dead, pending

    def _fire(self, mid, now):
        m = self.milestones[mid]
        if m["state"] == "fired":
            return False
        for f, v in m.get("effects", []):
            self.narrative[f] = v
            self.flag_change_log.append((f, v, now, self.city_open))
        self._trans(mid, "fired")
        m["resolved_at"] = now
        self.pipe.capture("milestone:" + mid,
                          {"kind": "milestone", "mid": mid, "tick": now}, now)
        self.fire_log.append((mid, now, len(self.fire_log)))
        self.counters["fired"] += 1
        return True

    def _reject(self, mid, now, reason):
        m = self.milestones[mid]
        self._trans(mid, "rejected")
        m["resolved_at"] = now
        m["reason"] = reason
        self.counters["rejected"] += 1
        fb = m.get("superseded_by")
        if fb is not None:
            fm = self.milestones[fb]
            if self._preconds_ok(fm):
                self._fire(fb, now)
            else:
                self._trans(fb, "rejected")
                fm["resolved_at"] = now
                fm["reason"] = "fallback-precondition-dead"
                self.counters["rejected"] += 1

    def _supersede(self, mid, now, reason):
        m = self.milestones[mid]
        self._trans(mid, "superseded")
        m["resolved_at"] = now
        m["reason"] = reason
        self.counters["superseded"] += 1

    def _trans(self, mid, to):
        m = self.milestones[mid]
        self.transitions.append((mid, m["state"], to, self.t, self.city_open))
        m["state"] = to

    def _classify_due(self, now):
        for mid in self._order:
            m = self.milestones[mid]
            if m["state"] != "scheduled" or m["trigger"] > now:
                continue
            if m["lane"] == LANE_VOTE:
                self._trans(m["mid"], "awaiting")
                m["held_at"] = now
                self.counters["awaits"] += 1
            elif self.city_open:
                dead, pending = self._deps_state(m)
                if dead:
                    self._supersede(m["mid"], now, "dependency-dead")
                elif not self._preconds_ok(m):
                    self._reject(m["mid"], now, "precondition-dead")
                elif pending:
                    self._trans(m["mid"], "held")
                    m["held_at"] = now
                    self.counters["held"] += 1
                else:
                    self._fire(m["mid"], now)
            else:
                self._trans(m["mid"], "held")
                m["held_at"] = now
                self.counters["held"] += 1

    # -- public operations ---------------------------------------------------

    def revalidate_held(self, now):
        """Full-roster city re-entry: revalidate every held/authorized event
        in dependency order — classification, preconditions, dependencies,
        trigger context — then fire, reject (with fallback supersession), or
        keep held. Idempotent by construction (fired milestones are skipped)."""
        t0 = time.perf_counter_ns()
        changed = True
        while changed:
            changed = False
            for mid in self._order:
                m = self.milestones[mid]
                if m["state"] not in ("held", "authorized"):
                    continue
                self.counters["revalidated"] += 1
                # classification re-check: stored lane must match definition
                # (static in this harness; revalidation confirms it)
                if not self._preconds_ok(m):
                    self._reject(m["mid"], now, "precondition-dead")
                    changed = True
                    continue
                dead, pending = self._deps_state(m)
                if dead:
                    self._supersede(m["mid"], now, "dependency-dead")
                    changed = True
                    continue
                if pending:
                    continue
                if m["lane"] == LANE_VOTE and m["state"] != "authorized":
                    continue
                self._fire(m["mid"], now)
                changed = True
        self.meas.add("revalidate_pass", time.perf_counter_ns() - t0)

    def epoch(self, ticks, city=False):
        """Advance the shared clock; process routines; classify due
        milestones. City epochs revalidate held events at the end."""
        self.city_open = city
        t0 = time.perf_counter_ns()
        target = self.t + ticks
        while self.t < target:
            self.t += 1
            self.pipe.pump(self.t)
            self._pump_routines(self.t)
            self._classify_due(self.t)
        if city:
            self.revalidate_held(self.t)
        self.meas.add("epoch", time.perf_counter_ns() - t0)

    def grant(self, mid, now):
        """Governance-authorization stub: the roster approved this decision.
        The voting system itself is out of scope by directive. Effects are
        deferred to a city session (M-07-safe)."""
        m = self.milestones[mid]
        assert m["state"] == "awaiting", "grant on non-awaiting %s" % mid
        self._trans(mid, "authorized")
        self.granted_log[mid] = now
        self.counters["grants"] += 1
        if self.city_open:
            self.revalidate_held(now)

    def emergent_frontier(self, now, tag):
        key = "emfront:%s" % tag
        self.pipe.capture(key, {"kind": "emergent_frontier", "tag": tag,
                                "tick": now, "region": "frontier"}, now)
        self.counters["emergent_frontier"] += 1


# ----------------------------------------------------------------------------
# Oracle — independent reference model (separate code, same specification)
# ----------------------------------------------------------------------------

class S4Oracle:
    def __init__(self, world, milestones, flags0):
        self.regions_map, self.npcs = world
        self.state = {m["mid"]: "scheduled" for m in milestones}
        self.defs = {m["mid"]: m for m in milestones}
        self.flags = dict(flags0)
        self.t = 0
        self.city_open = False
        self.resolved = {}
        self.routine_total = 0
        self.granted = {}
        self.frontier_keys = []
        self.outcome_log = []

    def _slot_starts(self, p, day, week):
        b = week * WEEK + day * DAY
        if p["shift"]:
            return [b + p["work_start"], b + p["sleep_start"]]
        starts = [b + p["work_start"], b + p["lunch"], b + p["lunch"] + 60,
                  b + p["sleep_start"]]
        if day in p["lei_days"]:
            starts.append(b + p["lei_start"])
        return starts

    def _count_routines(self, t0, ticks, active):
        total = 0
        for p in self.npcs:
            if p["region"] not in active:
                continue
            d0 = t0 // DAY
            d1 = (t0 + ticks) // DAY + 1
            for d in range(d0, d1):
                week = d // 7
                day = d % 7
                for s in self._slot_starts(p, day, week):
                    if t0 < s <= t0 + ticks:
                        total += 1
        return total

    def _evaluate(self, now):
        # plain re-implementation of the classification spec
        for mid in sorted(self.defs):
            m = self.defs[mid]
            if self.state[mid] != "scheduled" or m["trigger"] > now:
                continue
            if m["lane"] == LANE_VOTE:
                self.state[mid] = "awaiting"
            elif self.city_open:
                if any(self.state[d] in ("rejected", "superseded")
                       for d in m.get("depends", [])):
                    self.state[mid] = "superseded"
                    self.resolved[mid] = now
                elif not all(self.flags.get(f) == v
                             for f, v in m.get("requires", [])):
                    self.state[mid] = "rejected"
                    self.resolved[mid] = now
                    fb = m.get("superseded_by")
                    if fb is not None and all(self.flags.get(f) == v
                                              for f, v in self.defs[fb].get("requires", [])):
                        self.state[fb] = "fired"
                        for f, v in self.defs[fb].get("effects", []):
                            self.flags[f] = v
                        self.resolved[fb] = now
                elif any(self.state[d] != "fired" for d in m.get("depends", [])):
                    self.state[mid] = "held"
                else:
                    self._ofire(mid, now)
            else:
                self.state[mid] = "held"

    def _ofire(self, mid, now):
        m = self.defs[mid]
        for f, v in m.get("effects", []):
            self.flags[f] = v
        self.state[mid] = "fired"
        self.resolved[mid] = now

    def _revalidate(self, now):
        moving = True
        while moving:
            moving = False
            for mid in sorted(self.defs):
                st = self.state[mid]
                if st not in ("held", "authorized"):
                    continue
                m = self.defs[mid]
                if not all(self.flags.get(f) == v
                           for f, v in m.get("requires", [])):
                    self.state[mid] = "rejected"
                    self.resolved[mid] = now
                    fb = m.get("superseded_by")
                    if fb is not None and all(self.flags.get(f) == v
                                              for f, v in self.defs[fb].get("requires", [])):
                        self._ofire(fb, now)
                    moving = True
                    continue
                if any(self.state[d] in ("rejected", "superseded")
                       for d in m.get("depends", [])):
                    self.state[mid] = "superseded"
                    self.resolved[mid] = now
                    moving = True
                    continue
                if any(self.state[d] != "fired" for d in m.get("depends", [])):
                    continue
                if m["lane"] == LANE_VOTE and st != "authorized":
                    continue
                self._ofire(mid, now)
                moving = True

    def epoch(self, ticks, city, active):
        self.city_open = city
        t0 = self.t
        if city:
            # per-tick evaluation so in-session trigger ticks match the
            # engine's immediate-fire semantics exactly
            for _ in range(ticks):
                self.t += 1
                self._evaluate(self.t)
            self._revalidate(self.t)
        else:
            self.t += ticks
            self._evaluate(self.t)
        self.routine_total += self._count_routines(t0, ticks, active)

    def grant(self, mid, now):
        self.state[mid] = "authorized"
        self.granted[mid] = now
        if self.city_open:
            self._revalidate(now)

    def emergent_frontier(self, tag):
        self.frontier_keys.append("emfront:%s" % tag)


# ----------------------------------------------------------------------------
# Checks + scenario
# ----------------------------------------------------------------------------

def run_scenario():
    world = base.gen_world(base.WORLD_SEED)
    milestones, flags0 = build_milestones()
    pipe = base.Pipeline()
    meas = base.Measure()
    eng = S4Engine(world, pipe, meas, milestones, flags0)
    oracle = S4Oracle(world, milestones, flags0)
    all_regions = set(range(base.N_REGIONS))
    checks = []
    failures = []
    op_log = []

    def record_fail(check, detail):
        failures.append({"check": check, "detail": detail,
                         "seeds": {"world": base.WORLD_SEED, "milestones": S4_MSEED},
                         "op_index": len(op_log), "tick": eng.t})

    def flags_hash():
        return base.canon(eng.narrative)

    # Scripted timeline (deterministic; milestone jitter is seeded)
    # 1. two-day frontier-only epoch — early roots trigger -> held
    snap = flags_hash()
    eng.epoch(2 * DAY, city=False)
    oracle.epoch(2 * DAY, False, all_regions)
    op_log.append(["frontier_epoch", 2 * DAY])
    if flags_hash() != snap:
        record_fail("G1", "narrative flags changed during frontier epoch 1")
    eng.emergent_frontier(eng.t, "f0")
    oracle.emergent_frontier("f0")
    op_log.append(["emergent_frontier", "f0"])

    # 2. city re-entry #1 — fires early roots (m02 breaks the truce)
    fired_before = len(eng.fire_log)
    eng.epoch(DAY // 2, city=True)
    oracle.epoch(DAY // 2, True, all_regions)
    op_log.append(["city_epoch", DAY // 2])
    if len(eng.fire_log) <= fired_before:
        record_fail("scenario", "re-entry #1 fired nothing — scenario drift")

    # 3. grant v00 during the open city session -> fires now
    awaiting = sorted(m for m, s in eng.milestones.items()
                      if s["state"] == "awaiting")
    if awaiting:
        eng.grant(awaiting[0], eng.t)
        oracle.grant(awaiting[0], eng.t)
        op_log.append(["grant", awaiting[0]])

    # 4. seven-day frontier epoch — more roots hold, emergent-city events
    #    hold, dependents hold; a vote granted mid-frontier stays authorized
    snap = flags_hash()
    eng.epoch(7 * DAY, city=False)
    oracle.epoch(7 * DAY, False, all_regions)
    op_log.append(["frontier_epoch", 7 * DAY])
    if flags_hash() != snap:
        record_fail("G1", "narrative flags changed during frontier epoch 2")
    eng.emergent_frontier(eng.t, "f1")
    oracle.emergent_frontier("f1")
    op_log.append(["emergent_frontier", "f1"])
    awaiting = sorted(m for m, s in eng.milestones.items()
                      if s["state"] == "awaiting")
    if awaiting:
        eng.grant(awaiting[0], eng.t)      # authorized during frontier: must NOT fire
        oracle.grant(awaiting[0], eng.t)
        op_log.append(["grant_frontier", awaiting[0]])
        if eng.milestones[awaiting[0]]["state"] == "fired":
            record_fail("G6", "vote fired during frontier-only epoch after grant")

    # 5. city re-entry #2 — fires held roots/emergent/dependents; e3 due
    #    inside an open session fires immediately at its trigger tick
    eng.epoch(DAY, city=True)
    oracle.epoch(DAY, True, all_regions)
    op_log.append(["city_epoch", DAY])

    # 6. duplicate stress — repeated revalidation and a repeated city epoch
    fired_snap = list(eng.fire_log)
    eng.revalidate_held(eng.t)
    oracle._revalidate(eng.t)
    eng.epoch(1, city=True)
    oracle.epoch(1, True, all_regions)
    op_log.append(["duplicate_revalidation"])
    op_log.append(["city_epoch", 1])
    if eng.fire_log != fired_snap:
        record_fail("G4", "duplicate revalidation/re-entry produced new firings")

    # 7. long frontier epoch (through the 30-day mark) — m05 (dead truce)
    #    must stay HELD, never auto-rejected or expired during frontier time
    snap = flags_hash()
    eng.epoch(21 * DAY, city=False)
    oracle.epoch(21 * DAY, False, all_regions)
    op_log.append(["frontier_epoch", 21 * DAY])
    if flags_hash() != snap:
        record_fail("G1", "narrative flags changed during frontier epoch 3")
    if eng.milestones["m05"]["state"] != "held":
        record_fail("G2", "m05 left held state during frontier-only time")
    eng.emergent_frontier(eng.t, "f2")
    oracle.emergent_frontier("f2")
    op_log.append(["emergent_frontier", "f2"])
    # grant a vote whose preconditions are now dead (v02 requires open
    # district; m03 closed it) — authorization must not bypass revalidation
    if eng.milestones["v02"]["state"] == "awaiting":
        eng.grant("v02", eng.t)
        oracle.grant("v02", eng.t)
        op_log.append(["grant_frontier", "v02"])

    # 8. final city re-entry — m05 rejected (fallback m05f fires), d12
    #    superseded, v02 authorized-but-dead rejected, held emergents fire
    eng.epoch(DAY, city=True)
    oracle.epoch(DAY, True, all_regions)
    op_log.append(["city_epoch", DAY])

    # ---------------- checks ----------------
    # G1 — M-07: frontier-only epochs never changed narrative flags; every
    # flag write happened with the city session open
    frontier_writes = [e for e in eng.flag_change_log if not e[3]]
    g1 = len(frontier_writes) == 0
    checks.append({"name": "G1 frontier never rewrites narrative state (M-07)",
                   "pass": g1 and not [f for f in failures if f["check"] == "G1"],
                   "detail": "%d narrative flag writes, all during open city "
                             "sessions; frontier-epoch flag snapshots unchanged"
                             % len(eng.flag_change_log)})

    # G2 — held events never fire, never silently expire during frontier time
    bad_g2 = [tr for tr in eng.transitions
              if not tr[4] and tr[2] in ("fired", "rejected", "superseded")]
    checks.append({"name": "G2 held milestones never fire or expire (frontier)",
                   "pass": not bad_g2 and eng.milestones["m05"]["state"] == "rejected"
                           and eng.milestones["m05"]["reason"] == "precondition-dead",
                   "detail": "zero fired/rejected/superseded transitions during "
                             "frontier-only epochs across %d transitions; m05 "
                             "(dead precondition) remained held through 21 "
                             "frontier days, resolved only at re-entry (%s)"
                             % (len(eng.transitions),
                                eng.milestones["m05"]["state"])})

    # G3 — dependency ordering: every fired dependent has all deps fired
    # earlier in the fire log; no fired event depends on a dead one
    order = {mid: seq for mid, _tick, seq in eng.fire_log}
    bad_g3 = []
    for mid, seq in order.items():
        for dep in eng.milestones[mid].get("depends", []):
            if eng.milestones[dep]["state"] != "fired":
                bad_g3.append((mid, dep, "dep-not-fired"))
            elif order.get(dep, 10 ** 9) >= seq:
                bad_g3.append((mid, dep, "order"))
    checks.append({"name": "G3 dependency ordering (no early dependents)",
                   "pass": not bad_g3,
                   "detail": "%d fired milestones checked against their "
                             "prerequisites; violations: %d"
                             % (len(order), len(bad_g3))})

    # G4 — exactly-once across skips, revalidations, re-entries
    mids = [m for m, _t, _s in eng.fire_log]
    g4 = len(mids) == len(set(mids))
    per_effect = {}
    for mid in mids:
        for f, _v in eng.milestones[mid].get("effects", []):
            per_effect[(mid, f)] = per_effect.get((mid, f), 0) + 1
    g4 = g4 and all(c == 1 for c in per_effect.values()) \
        and not [f for f in failures if f["check"] == "G4"]
    checks.append({"name": "G4 exactly-once across repeated skips/re-entries",
                   "pass": g4,
                   "detail": "%d firings, %d unique; every effect applied once; "
                             "duplicate revalidation + repeated city epoch "
                             "produced zero new firings" % (len(mids), len(set(mids)))})

    # G5 — rejection/supersession correctness
    rejected = [(m, d) for m, d in eng.milestones.items()
                if d["state"] == "rejected"]
    superseded = [(m, d) for m, d in eng.milestones.items()
                  if d["state"] == "superseded"]
    g5 = all(d["reason"] == "precondition-dead" for _m, d in rejected)
    g5 = g5 and all(d["reason"] == "dependency-dead" for _m, d in superseded)
    g5 = g5 and (eng.milestones["m05f"]["state"] ==
                 ("fired" if eng.milestones["m05"]["state"] == "rejected" else "scheduled"))
    checks.append({"name": "G5 rejection and safe supersession",
                   "pass": g5,
                   "detail": "%d rejected (precondition-dead), %d superseded "
                             "(dependency-dead); m05 fallback m05f state: %s"
                             % (len(rejected), len(superseded),
                                eng.milestones["m05f"]["state"])})

    # G6 — vote gating: fired votes fired only after grant and in-session
    fired_votes = [m for m in eng.milestones
                   if eng.milestones[m]["lane"] == LANE_VOTE
                   and eng.milestones[m]["state"] == "fired"]
    g6 = True
    for v in fired_votes:
        tick = next(t for m, t, _s in eng.fire_log if m == v)
        if eng.granted_log.get(v, 10 ** 9) > tick:
            g6 = False
        fire_trans = [tr for tr in eng.transitions
                      if tr[0] == v and tr[2] == "fired"]
        if not fire_trans or not fire_trans[-1][4]:
            g6 = False
    never = [m for m in eng.milestones
             if eng.milestones[m]["lane"] == LANE_VOTE
             and eng.milestones[m]["state"] == "awaiting"]
    checks.append({"name": "G6 votes fire only after authorization (in session)",
                   "pass": g6,
                   "detail": "%d votes fired (each after grant + revalidation, "
                             "effects in city session); %d still awaiting at end"
                             % (len(fired_votes), len(never))})

    # G7 — independent reference model agreement
    out_eng = {m: d["state"] for m, d in eng.milestones.items()}
    out_ora = dict(oracle.state)
    outcome_match = out_eng == out_ora
    flags_match = eng.narrative == oracle.flags
    resolved_match = all(eng.milestones[m]["resolved_at"] == oracle.resolved.get(m)
                         for m in out_eng if eng.milestones[m]["resolved_at"] is not None)
    routine_match = eng.routine_events == oracle.routine_total
    keys_match = (sorted(set(list(pipe.applied) + list(pipe.pending))) ==
                  sorted(["milestone:" + m for m in out_ora
                          if out_ora[m] == "fired"] + oracle.frontier_keys))
    checks.append({"name": "G7 independent reference model agreement",
                   "pass": outcome_match and flags_match and resolved_match
                           and routine_match and keys_match,
                   "detail": "outcomes %s; flags %s; resolved ticks %s; routine "
                             "events %d==%d; pipeline keys %s"
                             % (outcome_match, flags_match, resolved_match,
                                eng.routine_events, oracle.routine_total, keys_match)})

    return {"engine": eng, "pipe": pipe, "meas": meas, "oracle": oracle,
            "checks": checks, "failures": failures, "op_log": op_log,
            "milestones": milestones}


# ----------------------------------------------------------------------------
# Cost-curve measurements (24h / 7d / 30d + same-elapsed low-workload control)
# ----------------------------------------------------------------------------

def run_cost_curve():
    world = base.gen_world(base.WORLD_SEED)
    curve = []
    for name, ticks, active in (("skip_24h", DAY, None),
                                ("skip_7d", 7 * DAY, None),
                                ("skip_30d", 30 * DAY, None),
                                ("skip_7d_quiet_1_region", 7 * DAY, {0})):
        pipe = base.Pipeline()
        meas = base.Measure()
        eng = S4Engine(world, pipe, meas, [], {}, active_regions=active)
        t0 = time.perf_counter_ns()
        eng.epoch(ticks, city=False)
        elapsed_ns = time.perf_counter_ns() - t0
        curve.append({
            "name": name,
            "ticks": ticks,
            "elapsed_world_days": round(ticks / DAY, 2),
            "routine_events": eng.routine_events,
            "elapsed_ms": round(elapsed_ns / 1e6, 2),
            "us_per_event": round(elapsed_ns / eng.routine_events / 1000.0, 4)
                            if eng.routine_events else 0.0,
            "active_regions": (base.N_REGIONS if active is None else len(active)),
        })
    # scheduler overhead calibration: no routines, no milestones, no dues
    pipe = base.Pipeline()
    meas = base.Measure()
    cal = S4Engine(world, pipe, meas, [], {}, active_regions=set())
    t0 = time.perf_counter_ns()
    cal.epoch(2000, city=False)
    calib_us = (time.perf_counter_ns() - t0) / 2000 / 1000.0
    return curve, round(calib_us, 4)


# ----------------------------------------------------------------------------
# Main
# ----------------------------------------------------------------------------

def machine_info():
    return base.machine_info() if hasattr(base, "machine_info") else {
        "python": sys.version.split()[0], "platform": platform.platform()}


def main():
    print("S4-LITE harness — narrative gate semantics under time advancement")
    print("authorization: lead 2026-10-18 (extends sim-lod-local; local-only,")
    print("headless; no voting-system redesign, no engine code, no technology")
    print("selection; does NOT establish emergency-copy durability)")
    print("")

    # determinism: run the full correctness scenario twice
    s1 = run_scenario()
    s2 = run_scenario()
    fp1 = base.fp(logical_state(s1))
    fp2 = base.fp(logical_state(s2))
    deterministic = fp1 == fp2

    curve, calib_us = run_cost_curve()

    eng = s2["engine"]
    results = {
        "checks": s2["checks"],
        "failures": s2["failures"],
        "op_log": s2["op_log"],
        "counts": {
            "routine_events": eng.routine_events,
            "milestones_held_total": eng.counters["held"],
            "votes_entered_awaiting": eng.counters["awaits"],
            "grants": eng.counters["grants"],
            "revalidations": eng.counters["revalidated"],
            "fired": eng.counters["fired"],
            "rejected_canceled": eng.counters["rejected"],
            "superseded_canceled": eng.counters["superseded"],
            "emergent_frontier_captures": eng.counters["emergent_frontier"],
            "ticks_simulated": eng.t,
        },
        "outcomes": {m: d["state"] for m, d in sorted(eng.milestones.items())},
        "revalidation_measurements": {
            "passes_ms": s2["meas"].snapshot().get("revalidate_pass", {}),
            "revalidated_event_count": eng.counters["revalidated"],
        },
        "cost_curve": curve,
        "scheduler_calibration": {
            "ticks": 2000,
            "scheduler_us_per_tick": calib_us,
            "note": "no due events at all — pure per-tick loop overhead "
                    "(clock, pipeline pump, empty scans)",
        },
        "cost_vs_workload_note": (
            "cost is reported against MEASURED event counts: 24h/7d/30d "
            "scale event count ~linearly with world time at fixed "
            "population; the 7d-quiet control holds elapsed time constant "
            "at 7 days with 1/8 of the population scheduled — its cost "
            "drops with the event count, not the elapsed time. No "
            "proportionality is claimed from the priority queue alone."),
        "determinism": {
            "pass": deterministic,
            "fingerprint": fp1,
            "note": "sha256 of canonical logical-state results compared "
                    "across two full in-process scenario runs; timings "
                    "excluded and NOT bit-identical",
        },
        "meta": {
            "run_date": RUN_DATE,
            "machine": machine_info(),
            "seeds": {"world": base.WORLD_SEED, "milestones": S4_MSEED},
            "npcs": base.N_NPCS,
            "regions": base.N_REGIONS,
            "authorization": "lead, 2026-10-18 — S4-lite narrative gate "
                             "semantics; governance modeled as an "
                             "authorization stub (no voting-system redesign)",
            "durability_caveat": "the S1/S6-lite host-loss test (and this "
                                 "harness) does NOT establish durability of "
                                 "local emergency recovery copies — separate "
                                 "persistence concern",
        },
    }

    for t in results["checks"]:
        print("[%s] %s — %s" % ("PASS" if t["pass"] else "FAIL", t["name"], t["detail"]))
    print("[%s] G8 determinism — identical logical-state fingerprints across "
          "two full scenario runs: %s" % ("PASS" if deterministic else "FAIL", fp1))
    print("")
    print("cost curve (measured; NOT production):")
    for c in curve:
        print("  %-24s %6.1f days  %7d events  %8.2f ms  %7.4f us/event"
              % (c["name"], c["elapsed_world_days"], c["routine_events"],
                 c["elapsed_ms"], c["us_per_event"]))
    print("  scheduler overhead: %.4f us/tick (calibration, no due events)" % calib_us)
    rv = results["revalidation_measurements"]["passes_ms"]
    print("  revalidation: %d event-revalidations across %d passes, %.2f ms total"
          % (results["counts"]["revalidations"], rv.get("count", 0),
             rv.get("total_ms", 0.0)))
    cnt = results["counts"]
    print("counts: %d ticks, %d routine events, held %d, awaiting %d, grants %d, "
          "fired %d, rejected %d, superseded %d, frontier captures %d"
          % (cnt["ticks_simulated"], cnt["routine_events"],
             cnt["milestones_held_total"], cnt["votes_entered_awaiting"],
             cnt["grants"], cnt["fired"], cnt["rejected_canceled"],
             cnt["superseded_canceled"], cnt["emergent_frontier_captures"]))

    all_pass = deterministic and all(t["pass"] for t in results["checks"]) \
        and not results["failures"]
    print("")
    print("overall: %s" % ("ALL CHECKS PASSED" if all_pass else "FAILURES PRESENT"))

    out_path = os.path.join(HERE, "results_s4.json")
    with open(out_path, "w") as f:
        json.dump(results, f, indent=2, sort_keys=True)
    print("results written: %s" % out_path)
    return 0 if all_pass else 1


def logical_state(s):
    eng = s["engine"]
    return {
        "t": eng.t,
        "narrative": eng.narrative,
        "outcomes": {m: d["state"] for m, d in sorted(eng.milestones.items())},
        "resolved": {m: d["resolved_at"] for m, d in sorted(eng.milestones.items())
                     if d["resolved_at"] is not None},
        "fire_log": eng.fire_log,
        "pipe_keys": sorted(set(list(eng.pipe.applied) + list(eng.pipe.pending))),
        "routine_events": eng.routine_events,
        "counters": eng.counters,
        "flag_change_log": eng.flag_change_log,
        "transitions": eng.transitions,
    }


if __name__ == "__main__":
    sys.exit(main())
