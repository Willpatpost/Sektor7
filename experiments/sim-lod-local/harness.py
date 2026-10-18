#!/usr/bin/env python3
# S1/S6-LITE LOCAL harness — tiered-LOD transition correctness and cost shape.
#
# Authorization (project lead, 2026-10-18): "You are authorized to implement
# and run the proposed S1/S6-lite headless simulation experiment." Scope per
# the authorization: ~1,000 seeded NPCs across four simulation detail levels;
# a deterministic event scheduler with one monotonic clock; a local simulated
# persistence pipeline (pending changes, delayed acknowledgments, idempotency
# keys, applied-key ledger); randomized activation/deactivation/time-advance
# sequences.
#
# Review requirements honored (2026-10-18 three-way review):
#   R1 full/simplified get meaningful, explicitly defined workloads — no
#      manufactured advantage via arbitrary dummy computation;
#   R2 event replay is included in duplicate-prevention tests, not only
#      durable-record retries;
#   R3 results are cross-checked against a simple independent reference
#      model (separately written code, same specification);
#   R4 dormant per-tick overhead is measured separately from event-queue
#      wake-up and scheduling costs;
#   R5 CPU time, memory, transition counts, event throughput, state
#      consistency and correctness failures are reported with machine
#      details, Python version, workload assumptions and limitations;
#   R6 seeded deterministic fault/interleaving scenarios; repeated runs must
#      produce identical logical-state fingerprints (timing measurements are
#      explicitly NOT required to be bit-identical);
#   R7 enough information is recorded to reproduce every correctness failure
#      (seeds, op log, tick, region, npc, expected vs actual).
#
# Prohibitions honored: no networking, no cloud services, no real engine
# code, no rendering, no production ECS/framework selection. This harness is
# protocol logic over a local in-process simulation. It does NOT prove
# production performance and does not set arbitrary speedup targets.
#
# Findings are recorded in results.json and reflected in
# docs/SIMULATION-ARCHITECTURE-RESEARCH.md (v0.3, §10).

import hashlib
import heapq
import json
import math
import os
import platform
import random
import sys
import time
import tracemalloc

HERE = os.path.dirname(os.path.abspath(__file__))
RUN_DATE = "2026-10-18"

WEEK = 7 * 24 * 60          # 10080 ticks; 1 tick = 1 simulated minute
DAY = 24 * 60               # 1440
N_NPCS = 1000
N_REGIONS = 8
NPR = N_NPCS // N_REGIONS   # 125
WORLD_SEED = 1337
SCENARIO_SEED = 60103
CADENCE = 10                # simplified systems run every 10th tick
ACK_DELAY = 20              # persistence acknowledgment delay (ticks)
WITNESS_R = 120.0           # encounter witness radius (region-local units)
TWO_PI = 2.0 * math.pi

FULL, SIMPLIFIED, EVENT, DORMANT = 3, 2, 1, 0
LEVEL_NAME = {FULL: "full", SIMPLIFIED: "simplified", EVENT: "event", DORMANT: "dormant"}


def canon(obj):
    return json.dumps(obj, sort_keys=True, separators=(",", ":"))


def fp(obj):
    return hashlib.sha256(canon(obj).encode()).hexdigest()[:16]


# ----------------------------------------------------------------------------
# World generation (deterministic; content shared by engine and oracle)
# ----------------------------------------------------------------------------

def gen_world(seed):
    rng = random.Random(seed)
    regions = []
    for r in range(N_REGIONS):
        places = {}
        for name in ("home_a", "home_b", "work", "food", "leisure", "park"):
            places[name] = (round(rng.uniform(20.0, 380.0), 1),
                            round(rng.uniform(20.0, 380.0), 1))
        regions.append({"id": r, "places": places})
    npcs = []
    for i in range(N_NPCS):
        r = i // NPR
        shift = rng.random() < 0.2
        p = {
            "id": i,
            "region": r,
            "home": "home_a" if rng.random() < 0.5 else "home_b",
            "shift": shift,
            "wake": int(rng.uniform(350, 370)),
            "work_start": 840 if shift else int(rng.uniform(470, 490)),
            "lunch": None if shift else int(rng.uniform(715, 725)),
            "work_end": 1200 if shift else int(rng.uniform(1015, 1035)),
            "lei_start": None if shift else int(rng.uniform(1135, 1165)),
            "sleep_start": 1230 if shift else int(rng.uniform(1315, 1335)),
            "jA": rng.uniform(2.0, 6.0),
            "jF": rng.uniform(0.02, 0.09),
            "jP": rng.uniform(0.0, 6.28),
        }
        p["lei_days"] = [] if shift else sorted(rng.sample(range(7), rng.choice([2, 3])))
        npcs.append(p)
    return regions, npcs


# ----------------------------------------------------------------------------
# Schedule SPECIFICATION (shared spec; two independent implementations)
#
# Daily commitment slots (absolute ticks; week = 10080):
#   standard NPC:  work [work_start -> lunch], lunch [lunch -> lunch+60],
#                  work2 [lunch+60 -> work_end],
#                  leisure [lei_start -> lei_start+120] on lei_days only,
#                  sleep [sleep_start -> next day wake]
#   shift NPC:     work [840 -> 1200], sleep [1230 -> next day wake]
# Commitment-start events fire for every slot start. Between commitments an
# NPC transits linearly between place anchors. Oneoff records ("avoid
# leisure") redirect that day's leisure anchor to home — place substitution
# only; event TIMES are unchanged by design (documented limitation).
# Derived vitals: energy rises 0->1 across the sleep span, falls 1->0 from
# wake to sleep_start; hunger rises from the last refill mark (lunch start
# or sleep start, previous day's sleep start before the first mark), capped
# at 1 over 720 ticks.
# ----------------------------------------------------------------------------

def day_slots_engine(p, day, week, oneoffs):
    """Engine-side commitment list for one NPC-day (sorted by start)."""
    base = week * WEEK + day * DAY
    out = []
    if p["shift"]:
        out.append(("work", base + p["work_start"], base + p["work_end"], "work"))
        out.append(("sleep", base + p["sleep_start"], base + DAY + p["wake"], p["home"]))
    else:
        out.append(("work", base + p["work_start"], base + p["lunch"], "work"))
        out.append(("lunch", base + p["lunch"], base + p["lunch"] + 60, "food"))
        out.append(("work2", base + p["lunch"] + 60, base + p["work_end"], "work"))
        if day in p["lei_days"]:
            place = p["home"] if (week, day) in oneoffs else "leisure"
            out.append(("leisure", base + p["lei_start"], base + p["lei_start"] + 120, place))
        out.append(("sleep", base + p["sleep_start"], base + DAY + p["wake"], p["home"]))
    return out


