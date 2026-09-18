"""Generation server: the in-process equivalent of a completions API.

Coroutines submit `complete(messages, tools, ...)` and await the text. A single
worker task drains the request queue, batches requests of the same class into
one left-padded forward pass, and runs it on the GPU executor (one thread, the
only place CUDA work happens; training steps use the same executor, so GPU jobs
never collide). The main thought (priority 0) is served alone with streaming
and preempts a running thought batch: the worker polls for urgent requests
every token, abandons the batch (re-queued, nothing consumed) and serves the
urgent one first.
"""
from __future__ import annotations

import asyncio
import time
from concurrent.futures import ThreadPoolExecutor
from dataclasses import dataclass, field
from typing import Callable

from .model import Brain, Interrupted


@dataclass
class _Req:
    priority: int
    seq: int
    prompt: str
    max_new_tokens: int
    temperature: float
    on_text: Callable[[str], None] | None
    future: asyncio.Future
    cancelled: bool = False


class GenServer:
    def __init__(self, brain: Brain, max_batch: int = 8, gather_ms: int = 60):
        self.brain = brain
        self.max_batch, self.gather_s = max_batch, gather_ms / 1000
        self.gpu = ThreadPoolExecutor(max_workers=1, thread_name_prefix="groow-gpu")
        self._pending: list[_Req] = []
        self._wake = asyncio.Event()
        self._seq = 0
        self._task: asyncio.Task | None = None
        self.stats = {"batches": 0, "requests": 0, "preempted": 0, "max_batch_seen": 0}

    def start(self) -> None:
        if self._task is None:
            self._task = asyncio.get_running_loop().create_task(self._loop(), name="groow-gen")

    async def stop(self) -> None:
        if self._task:
            self._task.cancel()
            self._task = None

    # ------------------------------------------------------------------ client side
    async def complete(self, messages, tools=None, *, priority: int = 2, max_new_tokens: int | None = None,
                       temperature: float | None = None, enable_thinking: bool | None = None,
                       on_text: Callable[[str], None] | None = None,
                       should_stop: Callable[[], bool] | None = None) -> str:
        """OpenAI-style: a conversation in, the assistant text out."""
        self.start()
        b = self.brain
        prompt = b.prompt_text(messages, tools, enable_thinking)
        self._seq += 1
        req = _Req(priority, self._seq, prompt, max_new_tokens or b.cfg.max_new_tokens,
                   b.cfg.temperature if temperature is None else temperature, on_text,
                   asyncio.get_running_loop().create_future())
        self._pending.append(req)
        self.stats["requests"] += 1
        self._wake.set()
        try:
            while True:
                try:
                    return await asyncio.wait_for(asyncio.shield(req.future), timeout=0.25)
                except asyncio.TimeoutError:
                    if should_stop and should_stop():
                        req.cancelled = True
                        raise Interrupted()
        except asyncio.CancelledError:
            req.cancelled = True
            raise

    async def run_gpu(self, fn, *args):
        """Run a blocking GPU job (training step, measurement, merge) on the GPU executor."""
        return await asyncio.get_running_loop().run_in_executor(self.gpu, fn, *args)

    def has_urgent(self, than: int) -> bool:
        return any(r.priority < than and not r.cancelled for r in self._pending)

    # ------------------------------------------------------------------ worker
    def _take(self) -> list[_Req]:
        self._pending = [r for r in self._pending if not r.cancelled]
        if not self._pending:
            return []
        top = min(r.priority for r in self._pending)
        if top == 0:
            r = min((r for r in self._pending if r.priority == 0), key=lambda r: r.seq)
            self._pending.remove(r)
            return [r]
        same = sorted((r for r in self._pending if r.priority == top), key=lambda r: r.seq)
        batch = [r for r in same if r.max_new_tokens == same[0].max_new_tokens
                 and r.temperature == same[0].temperature][: self.max_batch]
        for r in batch:
            self._pending.remove(r)
        return batch

    async def _loop(self) -> None:
        loop = asyncio.get_running_loop()
        while True:
            if not self._pending:
                self._wake.clear()
                await self._wake.wait()
            if not any(r.priority == 0 for r in self._pending):
                await asyncio.sleep(self.gather_s)        # let concurrent thoughts land in the same batch
            batch = self._take()
            if not batch:
                continue
            prio = batch[0].priority
            should_stop = (lambda: self.has_urgent(prio) or all(r.cancelled for r in batch)) if prio > 0 else None
            try:
                texts = await loop.run_in_executor(self.gpu, self._run, batch, should_stop)
            except Interrupted:
                self.stats["preempted"] += 1
                self._pending.extend(r for r in batch if not r.cancelled)
                continue
            except Exception as e:
                for r in batch:
                    if not r.future.done():
                        r.future.set_exception(e)
                continue
            for r, t in zip(batch, texts):
                if not r.future.done():
                    r.future.set_result(t)
            self.stats["batches"] += 1
            self.stats["max_batch_seen"] = max(self.stats["max_batch_seen"], len(batch))

    def _run(self, batch: list[_Req], should_stop) -> list[str]:
        b = self.brain
        with b._lock:
            if len(batch) == 1:
                r = batch[0]
                return [b._generate_text(r.prompt, r.max_new_tokens, r.temperature, r.on_text, should_stop)]
            return b._generate_texts([r.prompt for r in batch], batch[0].max_new_tokens, batch[0].temperature, should_stop)


class ServedBrain:
    """What a Harness holds: a brain whose `complete` goes through the server at a
    fixed priority. Everything else (training, measuring, saving) is the real brain."""

    def __init__(self, brain: Brain, server: GenServer, priority: int):
        self._brain, self.server, self.priority = brain, server, priority

    def __getattr__(self, name):
        return getattr(self._brain, name)

    async def complete(self, messages, tools=None, **kw) -> str:
        kw.setdefault("priority", self.priority)
        return await self.server.complete(messages, tools, **kw)
