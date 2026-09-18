"""Training sets: files in the home that say what to learn. Anyone may append (a skill's
self-play, the hippocampus, the passive step, the mentor); only the trainer consumes.

    state/training/<set>.jsonl     one sample per line, append-only
    state/training/cursor.json     how many lines of each set have been consumed

Sample kinds:
  {"kind": "sft", "messages": [{"role", "content"}...], "weights": {"user": .5, "assistant": 1}, "urgent": bool,
   "target_loss": float|null, "source": str}
  {"kind": "pg", "prompt": [messages], "completion": "text", "reward": float, "group": "id", "source": str}
`group` ties the decisions of one batch together: advantages are normalised within a group.
"""
from __future__ import annotations

import json
import os
import time
from pathlib import Path


class TrainingSets:
    def __init__(self, directory: Path | str):
        self.dir = Path(directory)
        self.dir.mkdir(parents=True, exist_ok=True)
        self.cursor_path = self.dir / "cursor.json"
        self.cursor: dict[str, int] = json.loads(self.cursor_path.read_text()) if self.cursor_path.exists() else {}

    # ------------------------------------------------------------------ produce
    def append(self, set_name: str, sample: dict) -> dict:
        name = "".join(c for c in set_name if c.isalnum() or c in "_-")[:40] or "misc"
        sample = {"ts": time.time(), **sample}
        with (self.dir / f"{name}.jsonl").open("a", encoding="utf-8") as f:
            f.write(json.dumps(sample, ensure_ascii=False, default=str) + "\n")
            f.flush()
            os.fsync(f.fileno())
        return sample

    # ------------------------------------------------------------------ consume
    def sets(self) -> list[str]:
        return sorted(p.stem for p in self.dir.glob("*.jsonl"))

    def pending(self, set_name: str | None = None, limit: int | None = None, urgent_only: bool = False) -> list[tuple[str, int, dict]]:
        """(set, line_index, sample) for unconsumed samples, oldest first."""
        out = []
        for name in ([set_name] if set_name else self.sets()):
            p = self.dir / f"{name}.jsonl"
            if not p.exists():
                continue
            start = self.cursor.get(name, 0)
            with p.open(encoding="utf-8") as f:
                for i, line in enumerate(f):
                    if i < start or not line.strip():
                        continue
                    try:
                        s = json.loads(line)
                    except json.JSONDecodeError:
                        continue
                    if urgent_only and not s.get("urgent"):
                        continue
                    out.append((name, i, s))
                    if limit and len(out) >= limit:
                        return out
        return out

    def mark(self, set_name: str, upto_line: int) -> None:
        """Everything up to and including this line of the set is consumed."""
        self.cursor[set_name] = max(self.cursor.get(set_name, 0), upto_line + 1)
        self.cursor_path.write_text(json.dumps(self.cursor, indent=1))

    def counts(self) -> dict:
        out = {}
        for name in self.sets():
            total = sum(1 for _ in (self.dir / f"{name}.jsonl").open("rb"))
            out[name] = {"total": total, "pending": max(0, total - self.cursor.get(name, 0))}
        return out
