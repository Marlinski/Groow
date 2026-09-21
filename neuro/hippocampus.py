"""The hippocampus: a day becomes something to practise.

It reads what the limbic system felt and writes training samples. Nothing here decides whether
a turn was good; that was decided already, from what happened and how a person reacted.

Two kinds come out of it, and the difference matters:

    imitate     only turns that went well, as examples to become more like
    reinforce   every turn, as a decision with a reward attached, good or bad

A turn that went round in circles is still worth something as the second kind. It is never
worth anything as the first, because an example is a recommendation.
"""
from __future__ import annotations

import json
import time
from pathlib import Path

from .config import Config
from .learning.trainingset import TrainingSets
from .memory import Journal
from .memory.day import exchanges, for_turn
from .stats import Stats

# How far back to read, and what counts as a turn worth imitating.
LOOKBACK = 600
WORTH_IMITATING = 0.1


def harvest(cfg: Config) -> dict:
    """Turn everything felt since last time into samples, and remember where we got to."""
    started = time.time()
    state = Path(cfg.state)
    stats = Stats(state)
    sets = TrainingSets(state / "training")

    mark = state / "hippocampus.json"
    seen = json.loads(mark.read_text()) if mark.exists() else {}
    since = float(seen.get("harvested_ts", 0.0))

    rows = stats.felt_turns_since(since)
    if not rows:
        return {"conversation": 0, "actions": 0}

    convo = exchanges(Journal(state / "main"), LOOKBACK)
    made = {"conversation": 0, "actions": 0}
    newest = since

    for r in rows:
        newest = max(newest, float(r["started"]))
        flags = [f for f in (r["flags"] or "").split(",") if f]
        valence = float(r["valence"] or 0.0)
        here = for_turn(convo, r["id"], r["started"])
        if not here or not here["final"]:
            continue

        if valence > WORTH_IMITATING and "repeat" not in flags and "exhausted" not in flags:
            messages = [{"role": m.get("role"), "content": m.get("content", "")}
                        for m in here["messages"]
                        if m.get("role") in ("user", "assistant") and m.get("content")]
            if len(messages) >= 2:
                sets.append("conversation", {
                    "kind": "sft", "messages": messages, "weights": cfg.role_weights,
                    "valence": valence, "turn": r["id"], "by": "hippocampus",
                })
                made["conversation"] += 1

        # One group for all of them on purpose. A conversation turn happens once, so a group of
        # one has nothing to compare against and its advantage is exactly zero: the step would
        # run and change nothing. Together, each turn is measured against how turns have been
        # going lately, which is what a baseline is for. Games keep their own per-round groups,
        # because there the same position really was played several ways.
        prompt = [{"role": m.get("role"), "content": m.get("content", "")}
                  for m in here["messages"][:1]]
        if prompt:
            sets.append("actions", {
                "kind": "pg", "prompt": prompt, "completion": here["final"],
                "reward": valence, "group": "turns", "tags": flags,
                "turn": r["id"], "by": "hippocampus",
            })
            made["actions"] += 1

    seen["harvested_ts"] = newest
    mark.write_text(json.dumps(seen, indent=1))
    stats.learned("harvest", made["conversation"] + made["actions"], None, json.dumps(made),
                  seconds=round(time.time() - started, 2))
    return made
