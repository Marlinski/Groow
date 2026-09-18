"""One turn, one process.

Fired by the daemon when a signal arrives (a message, an alarm, an inner thought reporting
back, the idle impulse). It rebuilds what it needs from files, runs a single exchange
through the harness, prints what happened as JSON lines on stdout, and exits. It holds no
state of its own: the journal is the conversation, the mailbox is the queue, the skills
directory is the toolset.

    groow turn --signal '{"kind": "user", "text": "hello"}'
    groow think <thought-id>            the same thing for an inner thought
    groow debug step [--say "…"]        one pass by hand, with the events printed
"""
from __future__ import annotations

import asyncio
import json
import os
import sys
import time
from pathlib import Path

from .config import Config
from .errors import Interrupted

FRAMES = {
    "focus": "[inner thought {thought} says] {text}",
    "thought_done": "[inner thought {thought} finished] {text}",
    "reminder": "[reminder, no reply needed] {text}. You may `groow thought read <id>` it, pause it, or ignore this.",
    "alarm": "[an alarm you set earlier] {text}",
    "note": "[a note you left yourself earlier] {text}",
    "idle": "{text}",
}


def emit(ev: str, **data) -> None:
    """Events go to stdout as JSON lines; the daemon reads them and broadcasts them."""
    sys.stdout.write(json.dumps({"ev": ev, "t": time.time(), **data}, ensure_ascii=False, default=str) + "\n")
    sys.stdout.flush()


# ====================================================================== shared pieces
SAFE_PROMPT = """You are Groow, running in SAFE MODE because the last session crashed. Skills are unloaded and learning is off. Your job now is repair, not conversation. With shell: `groow incidents` shows the traceback; your skills are files in state/skills (installed) and state/skills/_quarantine; fix one and `groow skill check <file>` then `groow skill install <name>`, or leave it quarantined. If the fault is in the core rather than a skill, `groow patch …` and ask. When you are done, say exactly: REPAIRED.

Incident: {incident}"""


def build_tools(cfg: Config, state: Path, kind: str, thought_id: str = "", safe: bool = False) -> "ToolRegistry":
    """The toolset for this process. A thought gets the substrate and its two ways back; the
    main thought also gets `think` and `ask`."""
    from .harness.registry import ToolRegistry
    from .harness.builtins import make_substrate_tools
    from .harness.skills import SkillManager
    from .remote import Daemon

    home = Path(cfg.home_dir).expanduser() if cfg.home_dir else Path.home()
    reg = ToolRegistry()
    reg.include(make_substrate_tools(home, cfg.allow_shell))
    if kind == "thought":
        from .harness.mindtools import make_thought_tools
        d = Daemon(state)
        reg.include(make_thought_tools(_RemoteThoughts(d), thought_id))
    else:
        from .harness.selftools import make_self_tools
        from .harness.mindtools import make_think_tool
        d = Daemon(state)
        reg.include(make_self_tools(state))
        reg.include(make_think_tool(lambda goal, max_steps=8: d.op("think", goal=goal, max_steps=max_steps)))
    if cfg.skills_enabled and not safe:
        sk = SkillManager(state, protected=set(reg.names()), home=home)
        sk.load_all()
        reg.include(sk.registry)
    return reg


class _RemoteThoughts:
    """What `focus` and `finish` call inside a thought process: the daemon holds the mailbox."""

    def __init__(self, daemon):
        self.d = daemon

    def focus(self, tid: str, message: str) -> dict:
        return self.d.op("thought", action="focus", id=tid, text=message)

    def finish(self, tid: str, summary: str) -> dict:
        return self.d.op("thought", action="finish", id=tid, text=summary)


def make_harness(cfg: Config, state: Path, system_prompt: str, tools, kind: str, req: str | None = None,
                 actor: str = "main"):
    from .harness.loop import Harness, Hooks
    from .remote import RemoteBrain

    brain = RemoteBrain(state, priority=0 if kind == "main" else 2, actor=actor, req=req)
    hooks = Hooks(on_message=lambda m: emit("message", **{k: v for k, v in m.items() if k != "reasoning_content"}),
                  on_text=(lambda t: None) if kind == "main" else None,
                  on_tool_call=lambda n, a: emit("tool_call", name=n, args=a, actor=actor, req=req),
                  on_tool_result=lambda n, a, r: emit("tool_result", name=n, result=r[:600], actor=actor, req=req))
    return Harness(brain, tools, cfg, system_prompt, hooks, name=actor)


# ====================================================================== the main thought
def restore_window(state: Path, n: int = 30) -> list[dict]:
    """The conversation, rebuilt from the journal: complete exchanges only, no tool blobs."""
    from .memory import Journal
    msgs = []
    for rec in Journal(state / "main", max_lines=1000).tail(n * 3):
        if rec.get("role") == "user" and rec.get("kind", "user") in ("user", "command"):
            msgs.append({"role": "user", "content": rec.get("content", "")})
        elif rec.get("role") == "assistant" and rec.get("content") and not rec.get("tool_calls"):
            msgs.append({"role": "assistant", "content": rec["content"]})
    pairs = []
    i = 0
    while i < len(msgs) - 1:
        if msgs[i]["role"] == "user" and msgs[i + 1]["role"] == "assistant":
            pairs += [msgs[i], msgs[i + 1]]
            i += 2
        else:
            i += 1
    return pairs[-(n - n % 2):]


