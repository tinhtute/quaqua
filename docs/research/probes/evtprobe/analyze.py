#!/usr/bin/env python3
"""Analyze evtprobe output: transition table, missed-transition count, latency bounds.

Inputs (from probe.py --out DIR):
  events.jsonl  one line per socket line: {t_ms, kind, msg}
  polls.jsonl   {t_ms, src:get|list, ...}
  ps.jsonl      {t_ms, ps}

Outputs: markdown fragments on stdout + summary json.
"""
import json
import os
import sys

D = sys.argv[1]

def load(path):
    rows = []
    with open(os.path.join(D, path)) as f:
        for line in f:
            line = line.strip()
            if line:
                rows.append(json.loads(line))
    return rows

events = load("events.jsonl")
polls = load("polls.jsonl")
pss = load("ps.jsonl")
meta = json.load(open(os.path.join(D, "meta.json")))

pane_kind = {}
pane_name = {}
for p in meta["panes0"]:
    pane_kind[p["pane_id"]] = p.get("workspace_id")
for a in meta["agents0"]:
    if a.get("name"):
        pane_name[a["pane_id"]] = a["name"]

# ---- subscription events ----
subs = []      # {t, event, pane_id, status, agent}
acks = []
for e in events:
    msg = e.get("msg") or {}
    if "event" in msg and "data" in msg:
        d = msg["data"]
        subs.append({"t": e["t_ms"], "event": msg["event"], "pane_id": d.get("pane_id"),
                     "status": d.get("agent_status"), "agent": d.get("agent"), "title": d.get("title")})
    elif "id" in msg and "result" in msg:
        acks.append({"t": e["t_ms"], "result": msg["result"]})
    elif "error" in msg:
        acks.append({"t": e["t_ms"], "error": msg["error"]})

status_events = [s for s in subs if s["event"] == "pane.agent_status_changed"]
other_events = [s for s in subs if s["event"] != "pane.agent_status_changed"]

def pane_name(kind):
    """Resolve a pane_id to the agent name that polls observed in it."""
    found = []
    for row in polls:
        if row.get("src") != "list":
            continue
        for a in row.get("agents") or []:
            if a.get("pane_id") == kind and a.get("name"):
                found.append(a["name"])
    return found[-1] if found else None

# ---- poll timeline per agent (from src=list, full snapshot) ----
timeline = {}   # name -> list of {t, status, seq}
for row in polls:
    if row.get("src") != "list":
        continue
    for a in row.get("agents") or []:
        if not a.get("name"):
            continue
        timeline.setdefault(a["name"], []).append({"t": row["t_ms"], "status": a["status"], "seq": a.get("seq")})

def transitions_from_polls(name):
    out = []
    prev = None
    for obs in timeline.get(name, []):
        if prev is None:
            prev = obs
            continue
        if obs["status"] != prev["status"]:
            out.append({"t_from": prev["t"], "t_at": obs["t"], "from": prev["status"], "to": obs["status"],
                        "seq_from": prev["seq"], "seq_at": obs["seq"]})
        prev = obs
    return out

def transitions_from_events(pane_id):
    out = []
    prev = None
    for s in status_events:
        if s["pane_id"] != pane_id:
            continue
        if prev is not None and s["status"] == prev["status"] and s["t"] - prev["t"] < 5000:
            # tolerate duplicate pushes of the same status within 5s as one transition
            continue
        out.append(s)
        prev = s
    return out

print("== ack/errors ==")
print(json.dumps(acks, indent=1))

names = sorted(timeline.keys())
summary = {"agents": {}, "events_total": len(subs), "status_events": len(status_events), "other_events": len(other_events)}
print("\n== per-agent: poll transitions vs event transitions ==")
print("| agent | pane | kind | poll transitions | event transitions | missed (poll>event) | seq delta |")
for name in names:
    pane_id = None
    for row in polls:
        if row.get("src") == "list":
            for a in row.get("agents") or []:
                if a.get("name") == name and a.get("pane_id"):
                    pane_id = a.get("pane_id")
    pts = transitions_from_polls(name)
    ets = transitions_from_events(pane_id) if pane_id else []
    seq_delta = None
    if timeline[name]:
        seq_delta = (timeline[name][-1]["seq"] or 0) - (timeline[name][0]["seq"] or 0)
    kind = None
    for row in polls:
        if row.get("src") == "list":
            for a in row.get("agents") or []:
                if a.get("name") == name and a.get("kind"):
                    kind = a.get("kind")
    print(f"| {name} | {pane_id} | {kind} | {len(pts)} | {len(ets)} | {len(pts)-len(ets)} | {seq_delta} |")
    summary["agents"][name] = {"pane_id": pane_id, "kind": kind,
        "poll_transitions": len(pts), "event_transitions": len(ets),
        "missed": len(pts) - len(ets), "seq_delta": seq_delta,
        "poll_list": [{"t": x["t_at"], "from": x["from"], "to": x["to"]} for x in pts],
        "evt_list": [{"t": x["t"], "from": None, "to": x["status"]} for x in ets]}

# ---- latency: pair each event transition with poll bracket ----
print("\n== latency bounds (event ts vs poll bracket; lower = event_ts - poll_new_ts, upper = event_ts - poll_prev_ts) ==")
print("| agent | to | poll_prev | poll_new | event_ts | lb_ms | ub_ms |")
lat = []
unconfirmed = 0
dupes = 0
# count duplicate consecutive same-status events per pane (raw over-delivery check)
last_by_pane = {}
for s in status_events:
    lp = last_by_pane.get(s["pane_id"])
    if lp and lp["status"] == s["status"]:
        dupes += 1
    last_by_pane[s["pane_id"]] = s
for name in names:
    pane_id = None
    for row in polls:
        if row.get("src") == "list":
            for a in row.get("agents") or []:
                if a.get("name") == name and a.get("pane_id"):
                    pane_id = a.get("pane_id")
    ets = transitions_from_events(pane_id) if pane_id else []
    tls = timeline.get(name, [])
    for e in ets:
        prev = None
        new = None
        for obs in tls:
            if obs["t"] <= e["t"]:
                prev = obs
            if obs["t"] >= e["t"] and obs["status"] == e["status"] and new is None:
                new = obs
        if prev is not None and new is not None and prev["status"] != e["status"]:
            lb = e["t"] - new["t"]
            ub = e["t"] - prev["t"]
            print(f"| {name} | {e['status']} | {prev['t']} | {new['t']} | {e['t']} | {lb} | {ub} |")
            lat.append({"agent": name, "to": e["status"], "lb": lb, "ub": ub})
        else:
            unconfirmed += 1
print(f"\nduplicate (consecutive same-status) events: {dupes}; unconfirmed (no poll bracket showing the new status): {unconfirmed}")
summary["dupes"] = dupes
summary["unconfirmed"] = unconfirmed
summary["latency"] = lat

print("\n== non-status events ==")
for s in other_events:
    print(s)

json.dump(summary, open(os.path.join(D, "summary.json"), "w"), indent=1)
print("\nwrote", os.path.join(D, "summary.json"))
