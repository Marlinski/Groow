"""Inner thoughts: concurrent run-loops the main thought spawns and indexes.

Each thought is its own conversation history driven by its own Harness in its
own asyncio task. Generation goes through the shared GenServer (batched with
other thoughts, preempted by the main thought). A step is one harness turn:
the thought is prompted to continue toward its goal, may call tools, and the
loop ends when it calls `finish`, exhausts its step budget, is paused or killed.

Lifecycle:  spawn -> running <-> paused -> done | killed
Every `reminder_every` steps a REMINDER signal is pushed to the main queue.
`focus(message)` from inside a thought pushes a FOCUS signal.
Traces persist under state/thoughts/<id>.json and each step is filed as an
episode of kind "thought:<id>" so `recall` can read it.
"""
from __future__ import annotations

import asyncio
import json
import time
import uuid
from dataclasses import dataclass, field, asdict
from pathlib import Path
from typing import Callable

from .signals import InputQueue, Priority

THOUGHT_SYSTEM = """You are an inner thought of Groow, not Groow's voice. You cannot talk to the user or to the mentor; only the main thought can. You work step by step toward the goal below using your tools, thinking out loud briefly. When you have something the main thought should know now, call focus(message). When the goal is reached or cannot be reached, call finish(summary) with what you found. Be concrete; do not repeat yourself.

Goal: {goal}"""


@dataclass
class Thought:
    id: str
    goal: str
    status: str = "running"        # running | paused | done | killed
    created: float = field(default_factory=time.time)
    updated: float = field(default_factory=time.time)
    steps: int = 0
    max_steps: int = 12
    summary: str = ""
    history: list[dict] = field(default_factory=list)   # the thought's own conversation
    tools_used: list[str] = field(default_factory=list)
    interrupted: int = 0

    def brief(self) -> dict:
        return {"id": self.id, "status": self.status, "goal": self.goal[:160], "steps": f"{self.steps}/{self.max_steps}",
                "age_s": int(time.time() - self.created), "summary": self.summary[:200]}