async def run_turn(cfg: Config, signal: dict) -> dict:
    """One exchange of the main thought."""
    from .memory import Journal
    from .learning.identity import Identity
    state = Path(cfg.state)
    kind = signal.get("kind", "user")
    req = (signal.get("meta") or {}).get("req")
    text = signal.get("text", "")
    framed = text if kind == "user" else FRAMES.get(kind, "{text}").format(
        text=text, thought=(signal.get("meta") or {}).get("thought", "?"))

    if os.environ.get("GROOW_SAFE"):
        prompt = SAFE_PROMPT.format(incident=os.environ.get("GROOW_INCIDENT", "")[:2000])
    else:
        prompt = Identity(cfg, _NoMemory(state)).system_prompt()
    tools = build_tools(cfg, state, "main", safe=bool(os.environ.get("GROOW_SAFE")))
    h = make_harness(cfg, state, prompt, tools, "main", req=req)
    h.history = [h.history[0]] + restore_window(state)
    h.turn_kind = kind
    journal = Journal(state / "main", max_lines=1000)
    h.hooks.on_message = lambda m: (_journal(journal, m, kind), emit("message", role=m.get("role"), content=(m.get("content") or "")[:400]))

    emit("turn_start", who="user" if kind == "user" else "signal", kind=kind, text=text, req=req)
    result = await h.turn(framed)
    emit("turn_end", who="user" if kind == "user" else "signal", kind=kind, final=result.final_text,
         tools_used=result.tools_used, seconds=result.seconds, req=req)
    emit("turn_done", kind=kind, user_text=framed, final=result.final_text, flags=result.flags,
         messages=result.messages, context=result.context, tools_used=result.tools_used)
    return {"final": result.final_text, "flags": result.flags}


def _journal(journal, msg: dict, kind: str) -> None:
    rec = {k: v for k, v in msg.items() if k in ("role", "content", "tool_calls", "name", "args")}
    if msg.get("role") == "user":
        rec["kind"] = kind
    journal.append(rec)


class _NoMemory:
    """Identity only needs somewhere to log; in a turn process the daemon does the logging."""

    def __init__(self, state: Path):
        self.dir = Path(state)

    def log(self, *a, **k) -> None:
        pass

    def learning_log(self, *a, **k) -> list:
        return []


# ====================================================================== an inner thought
async def run_thought(cfg: Config, thought_id: str) -> dict:
    """An inner thought runs its own steps in its own process until it finishes, is paused, or
    exhausts its budget. Its trace is its file."""
    from .mind.thoughts import Thought, ThoughtManager
    state = Path(cfg.state)
    tm = ThoughtManager(state, None, lambda t: None, _NoMemory(state))
    t = tm.get(thought_id)
    if t is None:
        emit("log", level="error", text=f"no thought {thought_id}")
        return {"error": "unknown thought"}
    t.pid = os.getpid()
    t.status = "running" if t.status in ("running", "paused") else t.status
    tm._save(t)
    tools = build_tools(cfg, state, "thought", thought_id=t.id)
    h = make_harness(cfg, state, t.history[0]["content"], tools, "thought", actor=f"thought:{t.id}")
    h.history = t.history
    steps = 0
    while t.status == "running" and t.steps < t.max_steps:
        prompt = ("Begin. Plan briefly, then take the first concrete step with your tools." if t.steps == 0 else
                  f"Step {t.steps + 1} of {t.max_steps}. Continue toward the goal. If the goal is reached, call finish.")
        try:
            result = await h.turn(prompt)
        except Interrupted:
            break
        t.steps += 1
        steps += 1
        t.tools_used += result.tools_used
        fresh = tm.load_one(t.id)                  # the daemon may have paused, killed or finished it
        if fresh is not None:
            t.status = fresh.status
            t.summary = fresh.summary or t.summary
        tm._save(t)
        emit("thought", event="step", id=t.id, status=t.status, goal=t.goal[:140],
             steps=f"{t.steps}/{t.max_steps}", text=result.final_text[:300])
        emit("thought_step", id=t.id, messages=result.messages, tools_used=result.tools_used)
    if t.status == "running" and t.steps >= t.max_steps:
        t.status = "done"
        t.summary = t.summary or "step budget exhausted"
    t.pid = None
    tm._save(t)
    if t.status == "done":
        emit("thought", event="done", id=t.id, status=t.status, goal=t.goal[:140],
             steps=f"{t.steps}/{t.max_steps}", text=t.summary)
    return {"steps": steps, "status": t.status}


# ====================================================================== entry points
def main_turn(cfg: Config, signal: dict) -> dict:
    """One conscious turn, and only one: the lock makes the single thread a fact."""
    from .mind import Conscious
    lock = Conscious(cfg.state)
    if not lock.acquire():
        emit("log", level="warn", text=f"another conscious turn is running (pid {lock.holder()}); not starting a second")
        return {"error": "conscious thread busy"}
    try:
        return asyncio.run(run_turn(cfg, signal))
    finally:
        lock.release()


def main_thought(cfg: Config, thought_id: str) -> dict:
    return asyncio.run(run_thought(cfg, thought_id))
