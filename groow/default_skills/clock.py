"""Clock: `remind "..." --in 2h` sets an alarm that comes back to you; `schedule` lists them.

Alarms live in state/schedule.json in your home and survive a reboot. When one is due it reaches you
as a signal, framed as "[an alarm you set earlier] ...". Nothing runs by itself: an alarm is a message
to you, and you decide then what to do about it.

    remind "check the deploy"   --in 30m
    remind "read the news"      --every 3h
    remind "sum up the day"     --every "daily 22:00"
    remind "call it a night"    --at 23:00
    schedule                    what is set, and when each fires next
    schedule cancel a1b2c3
"""
import json
import os
import urllib.request
from pathlib import Path

from groow.mind import Schedule


def _state() -> Path:
    return Path(os.environ.get("GROOW_STATE", "state"))


def _daemon(op: str, args: dict):
    """Ask the running daemon, so its in-memory schedule stays in step. None if it is asleep."""
    url_file = _state() / "groow.url"
    url = url_file.read_text().strip() if url_file.exists() else "http://127.0.0.1:7373"
    try:
        req = urllib.request.Request(url + "/op", data=json.dumps({"op": op, "args": args}).encode(),
                                     headers={"Content-Type": "application/json"})
        with urllib.request.urlopen(req, timeout=10) as r:
            return json.loads(r.read())
    except Exception:
        return None


def _opts(argv: list) -> dict:
    out, i = {}, 0
    while i < len(argv):
        a = argv[i]
        if a.startswith("--"):
            key = a[2:]
            val = argv[i + 1] if i + 1 < len(argv) and not argv[i + 1].startswith("--") else "true"
            out[key] = val
            i += 2
        else:
            out.setdefault("_", []).append(a)
            i += 1
    return out


def remind_main(argv: list) -> dict:
    """remind "text" --in 30m | --at 18:30 | --every 2h | --every "daily 06:30" """
    if not argv or argv[0] in ("-h", "--help"):
        return {"text": 'usage: remind "text" --in 30m | --at 18:30 | --every 2h | --every "daily 06:30"\n'
                        "       the text comes back to you when it is due; --every repeats until you cancel it"}
    if argv[0] == "--selftest":
        return {"ok": True}
    o = _opts(argv)
    text = " ".join(o.get("_", []))
    when = ("in " + o["in"]) if o.get("in") else (("at " + o["at"]) if o.get("at") else "")
    every = o.get("every", "")
    if not text:
        return {"error": 'what should the alarm say? remind "text" --in 30m'}
    args = {"text": text, "when": when, "every": every, "by": "groow"}
    r = _daemon("remind", args) or Schedule(_state()).add(text, when=when, every=every, by="groow")
    if r.get("error"):
        return r
    return {"text": f"alarm {r['id']}: \"{r['text']}\" first {r['first']}"
                    + (f", repeats {r['repeats']}" if r.get("repeats") not in ("", "no") else "")}


def schedule_main(argv: list) -> dict:
    """schedule | schedule cancel <id>: the alarms you have set."""
    if argv and argv[0] in ("-h", "--help"):
        return {"text": "usage: schedule            what is set, and when each fires next\n"
                        "       schedule cancel <id>"}
    if argv and argv[0] == "--selftest":
        return {"ok": True}
    if argv and argv[0] == "cancel":
        aid = argv[1] if len(argv) > 1 else ""
        r = _daemon("schedule", {"action": "cancel", "id": aid}) or Schedule(_state()).cancel(aid)
        return {"text": f"cancelled {r.get('cancelled', 0)} alarm(s)"}
    r = _daemon("schedule", {"action": "list"}) or Schedule(_state()).listing()
    alarms = r.get("alarms", [])
    if not alarms:
        return {"text": 'no alarms set. remind "read the news" --every 3h'}
    lines = ["your alarms:"]
    for a in alarms:
        lines.append(f"  {a['id']}  {a['next']:>8s}  {a['repeat']:<16s} {a['text'][:60]}")
    return {"text": "\n".join(lines)}


CLI = {"remind": "remind_main", "schedule": "schedule_main"}
TESTS = [("remind", {"argv": ["--selftest"]}, {"ok": True}), ("schedule", {"argv": ["--selftest"]}, {"ok": True})]