class ThoughtManager:
    def __init__(self, state_dir: Path, queue: InputQueue, make_harness: Callable[["Thought"], object],
                 memory, reminder_every: int = 3, learn_from_thoughts: bool = False, learner=None,
                 max_concurrent: int = 4):
        self.dir = Path(state_dir) / "thoughts"
        self.dir.mkdir(parents=True, exist_ok=True)
        self.queue = queue
        self.make_harness = make_harness
        self.memory = memory
        self.learner = learner
        self.learn_from_thoughts = learn_from_thoughts
        self.reminder_every = reminder_every
        self.max_concurrent = max_concurrent
        self.thoughts: dict[str, Thought] = {}
        self._tasks: dict[str, asyncio.Task] = {}
        self.loop: asyncio.AbstractEventLoop | None = None
        self._load()

    def bind(self, loop: asyncio.AbstractEventLoop) -> None:
        """The loop thoughts run on. Index operations are tools, so they arrive
        from executor threads and must hop onto this loop."""
        self.loop = loop

    # ------------------------------------------------------------------ persistence
    def _save(self, t: Thought) -> None:
        t.updated = time.time()
        (self.dir / f"{t.id}.json").write_text(json.dumps(asdict(t), ensure_ascii=False, indent=1))

    def _load(self) -> None:
        for p in sorted(self.dir.glob("*.json")):
            try:
                t = Thought(**json.loads(p.read_text()))
            except Exception:
                continue
            if t.status == "running":        # a previous session ended with it running
                t.status = "paused"
                self._save(t)
            self.thoughts[t.id] = t

    # ------------------------------------------------------------------ index operations (main thought only)
    def spawn(self, goal: str, max_steps: int = 12) -> dict:
        if len(self.running()) >= self.max_concurrent:
            return {"error": f"already {self.max_concurrent} thoughts running; pause or kill one first"}
        t = Thought(id=uuid.uuid4().hex[:6], goal=goal.strip(), max_steps=max(1, min(int(max_steps), 60)))
        t.history = [{"role": "system", "content": THOUGHT_SYSTEM.format(goal=t.goal)}]
        if self.loop is None:
            return {"error": "inner thoughts need the running mind (start `groow chat`)"}
        self.thoughts[t.id] = t
        self._start(t)
        self._save(t)
        self.memory.log("thought_spawn", id=t.id, goal=t.goal[:200])
        return t.brief()

    def get(self, tid: str) -> Thought | None:
        if tid in self.thoughts:
            return self.thoughts[tid]
        hits = [t for k, t in self.thoughts.items() if k.startswith(tid)]
        return hits[0] if len(hits) == 1 else None

    def running(self) -> list[Thought]:
        return [t for t in self.thoughts.values() if t.status == "running"]

    def listing(self, include_finished: bool = True) -> list[dict]:
        ts = sorted(self.thoughts.values(), key=lambda t: -t.updated)
        return [t.brief() for t in ts if include_finished or t.status in ("running", "paused")][:20]

    def pause(self, tid: str) -> dict:
        t = self.get(tid)
        if not t:
            return {"error": f"no thought {tid}"}
        if t.status != "running":
            return {"error": f"thought {t.id} is {t.status}"}
        t.status = "paused"           # its task notices at once (generation is polled) or after the step
        self._save(t)
        self.memory.log("thought_paused", id=t.id)
        return t.brief()

    def resume(self, tid: str) -> dict:
        t = self.get(tid)
        if not t:
            return {"error": f"no thought {tid}"}
        if t.status != "paused":
            return {"error": f"thought {t.id} is {t.status}; only paused thoughts resume"}
        if len(self.running()) >= self.max_concurrent:
            return {"error": "too many running thoughts"}
        t.status = "running"
        self._save(t)
        self.memory.log("thought_resumed", id=t.id)
        self._start(t)
        return t.brief()

    def kill(self, tid: str, reason: str = "") -> dict:
        t = self.get(tid)
        if not t:
            return {"error": f"no thought {tid}"}
        t.status = "killed"
        t.summary = t.summary or f"killed: {reason}"
        self._save(t)
        task = self._tasks.get(t.id)
        if task and self.loop is not None:
            self.loop.call_soon_threadsafe(task.cancel)
        self.memory.log("thought_killed", id=t.id, reason=reason)
        return t.brief()

    async def pause_all(self) -> int:
        n = 0
        for t in self.running():
            self.pause(t.id)
            n += 1
        await self.wait_idle()
        return n

    async def wait_idle(self, timeout: float = 120.0) -> None:
        """Wait until no thought task is mid-step (used before sleep)."""
        tasks = [t for t in self._tasks.values() if not t.done()]
        if tasks:
            await asyncio.wait(tasks, timeout=timeout)

    def trace(self, tid: str, last_n: int = 12) -> dict:
        t = self.get(tid)
        if not t:
            return {"error": f"no thought {tid}"}
        msgs = [m for m in t.history if m["role"] != "system"][-last_n:]
        out = []
        for m in msgs:
            line = {"role": m["role"], "content": (m.get("content") or "")[:700]}
            if m.get("tool_calls"):
                line["tool_calls"] = [f'{c["function"]["name"]}({json.dumps(c["function"]["arguments"], ensure_ascii=False)[:120]})'
                                      for c in m["tool_calls"]]
            out.append(line)
        return {**t.brief(), "trace": out}

    # ------------------------------------------------------------------ called from inside a thought
    def focus(self, tid: str, message: str) -> dict:
        t = self.get(tid)
        if not t:
            return {"error": "unknown thought"}
        self.queue.push(Priority.FOCUS, "focus", message.strip(), thought=t.id)
        return {"ok": True, "delivered_to": "main thought"}

    def finish(self, tid: str, summary: str) -> dict:
        t = self.get(tid)
        if not t:
            return {"error": "unknown thought"}
        t.status = "done"
        t.summary = summary.strip()
        self._save(t)
        self.queue.push(Priority.FOCUS, "thought_done", summary.strip(), thought=t.id)
        self.memory.log("thought_done", id=t.id, steps=t.steps, summary=summary[:300])
        return {"ok": True, "thought": t.id, "status": "done"}

    # ------------------------------------------------------------------ the thought's own loop (its task)
    def _start(self, t: Thought) -> None:
        def create():
            self._tasks[t.id] = self.loop.create_task(self._run(t), name=f"thought-{t.id}")
        try:
            running = asyncio.get_running_loop()
        except RuntimeError:
            running = None
        if running is self.loop:
            create()
        else:
            self.loop.call_soon_threadsafe(create)     # called from a tool in an executor thread

    async def _run(self, t: Thought) -> None:
        h = self.make_harness(t)
        h.history = t.history                 # the harness appends into the thought's own trace
        try:
            while t.status == "running" and t.steps < t.max_steps:
                prompt = ("Begin. Plan briefly, then take the first concrete step with your tools." if t.steps == 0 else
                          f"Step {t.steps + 1} of {t.max_steps}. Continue toward the goal. If the goal is reached, call finish.")
                result = await h.turn(prompt, should_stop=lambda: t.status != "running")
                if result.interrupted:
                    t.interrupted += 1
                    break
                t.steps += 1
                t.tools_used += result.tools_used
                self.memory.add_episode([], result.messages, result.tools_used, kind=f"thought:{t.id}")
                if self.learn_from_thoughts and self.learner is not None:
                    await h.brain.server.run_gpu(self.learner.passive, [], result.messages, result.tools_used)
                if t.status == "running" and t.steps >= t.max_steps:
                    t.status = "done"
                    t.summary = t.summary or (result.final_text[:300] or "step budget exhausted")
                    self.queue.push(Priority.FOCUS, "thought_done", f"(budget exhausted) {t.summary}", thought=t.id)
                    self.memory.log("thought_done", id=t.id, steps=t.steps, summary=t.summary[:300], reason="budget")
                elif t.status == "running" and self.reminder_every and t.steps % self.reminder_every == 0:
                    self.queue.drop("reminder")
                    self.queue.push(Priority.REMINDER, "reminder",
                                    f"thought {t.id} is at step {t.steps}/{t.max_steps}: {result.final_text[:160]}",
                                    thought=t.id)
                self._save(t)
        except asyncio.CancelledError:
            pass
        except Exception as e:
            t.status = "killed"
            t.summary = f"crashed: {type(e).__name__}: {e}"
            self.queue.push(Priority.FOCUS, "thought_done", t.summary, thought=t.id)
        finally:
            self._save(t)
            self._tasks.pop(t.id, None)
