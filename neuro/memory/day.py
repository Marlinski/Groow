"""The conversation as whole turns, for the passes that read it back.

The journal is a stream of messages. What the limbic system and the hippocampus
both want is something coarser: what came in, what was done about it, and what
was finally said. That shape is built once, here, so the two of them cannot
disagree about where one turn ends and the next begins.
"""
from __future__ import annotations

from .journal import Journal


def exchanges(journal: Journal, n: int = 200) -> list[dict]:
    """The last `n` records, grouped into exchanges, oldest first.

    An exchange starts at every incoming message, whatever woke the mind, and gathers
    everything until the next one. `final` is the last thing said without a tool call, which is
    what a person actually read.
    """
    out: list[dict] = []
    current: dict | None = None
    for rec in journal.tail(n):
        role = rec.get("role")
        if role == "user":
            if current:
                out.append(current)
            current = {"ts": rec.get("ts", 0.0), "kind": rec.get("kind", "user"),
                       "turn": rec.get("turn", ""), "user": rec.get("content", ""),
                       "messages": [rec], "final": ""}
        elif current is not None:
            current["messages"].append(rec)
            if role == "assistant" and rec.get("content") and not rec.get("tool_calls"):
                current["final"] = rec["content"]
    if current:
        out.append(current)
    return out


def nearest(convo: list[dict], ts: float) -> dict | None:
    """The exchange belonging to a turn that started at `ts`.

    Matched by the turn's own stamp where the journal has one, and by time otherwise, so that
    records written before turns were stamped are still usable.
    """
    if not convo:
        return None
    return min(convo, key=lambda c: abs(c["ts"] - ts))


def for_turn(convo: list[dict], turn_id: str, ts: float) -> dict | None:
    for c in convo:
        if turn_id and c.get("turn") == turn_id:
            return c
    return nearest(convo, ts)
