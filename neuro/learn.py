"""The meta-processes: what happens to a day after it has been lived.

The mind does not call any of this, and could not. The core runs it between turns and at
night, which is the point: what is learned is decided by how things went, not by the mind
deciding it did well.

    python -m neuro.learn feel         the limbic pass: score the turns that have ended
    python -m neuro.learn harvest      the hippocampus: turn what was felt into samples
    python -m neuro.learn train        ask the brain to make gradients from them
    python -m neuro.learn consolidate  ask it to merge the overlay into the base
    python -m neuro.learn nap          the short pass, between turns
    python -m neuro.learn night        all of it, ending in a merge

Feeling and harvesting happen here, on the processor, because they need only the journal, the
statistics and the judge, and can run while the mind is mid-turn. Anything that touches the
weights is sent to the brain instead: it holds them, and a second copy would not fit beside
the first.
"""
from __future__ import annotations

import argparse
import json
import time
from pathlib import Path

from .config import Config
from .hippocampus import harvest
from .limbic.feel import feel
from .stats import Stats


def brain_url(cfg: Config) -> str:
    return f"http://127.0.0.1:{getattr(cfg, 'brain_port', 7374)}"


def ask_the_brain(cfg: Config, what: str, payload: dict, quiet: bool = False) -> dict:
    """Ask the brain to do something with the weights, and follow along while it does.

    It answers in lines as it works, so a long pass is not silence. Requests arriving
    meanwhile wait in its queue rather than being refused: that is what sleeping means here.
    """
    import urllib.error
    import urllib.request

    url = f"{brain_url(cfg)}/{what}"
    req = urllib.request.Request(url, data=json.dumps(payload).encode(),
                                 headers={"Content-Type": "application/json"})
    out: dict = {}
    try:
        with urllib.request.urlopen(req, timeout=3600) as r:
            for line in r:
                line = line.strip()
                if not line:
                    continue
                d = json.loads(line)
                if "progress" in d:
                    if not quiet:
                        print(d["progress"], flush=True)
                elif d.get("done"):
                    out = d.get("report") or {}
                elif "error" in d:
                    return {"error": d["error"]}
    except urllib.error.URLError as e:
        return {"error": f"the brain is not answering at {url}: {e.reason}"}
    except Exception as e:
        return {"error": f"{type(e).__name__}: {e}"}
    return out


def _cost(report: dict):
    """What this pass cost, as one number, whichever way it practised.

    `last_loss` is set only by the batched path. A pass made entirely of samples drilled to a
    target never touches it, and those are the passes that matter most, so the loss they ended
    on is used instead. Without this every train row was recorded with no loss at all and there
    was no way to see whether any of it was working.

    A policy step's loss is not this number and is not returned here. It is not comparable with
    a supervised loss, and putting the two in one column would make a graph that lies; what
    says whether those passes are going well is the reward, which goes in the note.
    """
    import math

    loss = report.get("last_loss")
    if loss is not None and math.isfinite(float(loss)):
        return float(loss)
    after = [d["loss_after"] for d in report.get("drilled") or [] if d.get("loss_after") is not None]
    return sum(after) / len(after) if after else None


def timed(f):
    """Run something and say how long it took, in wall clock.

    Every pass is recorded with its duration, because "it ran" and "it ran for nine minutes"
    are different facts and only one of them tells you whether something is wrong.
    """
    started = time.time()
    out = f()
    return out, round(time.time() - started, 2)


def train(cfg: Config, max_samples: int = 32) -> dict:
    """Turn pending samples into weight changes, in the process that holds the weights."""
    started = time.time()
    report = ask_the_brain(cfg, "train", {"max_samples": max_samples})
    if report.get("consumed"):
        # What it practised and how hard, kept with the number: a loss on its own says nothing
        # about whether the pass was three easy samples or forty steps on one stubborn one.
        note = {k: report[k] for k in ("sft_steps", "pg_steps", "pg_loss", "mean_reward", "sets")
                if report.get(k) is not None and report.get(k) != 0}
        if report.get("drilled"):
            note["drilled"] = len(report["drilled"])
        if report.get("skipped_groups"):
            note["skipped"] = len(report["skipped_groups"])
        Stats(Path(cfg.state)).learned("train", report["consumed"], _cost(report), json.dumps(note),
                                       seconds=round(time.time() - started, 2))
    return report


