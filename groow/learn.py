"""The meta-processes: what happens to a day after it has been lived.

The mind does not call any of this. The core runs it between turns and at night,
which is the point: what is learned is decided by how things went, not by the
mind deciding it did well.

    python -m groow.learn feel     score recent turns and write what they felt like
    python -m groow.learn harvest  turn the day into training samples
    python -m groow.learn train    take pending samples and make gradients from them
    python -m groow.learn night    all three, then consolidate into the base weights

It reads the conversation from the journal and the turn records from the core's
database, which is readable only by root, so the mind can neither see nor edit
how it is being scored.
"""
from __future__ import annotations

import argparse
import json
import sqlite3
import time
from pathlib import Path

from .config import Config
from .learning.trainingset import TrainingSets
from .memory import Journal, Memory

# What the free signals are worth. Nothing here is self-reported: each one is
# something that can be observed from the outside.
WEIGHTS = {
    "tool_error": -0.5,
    "timeout": -0.7,
    "repeat": -0.6,
    "exhausted": -0.8,
    "truncated": -0.3,
    "restated": -0.8,
    "recovered": +0.6,
    "completed": +0.15,
}

# How much a person's reaction can move a turn's score, on top of the sensors.
APPROVAL_WEIGHT = 1.0


def sensor_valence(flags: list[str]) -> float:
    """Add the signals up, with each repeat of the same signal counting for less.

    Four failed commands in one turn is worse than one, but not four times worse;
    without the damping a single bad turn would drown out a week of good ones.
    """
    seen: dict[str, int] = {}
    total = 0.0
    for f in flags:
        w = WEIGHTS.get(f)
        if w is None:
            continue
        k = seen.get(f, 0)
        total += w / (1 + k)
        seen[f] = k + 1
    return round(total, 3)


class Db:
    """The core's statistics, read and written from the root side."""

    def __init__(self, path: Path):
        self.c = sqlite3.connect(path)
        self.c.row_factory = sqlite3.Row

    def unscored_turns(self, limit: int = 20) -> list[sqlite3.Row]:
        return list(self.c.execute(
            "SELECT t.* FROM turns t LEFT JOIN feelings f ON f.turn = t.id "
            "WHERE t.ended IS NOT NULL AND f.id IS NULL ORDER BY t.started LIMIT ?", (limit,)))

    def felt(self, turn: str, ts: float, sensors: float, approval, valence: float) -> None:
        self.c.execute(
            "INSERT INTO feelings (turn, ts, sensors, approval, valence) VALUES (?,?,?,?,?)",
            (turn, ts, sensors, approval, valence))
        self.c.commit()

    def learned(self, kind: str, samples: int, loss, note: str = "") -> None:
        self.c.execute("INSERT INTO learning (ts, kind, samples, loss, note) VALUES (?,?,?,?,?)",
                       (time.time(), kind, samples, loss, note))
        self.c.commit()


def exchanges(journal: Journal, n: int = 200) -> list[dict]:
    """The conversation as whole turns: what came in, what was done, what was said."""
    out: list[dict] = []
    current: dict | None = None
    for rec in journal.tail(n):
        role = rec.get("role")
        if role == "user":
            if current:
                out.append(current)
            current = {"ts": rec.get("ts", 0.0), "kind": rec.get("kind", "user"),
                       "user": rec.get("content", ""), "messages": [rec], "final": ""}
        elif current is not None:
            current["messages"].append(rec)
            if role == "assistant" and rec.get("content") and not rec.get("tool_calls"):
                current["final"] = rec["content"]
    if current:
        out.append(current)
    return out


def judge_for(cfg: Config):
    """The frozen judge. It never learns, so what counts as approval cannot drift."""
    try:
        from .limbic.judge import make_judge
        return make_judge(cfg.judge)
    except Exception as e:
        print(f"no judge ({e}); running on sensors alone")
        from .limbic.judge import NullJudge
        return NullJudge()


# ---------------------------------------------------------------------- feel
def feel(cfg: Config) -> dict:
    """Score the turns that have ended but have not yet been felt.

    A turn that a person replied to is scored with their reaction; everything else
    is scored on its sensors alone. Approval is deliberately late: the reaction has
    to exist before the turn can be judged, so learning is always a turn behind.
    """
    db = Db(Path(cfg.state) / "groow.db")
    journal = Journal(Path(cfg.state) / "main")
    turns = db.unscored_turns()
    if not turns:
        return {"scored": 0}

    judge = judge_for(cfg)
    convo = exchanges(journal, 400)
    scored = 0
    for t in turns:
        # A turn the body could not finish is not the mind's failure: the brain was down, or a
        # process was killed. It is recorded as felt so it is not looked at again, and scored
        # at nothing, because punishing it for a power cut would teach it the wrong lesson.
        if t["outcome"] != "ok":
            db.felt(t["id"], time.time(), 0.0, None, 0.0)
            scored += 1
            continue
        flags = [f for f in (t["flags"] or "").split(",") if f]
        sensors = sensor_valence(flags)
        approval = None

        # Find what was said in this turn, and whether a person answered it.
        here = min(range(len(convo)), key=lambda i: abs(convo[i]["ts"] - t["started"])) if convo else None
        if here is not None and here + 1 < len(convo):
            nxt = convo[here + 1]
            said = convo[here]["final"]
            if nxt["kind"] in ("user", "command") and said:
                try:
                    approval = judge.reaction(said, nxt["user"])
                except Exception:
                    approval = None
        valence = round(sensors + APPROVAL_WEIGHT * (approval or 0.0), 3)
        db.felt(t["id"], time.time(), sensors, approval, valence)
        scored += 1
    return {"scored": scored}