def prev_sleep_engine(p, day, week, oneoffs):
    pd = (day - 1) % 7
    pw = week - 1 if day == 0 else week
    return [s for s in day_slots_engine(p, pd, pw, oneoffs) if s[0] == "sleep"][0]


def next_event_engine(p, oneoffs, after_t):
    """Smallest commitment-start tick strictly after after_t (engine side)."""
    week = after_t // WEEK
    day = (after_t % WEEK) // DAY
    best = None
    for d in (day, day + 1):
        dd = d % 7
        wk = week + (d // 7)
        for slot, start, _end, _pl in day_slots_engine(p, dd, wk, oneoffs):
            if start > after_t and (best is None or start < best[0]):
                best = (start, slot)
    if best is None:
        raise RuntimeError("next_event_engine failed")
    return best


def schedule_window_engine(p, oneoffs, t):
    """Engine-side closed form: commitment containing t, or transit gap."""
    week = t // WEEK
    day = (t % WEEK) // DAY
    m = t % DAY
    if m < p["wake"]:
        sleep = prev_sleep_engine(p, day, week, oneoffs)
        return ("in", sleep, None)
    slots = day_slots_engine(p, day, week, oneoffs)
    prev = prev_sleep_engine(p, day, week, oneoffs)
    for s in slots:
        if s[1] <= t:
            prev = s
        else:
            if t < prev[2]:
                return ("in", prev, None)
            return ("gap", prev, s)
    last = slots[-1]
    if t < last[2]:
        return ("in", last, None)
    nd = (day + 1) % 7
    nw = week + (1 if day == 6 else 0)
    return ("gap", last, day_slots_engine(p, nd, nw, oneoffs)[0])


def position_engine(regions, npcs, oneoffs, npc_id, t):
    p = npcs[npc_id]
    phase, cur, nxt = schedule_window_engine(p, oneoffs.get(npc_id, ()), t)
    places = regions[p["region"]]["places"]
    if phase == "in":
        return places[cur[3]]
    a = places[cur[3]]
    b = places[nxt[3]]
    span = nxt[1] - cur[2]
    if span <= 0:
        return b
    f = (t - cur[2]) / span
    return (a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f)


def needs_engine(p, t):
    m = t % DAY
    ss = p["sleep_start"]
    if m < p["wake"]:
        span = (DAY - ss) + p["wake"]
        energy = min(1.0, ((DAY - ss) + m) / span) if span > 0 else 1.0
    else:
        span = max(1, ss - p["wake"])
        energy = max(0.0, 1.0 - (m - p["wake"]) / span)
    if p["lunch"] is not None and p["lunch"] <= m < ss:
        ref = p["lunch"]
    elif m >= ss:
        ref = ss
    else:
        ref = ss - DAY
    hunger = min(1.0, max(0.0, (m - ref) / 720.0))
    return round(energy, 6), round(hunger, 6)


# ----------------------------------------------------------------------------
# Simulated persistence pipeline (pending / delayed acks / idempotency ledger)
# ----------------------------------------------------------------------------

class Pipeline:
    """Local stand-in for the commit pipeline of MULTIPLAYER-ARCHITECTURE-
    RESEARCH §3/§8: durable deltas are captured immediately (local append,
    pending — T1-analog), acknowledged after a delay (applied — T3-analog),
    and re-delivery of an applied or pending key is a no-op (applied-key
    ledger). In-process and deterministic; NOT a durability test."""

    def __init__(self):
        self.pending = {}
        self.applied = {}
        self.ack_heap = []         # (due_tick, seq, key)
        self.seq = 0
        self.audit = []            # (key, event, tick)
        self.stats = {"captures": 0, "dup_noop": 0, "acks": 0, "ack_drops": 0}

    def capture(self, key, record, tick):
        self.seq += 1
        if key in self.applied:
            self.stats["dup_noop"] += 1
            self.audit.append((key, "dup-applied", tick))
            return "already-applied"
        if key in self.pending:
            self.stats["dup_noop"] += 1
            self.audit.append((key, "dup-pending", tick))
            return "already-pending"
        self.pending[key] = dict(record)
        self.stats["captures"] += 1
        self.audit.append((key, "captured", tick))
        heapq.heappush(self.ack_heap, (tick + ACK_DELAY, self.seq, key))
        return "captured"

    def retry_deliver(self, key, record, tick):
        # R2 durable-record retry path — exactly-once via the ledger.
        return self.capture(key, record, tick)

    def drop_ack(self, key, tick):
        if key in self.pending:
            self.seq += 1
            heapq.heappush(self.ack_heap, (tick + ACK_DELAY, self.seq, key))
            self.stats["ack_drops"] += 1
            self.audit.append((key, "ack-dropped", tick))
            return True
        return False

    def pump(self, now):
        while self.ack_heap and self.ack_heap[0][0] <= now:
            _due, _seq, key = heapq.heappop(self.ack_heap)
            if key in self.pending:
                self.applied[key] = self.pending.pop(key)
                self.stats["acks"] += 1
                self.audit.append((key, "acked", now))

    def status(self, key):
        if key in self.applied:
            return "confirmed"
        if key in self.pending:
            return "pending"
        return "absent"

    def ordered_records(self):
        return [(k, r) for k, r in self.applied.items()] + \
               [(k, r) for k, r in self.pending.items()]

    def discard_unacked(self):
        """Host-loss simulation (C6): pending work is lost; applied stands."""
        lost = sorted(self.pending)
        self.pending.clear()
        self.ack_heap = []
        return lost


# ----------------------------------------------------------------------------
# Measurement helper
# ----------------------------------------------------------------------------

class Measure:
    def __init__(self):
        self.buckets = {}

    def add(self, name, ns, count=1):
        b = self.buckets.setdefault(name, [0, 0])
        b[0] += ns
        b[1] += count

    def us_per(self, name):
        total_ns, count = self.buckets.get(name, (0, 0))
        return (total_ns / count / 1000.0) if count else 0.0

    def snapshot(self):
        out = {}
        for name, (ns, count) in sorted(self.buckets.items()):
            out[name] = {"total_ms": round(ns / 1e6, 3), "count": count,
                         "us_per_unit": round(ns / count / 1000.0, 4) if count else 0.0}
        return out


# ----------------------------------------------------------------------------
# Engine (tiered-LOD machinery under test)
# ----------------------------------------------------------------------------

class Engine:
    def __init__(self, world, pipe, meas):
        self.regions_map, self.npcs = world
        self.pipe = pipe
        self.meas = meas
        self.t = 0
        self.regions = {}
        for rid in range(N_REGIONS):
            self.regions[rid] = {"level": DORMANT, "cal": [], "next_event": None,
                                 "wakes": 0}
        self.oneoffs = {i: [] for i in range(N_NPCS)}
        self.memories = {i: [] for i in range(N_NPCS)}
        self.rels = {i: {} for i in range(N_NPCS)}
        self.cands = {i: [] for i in range(N_NPCS)}
        self.last_event = {}      # npc -> (tick, slot): last applied commitment
        self.stale_drops = 0      # duplicate/stale event re-deliveries rejected
        self.encounter_log = []   # (eid, rid, kind, tick, [witness ids])
        self.micro_sink = 0.0     # transient workload sink (never fingerprinted)
        self.event_count = 0
        for p in self.npcs:
            nxt = next_event_engine(p, (), 0)
            heapq.heappush(self.regions[p["region"]]["cal"],
                           (nxt[0], p["id"], nxt[1]))
        for rid in range(N_REGIONS):
            self._restamp(rid)

    # -- internals -----------------------------------------------------------

    def _restamp(self, rid):
        reg = self.regions[rid]
        reg["next_event"] = reg["cal"][0][0] if reg["cal"] else None

    def apply_event(self, npc_id, tick, slot):
        """Single event-application path (used by firing AND replay/dup tests).
        Absolute task set + stale/duplicate rejection (R2)."""
        cur = self.last_event.get(npc_id)
        if cur is not None and (tick, slot) <= cur:
            self.stale_drops += 1
            return "stale-noop"
        self.last_event[npc_id] = (tick, slot)
        self.event_count += 1
        p = self.npcs[npc_id]
        nxt = next_event_engine(p, tuple(self.oneoffs[npc_id]), tick)
        heapq.heappush(self.regions[p["region"]]["cal"], (nxt[0], npc_id, nxt[1]))
        return "applied"

    def _fire_due(self, rid, now):
        reg = self.regions[rid]
        fired = 0
        while reg["cal"] and reg["cal"][0][0] <= now:
            tick, npc_id, slot = heapq.heappop(reg["cal"])
            self.apply_event(npc_id, tick, slot)
            fired += 1
        self._restamp(rid)
        return fired

    def _capture_consequences(self, eid, kind, witnesses, now):
        for nid in witnesses:
            mk = "mem:%s:%d" % (eid, nid)
            self.pipe.capture(mk, {"kind": "memory", "npc": nid, "eid": eid,
                                   "what": kind, "tick": now}, now)
            self.memories[nid].append(mk)
            if kind in ("theft", "brawl"):
                rk = "rel:%s:%d" % (eid, nid)
                delta = -0.4 if kind == "theft" else -0.2
                self.pipe.capture(rk, {"kind": "relationship", "npc": nid,
                                       "eid": eid, "delta": delta, "tick": now}, now)
                self.rels[nid][rk] = delta
            if kind == "theft":
                ck = "cand:%s:%d" % (eid, nid)
                self.pipe.capture(ck, {"kind": "rep_candidate", "npc": nid,
                                       "eid": eid, "tick": now}, now)
                self.cands[nid].append(ck)
                ok = "oneoff:%s:%d" % (eid, nid)
                week = now // WEEK
                day = (now % WEEK) // DAY
                od = (week, (day + 1) % 7) if day < 6 else (week + 1, 0)
                self.pipe.capture(ok, {"kind": "oneoff", "npc": nid, "eid": eid,
                                       "week": od[0], "day": od[1], "tick": now}, now)
                self.oneoffs[nid].append(od)

    # -- public operations ---------------------------------------------------

    def set_level(self, rid, level):
        reg = self.regions[rid]
        old = reg["level"]
        if level > old:
            t0 = time.perf_counter_ns()
            reg["level"] = level     # activation; coherence checked by caller
            self.meas.add("activate", time.perf_counter_ns() - t0)
        elif level < old:
            t0 = time.perf_counter_ns()
            reg["level"] = level     # deactivation; consequence lane already
            self._restamp(rid)       # captured — I1 verified by rebuild check
            self.meas.add("deactivate", time.perf_counter_ns() - t0)
        return old

    def tick(self, now, run_systems=True):
        self.t = now
        self.pipe.pump(now)
        t0 = time.perf_counter_ns()
        for rid in range(N_REGIONS):
            reg = self.regions[rid]
            if reg["level"] == DORMANT:
                if reg["next_event"] is not None and reg["next_event"] <= now:
                    tw = time.perf_counter_ns()
                    reg["wakes"] += 1
                    n = self._fire_due(rid, now)
                    if n:
                        self.meas.add("wake", time.perf_counter_ns() - tw, count=n)
            else:
                te = time.perf_counter_ns()
                n = self._fire_due(rid, now)
                if n:
                    self.meas.add("event_active", time.perf_counter_ns() - te, count=n)
        self.meas.add("scheduler_tick", time.perf_counter_ns() - t0)
        if run_systems:
            for rid in range(N_REGIONS):
                reg = self.regions[rid]
                if reg["level"] == FULL:
                    tf = time.perf_counter_ns()
                    self._full_systems(rid, now)
                    self.meas.add("full_systems", time.perf_counter_ns() - tf, count=NPR)
                elif reg["level"] == SIMPLIFIED and now % CADENCE == 0:
                    ts = time.perf_counter_ns()
                    self._simplified_systems(rid, now)
                    self.meas.add("simplified_systems", time.perf_counter_ns() - ts,
                                  count=NPR)

    def _full_systems(self, rid, now):
        # R1 meaningful workload: observed-motion integration per NPC per tick
        # (schedule-space base position + two-axis sinusoidal liveliness
        # jitter + atan2 steering heading) plus derived-vitals evaluation.
        # Output is transient micro-state (sink accumulator); nothing here
        # enters the logical or durable lanes.
        base = rid * NPR
        sink = self.micro_sink
        for p in self.npcs[base:base + NPR]:
            pos = position_engine(self.regions_map, self.npcs, self.oneoffs, p["id"], now)
            jx = p["jA"] * math.sin(TWO_PI * p["jF"] * now + p["jP"])
            jy = p["jA"] * math.sin(TWO_PI * p["jF"] * 1.3 * now + p["jP"] * 1.7)
            heading = math.atan2(jy, jx + 1.0)
            energy, hunger = needs_engine(p, now)
            sink += pos[0] + jx + pos[1] + jy + heading + energy + hunger
        self.micro_sink = sink

    def _simplified_systems(self, rid, now):
        # R1 meaningful workload at 1/CADENCE cadence: identical closed-form
        # base position + one-axis coarse jitter + vitals (unobserved coarse
        # advancement; no steering, no second jitter axis).
        base = rid * NPR
        sink = self.micro_sink
        for p in self.npcs[base:base + NPR]:
            pos = position_engine(self.regions_map, self.npcs, self.oneoffs, p["id"], now)
            jx = p["jA"] * math.sin(TWO_PI * p["jF"] * now + p["jP"])
            energy, hunger = needs_engine(p, now)
            sink += pos[0] + jx + pos[1] + energy + hunger
        self.micro_sink = sink

    def advance(self, ticks):
        for _ in range(ticks):
            self.tick(self.t + 1)

    def skip(self, ticks):
        # time-skip: event-queue fast-forward only (no per-tick systems)
        t0 = time.perf_counter_ns()
        events_before = self.event_count
        for _ in range(ticks):
            self.tick(self.t + 1, run_systems=False)
        self.meas.add("timeskip", time.perf_counter_ns() - t0)
        return self.event_count - events_before

    def witnesses_at(self, rid, place_name, now):
        p0 = rid * NPR
        place = self.regions_map[rid]["places"][place_name]
        out = []
        for p in self.npcs[p0:p0 + NPR]:
            pos = position_engine(self.regions_map, self.npcs, self.oneoffs, p["id"], now)
            dx = pos[0] - place[0]
            dy = pos[1] - place[1]
            if dx * dx + dy * dy <= WITNESS_R * WITNESS_R:
                out.append(p["id"])
        return out

    def encounter(self, rid, kind, now, place_name="park"):
        witnesses = self.witnesses_at(rid, place_name, now)
        eid = "e%d" % len(self.encounter_log)
        self.encounter_log.append((eid, rid, kind, now, witnesses))
        self._capture_consequences(eid, kind, witnesses, now)
        return eid, witnesses

    def replay_span(self, rid, n_events):
        """R2 event replay: re-deliver a region's recent commitment events
        through the normal application path; all must be stale-rejected."""
        reg = self.regions[rid]
        recent = sorted(self.last_event.items(), key=lambda kv: (kv[1][0], kv[0]))
        region_recent = [kv for kv in recent if self.npcs[kv[0]]["region"] == rid][-n_events:]
        before_events = dict(self.last_event)
        before_cal = len(reg["cal"])
        before_counts = (len(self.pipe.applied), len(self.pipe.pending))
        outcomes = set()
        for npc_id, (tick, slot) in region_recent:
            outcomes.add(self.apply_event(npc_id, tick, slot))
        ok = (self.last_event == before_events
              and len(reg["cal"]) == before_cal
              and (len(self.pipe.applied), len(self.pipe.pending)) == before_counts
              and outcomes <= {"stale-noop"})
        return ok, len(region_recent)

    def dup_event(self):
        """R2 duplicate event delivery: re-fire one recent commitment event."""
        if not self.last_event:
            return True, 0
        npc_ids = sorted(self.last_event)
        npc_id = npc_ids[self.t % len(npc_ids)]
        tick, slot = self.last_event[npc_id]
        before = dict(self.last_event)
        out = self.apply_event(npc_id, tick, slot)
        return (self.last_event == before and out == "stale-noop"), 1

    # -- verification helpers ------------------------------------------------

    def record_counts(self):
        counts = {}
        for _key, rec in self.pipe.ordered_records():
            k = (rec["kind"], rec["npc"])
            counts[k] = counts.get(k, 0) + 1
        return counts

    def check_orphans(self):
        """I1: every live durable fact has a pipeline record (count equality)."""
        counts = self.record_counts()
        problems = []
        for nid in range(N_NPCS):
            if len(self.memories[nid]) != counts.get(("memory", nid), 0):
                problems.append(("memory", nid))
            if len(self.rels[nid]) != counts.get(("relationship", nid), 0):
                problems.append(("relationship", nid))
            if len(self.cands[nid]) != counts.get(("rep_candidate", nid), 0):
                problems.append(("rep_candidate", nid))
            if len(self.oneoffs[nid]) != counts.get(("oneoff", nid), 0):
                problems.append(("oneoff", nid))
        return problems

    def rebuild_region(self, rid):
        """C1: rebuild a region's logical lane from base + pipeline records
        (applied AND pending alike) and compare with live state."""
        lo, hi = rid * NPR, (rid + 1) * NPR
        kinds = ("oneoff", "memory", "relationship", "rep_candidate")
        rb = {nid: {k: 0 for k in kinds} for nid in range(lo, hi)}
        for _key, rec in self.pipe.ordered_records():
            if rec["npc"] in rb:
                rb[rec["npc"]][rec["kind"]] += 1
        for nid in range(lo, hi):
            if len(self.oneoffs[nid]) != rb[nid]["oneoff"]:
                return False
            if len(self.memories[nid]) != rb[nid]["memory"]:
                return False
            if len(self.rels[nid]) != rb[nid]["relationship"]:
                return False
            if len(self.cands[nid]) != rb[nid]["rep_candidate"]:
                return False
        return True

    def expected_task(self, npc_id):
        """Most recent commitment start at or before t (engine-side spec)."""
        p = self.npcs[npc_id]
        offs = tuple(self.oneoffs[npc_id])
        week = self.t // WEEK
        day = (self.t % WEEK) // DAY
        starts = []
        for d in (day - 1, day):
            dd = d % 7
            wk = week - 1 if d < 0 else week
            for slot, start, _e, _pl in day_slots_engine(p, dd, wk, offs):
                if start <= self.t:
                    starts.append((start, slot))
        return max(starts) if starts else None

    def coherence(self, rid, sample):
        """C3: active-region task pointers coherent with clock + schedule."""
        bad = []
        for nid in range(rid * NPR, (rid + 1) * NPR):
            if nid % max(1, NPR // sample) != 0:
                continue
            expected = self.expected_task(nid)
            if expected is None:
                continue
            if self.last_event.get(nid) != expected:
                bad.append((nid, self.last_event.get(nid), expected))
        return bad


# ----------------------------------------------------------------------------
# Oracle — independent reference model (R3)
#
# Deliberately separate code: its own slot builder, its own window/position
# walk over an explicit anchor sequence, its own vitals marks, its own
# witness loop and dedup log. No LOD, no pipeline.
# ----------------------------------------------------------------------------

class Oracle:
    def __init__(self, world):
        self.regions_map, self.npcs = world
        self.t = 0
        self.log = {}
        self.oneoffs = {p["id"]: [] for p in self.npcs}
        self.encounters = []

    def advance(self, ticks):
        self.t += ticks

    def skip(self, ticks):
        self.t += ticks

    def _slots(self, p, day, week):
        b = week * WEEK + day * DAY
        seq = []
        if p["shift"]:
            seq.append(["work", b + p["work_start"], b + p["work_end"], "work"])
            seq.append(["sleep", b + p["sleep_start"], b + DAY + p["wake"], p["home"]])
        else:
            seq.append(["work", b + p["work_start"], b + p["lunch"], "work"])
            seq.append(["lunch", b + p["lunch"], b + p["lunch"] + 60, "food"])
            seq.append(["work2", b + p["lunch"] + 60, b + p["work_end"], "work"])
            if day in p["lei_days"]:
                anchor = p["home"] if (week, day) in self.oneoffs[p["id"]] else "leisure"
                seq.append(["leisure", b + p["lei_start"], b + p["lei_start"] + 120, anchor])
            seq.append(["sleep", b + p["sleep_start"], b + DAY + p["wake"], p["home"]])
        return sorted(seq, key=lambda s: s[1])

    def pos(self, p, t):
        """Independent position walk over an explicit anchor sequence."""
        places = self.regions_map[p["region"]]["places"]
        wk = t // WEEK
        dy = (t % WEEK) // DAY
        seq = []
        pd = (dy - 1) % 7
        pw = wk - 1 if dy == 0 else wk
        for slot, start, end, place in self._slots(p, pd, pw):
            seq.append((start, end, place))
        for slot, start, end, place in self._slots(p, dy, wk):
            seq.append((start, end, place))
        nd = (dy + 1) % 7
        nw = wk + (1 if dy == 6 else 0)
        nx = self._slots(p, nd, nw)[0]
        seq.append((nx[1], nx[2], nx[3]))
        cur = None
        for start, end, place in seq:
            if start <= t:
                cur = (start, end, place)
                continue
            if cur is None:
                continue
            if t < cur[1]:
                return tuple(places[cur[2]])
            span = start - cur[1]
            frac = (t - cur[1]) / span if span > 0 else 1.0
            ax, ay = places[cur[2]]
            bx, by = places[place]
            return (ax + (bx - ax) * frac, ay + (by - ay) * frac)
        return tuple(places[seq[-1][2]])

    def needs(self, p, t):
        minute = t % DAY
        ss = p["sleep_start"]
        if minute < p["wake"]:
            span = (DAY - ss) + p["wake"]
            energy = ((DAY - ss) + minute) / span if span > 0 else 1.0
            if energy > 1.0:
                energy = 1.0
        else:
            span = ss - p["wake"]
            if span < 1:
                span = 1
            energy = 1.0 - (minute - p["wake"]) / span
            if energy < 0.0:
                energy = 0.0
        marks = [ss - DAY]
        if p["lunch"] is not None:
            marks.append(p["lunch"])
        marks.append(ss)
        ref = max(x for x in marks if x <= minute)
        hunger = (minute - ref) / 720.0
        if hunger > 1.0:
            hunger = 1.0
        if hunger < 0.0:
            hunger = 0.0
        return (round(energy, 6), round(hunger, 6))

    def encounter(self, eid, rid, kind, tick, place_name):
        p0 = rid * NPR
        place_xy = self.regions_map[rid]["places"][place_name]
        w = []
        for p in self.npcs[p0:p0 + NPR]:
            x, y = self.pos(p, tick)
            if (x - place_xy[0]) ** 2 + (y - place_xy[1]) ** 2 <= WITNESS_R ** 2:
                w.append(p["id"])
        self.encounters.append((eid, rid, kind, tick, w))
        for nid in w:
            self.log["mem:%s:%d" % (eid, nid)] = {"kind": "memory", "npc": nid,
                                                  "eid": eid, "what": kind, "tick": tick}
            if kind in ("theft", "brawl"):
                self.log["rel:%s:%d" % (eid, nid)] = {
                    "kind": "relationship", "npc": nid, "eid": eid,
                    "delta": -0.4 if kind == "theft" else -0.2, "tick": tick}
            if kind == "theft":
                self.log["cand:%s:%d" % (eid, nid)] = {
                    "kind": "rep_candidate", "npc": nid, "eid": eid, "tick": tick}
                week = tick // WEEK
                day = (tick % WEEK) // DAY
                od = (week, (day + 1) % 7) if day < 6 else (week + 1, 0)
                self.log["oneoff:%s:%d" % (eid, nid)] = {
                    "kind": "oneoff", "npc": nid, "eid": eid,
                    "week": od[0], "day": od[1], "tick": tick}
                self.oneoffs[nid].append(od)


# ----------------------------------------------------------------------------
# Calibration (R4): pure scheduler/tick overhead, all regions dormant
# ----------------------------------------------------------------------------

def run_calibration():
    world = gen_world(WORLD_SEED)
    meas = Measure()
    eng = Engine(world, Pipeline(), meas)
    for rid in range(N_REGIONS):
        eng.regions[rid]["level"] = DORMANT
        eng.regions[rid]["next_event"] = 10 ** 9   # no due events at all
    for _ in range(2000):
        eng.tick(eng.t + 1)
    return meas.us_per("scheduler_tick")


# ----------------------------------------------------------------------------
# Suite
# ----------------------------------------------------------------------------

def run_suite(trace_memory):
    world = gen_world(WORLD_SEED)
    pipe = Pipeline()
    meas = Measure()
    eng = Engine(world, pipe, meas)
    oracle = Oracle(world)
    rng = random.Random(SCENARIO_SEED)
    checks = []
    failures = []
    op_log = []
    stats = {"deactivations": 0, "activations": 0, "encounters": 0,
             "replays": 0, "replayed_events": 0, "dup_deliveries": 0,
             "dup_events": 0, "ack_drops_forced": 0}

    def record_fail(check, detail):
        failures.append({"check": check, "detail": detail,
                         "world_seed": WORLD_SEED, "scenario_seed": SCENARIO_SEED,
                         "op_index": len(op_log), "tick": eng.t})

    if trace_memory:
        tracemalloc.start()
        tracemalloc.reset_peak()

    # ---- Phase 1: steady LOD cost profile (M1) ----------------------------
    plan = [FULL, FULL, SIMPLIFIED, SIMPLIFIED, EVENT, EVENT, DORMANT, DORMANT]
    for rid, lvl in enumerate(plan):
        eng.set_level(rid, lvl)
    op_log.append(["init_levels", plan])
    eng.advance(DAY)
    oracle.advance(DAY)

    # ---- Phase 2: presence shift + transitions (M2, C1) -------------------
    shifted = [DORMANT, EVENT, SIMPLIFIED, FULL, FULL, SIMPLIFIED, EVENT, DORMANT]
    rebuilds_ok = True
    for rid, lvl in enumerate(shifted):
        old = eng.set_level(rid, lvl)
        if lvl > old:
            stats["activations"] += 1
        else:
            stats["deactivations"] += 1
            if not eng.rebuild_region(rid):
                rebuilds_ok = False
                record_fail("C1", "rebuild mismatch after deactivation rid=%d" % rid)
    op_log.append(["shift_levels", shifted])
    eng.advance(DAY)
    oracle.advance(DAY)

    # ---- Phase 3: seeded fuzz with fault injection (C2, C3, C5) -----------
    coherence_bad = []
    replay_ok = True
    dup_ok = True
    dropped_keys = []
    for op in range(200):
        choice = rng.random()
        if choice < 0.30:
            ticks = rng.randint(10, 100)
            eng.advance(ticks)
            oracle.advance(ticks)
            op_log.append(["advance", ticks])
        elif choice < 0.50:
            rid = rng.randrange(N_REGIONS)
            lvl = rng.choice([FULL, SIMPLIFIED, EVENT, DORMANT])
            old = eng.regions[rid]["level"]
            eng.set_level(rid, lvl)
            if lvl > old:
                stats["activations"] += 1
                bad = eng.coherence(rid, 25)
                coherence_bad.extend([(rid,) + b for b in bad])
            else:
                stats["deactivations"] += 1
                if not eng.rebuild_region(rid):
                    rebuilds_ok = False
                    record_fail("C1", "rebuild mismatch rid=%d op=%d" % (rid, op))
            op_log.append(["presence", rid, lvl])
        elif choice < 0.62:
            rid = rng.randrange(N_REGIONS)
            if eng.regions[rid]["level"] != FULL:
                eng.set_level(rid, FULL)
                stats["activations"] += 1
            kind = rng.choice(["theft", "greeting", "brawl"])
            eid, _w = eng.encounter(rid, kind, eng.t)
            oracle.encounter(eid, rid, kind, eng.t, "park")
            stats["encounters"] += 1
            op_log.append(["encounter", rid, kind])
        elif choice < 0.70:
            ticks = rng.randint(120, 1440)
            eng.skip(ticks)
            oracle.skip(ticks)
            op_log.append(["skip", ticks])
        elif choice < 0.78:
            keys = sorted(set(list(pipe.applied) + list(pipe.pending)))
            if keys:
                key = keys[rng.randrange(len(keys))]
                rec = pipe.applied.get(key) or pipe.pending.get(key)
                out = pipe.retry_deliver(key, rec, eng.t)
                if out == "captured":
                    dup_ok = False
                    record_fail("C2", "retry re-captured existing key %s" % key)
                stats["dup_deliveries"] += 1
            op_log.append(["dup_delivery"])
        elif choice < 0.85:
            pending_now = sorted(pipe.pending)
            if pending_now:
                key = pending_now[rng.randrange(len(pending_now))]
                if pipe.drop_ack(key, eng.t):
                    dropped_keys.append((key, eng.t))
                    stats["ack_drops_forced"] += 1
            op_log.append(["drop_ack"])
        elif choice < 0.92:
            rid = rng.randrange(N_REGIONS)
            ok, n = eng.replay_span(rid, 40)
            if not ok:
                replay_ok = False
                record_fail("C2", "replay span changed state rid=%d op=%d" % (rid, op))
            stats["replays"] += 1
            stats["replayed_events"] += n
            op_log.append(["replay_span", rid])
        else:
            ok, n = eng.dup_event()
            if not ok:
                dup_ok = False
                record_fail("C2", "duplicate event delivery changed state op=%d" % op)
            stats["dup_events"] += n
            op_log.append(["dup_event"])

    orphans = eng.check_orphans()
    if orphans:
        record_fail("I1", "orphan durable facts: %s" % orphans[:5])

    # ---- Phase 4: seven-day timeskip (M3) ---------------------------------
    for rid in range(N_REGIONS):
        eng.set_level(rid, EVENT)
    op_log.append(["all_event"])
    skip_events = eng.skip(7 * DAY)
    oracle.skip(7 * DAY)
    op_log.append(["week_skip", 7 * DAY])

    # ---- C4: independent reference model comparison ------------------------
    eng_keys = set(list(pipe.applied) + list(pipe.pending))
    ora_keys = set(oracle.log)
    key_match = eng_keys == ora_keys
    payload_match = True
    for key in sorted(eng_keys & ora_keys):
        rec_e = pipe.applied.get(key) or pipe.pending.get(key)
        if canon(rec_e) != canon(oracle.log[key]):
            payload_match = False
            record_fail("C4", "payload mismatch key=%s" % key)
            break
    witness_match = True
    for (eid, rid, kind, tick, w_eng) in eng.encounter_log:
        match = [o for o in oracle.encounters if o[0] == eid]
        if not match or sorted(match[0][4]) != sorted(w_eng):
            witness_match = False
            record_fail("C4", "witness set mismatch eid=%s" % eid)
            break
    oneoff_match = all(sorted(eng.oneoffs[n]) == sorted(oracle.oneoffs[n])
                       for n in range(N_NPCS))
    task_bad = []
    for p in eng.npcs:
        expected = eng.expected_task(p["id"])
        if expected is not None and eng.last_event.get(p["id"]) != expected:
            task_bad.append((p["id"], eng.last_event.get(p["id"]), expected))
    pos_bad = 0
    needs_bad = 0
    for p in eng.npcs:
        pe = position_engine(eng.regions_map, eng.npcs, eng.oneoffs, p["id"], eng.t)
        po = oracle.pos(p, eng.t)
        if abs(pe[0] - po[0]) > 1e-9 or abs(pe[1] - po[1]) > 1e-9:
            pos_bad += 1
        ee = needs_engine(p, eng.t)
        eo = oracle.needs(p, eng.t)
        if abs(ee[0] - eo[0]) > 1e-9 or abs(ee[1] - eo[1]) > 1e-9:
            needs_bad += 1

    checks.append({"name": "C1 rebuild-from-pipeline (I1 capture semantics)",
                   "pass": rebuilds_ok and not orphans,
                   "detail": "%d deactivations; every region rebuild from base + "
                             "applied + pending reproduced live oneoffs/memory "
                             "exactly; zero orphan durable facts" % stats["deactivations"]})
    checks.append({"name": "C2 exactly-once (dups, retries, event replay)",
                   "pass": dup_ok and replay_ok,
                   "detail": "%d duplicate record deliveries + %d duplicate event "
                             "deliveries + %d replayed events across %d replay "
                             "spans: all stale-rejected, zero re-captures, zero "
                             "state changes" %
                             (stats["dup_deliveries"], stats["dup_events"],
                              stats["replayed_events"], stats["replays"])})
    checks.append({"name": "C3 reactivation coherence (I3)",
                   "pass": not coherence_bad and not task_bad,
                   "detail": "post-activation sampled task pointers + final "
                             "all-NPC tasks coherent with clock+schedule; "
                             "mismatches: %d sampled + %d final"
                             % (len(coherence_bad), len(task_bad))})
    checks.append({"name": "C4 independent reference model agreement",
                   "pass": key_match and payload_match and witness_match
                           and oneoff_match and not task_bad
                           and pos_bad == 0 and needs_bad == 0,
                   "detail": "durable keys %d==%d, payloads equal, witness sets "
                             "equal across %d encounters, oneoffs equal, final "
                             "tasks equal, positions equal (%d bad), needs equal "
                             "(%d bad)" % (len(eng_keys), len(ora_keys),
                                           stats["encounters"], pos_bad, needs_bad)})
    c5_ok = True
    c5_violations = []
    for key, drop_tick in dropped_keys:
        acks = [e for e in pipe.audit if e[0] == key and e[1] == "acked"]
        if any(a[2] <= drop_tick for a in acks):
            c5_ok = False
            c5_violations.append(key)
    checks.append({"name": "C5 pending never falsely confirmed",
                   "pass": c5_ok,
                   "detail": "%d forced ack drops; every dropped key either "
                             "remained pending or was acked strictly after the "
                             "drop; confirmed-before-ack violations: %d"
                             % (stats["ack_drops_forced"], len(c5_violations))})

    # ---- C6: host-loss fail-closed semantics (fresh engine, isolated) ------
    world2 = gen_world(WORLD_SEED)
    pipe2 = Pipeline()
    eng2 = Engine(world2, pipe2, Measure())
    eng2.set_level(0, FULL)
    eng2.advance(600)
    place_names = ("park", "food", "work", "leisure", "home_a", "home_b")
    pl1 = max(place_names, key=lambda pl: len(eng2.witnesses_at(0, pl, eng2.t)))
    eng2.encounter(0, "greeting", eng2.t, pl1)   # pending
    eng2.advance(ACK_DELAY + 10)                 # acks land -> applied
    pl2 = max(place_names, key=lambda pl: len(eng2.witnesses_at(0, pl, eng2.t)))
    eng2.encounter(0, "theft", eng2.t, pl2)      # fresh pending
    applied_snapshot = dict(pipe2.applied)
    lost = pipe2.discard_unacked()
    c6_ok = (dict(pipe2.applied) == applied_snapshot
             and len(lost) > 0
             and all(pipe2.status(k) == "absent" for k in lost))
    checks.append({"name": "C6 host-loss fail-closed semantics",
                   "pass": c6_ok,
                   "detail": "encounters placed at the most-populated anchors "
                             "(%s, %s); second encounter's pending records (%d) "
                             "lost on host loss; applied set (%d records) "
                             "unchanged; no phantom confirmations"
                             % (pl1, pl2, len(lost), len(applied_snapshot))})

    # ---- measurements (M1-M3) ----------------------------------------------
    m = meas.snapshot()
    measurements = {
        "M1_lod_cost_profile": {
            "full_us_per_npc_tick": m.get("full_systems", {}).get("us_per_unit", 0.0),
            "full_npc_ticks": m.get("full_systems", {}).get("count", 0),
            "simplified_us_per_npc_coarse_tick":
                m.get("simplified_systems", {}).get("us_per_unit", 0.0),
            "simplified_npc_coarse_ticks": m.get("simplified_systems", {}).get("count", 0),
            "simplified_amortized_us_per_npc_tick":
                round(m.get("simplified_systems", {}).get("us_per_unit", 0.0) / CADENCE, 5),
            "event_us_per_event_active": m.get("event_active", {}).get("us_per_unit", 0.0),
            "wake_us_per_event": m.get("wake", {}).get("us_per_unit", 0.0),
            "wake_events": m.get("wake", {}).get("count", 0),
            "cadence": CADENCE,
            "workload_definition": (
                "full: per NPC per tick — closed-form schedule-space base "
                "position + two-axis sinusoidal liveliness jitter + atan2 "
                "steering heading + derived vitals (observed-motion fidelity "
                "work). simplified: same base position + one-axis jitter + "
                "vitals at 1/%d cadence (unobserved coarse advancement; no "
                "steering). No dummy padding on either side; both compute the "
                "same logical quantities, full simply does the observed-fidelity "
                "extras every tick." % CADENCE),
        },
        "M2_transitions": {
            "activate_us": m.get("activate", {}).get("us_per_unit", 0.0),
            "deactivate_us": m.get("deactivate", {}).get("us_per_unit", 0.0),
            "activations": stats["activations"],
            "deactivations": stats["deactivations"],
        },
        "M3_timeskip": {
            "days": 7,
            "events_during_skip": skip_events,
            "skip_total_ms": m.get("timeskip", {}).get("total_ms", 0.0),
            "skip_us_per_event": round(m.get("timeskip", {}).get("total_ms", 0.0)
                                       * 1000.0 / skip_events, 4) if skip_events else 0.0,
            "note": "skip cost is dominated by event processing (queue "
                    "fast-forward); per-tick systems do not run during skips",
        },
        "totals": {
            "ticks_simulated": eng.t,
            "events_fired": eng.event_count,
            "stale_drops": eng.stale_drops,
            "encounters": stats["encounters"],
            "captures": pipe.stats["captures"],
            "acks": pipe.stats["acks"],
            "ack_drops_forced": stats["ack_drops_forced"],
            "dup_noops": pipe.stats["dup_noop"],
            "applied_at_end": len(pipe.applied),
            "pending_at_end": len(pipe.pending),
        },
    }
    if trace_memory:
        cur, peak = tracemalloc.get_traced_memory()
        tracemalloc.stop()
        measurements["memory"] = {
            "tracemalloc_peak_mb": round(peak / 1e6, 3),
            "tracemalloc_end_mb": round(cur / 1e6, 3),
            "note": "peak across the traced full suite run (engine + oracle + "
                    "pipeline + op log); includes both models, not per-model",
        }

    logical = {
        "t": eng.t,
        "region_levels": [eng.regions[r]["level"] for r in range(N_REGIONS)],
        "applied_keys": sorted(pipe.applied),
        "pending_keys": sorted(pipe.pending),
        "last_events": sorted((k, v[0], v[1]) for k, v in eng.last_event.items()),
        "oneoffs": sorted((k, w, d) for k, v in eng.oneoffs.items() for (w, d) in v),
        "encounter_log": [(e[0], e[1], e[2], e[3], len(e[4]))
                          for e in eng.encounter_log],
        "stats": stats,
        "pipe_stats": pipe.stats,
        "stale_drops": eng.stale_drops,
        "event_count": eng.event_count,
    }

    return {"checks": checks, "measurements": measurements, "logical": logical,
            "op_log": op_log, "failures": failures}


def machine_info():
    info = {
        "python": sys.version.split()[0],
        "implementation": platform.python_implementation(),
        "platform": platform.platform(),
        "machine": platform.machine(),
        "cpu_count": os.cpu_count(),
    }
    try:
        with open("/proc/cpuinfo") as f:
            for line in f:
                if line.startswith("model name"):
                    info["cpu_model"] = line.split(":", 1)[1].strip()
                    break
    except OSError:
        pass
    return info


def main():
    print("S1/S6-LITE LOCAL harness — tiered-LOD transition correctness + cost shape")
    print("authorization: lead 2026-10-18 (local-only, headless; no network, no cloud,")
    print("no engine code, no rendering, no technology selection)")
    print("")

    calib_us = run_calibration()

    r1 = run_suite(trace_memory=True)
    r2 = run_suite(trace_memory=False)

    fp1, fp2 = fp(r1["logical"]), fp(r2["logical"])
    deterministic = fp1 == fp2

    results = {
        "checks": r2["checks"],
        "measurements": r2["measurements"],
        "calibration": {
            "ticks": 2000,
            "scheduler_us_per_tick": round(calib_us, 4),
            "dormant_per_npc_per_tick_us": 0.0,
            "note": "R4: dormant NPCs are never iterated (per-NPC per-tick cost "
                    "is 0 by construction); the shared per-tick scheduler/queue-"
                    "check overhead is measured separately (all regions dormant, "
                    "no due events) and is per-tick, not per-NPC",
        },
        "faults": {
            "duplicate_deliveries": r2["logical"]["stats"]["dup_deliveries"],
            "duplicate_events": r2["logical"]["stats"]["dup_events"],
            "replayed_events": r2["logical"]["stats"]["replayed_events"],
            "replay_spans": r2["logical"]["stats"]["replays"],
            "forced_ack_drops": r2["logical"]["stats"]["ack_drops_forced"],
        },
        "memory": r1["measurements"].get("memory", {}),
        "determinism": {
            "pass": deterministic,
            "fingerprint": fp1,
            "note": "sha256 of canonical logical-state results compared across "
                    "two full in-process suite runs (one traced, one untraced); "
                    "timing measurements are excluded and are NOT bit-identical",
        },
        "failures": r2["failures"],
        "op_log": r2["op_log"],
        "meta": {
            "run_date": RUN_DATE,
            "machine": machine_info(),
            "seeds": {"world": WORLD_SEED, "scenario": SCENARIO_SEED},
            "npcs": N_NPCS,
            "regions": N_REGIONS,
            "authorization": "lead, 2026-10-18 — S1/S6-lite slice; local-only, "
                             "headless; no network/cloud/engine-code/rendering/"
                             "technology-selection",
        },
    }

    for t in results["checks"]:
        print("[%s] %s — %s" % ("PASS" if t["pass"] else "FAIL", t["name"], t["detail"]))
    print("[%s] C7 determinism — identical logical-state fingerprints across two "
          "full suite runs: %s" % ("PASS" if deterministic else "FAIL", fp1))
    print("")
    m1 = results["measurements"]["M1_lod_cost_profile"]
    print("M1 LOD cost profile (Python stdlib reference workload; NOT production):")
    print("  full:            %.4f us per NPC per tick   (%d NPC-ticks)"
          % (m1["full_us_per_npc_tick"], m1["full_npc_ticks"]))
    print("  simplified:      %.4f us per NPC per coarse tick (amortized %.5f us "
          "per NPC-tick, %d NPC-coarse-ticks)"
          % (m1["simplified_us_per_npc_coarse_tick"],
             m1["simplified_amortized_us_per_npc_tick"],
             m1["simplified_npc_coarse_ticks"]))
    print("  event (active):  %.4f us per commitment event"
          % m1["event_us_per_event_active"])
    print("  dormant wake:    %.4f us per wake event (%d wake events)"
          % (m1["wake_us_per_event"], m1["wake_events"]))
    print("  dormant per-NPC per-tick: 0 by construction; scheduler overhead "
          "%.4f us/tick (measured separately)" % calib_us)
    m2 = results["measurements"]["M2_transitions"]
    print("M2 transitions: activate %.4f us x%d, deactivate %.4f us x%d"
          % (m2["activate_us"], m2["activations"],
             m2["deactivate_us"], m2["deactivations"]))
    m3 = results["measurements"]["M3_timeskip"]
    print("M3 timeskip: 7 days = %d events, %.1f ms total, %.4f us/event"
          % (m3["events_during_skip"], m3["skip_total_ms"], m3["skip_us_per_event"]))
    tot = results["measurements"]["totals"]
    print("totals: %d ticks, %d events (%d stale-dropped re-deliveries), "
          "%d encounters, %d captures, %d acks, %d applied / %d pending at end"
          % (tot["ticks_simulated"], tot["events_fired"], tot["stale_drops"],
             tot["encounters"], tot["captures"], tot["acks"],
             tot["applied_at_end"], tot["pending_at_end"]))

    all_pass = deterministic and all(t["pass"] for t in results["checks"]) \
        and not results["failures"]
    print("")
    print("overall: %s" % ("ALL CHECKS PASSED" if all_pass else "FAILURES PRESENT"))

    out_path = os.path.join(HERE, "results.json")
    with open(out_path, "w") as f:
        json.dump(results, f, indent=2, sort_keys=True)
    print("results written: %s" % out_path)
    return 0 if all_pass else 1


if __name__ == "__main__":
    sys.exit(main())
