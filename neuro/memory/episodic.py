"""Episodic memory: what happened, what was learned, and how learning went.

This is the *non-weight* side of memory. It exists for two reasons: rehearsal
(replaying old episodes while learning new ones, to fight catastrophic
forgetting) and introspection (the model can recall past episodes and read its
own learning curves). Everything is append-only JSONL under state/.
"""
from __future__ import annotations

import json
import random
import re
import threading
import time
import uuid
from pathlib import Path


DEFAULT_PROBES = [
    {"q": "What is the capital of France?", "a": "The capital of France is Paris."},
    {"q": "What is 12 times 12?", "a": "12 times 12 is 144."},
    {"q": "Write a Python one-liner that prints hello world.", "a": "print(\"hello world\")"},
    {"q": "Who wrote 'Pride and Prejudice'?", "a": "Jane Austen wrote 'Pride and Prejudice'."},
    {"q": "Name the three primary colours of light.", "a": "Red, green and blue."},
    {"q": "Translate 'thank you' into French.", "a": "'Thank you' in French is 'merci'."},
    {"q": "What gas do plants absorb from the air?", "a": "Plants absorb carbon dioxide (CO2)."},
    {"q": "Sort these numbers ascending: 5, 2, 9, 1.", "a": "1, 2, 5, 9."},
]