# ------------------------------------------------------------------ harvest
def harvest(cfg: Config) -> dict:
    """Turn what has been felt into things to practise.

    Only turns that went well become examples to imitate. A turn that went round in
    circles is still useful, but as a policy sample with a negative reward, not as
    something to copy.
    """
    state = Path(cfg.state)
    db = Db(state / "groow.db")
    journal = Journal(state / "main")
    sets = TrainingSets(state / "training")

    cursor_path = state / "learn.json"
    cursor = json.loads(cursor_path.read_text()) if cursor_path.exists() else {}
    since = float(cursor.get("harvested_ts", 0.0))

    # Only turns the mind actually completed. An abandoned one ends with the core's own
    # apology in the conversation, and training on that would teach it to apologise.
    rows = list(db.c.execute(
        "SELECT t.*, f.valence FROM turns t JOIN feelings f ON f.turn = t.id "
        "WHERE t.started > ? AND t.outcome = 'ok' ORDER BY t.started", (since,)))
    if not rows:
        return {"harvested": 0}

    convo = exchanges(journal, 600)
    by_ts = sorted(convo, key=lambda c: c["ts"])
    made = {"conversation": 0, "actions": 0}
    newest = since

    for r in rows:
        newest = max(newest, float(r["started"]))
        flags = [f for f in (r["flags"] or "").split(",") if f]
        valence = float(r["valence"] or 0.0)
        match = min(by_ts, key=lambda c: abs(c["ts"] - r["started"])) if by_ts else None
        if not match or not match["final"]:
            continue

        good = valence > 0.1 and "repeat" not in flags and "exhausted" not in flags
        if good:
            messages = [{"role": m.get("role"), "content": m.get("content", "")}
                        for m in match["messages"] if m.get("role") in ("user", "assistant") and m.get("content")]
            if len(messages) >= 2:
                sets.append("conversation", {
                    "kind": "sft", "messages": messages,
                    "weights": cfg.role_weights, "valence": valence,
                    "turn": r["id"], "by": "learn.harvest",
                })
                made["conversation"] += 1

        # Every turn, good or bad, is a decision that can be reinforced or discouraged.
        prompt = [{"role": m.get("role"), "content": m.get("content", "")}
                  for m in match["messages"][:1]]
        if prompt and match["final"]:
            sets.append("actions", {
                "kind": "pg", "prompt": prompt, "completion": match["final"],
                "reward": valence, "group": f"turn:{r['id']}",
                "tags": flags, "by": "learn.harvest",
            })
            made["actions"] += 1

    cursor["harvested_ts"] = newest
    cursor_path.write_text(json.dumps(cursor, indent=1))
    db.learned("harvest", made["conversation"] + made["actions"], None, json.dumps(made))
    return {"harvested": made}


# -------------------------------------------------------------------- train
def train(cfg: Config, max_samples: int = 32) -> dict:
    """Take pending samples and turn them into weight changes."""
    from .brain.model import Brain
    from .learning.learner import Learner
    from .learning.trainer import Trainer

    state = Path(cfg.state)
    memory = Memory(state)
    brain = Brain(cfg).load()
    learner = Learner(brain, memory, cfg)
    trainer = Trainer(brain, memory, learner, TrainingSets(state / "training"), cfg)
    report = trainer.consume(max_samples=max_samples, on_progress=lambda m: print(m, flush=True))
    if report.get("consumed"):
        brain.save()
        Db(state / "groow.db").learned("train", report["consumed"], report.get("last_loss"), "")
    return report


# -------------------------------------------------------------------- night
def night(cfg: Config) -> dict:
    """The whole cycle: score the day, harvest it, practise it, then consolidate."""
    out = {"feel": feel(cfg), "harvest": harvest(cfg)}
    out["train"] = train(cfg, max_samples=cfg.idle_nap_max_samples)
    try:
        from .brain.model import Brain
        brain = Brain(cfg).load()
        out["consolidate"] = brain.consolidate(keep_previous=cfg.keep_previous_base)
        brain.save()
    except Exception as e:
        out["consolidate"] = {"error": f"{type(e).__name__}: {e}"}
    return out


def main() -> None:
    ap = argparse.ArgumentParser(description="what happens to a day after it has been lived")
    ap.add_argument("what", choices=["feel", "harvest", "train", "night"])
    ap.add_argument("--config", default="groow.json")
    ap.add_argument("--state", default=None)
    ap.add_argument("--max", type=int, default=32)
    args = ap.parse_args()

    cfg = Config.load(Path(args.config))
    if args.state:
        cfg.state_dir = args.state
    fn = {"feel": feel, "harvest": harvest, "night": night}.get(args.what)
    out = train(cfg, args.max) if args.what == "train" else fn(cfg)
    print(json.dumps(out, indent=1, default=str))


if __name__ == "__main__":
    main()
