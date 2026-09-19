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


def train(cfg: Config, max_samples: int = 32) -> dict:
    """Turn pending samples into weight changes, in the process that holds the weights."""
    report = ask_the_brain(cfg, "train", {"max_samples": max_samples})
    if report.get("consumed"):
        Stats(Path(cfg.state)).learned("train", report["consumed"], report.get("last_loss"), "")
    return report


def consolidate(cfg: Config) -> dict:
    """Merge what was practised into the base weights.

    Nothing reloads afterwards. The process that merged them is the one that serves, so what
    it learned is what it answers from, from the next request onward.
    """
    report = ask_the_brain(cfg, "consolidate", {})
    if not report.get("error"):
        Stats(Path(cfg.state)).learned("consolidate", 0, None, json.dumps(report))
    return report


def nap(cfg: Config) -> dict:
    """The short pass, between turns: feel, harvest, practise a little.

    No merge and no long drills, because a person may speak at any moment.
    """
    return {
        "feel": feel(cfg),
        "harvest": harvest(cfg),
        "train": train(cfg, max_samples=cfg.nap_max_samples),
    }


def night(cfg: Config) -> dict:
    """The whole cycle, ending in the merge that makes it permanent."""
    return {
        "feel": feel(cfg),
        "harvest": harvest(cfg),
        "train": train(cfg, max_samples=cfg.idle_nap_max_samples),
        "consolidate": consolidate(cfg),
    }


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
