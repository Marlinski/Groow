"""Self tools: the acts that only Groow can do to itself, and the one channel to its mentor.

    learn(question, answer, source)   the only deliberate weight change: drill question -> answer
    quiz(question, expected)          measure what the weights know (no change)
    ask_mentor(question)              leave a question for Marlinski
"""
from __future__ import annotations

import json
import time

from ..learning import Learner
from .registry import ToolRegistry


def make_self_tools(learner: Learner, identity=None) -> ToolRegistry:
    reg = ToolRegistry()
    brain, memory = learner.brain, learner.memory
    inbox = memory.dir / "mentor_inbox.jsonl"

    @reg.tool(group="self", executor="gpu")
    def learn(question: str, answer: str, source: str = "", target_loss: float = 0.0) -> dict:
        """Learn something into your weights: repeated training on question -> answer, then it is filed as a
        lesson. Use the source's own words for the answer and include the date and origin; never invent. Use it
        when someone asks you to remember something, or when you read something true and worth keeping.

        Args:
            question: the question this knowledge answers
            answer: the answer, in the source's wording, including when it happened
            source: where it comes from (publication and date, a URL, or "Marlinski, 2026-09-18")
            target_loss: 0 = a few passes; otherwise keep training until the recite loss is below this (e.g. 0.15 to know it by heart)
        """
        r = learner.learn(question, answer, source, target_loss=target_loss or None,
                          on_progress=lambda s, l: reg.progress(f"learn step {s + 1}: loss {l:.3f}"))
        brain.save()
        return r

    @reg.tool(group="self", executor="gpu")
    def quiz(question: str, expected: str = "") -> dict:
        """Test yourself: your current answer to a question and, if an expected answer is given, how surprising
        it is to your weights (loss below 0.5 = you know it, above 2 = you do not). Changes nothing.

        Args:
            question: the question to ask yourself
            expected: the correct answer, if known
        """
        return learner.quiz(question, expected or None)

    @reg.tool(group="self")
    def ask_mentor(question: str, context: str = "") -> dict:
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