class Memory:
    def __init__(self, state_dir: Path):
        self.dir = Path(state_dir)
        self.dir.mkdir(parents=True, exist_ok=True)
        self.episodes_path = self.dir / "episodes.jsonl"
        self.lessons_path = self.dir / "lessons.jsonl"
        self.log_path = self.dir / "learning_log.jsonl"
        self.probes_path = self.dir / "probes.json"
        self.games_dir = self.dir / "games"
        self.games_dir.mkdir(exist_ok=True)
        if not self.probes_path.exists():
            self.probes_path.write_text(json.dumps({"probes": DEFAULT_PROBES, "history": []}, indent=2))

    # ------------------------------------------------------------------ helpers
    _io = threading.Lock()

    @classmethod
    def _append(cls, path: Path, rec: dict) -> None:
        with cls._io, path.open("a") as f:
            f.write(json.dumps(rec, ensure_ascii=False) + "\n")

    @staticmethod
    def _read(path: Path) -> list[dict]:
        if not path.exists():
            return []
        out = []
        for line in path.read_text().splitlines():
            line = line.strip()
            if line:
                try:
                    out.append(json.loads(line))
                except json.JSONDecodeError:
                    pass
        return out

    # ------------------------------------------------------------------ episodes
    def add_episode(self, context: list[dict], turn: list[dict], tools_used: list[str], kind: str = "chat") -> str:
        eid = uuid.uuid4().hex[:10]
        self._append(self.episodes_path, {
            "id": eid, "ts": time.time(), "kind": kind, "context": context, "turn": turn,
            "tools_used": tools_used, "feedback": 0})
        return eid

    def episodes(self) -> list[dict]:
        return self._read(self.episodes_path)

    def set_feedback(self, eid: str, value: int) -> bool:
        eps = self.episodes()
        hit = False
        for e in eps:
            if e["id"] == eid:
                e["feedback"] = value
                hit = True
        if hit:
            self.episodes_path.write_text("".join(json.dumps(e, ensure_ascii=False) + "\n" for e in eps))
        return hit

    def sample_for_rehearsal(self, k: int, exclude: str | None = None) -> list[dict]:
        """Prefer episodes marked good, never replay ones marked bad."""
        pool = [e for e in self.episodes() if e.get("feedback", 0) >= 0 and e["id"] != exclude]
        if not pool:
            return []
        weights = [2.0 if e.get("feedback", 0) > 0 else 1.0 for e in pool]
        k = min(k, len(pool))
        return random.choices(pool, weights=weights, k=k)

    # ------------------------------------------------------------------ lessons
    def add_lesson(self, title: str, text: str, result: dict) -> None:
        self._append(self.lessons_path, {"ts": time.time(), "title": title, "text": text, "result": result})

    def lessons(self) -> list[dict]:
        return self._read(self.lessons_path)

    # ------------------------------------------------------------------ learning log
    def log(self, kind: str, **fields) -> None:
        self._append(self.log_path, {"ts": time.time(), "kind": kind, **fields})

    def learning_log(self, last: int = 200) -> list[dict]:
        return self._read(self.log_path)[-last:]

    # ------------------------------------------------------------------ probes (drift detector)
    def probes(self) -> dict:
        return json.loads(self.probes_path.read_text())

    def record_probe(self, losses: list[float], step: int) -> None:
        d = self.probes()
        d["history"].append({"ts": time.time(), "step": step, "losses": losses,
                             "mean": sum(losses) / max(1, len(losses))})
        self.probes_path.write_text(json.dumps(d, indent=2))

    # ------------------------------------------------------------------ recall
    def recall(self, query: str, k: int = 5) -> list[dict]:
        """Cheap lexical recall over lessons and episodes."""
        qt = set(_tokens(query))
        if not qt:
            return []
        scored = []
        for l in self.lessons():
            txt = l["title"] + " " + l["text"]
            s = _overlap(qt, txt)
            if s:
                scored.append((s, {"type": "lesson", "title": l["title"], "text": l["text"][:600]}))
        for e in self.episodes():
            txt = " ".join(str(m.get("content", "")) for m in e["turn"])
            s = _overlap(qt, txt)
            if s:
                snippet = " | ".join(f'{m["role"]}: {str(m.get("content",""))[:160]}' for m in e["turn"] if m.get("content"))
                scored.append((s, {"type": "episode", "id": e["id"], "when": _ago(e["ts"]), "text": snippet[:600]}))
        scored.sort(key=lambda x: -x[0])
        return [r for _, r in scored[:k]]

    # ------------------------------------------------------------------ full transcripts
    @staticmethod
    def transcript(ep: dict, max_chars: int = 4000) -> dict:
        lines = []
        for m in ep.get("context", []) + ep["turn"]:
            c = m.get("content") or ""
            if m.get("tool_calls"):
                c += " " + " ".join(f'[call {t["function"]["name"]}({json.dumps(t["function"]["arguments"], ensure_ascii=False)[:200]})]'
                                    for t in m["tool_calls"])
            lines.append(f'{m["role"]}: {c}')
        text = "\n".join(lines)
        return {"id": ep["id"], "kind": ep.get("kind", "chat"), "when": _ago(ep["ts"]), "feedback": ep.get("feedback", 0),
                "transcript": text[-max_chars:], "truncated": len(text) > max_chars}

    def episode(self, eid: str) -> dict | None:
        for e in self.episodes():
            if e["id"] == eid or e["id"].startswith(eid):
                return e
        return None

    def recent_episodes(self, n: int = 5, kind: str | None = None) -> list[dict]:
        eps = self.episodes()
        if kind:
            eps = [e for e in eps if e.get("kind", "chat") == kind or e.get("kind", "").startswith(kind)]
        return eps[-n:]

    def search_episodes(self, query: str, k: int = 5) -> list[dict]:
        qt = set(_tokens(query))
        scored = []
        for e in self.episodes():
            txt = " ".join(str(m.get("content", "")) for m in e["turn"])
            sc = _overlap(qt, txt)
            if sc:
                scored.append((sc, e))
        scored.sort(key=lambda x: -x[0])
        return [e for _, e in scored[:k]]

    def stats(self) -> dict:
        eps = self.episodes()
        return {"episodes": len(eps), "good": sum(1 for e in eps if e.get("feedback", 0) > 0),
                "bad": sum(1 for e in eps if e.get("feedback", 0) < 0), "lessons": len(self.lessons()),
                "log_entries": len(self._read(self.log_path))}


def _tokens(s: str) -> list[str]:
    return [t for t in re.findall(r"[a-zA-Z0-9éèàùçâêîôû]+", s.lower()) if len(t) > 2]


def _overlap(q: set[str], text: str) -> float:
    t = set(_tokens(text))
    if not t:
        return 0.0
    return len(q & t) / (len(q) ** 0.5 * len(t) ** 0.5)


def _ago(ts: float) -> str:
    d = time.time() - ts
    for unit, n in (("d", 86400), ("h", 3600), ("m", 60)):
        if d >= n:
            return f"{int(d // n)}{unit} ago"
    return "just now"
