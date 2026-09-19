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

    def learned(self, kind: str, samples: int, loss, note: str = "") -> None:
        self.c.execute("INSERT INTO learning (ts, kind, samples, loss, note) VALUES (?,?,?,?,?)",
                       (time.time(), kind, samples, loss, note))
        self.c.commit()
