"""Self tools: the one channel to the mentor. Learning is not a tool: it is a meta-process
(the trainer consumes training sets prepared by the hippocampus, by skills and by the mentor)."""
from __future__ import annotations

import json
import time

from .registry import ToolRegistry


def make_self_tools(learner=None, identity=None, memory=None) -> ToolRegistry:
    reg = ToolRegistry()
    mem = memory or (learner.memory if learner is not None else None)
    inbox = mem.dir / "mentor_inbox.jsonl"

    @reg.tool(group="self")
    def ask(question: str, context: str = "") -> dict:
        """Leave a question for Marlinski, your owner and mentor; he reads the inbox when he next talks to you.
        For what you cannot resolve alone: what to learn, how to behave, whether a fact is right, anything that
        needs root or a change to your body.

        Args:
            question: the question, in one or two sentences
            context: optional: what led you to ask
        """
        rec = {"ts": time.time(), "question": question.strip(), "context": context.strip(), "answered": False}
        with inbox.open("a") as f:
            f.write(json.dumps(rec, ensure_ascii=False) + "\n")
        return {"ok": True, "queued": question.strip()[:200]}

    return reg
