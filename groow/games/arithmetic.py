"""Mental arithmetic drill. The rules know the answer; the model does not get told
it, only whether it was right."""
from __future__ import annotations

import random
import re

from .base import Episode, Game, Policy

SYSTEM = "Answer with the final number only."


class ArithEpisode(Episode):
    def __init__(self, rng: random.Random, level: int = 2):
        ops = ["+", "-", "*"]
        op = rng.choice(ops)
        hi = {1: 20, 2: 99, 3: 999}[level]
        a, b = rng.randint(2, hi), rng.randint(2, hi)
        if op == "*":
            b = rng.randint(2, 12 if level < 3 else 99)
        self.question = f"{a} {op} {b}"
        self.answer = eval(self.question)
        self.reply: str | None = None
        self.reward: float | None = None

    @property
    def done(self) -> bool:
        return self.reward is not None

    def prompt(self) -> list[dict]:
        return [{"role": "system", "content": SYSTEM}, {"role": "user", "content": f"What is {self.question}?"}]

    def act(self, text: str) -> None:
        self.reply = text
        nums = re.findall(r"-?\d+", text.replace(",", ""))
        if not nums:
            self.reward = -1.0
        else:
            self.reward = 1.0 if int(nums[-1]) == self.answer else -0.5

    def credits(self) -> list[float]:
        return [self.reward]


class Arithmetic(Game):
    name = "arithmetic"
    description = "Mental arithmetic (+, -, x up to 3 digits). Reward: correct +1, wrong -0.5, no number -1."

    def __init__(self, level: int = 2):
        self.level = level

    def new_episode(self, rng: random.Random) -> Episode:
        return ArithEpisode(rng, self.level)

    def evaluate(self, policy: Policy, n: int, rng: random.Random) -> dict:
        eps = [ArithEpisode(rng, self.level) for _ in range(n)]
        outs = policy([e.prompt() for e in eps])
        for e, o in zip(eps, outs):
            e.act(o)
        correct = sum(1 for e in eps if e.reward == 1.0)
        return {"accuracy": round(correct / n, 3), "n": n, "level": self.level}
