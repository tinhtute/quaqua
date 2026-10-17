#!/usr/bin/env python3
"""herdr 0.9.5 events reliability probe (read-only).

Holds ONE events.subscribe connection over the herdr unix socket, logs every
subscription event with a local receive timestamp, and in parallel:
  - polls `herdr agent get <name>` for each live agent every --poll-get sec
  - polls `herdr agent list` every --poll-list sec (tight latency bounds)
  - snapshots `ps -axo pid,ppid,command` every --ps sec (idle-with-children check)

Never sends input to any pane; only reads. Outputs NDJSON files in --out.
"""
import json
import os
import socket
import subprocess
import sys
import threading
import time

def now_ms():
    return int(time.time() * 1000)

def herdr(*args, timeout=10):
    r = subprocess.run(["herdr", *args], capture_output=True, text=True, timeout=timeout)
    if r.returncode != 0:
        return {"_error": r.stderr.strip() or f"exit {r.returncode}"}
    try:
        return json.loads(r.stdout)
    except json.JSONDecodeError:
        return {"_raw": r.stdout}

class Jsonl:
    def __init__(self, path):
        self.f = open(path, "a", buffering=1)
        self.lock = threading.Lock()
    def write(self, obj):
        with self.lock:
            self.f.write(json.dumps(obj, separators=(",", ":")) + "\n")

def live_agents():
    data = herdr("agent", "list")
    agents = (data.get("result") or {}).get("agents") or []
    return agents

def all_panes():
    ws = (herdr("workspace", "list").get("result") or {}).get("workspaces") or []
    panes = []
    for w in ws:
        wid = w.get("workspace_id")
        data = herdr("pane", "list", "--workspace", wid)
        plist = (data.get("result") or {}).get("panes") or []
        for p in plist:
            panes.append(p)
    return panes

def main():
    out = sys.argv[1]
    duration = float(os.environ.get("EVT_DURATION", "1200"))
    poll_get = float(os.environ.get("EVT_POLL_GET", "10"))
    poll_list = float(os.environ.get("EVT_POLL_LIST", "2"))
    ps_every = float(os.environ.get("EVT_PS", "30"))
    os.makedirs(out, exist_ok=True)
    stop = threading.Event()

    agents0 = live_agents()
    panes0 = all_panes()
    agent_panes = sorted({a["pane_id"] for a in agents0})
    subs = []
    for p in panes0:
        subs.append({"type": "pane.agent_status_changed", "pane_id": p["pane_id"]})
    for pid in agent_panes:
        subs.append({"type": "pane.exited", "pane_id": pid})
    subs.append({"type": "pane.agent_detected"})

    meta = {
        "t0_ms": now_ms(),
        "duration_s": duration,
        "poll_get_s": poll_get,
        "poll_list_s": poll_list,
        "socket": os.environ.get("HERDR_SOCKET_PATH"),
        "herdr_version": subprocess.run(["herdr", "--version"], capture_output=True, text=True).stdout.strip(),
        "agents0": [{"name": a.get("name"), "pane_id": a.get("pane_id"), "kind": a.get("agent"), "status": a.get("agent_status"), "seq": a.get("state_change_seq")} for a in agents0],
        "panes0": [{"pane_id": p["pane_id"], "workspace_id": p.get("workspace_id")} for p in panes0],
        "subscriptions": subs,
    }
    with open(os.path.join(out, "meta.json"), "w") as f:
        json.dump(meta, f, indent=1)

    ev = Jsonl(os.path.join(out, "events.jsonl"))
    polls = Jsonl(os.path.join(out, "polls.jsonl"))
    pss = Jsonl(os.path.join(out, "ps.jsonl"))

    # --- subscription reader ---
    def reader():
        s = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        s.connect(meta["socket"])
        req = {"id": "evtprobe", "method": "events.subscribe", "params": {"subscriptions": subs}}
        s.sendall((json.dumps(req) + "\n").encode())
        buf = b""
        s.settimeout(1.0)
        t_connected = now_ms()
        ev.write({"t_ms": t_connected, "kind": "connected"})
        while not stop.is_set():
            try:
                chunk = s.recv(65536)
            except socket.timeout:
                continue
            if not chunk:
                ev.write({"t_ms": now_ms(), "kind": "closed"})
                break
            buf += chunk
            while b"\n" in buf:
                line, buf = buf.split(b"\n", 1)
                if not line.strip():
                    continue
                try:
                    obj = json.loads(line)
                except json.JSONDecodeError:
                    obj = {"_raw": line.decode(errors="replace")}
                ev.write({"t_ms": now_ms(), "kind": "line", "msg": obj})
        s.close()

    # --- pollers ---
    names = [a["name"] for a in agents0 if a.get("name")]
    def poll_get_loop():
        while not stop.is_set():
            nms = list(names)
            for n in nms:
                data = herdr("agent", "get", n)
                ag = (data.get("result") or {}).get("agent") or {}
                polls.write({"t_ms": now_ms(), "src": "get", "name": n, "status": ag.get("agent_status"),
                             "seq": ag.get("state_change_seq"), "revision": ag.get("revision"),
                             "title": ag.get("terminal_title_stripped"), "err": data.get("_error")})
            stop.wait(poll_get)

    def poll_list_loop():
        while not stop.is_set():
            data = herdr("agent", "list")
            ags = (data.get("result") or {}).get("agents") or []
            row = {"t_ms": now_ms(), "src": "list", "agents": [
                {"name": a.get("name"), "pane_id": a.get("pane_id"), "kind": a.get("agent"),
                 "status": a.get("agent_status"), "seq": a.get("state_change_seq"),
                 "revision": a.get("revision")} for a in ags], "err": data.get("_error")}
            polls.write(row)
            # refresh name set so newly started agents get polled by name too
            for a in ags:
                if a.get("name") and a["name"] not in names:
                    names.append(a["name"])
            stop.wait(poll_list)

    def ps_loop():
        while not stop.is_set():
            r = subprocess.run(["ps", "-axo", "pid=,ppid=,command="], capture_output=True, text=True)
            pss.write({"t_ms": now_ms(), "ps": r.stdout})
            stop.wait(ps_every)

    threads = [threading.Thread(target=f, daemon=True) for f in (reader, poll_get_loop, poll_list_loop, ps_loop)]
    for t in threads:
        t.start()
    t_end = time.time() + duration
    try:
        while time.time() < t_end:
            time.sleep(1)
    except KeyboardInterrupt:
        pass
    stop.set()
    for t in threads:
        t.join(timeout=3)
    with open(os.path.join(out, "meta_done.json"), "w") as f:
        json.dump({"t_end_ms": now_ms(), "events_lines": os.path.getsize(os.path.join(out, "events.jsonl"))}, f)
    print("probe done", now_ms())

if __name__ == "__main__":
    main()
