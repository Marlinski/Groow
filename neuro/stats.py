"""The core's statistics, read and written from the root side.

The core owns this database and keeps it unreadable to the mind, which is the
point of it: how a turn scored is not something the mind can see or edit. The
passes that score and harvest run as the core does, so they can open it.
"""
from __future__ import annotations

import sqlite3
import time
from pathlib import Path


class Stats:
    def __init__(self, state: Path):
        self.c = sqlite3.connect(Path(state) / "groow.db")
        self.c.row_factory = sqlite3.Row

    # ------------------------------------------------------------ reading
    def unscored_turns(self, limit: int = 20) -> list[sqlite3.Row]:
        """Turns that have ended and have not yet been felt, oldest first."""
        return list(self.c.execute(
            "SELECT t.* FROM turns t LEFT JOIN feelings f ON f.turn = t.id "
            "WHERE t.ended IS NOT NULL AND f.id IS NULL ORDER BY t.started LIMIT ?", (limit,)))

    def felt_turns_since(self, since: float) -> list[sqlite3.Row]:
        """Turns the mind actually completed, with what they felt like.

        Only turns that ended properly. An abandoned one finishes with the core's own apology
        in the conversation, and practising that would teach it to apologise.
        """
        return list(self.c.execute(
            "SELECT t.*, f.valence FROM turns t JOIN feelings f ON f.turn = t.id "
            "WHERE t.started > ? AND t.outcome = 'ok' ORDER BY t.started", (since,)))

    # ------------------------------------------------------------ writing
    def felt(self, turn: str, ts: float, sensors: float, approval, valence: float) -> None:
        self.c.execute(
            "INSERT INTO feelings (turn, ts, sensors, approval, valence) VALUES (?,?,?,?,?)",
            (turn, ts, sensors, approval, valence))
        self.c.commit()

    def learned(self, kind: str, samples: int, loss, note: str = "", seconds: float | None = None) -> None:
        """Record one pass: what it was, how much it practised, what it cost, how long it took.

        `seconds` is wall clock, which is the number anyone actually wants: a night that takes
        four minutes and a night that takes forty are different events even when they did the
        same work.
        """
        self.c.execute(
            "INSERT INTO learning (ts, kind, samples, loss, seconds, note) VALUES (?,?,?,?,?,?)",
            (time.time(), kind, samples, loss, seconds, note))
        self.c.commit()
