"""Tic-tac-toe: `tictactoe play [--rounds N]` self-play whose outcomes become training samples; `tictactoe eval` measures you."""
import json
import os
import random
import re
import time
import urllib.request
import uuid
from pathlib import Path

LINES = [(0, 1, 2), (3, 4, 5), (6, 7, 8), (0, 3, 6), (1, 4, 7), (2, 5, 8), (0, 4, 8), (2, 4, 6)]
SYSTEM = ("You are playing tic-tac-toe. Cells are numbered 1-9 left to right, top to bottom. "
          "Answer with the number of the cell you play, and nothing else.")


def _url():
    p = Path(os.environ.get("GROOW_STATE", "state")) / "groow.url"
    return p.read_text().strip() if p.exists() else "http://127.0.0.1:7373"


def _complete(convs, max_new_tokens=6, temperature=1.0):
    req = urllib.request.Request(_url() + "/complete", data=json.dumps(
        {"messages": convs, "max_new_tokens": max_new_tokens, "temperature": temperature}).encode(),
        headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=600) as r:
        return json.loads(r.read())["completions"]


def _winner(b):
    for a, c, d in LINES:
        if b[a] != "." and b[a] == b[c] == b[d]:
            return b[a]
    return None


def _render(b):
    return "\n".join(" ".join(b[r * 3 + c] if b[r * 3 + c] != "." else str(r * 3 + c + 1) for c in range(3)) for r in range(3))


def _prompt(b, player):
    free = ", ".join(str(i + 1) for i, c in enumerate(b) if c == ".")
    return [{"role": "system", "content": SYSTEM},
            {"role": "user", "content": f"Board:\n{_render(b)}\n\nFree cells: {free}\nYou are {player}. Which cell do you play?"}]


def _move(text):
    m = re.search(r"[1-9]", text or "")
    return int(m.group()) - 1 if m else None


class Game:
    def __init__(self, opponent=None, rng=None):
        self.b, self.player, self.result, self.moves = ["."] * 9, "X", None, []
        self.opponent, self.rng = opponent, rng or random.Random()
        self.side = self.rng.choice("XO") if opponent else None
        if self.opponent and self.player != self.side:
            self._random_move()

    def _apply(self, cell, who):
        if cell is None or self.b[cell] != ".":
            self.result = f"illegal:{who}"
            return
        self.b[cell] = who
        w = _winner(self.b)
        self.result = w if w else ("draw" if "." not in self.b else None)
        if self.result is None:
            self.player = "O" if who == "X" else "X"

    def _random_move(self):
        self._apply(self.rng.choice([i for i, c in enumerate(self.b) if c == "."]), self.player)

    def act(self, text, explored=False):
        who, cell = self.player, _move(text)
        self.moves.append((who, _prompt(self.b, who), text, explored))
        self._apply(cell, who)
        if self.result is None and self.opponent and self.player != self.side:
            self._random_move()

    def credits(self):
        out = []
        for who, prompt, text, _ in self.moves:
            if self.result == f"illegal:{who}":
                out.append(-1.0)
            elif str(self.result).startswith("illegal"):
                out.append(0.0)
            elif self.result == "draw":
                out.append(0.2)
            else:
                out.append(1.0 if self.result == who else -1.0)
        return out


def _play(rounds=3, episodes=16, explore=0.25, rng=None):
    """Self-play; every decision with its reward is appended to the tictactoe training set."""
    rng = rng or random.Random()
    tdir = Path(os.environ.get("GROOW_STATE", "state")) / "training"
    tdir.mkdir(parents=True, exist_ok=True)
    summary = []
    for r in range(rounds):
        games = [Game(rng=rng) for _ in range(episodes)]
        while any(g.result is None for g in games):
            live = [g for g in games if g.result is None]
            outs = _complete([_prompt(g.b, g.player) for g in live])
            for g, o in zip(live, outs):
                if rng.random() < explore:
                    free = [i for i, c in enumerate(g.b) if c == "."]
                    g.act(str(rng.choice(free) + 1), explored=True)
                else:
                    g.act(o)
        group, n, total = uuid.uuid4().hex[:8], 0, 0.0
        with (tdir / "tictactoe.jsonl").open("a") as f:
            for g in games:
                for (who, prompt, text, _), reward in zip(g.moves, g.credits()):
                    f.write(json.dumps({"ts": time.time(), "kind": "pg", "prompt": prompt, "completion": (re.search(r"[1-9]", text or "") or [text[:3]])[0]
                                        if re.search(r"[1-9]", text or "") else (text or "?")[:3], "reward": reward, "group": group,
                                        "source": "tictactoe self-play"}, ensure_ascii=False) + "\n")
                    n += 1; total += reward
        illegal = sum(1 for g in games if str(g.result).startswith("illegal"))
        summary.append({"round": r + 1, "decisions": n, "mean_reward": round(total / max(1, n), 3), "illegal_games": illegal})
    return {"rounds": summary, "training_set": str(tdir / "tictactoe.jsonl"),
            "note": "samples are pending until the next nap or night (groow training shows them)"}


def _evaluate(n=24, rng=None):
    rng = rng or random.Random()
    games = [Game(opponent="random", rng=rng) for _ in range(n)]
    while any(g.result is None for g in games):
        live = [g for g in games if g.result is None]
        for g, o in zip(live, _complete([_prompt(g.b, g.player) for g in live], temperature=0.0)):
            g.act(o)
    res = {"win": 0, "draw": 0, "loss": 0, "illegal": 0}
    for g in games:
        res["illegal" if str(g.result).startswith("illegal") else "draw" if g.result == "draw" else "win" if g.result == g.side else "loss"] += 1
    res["score"] = round((res["win"] - res["loss"] - res["illegal"]) / n, 3)
    return res


def main(argv: list) -> dict:
    """tictactoe play [--rounds N] [--episodes N] | tictactoe eval [--games N]"""
    if not argv or argv[0] in ("-h", "--help"):
        return {"text": "usage: tictactoe play [--rounds 3] [--episodes 16]   self-play -> training samples\n"
                        "       tictactoe eval [--games 24]                  your skill against a random player"}
    if argv[0] == "--selftest":
        g = Game(rng=random.Random(0)); g.act("5"); g.act("1"); return {"ok": True, "board": _render(g.b)}
    opts = {argv[i].lstrip("-"): int(argv[i + 1]) for i in range(1, len(argv) - 1, 2) if argv[i].startswith("--")}
    try:
        if argv[0] == "play":
            return _play(rounds=opts.get("rounds", 3), episodes=opts.get("episodes", 16))
        if argv[0] == "eval":
            return _evaluate(n=opts.get("games", 24))
    except Exception as e:
        return {"error": f"{type(e).__name__}: {e}", "hint": "is groow awake? this needs its /complete endpoint"}
    return {"error": f"unknown action {argv[0]}"}


CLI = {"tictactoe": "main"}
TESTS = [("tictactoe", {"argv": ["--selftest"]}, {"ok": True})]
