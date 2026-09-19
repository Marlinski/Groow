"""The limbic pass: what happened becomes how it felt.

Nothing asks the mind how it thinks it did. This reads the turns that have ended and scores
them from two sources, neither of which is the mind's own opinion:

    the sensors   free signals from what actually happened: a command failed, the same call
                  was made twice, the round budget ran out, it recovered from its own error
    the judge     a small frozen model reading how a person reacted to what was said

Approval is deliberately late. A reaction has to exist before a turn can be judged by it, so
the score of one turn is decided by the turn after it, and the last turn of a day waits for
tomorrow. That is the price of not letting it mark its own work.

A turn the body failed to finish is scored at nothing. The brain being down is not the mind's
mistake, and charging it for one would teach it the wrong lesson.
"""
from __future__ import annotations

import time
from pathlib import Path

from ..config import Config
from ..memory import Journal
from ..memory.day import exchanges, for_turn
from ..stats import Stats
from . import sensors
from .judge import make_judge, NullJudge

# How far a person's reaction can move a turn's score, on top of what the sensors saw.
APPROVAL_WEIGHT = 1.0

# How many exchanges to read back when looking for the reaction to a turn.
LOOKBACK = 400


def judge_for(cfg: Config):
    """The frozen judge. It never learns, so what counts as approval cannot drift."""
    try:
        return make_judge(cfg.judge)
    except Exception as e:
        print(f"no judge ({e}); running on sensors alone")
        return NullJudge()


def feel(cfg: Config) -> dict:
    """Score every turn that has ended and has not yet been felt."""
    state = Path(cfg.state)
    stats = Stats(state)
    turns = stats.unscored_turns()
    if not turns:
        return {"scored": 0}

    judge = judge_for(cfg)
    convo = exchanges(Journal(state / "main"), LOOKBACK)
    scored = judged = 0

    for t in turns:
        now = time.time()
        if t["outcome"] != "ok":
            # Recorded as felt so it is not looked at again, and worth nothing either way.
            stats.felt(t["id"], now, 0.0, None, 0.0)
            scored += 1
            continue

        flags = [f for f in (t["flags"] or "").split(",") if f]
        sensed = round(sensors.valence_of(sensors.from_flags(flags)), 3)
        approval = None

        here = for_turn(convo, t["id"], t["started"])
        if here is not None:
            after = _next_after(convo, here)
            if after is not None and after["kind"] in ("user", "command") and here["final"]:
                try:
                    approval = judge.reaction(here["final"], after["user"])
                except Exception:
                    approval = None
        if approval is not None:
            judged += 1

        valence = round(sensed + APPROVAL_WEIGHT * approval, 3) if approval is not None else sensed
        stats.felt(t["id"], now, sensed, approval, valence)
        scored += 1

    return {"scored": scored, "judged": judged, "judge": judge.name}


def _next_after(convo: list[dict], here: dict) -> dict | None:
    """The exchange that followed this one, which is where a reaction would be."""
    try:
        i = convo.index(here)
    except ValueError:
        return None
    return convo[i + 1] if i + 1 < len(convo) else None
