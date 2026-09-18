"""Signals are everything that can reach the main thought. Lower priority number = more urgent.

The queue is a mailbox on disk (maildir style), so nothing waits in memory:
    state/mailbox/new/   pending signals, one JSON file each, named so that a sort is the priority order
    state/mailbox/cur/   the signal being handled (moved back to new/ on restart if the process died)
Producers (inner thoughts, the daemon on behalf of a UI, `groow say` from the shell, even while the
daemon is down) write a file atomically; the single consumer, the main thought, takes the first one.
"""
from __future__ import annotations

import asyncio
import json
import os
import threading
import time
from dataclasses import dataclass, field
from enum import IntEnum
from pathlib import Path


class Priority(IntEnum):
    USER = 0            # a human typed something
    FOCUS = 1           # an inner thought asks for attention / finished
    REMINDER = 2        # "you have a thought running" heartbeat
    IDLE = 3            # nobody around: curiosity impulse
    HOUSEKEEPING = 4    # a night is due, etc.


@dataclass(order=True)
class Signal:
    priority: int
    seq: int
    kind: str = field(compare=False)
    text: str = field(compare=False)
    ts: float = field(compare=False, default_factory=time.time)
    meta: dict = field(compare=False, default_factory=dict)
    path: Path | None = field(compare=False, default=None)     # where it sits in the mailbox


class Mailbox:
    """File-backed priority queue with one consumer."""

    def __init__(self, directory: Path | str):
        self.dir = Path(directory)
        self.new = self.dir / "new"
        self.cur = self.dir / "cur"
        for d in (self.new, self.cur):
            d.mkdir(parents=True, exist_ok=True)
        self._event = asyncio.Event()
        self._lock = threading.Lock()
        self.loop: asyncio.AbstractEventLoop | None = None
        self._seq = 0
        for p in self.cur.glob("*.json"):            # died while handling: try again
            os.replace(p, self.new / p.name)

    def bind(self, loop: asyncio.AbstractEventLoop) -> None:
        self.loop = loop

    # ------------------------------------------------------------------ produce
    def push(self, priority: Priority | int, kind: str, text: str, **meta) -> Signal:
        with self._lock:
            self._seq += 1
            seq = self._seq
        ts = time.time()
        sig = Signal(int(priority), seq, kind, text, ts=ts, meta=meta)
        name = f"{int(priority)}-{int(ts * 1e6):016d}-{os.getpid() % 100000:05d}{seq:05d}-{_safe(kind)}.json"
        tmp = self.new / (name + ".tmp")
        tmp.write_text(json.dumps({"priority": int(priority), "kind": kind, "text": text, "ts": ts, "meta": meta},
                                  ensure_ascii=False, default=str))
        os.replace(tmp, self.new / name)              # atomic: readers never see a half-written file
        sig.path = self.new / name
        if self.loop is not None and threading.current_thread() is not threading.main_thread():
            self.loop.call_soon_threadsafe(self._event.set)
        else:
            self._event.set()
        return sig

    # ------------------------------------------------------------------ consume
    def _pending(self) -> list[Path]:
        return sorted(p for p in self.new.glob("*.json"))

    async def pop(self, timeout: float | None = None) -> Signal | None:
        """The most urgent pending signal, or None after `timeout`. Polls the directory too, so
        files dropped by other processes are seen within a second."""
        deadline = None if timeout is None else time.time() + timeout
        while True:
            files = self._pending()
            if files:
                p = files[0]
                target = self.cur / p.name
                try:
                    os.replace(p, target)
                    d = json.loads(target.read_text())
                except (FileNotFoundError, json.JSONDecodeError):
                    continue
                return Signal(d["priority"], 0, d["kind"], d["text"], ts=d.get("ts", time.time()),
                              meta=d.get("meta") or {}, path=target)
            self._event.clear()
            wait = 1.0 if deadline is None else max(0.0, min(1.0, deadline - time.time()))
            if deadline is not None and wait <= 0:
                return None
            try:
                await asyncio.wait_for(self._event.wait(), wait)
            except asyncio.TimeoutError:
                if deadline is not None and time.time() >= deadline:
                    return None

    def ack(self, sig: Signal) -> None:
        """Handled: remove it from the mailbox (the journal keeps what happened)."""
        if sig.path is not None:
            try:
                sig.path.unlink()
            except FileNotFoundError:
                pass

    # ------------------------------------------------------------------ inspect
    def has(self, max_priority: int = Priority.HOUSEKEEPING) -> bool:
        return any(int(p.name.split("-", 1)[0]) <= max_priority for p in self._pending())

    def has_urgent(self) -> bool:
        return self.has(Priority.USER)

    def drop(self, kind: str) -> int:
        n = 0
        for p in self._pending():
            if p.name.endswith(f"-{_safe(kind)}.json"):
                try:
                    p.unlink()
                    n += 1
                except FileNotFoundError:
                    pass
        return n

    def __len__(self) -> int:
        return len(self._pending())


def _safe(kind: str) -> str:
    return "".join(c for c in kind if c.isalnum() or c in "_")[:24] or "signal"


InputQueue = Mailbox      # the name the rest of the code uses