def consolidate(cfg: Config) -> dict:
    """Merge what was practised into the base weights.

    Nothing reloads afterwards. The process that merged them is the one that serves, so what
    it learned is what it answers from, from the next request onward.
    """
    started = time.time()
    report = ask_the_brain(cfg, "consolidate", {})
    if not report.get("error"):
        Stats(Path(cfg.state)).learned("consolidate", 0, None, json.dumps(report),
                                       seconds=round(time.time() - started, 2))
    return report


def _pass(cfg: Config, name: str, steps: list[tuple[str, object]]) -> dict:
    """Run the steps of one pass in order, and record the pass itself as well as its parts.

    The parts each write their own row as they go. This is the row for the whole thing: how
    long the creature spent asleep, how much it practised, and what it cost — which is the
    question anyone actually asks of an operator's log.
    """
    out: dict = {}
    started = time.time()
    for step_name, step in steps:
        out[step_name], out[f"{step_name}_seconds"] = timed(lambda s=step: s(cfg))
    seconds = round(time.time() - started, 2)

    trained = out.get("train") or {}
    note = {
        "scored": (out.get("feel") or {}).get("scored", 0),
        "harvested": sum(v for k, v in (out.get("harvest") or {}).items() if isinstance(v, int)),
        "sft_steps": trained.get("sft_steps", 0),
        "pg_steps": trained.get("pg_steps", 0),
        "merged": bool(out.get("consolidate")),
        "parts": {k.removesuffix("_seconds"): v for k, v in out.items() if k.endswith("_seconds")},
    }
    if any(r.get("error") for r in out.values() if isinstance(r, dict)):
        note["errors"] = [r["error"] for r in out.values() if isinstance(r, dict) and r.get("error")]
    Stats(Path(cfg.state)).learned(name, trained.get("consumed", 0), _cost(trained),
                                   json.dumps(note), seconds=seconds)
    out["seconds"] = seconds
    return out


def nap(cfg: Config) -> dict:
    """The short pass, between turns: feel, harvest, practise a little.

    No merge and no long drills, because a person may speak at any moment.
    """
    return _pass(cfg, "nap", [
        ("feel", feel),
        ("harvest", harvest),
        ("train", lambda c: train(c, max_samples=c.nap_max_samples)),
    ])


def night(cfg: Config) -> dict:
    """The whole cycle, ending in the merge that makes it permanent."""
    return _pass(cfg, "night", [
        ("feel", feel),
        ("harvest", harvest),
        ("train", lambda c: train(c, max_samples=c.idle_nap_max_samples)),
        ("consolidate", consolidate),
    ])


PASSES = {"feel": feel, "harvest": harvest, "consolidate": consolidate, "nap": nap, "night": night}


def main() -> None:
    ap = argparse.ArgumentParser(description="what happens to a day after it has been lived")
    ap.add_argument("what", choices=sorted(PASSES) + ["train"])
    ap.add_argument("--config", default="groow.json")
    ap.add_argument("--state", default=None)
    ap.add_argument("--max", type=int, default=32)
    args = ap.parse_args()

    cfg = Config.load(Path(args.config))
    if args.state:
        cfg.state_dir = args.state

    out = train(cfg, args.max) if args.what == "train" else PASSES[args.what](cfg)
    print(json.dumps(out, indent=1, default=str))
    if isinstance(out, dict) and (out.get("error") or any(
            isinstance(v, dict) and v.get("error") for v in out.values())):
        raise SystemExit(1)


if __name__ == "__main__":
    main()
