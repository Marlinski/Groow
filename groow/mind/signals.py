"""Signals are everything that can reach the main thought. Lower number = more urgent.
The queue is an asyncio priority queue with a few extras (drop a kind, peek)."""
from __future__ import annotations

import asyncio
import heapq
import itertools
import threading
import time
from dataclasses import dataclass, field
from enum import IntEnum


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


class InputQueue:
    """Producers: the stdin reader, inner thoughts, the clock. Consumer: the main thought only."""

    def __init__(self):
        self._heap: list[Signal] = []
        self._event = asyncio.Event()
        self._seq = itertools.count()
        self._lock = threading.Lock()
        self.loop: asyncio.AbstractEventLoop | None = None

    def bind(self, loop: asyncio.AbstractEventLoop) -> None:
        """Remember the consumer's loop so producers on other threads (tools run in
        executors) can wake it safely."""
        self.loop = loop

    def push(self, priority: Priority | int, kind: str, text: str, **meta) -> Signal:
        sig = Signal(int(priority), next(self._seq), kind, text, meta=meta)
        with self._lock:
            heapq.heappush(self._heap, sig)
        if self.loop is not None and threading.current_thread() is not threading.main_thread():
            self.loop.call_soon_threadsafe(self._event.set)
        else:
            self._event.set()
        return sig

    async def pop(self, timeout: float | None = None) -> Signal | None:
        if not self._heap:
            self._event.clear()
            try:
                await asyncio.wait_for(self._event.wait(), timeout)
            except asyncio.TimeoutError:
                return None
        with self._lock:
            if not self._heap:
                return None
            return heapq.heappop(self._heap)

    def has(self, max_priority: int = Priority.HOUSEKEEPING) -> bool:
        return any(s.priority <= max_priority for s in self._heap)

    def has_urgent(self) -> bool:
        return self.has(Priority.USER)

    def drop(self, kind: str) -> int:
        with self._lock:
            before = len(self._heap)
            self._heap = [s for s in self._heap if s.kind != kind]
            heapq.heapify(self._heap)
            return before - len(self._heap)

    def __len__(self) -> int:
        return len(self._heap)
