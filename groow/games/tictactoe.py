"""Tic-tac-toe by self-play. Both sides are the same policy. Rewards come only
from the rules: +1 for the winner's moves, -1 for the loser's, 0 for a draw,
-1 for an illegal or unparseable move (which ends the game)."""
from __future__ import annotations

import random
import re

from .base import Episode, Game, Policy

LINES = [(0, 1, 2), (3, 4, 5), (6, 7, 8), (0, 3, 6), (1, 4, 7), (2, 5, 8), (0, 4, 8), (2, 4, 6)]
SYSTEM = ("You are playing tic-tac-toe. Cells are numbered 1-9 left to right, top to bottom. "
          "Answer with the number of the cell you play, and nothing else.")


def winner(b: list[str]) -> str | None:
    for a, c, d in LINES:
        if b[a] != "." and b[a] == b[c] == b[d]:
            return b[a]
    return None


def render(b: list[str]) -> str:
    rows = []
    for r in range(3):
        rows.append(" ".join(b[r * 3 + c] if b[r * 3 + c] != "." else str(r * 3 + c + 1) for c in range(3)))
    return "\n".join(rows)


def parse_move(text: str) -> int | None:
    m = re.search(r"[1-9]", text)
    return int(m.group()) - 1 if m else None


class TTTEpisode(Episode):
    def __init__(self, opponent: str | None = None, rng: random.Random | None = None):
        self.board = ["."] * 9
        self.player = "X"
        self.opponent = opponent          # None = self-play, "random" = policy vs random (evaluation)
        self.rng = rng or random.Random()
        self.policy_side = self.rng.choice("XO") if opponent else None
        self.moves: list[tuple[str, int | None]] = []   # (player, cell) decisions made by the policy
        self.result: str | None = None    # "X", "O", "draw", "illegal:X"
        if self.opponent and self.player != self.policy_side:
            self._opponent_move()

    @property
    def done(self) -> bool:
        return self.result is not None

    def free(self) -> list[int]:
        return [i for i, c in enumerate(self.board) if c == "."]

    def prompt(self) -> list[dict]:
        free = ", ".join(str(i + 1) for i in self.free())
        return [{"role": "system", "content": SYSTEM},
                {"role": "user", "content": f"Board:\n{render(self.board)}\n\nFree cells: {free}\n"
                                            f"You are {self.player}. Which cell do you play?"}]

    def explore(self, rng: random.Random) -> str | None:
        return str(rng.choice(self.free()) + 1)

    def _apply(self, cell: int | None, who: str) -> None:
        if cell is None or self.board[cell] != ".":
            self.result = f"illegal:{who}"
            return
        self.board[cell] = who
        w = winner(self.board)
        if w:
            self.result = w
        elif "." not in self.board:
            self.result = "draw"
        else:
            self.player = "O" if who == "X" else "X"

    def _opponent_move(self) -> None:
        free = [i for i, c in enumerate(self.board) if c == "."]
        self._apply(self.rng.choice(free), self.player)

    def act(self, text: str) -> None:
        who = self.player
        cell = parse_move(text)
        self.moves.append((who, cell))
        self._apply(cell, who)
        if not self.done and self.opponent and self.player != self.policy_side:
            self._opponent_move()

    def credits(self) -> list[float]:
        out = []
        for who, cell in self.moves:
            if self.result == f"illegal:{who}":
                out.append(-1.0)
            elif self.result and self.result.startswith("illegal"):
                out.append(0.0)              # the other side blundered the format; no signal
            elif self.result == "draw":
                out.append(0.2)              # a draw is respectable in tic-tac-toe
            elif self.result == who:
                out.append(1.0)
            else:
                out.append(-1.0)
        return out


class TicTacToe(Game):
    name = "tictactoe"
    description = "Tic-tac-toe self-play. Reward from the rules: win +1, loss -1, draw +0.2, illegal move -1."

    def new_episode(self, rng: random.Random) -> Episode:
        return TTTEpisode(rng=rng)

    def evaluate(self, policy: Policy, n: int, rng: random.Random) -> dict:
        eps = [TTTEpisode(opponent="random", rng=rng) for _ in range(n)]
        while any(not e.done for e in eps):
            live = [e for e in eps if not e.done]
            outs = policy([e.prompt() for e in live])
            for e, o in zip(live, outs):
                e.act(o)
        res = {"win": 0, "draw": 0, "loss": 0, "illegal": 0}
        for e in eps:
            if e.result.startswith("illegal"):
                res["illegal"] += 1
            elif e.result == "draw":
                res["draw"] += 1
            elif e.result == e.policy_side:
                res["win"] += 1
            else:
                res["loss"] += 1
        res["score"] = round((res["win"] - res["loss"] - res["illegal"]) / n, 3)
        res["opponent"] = "random"
        return res
