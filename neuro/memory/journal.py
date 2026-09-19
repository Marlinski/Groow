"""Journal: an append-only, rotating trace on disk.

One directory, one JSON line per record, files named by the moment they were
opened (`2026-09-18_143005.jsonl`). A new file starts when the day changes or
the current file reaches `max_lines`. Every append is flushed and fsynced, so
what was written is on disk before the next thing happens; a crash loses at
most the record being written.

`tail(n)` reads the newest records back across files, which is how the main
thought rebuilds its rolling conversation after a reboot and how the UI gets
its replay.
"""
from __future__ import annotations

import json
import os
import time
from pathlib import Path


class Journal:
    def __init__(self, directory: Path | str, max_lines: int = 1000):
        self.dir = Path(directory)
        self.dir.mkdir(parents=True, exist_ok=True)
        self.max_lines = max_lines
        self._fh = None
        self._path: Path | None = None
        self._lines = 0
        self._day = ""
        self._resume()

    # ------------------------------------------------------------------ files
    def files(self) -> list[Path]:
        return sorted(self.dir.glob("*.jsonl"))

    def _resume(self) -> None:
        """Continue the newest file if it is from today and not full."""
        files = self.files()
        if not files:
            return
        last = files[-1]
        today = time.strftime("%Y-%m-%d")
        if last.name.startswith(today):
            n = sum(1 for _ in last.open("rb"))
            if n < self.max_lines:
                self._path, self._lines, self._day = last, n, today
                self._fh = last.open("a", encoding="utf-8")

    def _rotate(self) -> None:
        if self._fh:
            self._fh.close()
        self._day = time.strftime("%Y-%m-%d")
        name = time.strftime("%Y-%m-%d_%H%M%S") + ".jsonl"
        self._path = self.dir / name
        if self._path.exists():                      # two rotations in one second
            self._path = self.dir / (time.strftime("%Y-%m-%d_%H%M%S") + f"_{int(time.time()*1000)%1000:03d}.jsonl")
        self._fh = self._path.open("a", encoding="utf-8")
        self._lines = 0

    # ------------------------------------------------------------------ write
    def append(self, rec: dict) -> dict:
        rec = {"ts": rec.get("ts", time.time()), **{k: v for k, v in rec.items() if k != "ts"}}
        if self._fh is None or self._lines >= self.max_lines or self._day != time.strftime("%Y-%m-%d"):
            self._rotate()
        self._fh.write(json.dumps(rec, ensure_ascii=False, default=str) + "\n")
        self._fh.flush()
        try:
            os.fsync(self._fh.fileno())
        except OSError:
            pass
        self._lines += 1
        return rec

    # ------------------------------------------------------------------ read
    def tail(self, n: int) -> list[dict]:
        out: list[dict] = []
        for p in reversed(self.files()):
            lines = p.read_text(encoding="utf-8").splitlines()
            for line in reversed(lines):
                line = line.strip()
                if not line:
                    continue
                try:
                    out.append(json.loads(line))
                except json.JSONDecodeError:
                    continue
                if len(out) >= n:
                    return list(reversed(out))
        return list(reversed(out))

    def count(self) -> int:
        return sum(1 for p in self.files() for _ in p.open("rb"))

    def close(self) -> None:
        if self._fh:
            self._fh.close()
            self._fh = None
