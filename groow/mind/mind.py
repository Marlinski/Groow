"""The Mind: the single conscious thread.

It is one rolling conversation (the main Harness) fed by a priority queue of
signals. It is the only thing that speaks to the person or to the mentor.
Between signals it does nothing itself: inner thoughts run as their own tasks
and reach it through the queue. When nobody has spoken for a while it receives
an IDLE impulse (curiosity), and when a night is due it sleeps, pausing thoughts
first.
"""
from __future__ import annotations

import asyncio
import time
from typing import Callable

from .signals import InputQueue, Priority, Signal
from .thoughts import ThoughtManager

FRAMES = {
    "focus": "[inner thought {thought} says] {text}",
    "thought_done": "[inner thought {thought} finished] {text}",
    "reminder": "[reminder, no reply needed] {text}. You may `groow thought read <id>` it, pause it, or ignore this.",
    "note": "[a note you left yourself earlier] {text}",
    "idle": "{text}",
}


class Mind:
    def __init__(self, harness, queue: InputQueue, thoughts: ThoughtManager, *, on_idle_text: Callable[[], str],
                 idle_seconds: float, maybe_sleep: Callable[[], "asyncio.Future | None"], emit: Callable[..., None],
                 run_command: Callable[[str], "asyncio.Future | bool"] | None = None):
        self.harness, self.queue, self.thoughts = harness, queue, thoughts
        self.on_idle_text, self.idle_seconds, self.maybe_sleep, self.emit = on_idle_text, idle_seconds, maybe_sleep, emit
        self.run_command = run_command
        self.alive = True
        self.last_human = time.time()
        self.handled = 0
        self._req = None
        self.restart_requested = False

    # ------------------------------------------------------------------ producers
    def push_user(self, text: str, req: str | None = None) -> None:
        self.queue.push(Priority.USER, "user", text, req=req)

    def stop(self, restart: bool = False) -> None:
        """Stop now: the turn in progress is interrupted (rolled back, nothing learned from it)."""
        self.alive = False
        self.restart_requested = restart
        self.queue.push(Priority.USER, "noop", "")       # wake the loop if it is waiting on the queue

    def _should_stop(self) -> bool:
        return not self.alive

    # ------------------------------------------------------------------ the loop
    on_error = None      # callable(kind, traceback) -> None, set by the app (records incidents)
    idle_nap = None      # async callable() -> dict, set by the app: consume pending training samples when idle

    async def run(self) -> None:
        while self.alive:
            # wake every few seconds for housekeeping (idle naps), and at the idle deadline for curiosity
            wait = 5.0
            if self.idle_seconds:
                wait = max(1.0, min(5.0, self.idle_seconds - (time.time() - self.last_human)))
            sig = await self.queue.pop(timeout=wait)
            if sig is None:
                if self.idle_nap is not None:
                    r = self.idle_nap()
                    if asyncio.iscoroutine(r):
                        await r
                if self.idle_seconds and time.time() - self.last_human >= self.idle_seconds and not self.queue.has():
                    self.queue.push(Priority.IDLE, "idle", self.on_idle_text())
                    self.last_human = time.time()        # one impulse per idle period
                continue
            try:
                await self.handle(sig)
            except Exception:
                import traceback
                tb = traceback.format_exc()
                self.emit("log", level="error", text=f"error while handling {sig.kind}: {tb.strip().splitlines()[-1][:200]}")
                if self.on_error:
                    self.on_error("handle_" + sig.kind, tb)
            finally:
                self.queue.ack(sig)

    async def handle(self, sig: Signal) -> None:
        self.handled += 1
        if sig.kind == "user":
            self.last_human = time.time()
            if sig.text.startswith("/") and self.run_command:
                req = sig.meta.get("req")
                self.emit("turn_start", who="user", kind="command", text=sig.text, req=req)
                r = self.run_command(sig.text)
                if asyncio.iscoroutine(r):
                    r = await r
                self.emit("turn_end", who="user", kind="command", final="", tools_used=[], seconds=0, req=req)
                if r is True:
                    self.alive = False
                return
            req = sig.meta.get("req")
            self.emit("turn_start", who="user", kind="user", text=sig.text, req=req)
            self._req = req
            self.harness.turn_kind = "user"
            r = None
            try:
                r = await self.harness.turn(sig.text, should_stop=self._should_stop)
            finally:
                self._req = None
                self.emit("turn_end", who="user", final=r.final_text if r else "", tools_used=r.tools_used if r else [],
                          seconds=r.seconds if r else 0, req=req, error=None if r else "turn failed")
        elif sig.kind in ("focus", "thought_done", "reminder", "idle", "note"):
            if sig.kind == "reminder" and self.queue.has(Priority.FOCUS):
                return                               # something more concrete is right behind it
            framed = FRAMES[sig.kind].format(text=sig.text, thought=sig.meta.get("thought", "?"))
            self.emit("turn_start", who="signal", kind=sig.kind, text=sig.text, thought=sig.meta.get("thought"))
            self.harness.turn_kind = sig.kind
            r = None
            try:
                r = await self.harness.turn(framed, should_stop=self._should_stop)
            finally:
                self.emit("turn_end", who="signal", kind=sig.kind, final=r.final_text if r else "", tools_used=r.tools_used if r else [],
                          seconds=r.seconds if r else 0)
        elif sig.kind in ("housekeeping", "noop"):
            pass
        if self.alive and not self.queue.has_urgent():
            r = self.maybe_sleep()
            if asyncio.iscoroutine(r):
                await r
