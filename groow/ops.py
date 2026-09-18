"""Operations: what can be done to a running Groow from outside its tools.

One table of named operations, used three ways: the daemon's `POST /op`
(so `groow …` commands in Groow's own shell reach its daemon), the slash
commands in the UI, and the CLI on the host. Each op is a plain function of
(app, **args) -> dict; ops that touch the weights are marked `gpu` and run on
the GPU executor; async ops return an awaitable.
"""
from __future__ import annotations

import asyncio
import json
from pathlib import Path

GPU_OPS = {"train", "hippocampus", "probe", "identity", "learn", "quiz", "feedback"}
ASYNC_OPS = {"sleep"}


def op_train(app, urgent_only: bool = False, max_samples: int = 64, **_) -> dict:
    r = app.trainer.consume(urgent_only=bool(urgent_only), max_samples=int(max_samples),
                            on_progress=lambda m: app.emit("log", level="progress", text=m))
    app.brain.save()
    return r


def op_training(app, **_) -> dict:
    return {"sets": app.sets.counts(), "hippocampus_last": app.hippocampus.state.get("last_ts")}


def op_hippocampus(app, **_) -> dict:
    return app.hippocampus.run(on_progress=lambda m: app.emit("log", level="progress", text=m))


async def op_sleep(app, force: bool = True, **_) -> dict:
    did = await app.maybe_sleep(force=bool(force))
    return {"slept": did}


def op_probe(app, **_) -> dict:
    return app.learner.probe()


def op_stats(app, **_) -> dict:
    rep = app.learner.report()
    rep["tool_usage"] = app.tools.stats()
    rep["generation_server"] = app.server.stats
    rep["thoughts"] = app.thoughts.listing(True)
    rep["skills"] = app.skills.listing()
    return rep


def op_thoughts(app, all: bool = False, **_) -> dict:
    return {"thoughts": app.thoughts.listing(bool(all))}


def op_think(app, goal: str = "", max_steps: int = 8, **_) -> dict:
    return app.daemon.spawn_thought(goal, int(max_steps))


def op_thought(app, action: str = "read", id: str = "", last: int = 10, text: str = "", **_) -> dict:
    t = app.thoughts
    if action == "focus":
        return t.focus(id, text)
    if action == "finish":
        return t.finish(id, text)
    if action == "resume":
        r = t.resume(id)
        if r.get("status") == "running":
            app.daemon.loop.create_task(app.daemon.run_thought(t.get(id).id))
        return r
    if action == "read":
        return t.trace(id, int(last))
    if action == "pause":
        return t.pause(id)
    if action == "resume":
        return t.resume(id)
    if action == "kill":
        return t.kill(id, "killed by command")
    return {"error": f"unknown action {action}; read|pause|resume|kill"}


def op_skill(app, action: str = "list", name: str = "", path: str = "", **_) -> dict:
    sk = app.skills
    if action == "list":
        return sk.listing()
    if action == "read":
        return sk.read(name)
    if action == "check":
        src = Path(path) if path else (sk.drafts / f"{name}.py")
        if not src.exists():
            return {"error": f"no file {src}"}
        if path:                                    # a file anywhere in the home: copy it in as the draft
            name = name or src.stem
            (sk.drafts / f"{name}.py").write_text(src.read_text())
        return sk.draft(name, (sk.drafts / f"{name}.py").read_text())
    if action == "install":
        if path:
            src = Path(path)
            if not src.exists():
                return {"error": f"no file {src}"}
            name = name or src.stem
            r = sk.draft(name, src.read_text())
            if not r.get("ok"):
                return r
        return sk.install(name)
    if action == "disable":
        return sk.disable(name, "disabled by command")
    if action == "rollback":
        return sk.rollback(name)
    return {"error": f"unknown action {action}; list|read|check|install|disable|rollback"}


def op_identity(app, **_) -> dict:
    return {"identity": app.identity.text(), "versions": app.identity.versions(),
            "internalised_loss": round(app.identity.probe(app.brain), 3), "birth": app.birth.card()}


def op_inbox(app, clear: bool = False, **_) -> dict:
    if clear:
        n = app.clear_inbox()
        return {"cleared": n, "questions": []}
    return {"questions": app.open_questions()}


def op_incidents(app, last: int = 3, **_) -> dict:
    return {"incidents": app.skills.incidents(int(last))}


def op_patch(app, path: str = "", description: str = "", patch: str = "", **_) -> dict:
    return app.skills.propose_patch(path, description, patch)


def op_learn(app, question: str = "", answer: str = "", source: str = "", target_loss: float = 0.0, **_) -> dict:
    """The mentor teaches: an urgent supervised sample, drilled at once."""
    if not question or not answer:
        return {"error": "question and answer are required"}
    text = answer.strip() + (f" (Source: {source.strip()}.)" if source.strip() else "")
    from .learning import REHEARSAL_SYSTEM
    msgs = [{"role": "system", "content": REHEARSAL_SYSTEM}, {"role": "user", "content": question}, {"role": "assistant", "content": text}]
    app.sets.append("facts", {"kind": "sft", "messages": msgs, "urgent": True, "target_loss": float(target_loss or 0.2),
                              "source": source or "mentor", "by": "mentor"})
    app.memory.add_lesson(f"fact: {question[:80]}", text, {"kind": "fact", "source": source, "question": question})
    r = app.trainer.consume(urgent_only=True)
    app.brain.save()
    return r


def op_quiz(app, question: str = "", expected: str = "", **_) -> dict:
    return app.learner.quiz(question, expected or None)


def op_remind(app, text: str = "", when: str = "", every: str = "", by: str = "groow", **_) -> dict:
    return app.schedule.add(text, when=when, every=every, by=by)


def op_schedule(app, action: str = "list", id: str = "", **_) -> dict:
    if action == "cancel":
        return app.schedule.cancel(id)
    return app.schedule.listing()


def op_feedback(app, value: int = 1, **_) -> dict:
    if not app.last_episode:
        return {"error": "nothing to rate yet"}
    return app.learner.feedback(app.last_episode, int(value))


OPS = {
    "train": op_train, "training": op_training, "hippocampus": op_hippocampus, "sleep": op_sleep, "probe": op_probe, "stats": op_stats,
    "thoughts": op_thoughts, "thought": op_thought, "think": op_think, "skill": op_skill, "identity": op_identity, "inbox": op_inbox,
    "incidents": op_incidents, "patch": op_patch, "feedback": op_feedback, "learn": op_learn, "quiz": op_quiz,
    "remind": op_remind, "schedule": op_schedule,
}


async def run_op(app, name: str, args: dict | None = None) -> dict:
    """Run a named op on the running app, on the right executor."""
    fn = OPS.get(name)
    if fn is None:
        return {"error": f"unknown op {name!r}", "available": sorted(OPS)}
    args = args or {}
    try:
        if name in ASYNC_OPS:
            return await fn(app, **args)
        loop = asyncio.get_running_loop()
        executor = app.server.gpu if name in GPU_OPS else None
        return await loop.run_in_executor(executor, lambda: fn(app, **args))
    except TypeError as e:
        return {"error": f"bad arguments for {name}: {e}"}
    except Exception as e:
        return {"error": f"{type(e).__name__}: {e}"}
