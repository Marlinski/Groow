"""Arithmetic: `arithmetic drill [--rounds N] [--level 1|2|3]` mental arithmetic, answers scored and logged to your activity log."""
import json
import os
import random
import re
import time
import urllib.request
import uuid
from pathlib import Path

SYSTEM = "Answer with the final number only."


def _url():
    p = Path(os.environ.get("GROOW_STATE", "state")) / "groow.url"
    return p.read_text().strip() if p.exists() else "http://127.0.0.1:7373"


def _complete(convs, max_new_tokens=12, temperature=1.0):
    req = urllib.request.Request(_url() + "/complete", data=json.dumps(
        {"messages": convs, "max_new_tokens": max_new_tokens, "temperature": temperature}).encode(),
        headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=600) as r:
        return json.loads(r.read())["completions"]


def _question(rng, level):
    op = rng.choice("+-*")
    hi = {1: 20, 2: 99, 3: 999}[level]
    a, b = rng.randint(2, hi), rng.randint(2, hi)
    if op == "*":
        b = rng.randint(2, 12 if level < 3 else 99)
    return f"{a} {op} {b}", eval(f"{a}{op}{b}")


def _drill(rounds=3, n=16, level=2, rng=None):
    rng = rng or random.Random()
    logp = Path(os.environ.get("GROOW_STATE", "state")) / "log" / "activity.jsonl"
    logp.parent.mkdir(parents=True, exist_ok=True)
    summary = []
    for r in range(rounds):
        qs = [_question(rng, level) for _ in range(n)]
        convs = [[{"role": "system", "content": SYSTEM}, {"role": "user", "content": f"What is {q}?"}] for q, _ in qs]
        outs = _complete(convs)
        group, correct = uuid.uuid4().hex[:8], 0
        with logp.open("a") as f:
            for (q, ans), conv, out in zip(qs, convs, outs):
                nums = re.findall(r"-?\d+", (out or "").replace(",", ""))
                reward = -1.0 if not nums else (1.0 if int(nums[-1]) == ans else -0.5)
                correct += reward == 1.0
                f.write(json.dumps({"ts": time.time(), "kind": "decision", "skill": "arithmetic", "prompt": conv,
                                    "completion": (out or "").strip()[:20], "reward": reward, "group": group,
                                    "tags": ["drill", "arithmetic", f"level{level}"]}) + "\n")
        summary.append({"round": r + 1, "accuracy": round(correct / n, 3)})
    return {"rounds": summary, "logged_to": str(logp),
            "note": "your answers and their scores are in your activity log; you learn from them at your next nap"}


def main(argv: list) -> dict:
    """arithmetic drill [--rounds N] [--level 1|2|3]"""
    if not argv or argv[0] in ("-h", "--help"):
        return {"text": "usage: arithmetic drill [--rounds 3] [--level 2]   questions, answers scored, logged"}
    if argv[0] == "--selftest":
        q, a = _question(random.Random(0), 1); return {"ok": True, "example": f"{q} = {a}"}
    opts = {argv[i].lstrip("-"): int(argv[i + 1]) for i in range(1, len(argv) - 1, 2) if argv[i].startswith("--")}
    try:
        if argv[0] == "drill":
            return _drill(rounds=opts.get("rounds", 3), level=opts.get("level", 2))
    except Exception as e:
        return {"error": f"{type(e).__name__}: {e}", "hint": "is groow awake? this needs its /complete endpoint"}
    return {"error": f"unknown action {argv[0]}"}


CLI = {"arithmetic": "main"}
TESTS = [("arithmetic", {"argv": ["--selftest"]}, {"ok": True})]
