"""The hippocampus: prepares training samples from what happened, so that learning is a
meta-process and not a decision the conscious thread takes.

At a night (or on demand) it reads the journal since the last pass, asks the model to list
the facts that were *stated* there (by the person, or read from a source) as question/answer
pairs, keeps only answers that quote the journal verbatim (no invention), measures how
surprising each still is to the weights, and appends the surprising ones to the `facts`
training set. Things the person asked to remember are marked urgent.
"""
from __future__ import annotations

import json
import re
import time
from pathlib import Path

from ..brain import build_sample
from ..config import Config
from ..memory import Memory, Journal
from .learner import REHEARSAL_SYSTEM
from .trainingset import TrainingSets

EXTRACT_PROMPT = """Below is a transcript. List the concrete facts that are STATED in it as true (by the person talking, or quoted from a source it read): names, numbers, dates, places, decisions, preferences. Ignore questions, opinions about the assistant, and anything not actually asserted.

Reply with one JSON object per line, nothing else:
{{"q": "a question this fact answers", "a": "the fact, quoting the transcript's own words", "source": "who said it or where it was read", "remember": true|false}}
Set "remember": true only when the person explicitly asked to remember or learn it.

Transcript:
{transcript}"""

WEIGHTS = {"user": 0.0, "assistant": 1.0, "system": 0.0, "tool": 0.0}


class Hippocampus:
    def __init__(self, brain, memory: Memory, journal: Journal, sets: TrainingSets, cfg: Config):
        self.brain, self.memory, self.journal, self.sets, self.cfg = brain, memory, journal, sets, cfg
        self.state_path = Path(cfg.state) / "hippocampus.json"
        self.state = json.loads(self.state_path.read_text()) if self.state_path.exists() else {"last_ts": 0.0}

    def _transcript_since(self, since: float, max_chars: int = 9000) -> tuple[str, float]:
        recs = [r for r in self.journal.tail(400) if r.get("ts", 0) > since]
        lines = []
        for r in recs:
            role = r.get("role")
            if role == "user" and r.get("kind", "user") in ("user", "command"):
                lines.append(f"person: {r.get('content', '')}")
            elif role == "assistant" and r.get("content"):
                lines.append(f"assistant: {r['content']}")
            elif role == "tool" and r.get("name") == "shell":
                try:
                    out = json.loads(r.get("content") or "{}").get("stdout", "")
                except json.JSONDecodeError:
                    out = ""
                if out and (r.get("args") or {}).get("command", "").startswith(("web ", "news")):
                    lines.append(f"read ({(r.get('args') or {}).get('command', '')[:60]}): {out[:1500]}")
        text = "\n".join(lines)
        return text[-max_chars:], (recs[-1]["ts"] if recs else since)

    @staticmethod
    def _grounded(answer: str, transcript: str) -> bool:
        """The answer must quote the transcript: every run of 5+ words in it must appear there."""
        norm = lambda s: re.sub(r"\s+", " ", s.lower())
        t = norm(transcript)
        words = norm(answer).split()
        if len(words) < 3:
            return False
        if len(words) <= 6:
            return norm(answer) in t
        for i in range(0, len(words) - 4, 3):
            if " ".join(words[i:i + 5]) not in t:
                return False
        return True

    def run(self, surprise_threshold: float = 1.0, max_facts: int = 20, on_progress=None) -> dict:
        t0 = time.time()
        transcript, last_ts = self._transcript_since(self.state.get("last_ts", 0.0))
        if len(transcript) < 80:
            return {"candidates": 0, "kept": 0, "note": "nothing new in the journal"}
        raw = self.brain.generate([{"role": "system", "content": "You extract facts from transcripts. You output only JSON lines."},
                                   {"role": "user", "content": EXTRACT_PROMPT.format(transcript=transcript)}],
                                  max_new_tokens=700, temperature=0.0, enable_thinking=False)
        kept, candidates, rejected = [], 0, {"ungrounded": 0, "known": 0, "malformed": 0}
        for line in raw.splitlines():
            line = line.strip().strip(",")
            if not line.startswith("{"):
                continue
            try:
                c = json.loads(line)
            except json.JSONDecodeError:
                rejected["malformed"] += 1
                continue
            q, a = str(c.get("q", "")).strip(), str(c.get("a", "")).strip()
            if not q or not a:
                continue
            candidates += 1
            if not self._grounded(a, transcript):
                rejected["ungrounded"] += 1
                continue
            msgs = [{"role": "system", "content": REHEARSAL_SYSTEM}, {"role": "user", "content": q}, {"role": "assistant", "content": a}]
            surprise = self.brain.sample_loss(build_sample(self.brain.tok, msgs, WEIGHTS, max_len=self.cfg.train_max_len))
            urgent = bool(c.get("remember"))
            if surprise < surprise_threshold and not urgent:
                rejected["known"] += 1
                continue
            src = str(c.get("source", "conversation"))[:80]
            self.sets.append("facts", {"kind": "sft", "messages": msgs, "weights": WEIGHTS, "urgent": urgent,
                                       "target_loss": 0.2 if urgent else None, "source": f"{src}, {time.strftime('%Y-%m-%d')}",
                                       "surprise": round(surprise, 3), "by": "hippocampus"})
            self.memory.add_lesson(f"fact: {q[:80]}", a, {"kind": "fact", "source": src, "question": q, "surprise": round(surprise, 3)})
            kept.append({"q": q[:80], "surprise": round(surprise, 2), "urgent": urgent})
            if on_progress:
                on_progress(f"fact kept ({surprise:.2f}): {q[:70]}")
            if len(kept) >= max_facts:
                break
        self.state["last_ts"] = last_ts
        self.state_path.write_text(json.dumps(self.state))
        result = {"candidates": candidates, "kept": len(kept), "rejected": rejected, "facts": kept, "seconds": round(time.time() - t0, 1)}
        self.memory.log("hippocampus", **{k: v for k, v in result.items() if k != "facts"}, step=self.brain.meta["steps"])
        return result
