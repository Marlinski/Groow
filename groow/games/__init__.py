"""Game registry. Built-in games plus games the model invented (state/games/*.py)."""
from __future__ import annotations

import random
from pathlib import Path

from .base import Episode, Game, Policy
from .tictactoe import TicTacToe
from .arithmetic import Arithmetic

BUILTIN: dict[str, Game] = {
    "tictactoe": TicTacToe(),
    "arithmetic": Arithmetic(level=2),
    "arithmetic-easy": Arithmetic(level=1),
    "arithmetic-hard": Arithmetic(level=3),
}


def load_invented(games_dir: Path) -> dict[str, Game]:
    """Execute state/games/<name>.py files; each must define GAME (a Game instance)."""
    out = {}
    for p in sorted(Path(games_dir).glob("*.py")):
        ns: dict = {"Game": Game, "Episode": Episode, "random": random}
        try:
            exec(compile(p.read_text(), str(p), "exec"), ns)
            g = ns.get("GAME")
            if isinstance(g, Game):
                g.name = p.stem
                out[p.stem] = g
        except Exception as e:  # a broken invented game must not crash the brain
            print(f"[games] could not load {p.name}: {e}")
    return out


def validate_game_source(source: str) -> tuple[bool, str]:
    """Dry-run a candidate game with a dumb random-text policy."""
    ns: dict = {"Game": Game, "Episode": Episode, "random": random}
    try:
        exec(compile(source, "<invented>", "exec"), ns)
        g = ns.get("GAME")
        if not isinstance(g, Game):
            return False, "source must define GAME, an instance of a Game subclass"
        rng = random.Random(0)
        for _ in range(3):
            ep = g.new_episode(rng)
            steps = 0
            while not ep.done and steps < 50:
                p = ep.prompt()
                assert isinstance(p, list) and p and "role" in p[0], "prompt() must return chat messages"
                ep.act(str(rng.randint(0, 100)))
                steps += 1
            cr = ep.credits()
            assert isinstance(cr, list) and all(isinstance(c, (int, float)) for c in cr), "credits() must return a list of numbers"
        res = g.evaluate(lambda prompts: ["1" for _ in prompts], 3, rng)
        assert isinstance(res, dict), "evaluate() must return a dict"
        return True, f"ok; sample evaluation: {res}"
    except Exception as e:
        return False, f"{type(e).__name__}: {e}"


__all__ = ["Game", "Episode", "Policy", "BUILTIN", "load_invented", "validate_game_source"]
