"""The clock: alarms and periodic tasks Groow (or its mentor) sets for itself.

A schedule is a file in the home, so it survives a reboot and Groow can read it
like any other file:

    state/schedule.json   [{id, text, when, repeat, enabled, created, last_fired, fires}]

`when` is an absolute unix time; `repeat` is a number of seconds, or a daily
wall-clock time ("daily 06:30"), or null for a one-shot. The mind checks what is
due on every housekeeping wake and pushes the text as a signal, framed as an
alarm the past self set. Nothing here runs code: an alarm is a message to the
conscious thread, which then decides what to do with it.

    groow remind "read the news"      --in 2h
    groow remind "check the CI logs"  --at 18:30
    groow remind "read the news"      --every 3h
    groow remind "sum up the day"     --every "daily 22:00"
    groow schedule            list what is set
    groow schedule cancel <id>
"""
from __future__ import annotations

import json
import re
import time
import uuid
from datetime import datetime, timedelta
from pathlib import Path

UNITS = {"s": 1, "m": 60, "h": 3600, "d": 86400, "w": 604800}
_EVERY_RE = re.compile(r"^\s*(?:every\s+)?(\d+(?:\.\d+)?)\s*([smhdw])\s*$", re.I)
_DAILY_RE = re.compile(r"^\s*(?:daily|every day)(?:\s+at)?\s+(\d{1,2}):(\d{2})\s*$", re.I)
_AT_RE = re.compile(r"^\s*(?:at\s+)?(\d{1,2}):(\d{2})\s*$")


def parse_delay(spec: str) -> float | None:
    """'30m', '2h', '1d' -> seconds."""
    m = _EVERY_RE.match(spec or "")
    return float(m.group(1)) * UNITS[m.group(2).lower()] if m else None


def parse_repeat(spec: str) -> tuple[float | None, str | None]:
    """-> (seconds, daily_hhmm). One of them, or (None, None) if it is not a repeat."""
    if not spec:
        return None, None
    d = _DAILY_RE.match(spec)
    if d:
        return None, f"{int(d.group(1)):02d}:{d.group(2)}"
    return parse_delay(spec), None


def next_daily(hhmm: str, after: float | None = None) -> float:
    after = after or time.time()
    h, m = (int(x) for x in hhmm.split(":"))
    base = datetime.fromtimestamp(after)
    target = base.replace(hour=h, minute=m, second=0, microsecond=0)
    if target.timestamp() <= after:
        target += timedelta(days=1)
    return target.timestamp()


def parse_when(spec: str) -> float | None:
    """'in 30m' / '30m' -> now+30m; 'at 18:30' / '18:30' -> the next 18:30; an ISO time -> itself."""
    if not spec:
        return None
    s = spec.strip()
    if s.lower().startswith("in "):
        d = parse_delay(s[3:])
        return time.time() + d if d else None
    d = parse_delay(s)
    if d is not None:
        return time.time() + d
    a = _AT_RE.match(s)
    if a:
        return next_daily(f"{int(a.group(1)):02d}:{a.group(2)}")
    try:
        return datetime.fromisoformat(s).timestamp()
    except ValueError:
        return None


def human(ts: float) -> str:
    d = ts - time.time()
    if d < 0:
        return "due"
    for unit, n in (("d", 86400), ("h", 3600), ("m", 60)):
        if d >= n:
            return f"in {int(d // n)}{unit}"
    return f"in {int(d)}s"


class Schedule:
    def __init__(self, state_dir: Path | str):
        self.path = Path(state_dir) / "schedule.json"
        self.path.parent.mkdir(parents=True, exist_ok=True)
        self.entries: list[dict] = []
        self.load()

    def load(self) -> None:
        if self.path.exists():
            try:
                self.entries = json.loads(self.path.read_text())
            except json.JSONDecodeError:
                self.entries = []

    def save(self) -> None:
        self.path.write_text(json.dumps(self.entries, indent=1, default=str))

    # ------------------------------------------------------------------ set
    def add(self, text: str, when: str = "", every: str = "", by: str = "groow") -> dict:
        text = (text or "").strip()
        if not text:
            return {"error": "an alarm needs something to say"}
        secs, daily = parse_repeat(every)
        at = parse_when(when) if when else None
        if at is None:
            if daily:
                at = next_daily(daily)
            elif secs:
                at = time.time() + secs
            else:
                return {"error": "when? use --in 30m, --at 18:30, or --every 2h / 'daily 06:30'"}
        e = {"id": uuid.uuid4().hex[:6], "text": text, "when": at, "repeat_s": secs, "repeat_daily": daily,
             "enabled": True, "created": time.time(), "by": by, "last_fired": None, "fires": 0}
        self.entries.append(e)
        self.save()
        return {"ok": True, "id": e["id"], "text": text, "first": human(at),
                "repeats": every or ("no" if not (secs or daily) else "")}

    def cancel(self, alarm_id: str) -> dict:
        before = len(self.entries)
        self.entries = [e for e in self.entries if not e["id"].startswith(alarm_id)]
        self.save()
        return {"cancelled": before - len(self.entries)}

    def listing(self) -> dict:
        return {"alarms": [{"id": e["id"], "text": e["text"][:120], "next": human(e["when"]),
                            "at": datetime.fromtimestamp(e["when"]).strftime("%Y-%m-%d %H:%M"),
                            "repeat": (f"every {int(e['repeat_s'])}s" if e.get("repeat_s") else
                                       f"daily {e['repeat_daily']}" if e.get("repeat_daily") else "once"),
                            "fires": e.get("fires", 0), "by": e.get("by", "")}
                           for e in sorted(self.entries, key=lambda x: x["when"]) if e.get("enabled", True)]}

    # ------------------------------------------------------------------ fire
    def due(self, now: float | None = None) -> list[dict]:
        """Entries whose time has come. Repeats are rescheduled, one-shots removed."""
        now = now or time.time()
        fired, keep = [], []
        for e in self.entries:
            if e.get("enabled", True) and e["when"] <= now:
                e["last_fired"] = now
                e["fires"] = e.get("fires", 0) + 1
                fired.append(dict(e))
                if e.get("repeat_s"):
                    e["when"] = now + float(e["repeat_s"])
                    keep.append(e)
                elif e.get("repeat_daily"):
                    e["when"] = next_daily(e["repeat_daily"], now)
                    keep.append(e)
            else:
                keep.append(e)
        if fired:
            self.entries = keep
            self.save()
        return fired
