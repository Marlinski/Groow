"""The hippocampus: logs become training sets. Nothing else writes training data.

The conscious mind acts and leaves traces; the limbic system says how those traces felt;
this turns both into samples. Neither the mind nor a skill can annotate its own reward.

Sources (files in the home, each with a cursor so nothing is learned twice):
    state/main/*.jsonl        the conversation journal: every message, every tool call and result
    state/log/activity.jsonl  what skills log when they run in bulk (a game's decisions)
Output:
    state/training/<set>.jsonl  samples the trainer consumes

Three passes:
    nap(turn)          the turn that just ended: held until its valence is final (approval is
                       late), then the exchange becomes a supervised sample weighted by how it
                       felt, and every tool call becomes a decision with its own credit
    digest_activity()  new activity records become rewarded samples
    night()            facts stated in the day's transcript (verbatim quotes only, surprise-gated)
"""
from __future__ import annotations

import json
import re
import time
from pathlib import Path

from ..brain import build_sample, trim_messages
from ..config import Config
from ..memory import Memory, Journal
from .prompts import REHEARSAL_SYSTEM
from .trainingset import TrainingSets

EXTRACT_PROMPT = """Below is a transcript between a person ({person}) and an assistant. List the concrete facts that are STATED in it as true (by {person}, or quoted from a source that was read): names, numbers, dates, places, decisions, preferences. Ignore questions, opinions about the assistant, dates of today, and anything not actually asserted.

Reply with one JSON object per line, nothing else:
{{"q": "a question about this fact, in the third person (say '{person}', never 'my' or 'I')", "a": "the fact, quoting the transcript's own words exactly", "source": "{person}, or the name of the source it was read from"}}

Transcript:
{transcript}"""

ANSWER_WEIGHTS = {"user": 0.0, "assistant": 1.0, "system": 0.0, "tool": 0.0}


def _tool_call_text(calls: list) -> str:
    """The text the model emitted for these calls, as the chat template renders it."""
    return "\n".join('<tool_call>\n{"name": "%s", "arguments": %s}\n</tool_call>'
                     % (c["function"]["name"], json.dumps(c["function"]["arguments"], ensure_ascii=False, sort_keys=True))
                     for c in calls)


