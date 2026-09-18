"""A *game* is any situation with rules that can score the model's actions
without a human in the loop. The model plays, the rules hand out rewards, and
the rewards become weight updates. Nothing here knows about neural networks.

Protocol
--------
Game.new_episode(rng) -> Episode
Episode.done            -> bool
Episode.prompt()        -> list[message]   what the policy sees for its next decision
Episode.act(text)       -> None            apply the policy's raw text output
Episode.credits()       -> list[float]     reward per decision, same order as the decisions were made
Game.evaluate(policy, n, rng) -> dict      how good is the policy? (policy: list[prompt] -> list[text])
"""
from __future__ import annotations

import random
from abc import ABC, abstractmethod
from typing import Callable

Policy = Callable[[list[list[dict]]], list[str]]   # batch of prompts -> batch of completions


class Episode(ABC):
    @property
    @abstractmethod
    def done(self) -> bool: ...

    @abstractmethod
    def prompt(self) -> list[dict]: ...

    @abstractmethod
    def act(self, text: str) -> None: ...

    @abstractmethod
    def credits(self) -> list[float]: ...

    def explore(self, rng: random.Random) -> str | None:
        """Optional: a random *legal* action as text, used for epsilon-exploration
        when the policy has collapsed onto one answer. None = no exploration."""
        return None


class Game(ABC):
    name: str = "game"
    description: str = ""

    @abstractmethod
    def new_episode(self, rng: random.Random) -> Episode: ...

    @abstractmethod
    def evaluate(self, policy: Policy, n: int, rng: random.Random) -> dict: ...
