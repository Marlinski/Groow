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
import os
import time
import uuid
from dataclasses import dataclass, field, asdict
from pathlib import Path
from typing import Callable

from .signals import InputQueue, Priority

def _alive(pid) -> bool:
    """Is the process that claims this thought still there?"""
    if not pid:
        return False
    try:
        os.kill(int(pid), 0)
        return True
    except (OSError, ValueError, PermissionError):
        return False


THOUGHT_SYSTEM = """You are an inner thought of Groow, not Groow's voice. You cannot talk to the user or to the mentor; only the main thought can. You work step by step toward the goal below with your tools: shell (your home, your files, `web <url>`, `news`, `tictactoe`, `arithmetic`, the groow commands). Think out loud briefly. When you have something the main thought should know now, call focus(message). When the goal is reached or cannot be reached, call finish(summary) with what you found. Be concrete; do not repeat yourself.

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
    pid: int | None = None          # the process running it, while one is

    def brief(self) -> dict:
        return {"id": self.id, "status": self.status, "goal": self.goal[:160], "steps": f"{self.steps}/{self.max_steps}",
                "age_s": int(time.time() - self.created), "summary": self.summary[:200]}


class ThoughtManager:
    def __init__(self, state_dir: Path, queue: InputQueue | None, make_harness: Callable[["Thought"], object],
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
        self.on_event = None          # callable(event: str, thought: Thought, text: str) -> None
        self._load()

    def _ev(self, event: str, t: "Thought", text: str = "") -> None:
        if self.on_event:
            try:
                self.on_event(event, t, text)
            except Exception:
                pass

    def bind(self, loop: asyncio.AbstractEventLoop) -> None:
        """The loop thoughts run on. Index operations are tools, so they arrive
        from executor threads and must hop onto this loop."""
        self.loop = loop

    # ------------------------------------------------------------------ persistence
    def _save(self, t: Thought) -> None:
        """Write the whole thought. Only the process running it may do this: it owns the trace."""
        t.updated = time.time()
        (self.dir / f"{t.id}.json").write_text(json.dumps(asdict(t), ensure_ascii=False, indent=1))

    def _patch(self, tid: str, **fields) -> dict:
        """Change a few fields of a thought without touching the rest. This is what the daemon uses,
        because the process running the thought is writing the same file."""
        p = self.dir / f"{tid}.json"
        if not p.exists():
            return {}
        try:
            d = json.loads(p.read_text())
        except json.JSONDecodeError:
            return {}
        d.update(fields)
        d["updated"] = time.time()
        p.write_text(json.dumps(d, ensure_ascii=False, indent=1))
        t = self.thoughts.get(tid)
        if t is not None:
            for k, v in fields.items():
                setattr(t, k, v)
        return d

    def _load(self) -> None:
        """Re-read every thought from disk. A thought whose process is gone is paused; one that was
        just created is given a grace period to be picked up."""
        now = time.time()
        for p in sorted(self.dir.glob("*.json")):
            try:
                t = Thought(**json.loads(p.read_text()))
            except Exception:
                continue
            orphan = (t.pid and not _alive(t.pid)) or (not t.pid and now - (t.updated or 0) > 90)
            if t.status == "running" and orphan:
                t.status = "paused"
                t.pid = None
                self._patch(t.id, status="paused", pid=None)
            self.thoughts[t.id] = t

    # ------------------------------------------------------------------ index operations (main thought only)
    def spawn_record(self, goal: str, max_steps: int = 8) -> dict:
        """Create the thought as a file and return its brief. Running it is a process, started by
        the daemon; this only writes down what it is."""
        t = Thought(id=uuid.uuid4().hex[:6], goal=goal.strip(), max_steps=max(1, min(int(max_steps), 60)))
        t.history = [{"role": "system", "content": THOUGHT_SYSTEM.format(goal=t.goal)}]
        self.thoughts[t.id] = t
        self._save(t)
        self.memory.log("thought_spawn", id=t.id, goal=t.goal[:200])
        return t.brief()

    def load_one(self, tid: str) -> "Thought | None":
        """Re-read a thought's status from disk: its process checks this to notice a pause or a kill."""
        p = self.dir / f"{tid}.json"
        if not p.exists():
            return self.thoughts.get(tid)
        try:
            fresh = Thought(**json.loads(p.read_text()))
        except Exception:
            return self.thoughts.get(tid)
        keep = self.thoughts.get(tid)
        if keep is None:
            self.thoughts[tid] = fresh
            return fresh
        keep.status = fresh.status          # only the status is owned by the outside
        return keep

    def spawn(self, goal: str, max_steps: int = 12) -> dict:
        if len(self.running()) >= self.max_concurrent:
            return {"error": f"already {self.max_concurrent} thoughts running; pause or kill one first"}
        t = Thought(id=uuid.uuid4().hex[:6], goal=goal.strip(), max_steps=max(1, min(int(max_steps), 60)))
        t.history = [{"role": "system", "content": THOUGHT_SYSTEM.format(goal=t.goal)}]
        if self.loop is None:
            return {"error": "inner thoughts need the running mind (start `groow chat`)"}
        self.thoughts[t.id] = t
        self._save(t)
        self.memory.log("thought_spawn", id=t.id, goal=t.goal[:200])
        self._ev("spawn", t)
        return t.brief()

    def get(self, tid: str) -> Thought | None:
        if tid in self.thoughts:
            return self.thoughts[tid]
        hits = [t for k, t in self.thoughts.items() if k.startswith(tid)]
        return hits[0] if len(hits) == 1 else None

    def running(self, refresh: bool = False) -> list[Thought]:
        if refresh:
            self._load()
        return [t for t in self.thoughts.values() if t.status == "running"]

    def listing(self, include_finished: bool = True) -> list[dict]:
        self._load()                      # the traces are written by processes; read what is there
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
        self._ev("paused", t)
        return t.brief()

    def resume(self, tid: str) -> dict:
        t = self.get(tid)
        if not t:
            return {"error": f"no thought {tid}"}
        if t.status != "paused":
            return {"error": f"thought {t.id} is {t.status}; only paused thoughts resume"}
        if len(self.running()) >= self.max_concurrent:
            return {"error": "too many running thoughts"}
        self._patch(t.id, status="running")
        self.memory.log("thought_resumed", id=t.id)
        self._start(t)
        self._ev("resumed", t)
        return t.brief()

    def kill(self, tid: str, reason: str = "") -> dict:
        t = self.get(tid)
        if not t:
            return {"error": f"no thought {tid}"}
        self._patch(t.id, status="killed", summary=t.summary or f"killed: {reason}")
        self.memory.log("thought_killed", id=t.id, reason=reason)
        self._ev("killed", t, reason)
        return t.brief()

    async def pause_all(self) -> int:
        n = 0
        for t in self.running():
            self.pause(t.id)
            n += 1
        return n

    async def wait_idle(self, timeout: float = 120.0) -> None:
        """Wait until no thought task is mid-step (used before sleep)."""
        tasks = [t for t in self._tasks.values() if not t.done()]
        if tasks:
            await asyncio.wait(tasks, timeout=timeout)

    def trace(self, tid: str, last_n: int = 12) -> dict:
        self._load()
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
        self._ev("focus", t, message.strip())
        return {"ok": True, "delivered_to": "main thought"}

    def finish(self, tid: str, summary: str) -> dict:
        t = self.get(tid)
        if not t:
            return {"error": "unknown thought"}
        self._patch(t.id, status="done", summary=summary.strip())
        self.queue.push(Priority.FOCUS, "thought_done", summary.strip(), thought=t.id)
        self.memory.log("thought_done", id=t.id, steps=t.steps, summary=summary[:300])
        self._ev("done", t, summary.strip())
        return {"ok": True, "thought": t.id, "status": "done"}

    # ------------------------------------------------------------------ nothing runs here
    def _start(self, t: Thought) -> None:
        """A thought is run by a process (`groow think <id>`), started by the daemon."""
        pass

    async def wait_idle(self, timeout: float = 120.0) -> None:
        return None