class Hippocampus:
    def __init__(self, brain, memory: Memory, journal: Journal, sets: TrainingSets, cfg: Config, limbic=None):
        self.brain, self.memory, self.journal, self.sets, self.cfg = brain, memory, journal, sets, cfg
        self.limbic = limbic
        self.person = "the person"
        self.activity = Path(cfg.state) / "log" / "activity.jsonl"
        self.activity.parent.mkdir(parents=True, exist_ok=True)
        self.held_path = Path(cfg.state) / "limbic" / "held.json"
        self.held_path.parent.mkdir(parents=True, exist_ok=True)
        self.awaiting_path = Path(cfg.state) / "limbic" / "awaiting.json"
        self.state_path = Path(cfg.state) / "hippocampus.json"
        self.state = json.loads(self.state_path.read_text()) if self.state_path.exists() else {"night_ts": 0.0, "activity_line": 0}

    def _save(self) -> None:
        self.state_path.write_text(json.dumps(self.state))

    # ================================================================== nap: one finished turn
    def nap(self, turn_messages: list[dict], context: list[dict], flags: list[str], kind: str = "user",
            user_text: str = "", final_text: str = "") -> dict:
        out = {"held": 0, "sft": 0, "decisions": 0, "facts": 0, "skipped": None, "valence": None, "pending": False}
        ctx = [m for m in trim_messages(context, self.cfg.context_messages_kept) if m["role"] != "system"]
        bad = bool(flags) and not self.cfg.learn_from_bad_turns
        eid = self.memory.add_episode(ctx, turn_messages,
                                      [c["function"]["name"] for m in turn_messages for c in m.get("tool_calls") or []],
                                      kind="chat:bad" if bad else "chat")
        felt = self.limbic.feel(turn_messages, flags, final_text, kind=kind, user_text=user_text) if self.limbic else {"valence": 0.0}
        self._hold(eid, ctx, turn_messages, kind, bad)
        out["held"] = len(self._held())
        if felt.get("pending"):
            out["pending"] = True
            out["valence"] = felt.get("sensor_valence")
        else:
            out.update(self._release(float(felt.get("valence") or 0.0)))
        self.memory.log("nap", **out, step=self.brain.meta["steps"])
        return out

    # ---- turns wait here until their valence is final (approval arrives with the next message)
    def _held(self) -> list:
        if not self.held_path.exists():
            return []
        try:
            return json.loads(self.held_path.read_text())
        except json.JSONDecodeError:
            return []

    def _hold(self, eid: str, ctx: list[dict], turn: list[dict], kind: str, bad: bool) -> None:
        held = self._held()
        held.append({"eid": eid, "ctx": ctx, "turn": turn, "kind": kind, "bad": bad, "ts": time.time()})
        self.held_path.write_text(json.dumps(held[-8:], ensure_ascii=False, default=str))

    def _release(self, valence: float) -> dict:
        """The oldest held turn now has a valence: turn it into samples."""
        out = {"sft": 0, "decisions": 0, "facts": 0, "skipped": None, "valence": round(valence, 3)}
        held = self._held()
        if not held:
            return out
        rec = held.pop(0)
        self.held_path.write_text(json.dumps(held, ensure_ascii=False, default=str))
        ctx, turn = rec["ctx"], rec["turn"]
        if rec["bad"]:
            out["skipped"] = ["bad turn"]
            self.memory.set_feedback(rec["eid"], -1)
            return out
        # 1. the exchange, weighted by how it felt
        weights = dict(self.cfg.role_weights)
        if valence < -0.4:
            weights["assistant"] = -0.5                      # unlikelihood: make that answer less likely
            weights["user"] = 0.0
            self.memory.set_feedback(rec["eid"], -1)
        else:
            weights["assistant"] = weights.get("assistant", 1.0) * (1.0 + max(0.0, min(1.0, valence)))
            if rec["kind"] != "user":
                weights["user"] = 0.0
        msgs = [{"role": "system", "content": REHEARSAL_SYSTEM}] + ctx + turn
        self.sets.append("conversation", {"kind": "sft", "messages": msgs, "weights": weights, "episode": rec["eid"],
                                          "valence": round(valence, 3), "by": "hippocampus.nap"})
        out["sft"] = 1
        # 2. every tool call as a decision, credited by the limbic system
        history = [{"role": "system", "content": REHEARSAL_SYSTEM}] + list(ctx)
        prompts = {}
        for i, m in enumerate(turn):
            if m["role"] == "assistant" and m.get("tool_calls"):
                prompts[i] = [dict(x) for x in history]
            history.append(m)
        group = f"turn:{rec['eid']}"
        for i, reward, tag in (self.limbic.credit(turn, valence) if self.limbic else []):
            m = turn[i]
            completion = (m.get("content") or "").strip()
            completion = (completion + "\n" if completion else "") + _tool_call_text(m["tool_calls"])
            self.sets.append("actions", {"kind": "pg", "prompt": prompts.get(i, [dict(x) for x in ctx]),
                                         "completion": completion, "reward": reward, "group": group,
                                         "tags": [tag], "by": "hippocampus.nap"})
            out["decisions"] += 1
            self._await_outcome(m, prompts.get(i, []), completion, turn)
        return out

    # ---- decisions whose result comes much later (a question to the mentor) ----------------
    def _awaiting(self) -> dict:
        if not self.awaiting_path.exists():
            return {}
        try:
            return json.loads(self.awaiting_path.read_text())
        except json.JSONDecodeError:
            return {}

    def _await_outcome(self, msg: dict, prompt: list, completion: str, turn: list) -> None:
        """An `ask` call is judged by whether it is ever answered, which is not known yet. Keep what
        would be needed to credit it, and wait."""
        calls = [c for c in msg.get("tool_calls") or [] if c["function"]["name"] == "ask"]
        if not calls:
            return
        qid = None
        for j, t in enumerate(turn):
            if t.get("role") == "tool":
                try:
                    qid = json.loads(t.get("content") or "{}").get("id")
                except json.JSONDecodeError:
                    qid = None
                if qid:
                    break
        if not qid:
            return
        pending = self._awaiting()
        pending[qid] = {"prompt": prompt, "completion": completion, "ts": time.time()}
        self.awaiting_path.parent.mkdir(parents=True, exist_ok=True)
        self.awaiting_path.write_text(json.dumps(pending, ensure_ascii=False, default=str)[:400000])

    def credit_question(self, qid: str, status: str, reward: float) -> dict:
        """The late result of asking: answered, expired, or dropped for a newer question. The decision
        to ask is trained on that, long after the turn is over."""
        pending = self._awaiting()
        rec = pending.pop(qid, None)
        if rec is None:
            return {"credited": False}
        self.awaiting_path.write_text(json.dumps(pending, ensure_ascii=False, default=str))
        self.sets.append("actions", {"kind": "pg", "prompt": rec["prompt"], "completion": rec["completion"],
                                     "reward": float(reward), "group": f"question:{qid}",
                                     "tags": ["ask", status], "by": "hippocampus.question"})
        self.memory.log("question_credited", id=qid, status=status, reward=reward,
                        waited_s=round(time.time() - rec["ts"], 1), step=self.brain.meta["steps"])
        return {"credited": True, "status": status, "reward": reward}

    def flush(self, force: bool = False) -> dict:
        """Nobody is going to react: finalise on sensors alone, once the hold has really elapsed."""
        rec = self.limbic.flush_pending(force=force) if self.limbic else None
        if rec is None:
            return {"released": 0}
        return {"released": 1, **self._release(float(rec.get("valence") or 0.0))}

    # ================================================================== activity log (skills, games)
    def digest_activity(self) -> dict:
        if not self.activity.exists():
            return {"decisions": 0}
        start = self.state.get("activity_line", 0)
        n, last = 0, start - 1
        with self.activity.open(encoding="utf-8") as f:
            for i, line in enumerate(f):
                if i < start or not line.strip():
                    continue
                last = i
                try:
                    r = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if r.get("kind") == "decision" and {"prompt", "completion", "reward"} <= set(r):
                    self.sets.append(r.get("skill", "activity"), {"kind": "pg", "prompt": r["prompt"], "completion": str(r["completion"]),
                                                                  "reward": float(r["reward"]), "group": r.get("group", f"line{i}"),
                                                                  "tags": r.get("tags", []), "by": "hippocampus.activity"})
                    n += 1
                elif r.get("kind") == "example" and "messages" in r:
                    self.sets.append(r.get("skill", "activity"), {"kind": "sft", "messages": r["messages"], "weights": r.get("weights"),
                                                                  "tags": r.get("tags", []), "by": "hippocampus.activity"})
                    n += 1
        self.state["activity_line"] = last + 1
        self._save()
        if n:
            self.memory.log("digest", samples=n, step=self.brain.meta["steps"])
        return {"decisions": n}

    # ================================================================== night: facts from the day
    def _transcript_since(self, since: float, max_chars: int = 9000) -> tuple[str, float]:
        recs = [r for r in self.journal.tail(600) if r.get("ts", 0) > since]
        lines = []
        for r in recs:
            role = r.get("role")
            if role == "user" and r.get("kind", "user") in ("user", "command"):
                lines.append(f"person: {r.get('content', '')}")
            elif role == "assistant" and r.get("content"):
                lines.append(f"assistant: {r['content']}")
            elif role == "tool" and r.get("name") == "shell":
                cmd = (r.get("args") or {}).get("command", "")
                if cmd.startswith(("web ", "news")):
                    try:
                        out = json.loads(r.get("content") or "{}").get("stdout", "")
                    except json.JSONDecodeError:
                        out = ""
                    if out:
                        lines.append(f"read ({cmd[:60]}): {out[:1500]}")
        text = "\n".join(lines)
        return text[-max_chars:], (recs[-1]["ts"] if recs else since)

    @staticmethod
    def _grounded(answer: str, transcript: str) -> bool:
        norm = lambda s: re.sub(r"\s+", " ", s.lower())
        t, words = norm(transcript), norm(answer).split()
        if len(words) < 3:
            return False
        if len(words) <= 6:
            return norm(answer) in t
        return all(" ".join(words[i:i + 5]) in t for i in range(0, len(words) - 4, 3))

    def _extract_facts(self, transcript: str, threshold: float, max_facts: int = 20, on_progress=None) -> int:
        if len(transcript) < 80:
            return 0
        person = self.person
        raw = self.brain.generate([{"role": "system", "content": "You extract facts from transcripts. You output only JSON lines."},
                                   {"role": "user", "content": EXTRACT_PROMPT.format(transcript=transcript, person=person)}],
                                  max_new_tokens=700, temperature=0.0, enable_thinking=False)
        kept = 0
        for line in raw.splitlines():
            line = line.strip().strip(",")
            if not line.startswith("{"):
                continue
            try:
                c = json.loads(line)
            except json.JSONDecodeError:
                continue
            q, a = str(c.get("q", "")).strip(), str(c.get("a", "")).strip()
            if not q or not a or not self._grounded(a, transcript):
                continue
            src = str(c.get("source", person)).strip()[:80] or person
            stored = f'{src} said: "{a}"' if src.lower().startswith(person.lower()) else f'{src}: "{a}"'
            msgs = [{"role": "system", "content": REHEARSAL_SYSTEM}, {"role": "user", "content": q},
                    {"role": "assistant", "content": stored}]
            surprise = self.brain.sample_loss(build_sample(self.brain.tok, msgs, ANSWER_WEIGHTS, max_len=self.cfg.train_max_len))
            if surprise < threshold:
                continue
            self.sets.append("facts", {"kind": "sft", "messages": msgs, "weights": ANSWER_WEIGHTS,
                                       "source": f"{src}, {time.strftime('%Y-%m-%d')}", "surprise": round(surprise, 3),
                                       "by": "hippocampus.facts"})
            self.memory.add_lesson(f"fact: {q[:80]}", stored, {"kind": "fact", "source": src, "question": q,
                                                                "surprise": round(surprise, 3)})
            kept += 1
            if on_progress:
                on_progress(f"fact ({surprise:.2f}): {q[:70]}")
            if kept >= max_facts:
                break
        return kept

    def night(self, surprise_threshold: float = 1.0, on_progress=None) -> dict:
        t0 = time.time()
        flushed = self.flush()
        act = self.digest_activity()
        transcript, last_ts = self._transcript_since(self.state.get("night_ts", 0.0))
        kept = self._extract_facts(transcript, threshold=surprise_threshold, on_progress=on_progress) if transcript else 0
        self.state["night_ts"] = last_ts
        self._save()
        result = {"facts": kept, "activity_samples": act["decisions"], "released": flushed.get("released", 0),
                  "transcript_chars": len(transcript), "seconds": round(time.time() - t0, 1)}
        self.memory.log("hippocampus", **result, step=self.brain.meta["steps"])
        return result

    run = night
