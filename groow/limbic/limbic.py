"""The limbic system: what happened becomes how it felt.

It is the only thing that assigns valence, and neither the conscious mind nor a skill
can write into it. Two channels feed it:

    sensors   free, unambiguous, from the transcript itself (a command failed, a call
              timed out, the same call twice, no answer, a fix after a failure)
    judge     a small frozen scorer for the ambiguous part (how the person reacted)

Approval arrives late: you do not know whether what you said landed until the person
answers. So a turn is held *pending* until the next human message, judged as a reaction
to it, and only then finalised. Signals with no reaction (an idle impulse, a reminder,
an inner thought's report) finalise at once with sensors only.

Two accumulators with decay, pain and pleasure, persist across sessions. They are the
mood: sleep pressure rises with pain and with steps since the last night; boredom rises
when nothing has been surprising for a while.
"""
from __future__ import annotations

import json
import time
from pathlib import Path

from . import sensors
from .judge import make_judge


class Limbic:
    def __init__(self, state_dir: Path | str, judge_kind: str = "laya", decay_halflife_s: float = 1800.0,
                 hold_seconds: float = 300.0):
        self.dir = Path(state_dir) / "limbic"
        self.dir.mkdir(parents=True, exist_ok=True)
        self.valence_log = self.dir / "valence.jsonl"
        self.state_path = self.dir / "state.json"
        self.pending_path = self.dir / "pending.json"
        self.judge = make_judge(judge_kind)
        if hasattr(self.judge, "warm"):
            self.judge.warm()
        self.halflife = decay_halflife_s
        self.hold_seconds = hold_seconds
        self.state = json.loads(self.state_path.read_text()) if self.state_path.exists() else {
            "pain": 0.0, "pleasure": 0.0, "ts": time.time(), "turns": 0, "surprise_mean": None}
        self.pending = json.loads(self.pending_path.read_text()) if self.pending_path.exists() else None

    # ------------------------------------------------------------------ accumulators
    def _decay(self) -> None:
        now = time.time()
        dt = max(0.0, now - self.state.get("ts", now))
        k = 0.5 ** (dt / self.halflife)
        self.state["pain"] *= k
        self.state["pleasure"] *= k
        self.state["ts"] = now

    def _accumulate(self, valence: float) -> None:
        self._decay()
        if valence < 0:
            self.state["pain"] += -valence
        else:
            self.state["pleasure"] += valence
        self.state["turns"] = self.state.get("turns", 0) + 1
        self.state_path.write_text(json.dumps(self.state))

    def mood(self) -> dict:
        self._decay()
        pain, pleasure = round(self.state["pain"], 3), round(self.state["pleasure"], 3)
        tone = "content" if pleasure - pain > 0.8 else "sore" if pain - pleasure > 0.8 else "even"
        return {"pain": pain, "pleasure": pleasure, "tone": tone, "turns": self.state.get("turns", 0),
                "judge": self.judge.name if getattr(self.judge, "available", False) else "none"}

    # ------------------------------------------------------------------ one turn
    def feel(self, turn_messages: list[dict], flags: list[str], final_text: str, kind: str = "user",
             user_text: str = "") -> dict:
        """Sense a finished turn. Returns the record that is ready to be learned from (the *previous*
        turn, once the person has reacted), or this turn's own record when no reaction is expected."""
        sig = sensors.turn_signals(turn_messages, flags, final_text)
        own = {"ts": time.time(), "kind": kind, "signals": sig, "sensor_valence": round(sensors.valence_of(sig), 3),
               "final": final_text[:600], "user_text": user_text[:600], "approval": None, "valence": None}

        ready = None
        # 1. this human message is the reaction to the turn that is pending
        if kind == "user" and self.pending is not None:
            prev = self.pending
            approval = self.judge.reaction(prev.get("final", ""), user_text) if user_text else None
            if approval is None and user_text and sensors.similarity(user_text, prev.get("user_text", "")) > 0.55:
                approval = sensors.WEIGHTS["restated"]          # they had to say it again
            prev["approval"] = None if approval is None else round(approval, 3)
            prev["valence"] = round(prev["sensor_valence"] + (approval or 0.0), 3)
            ready = prev
            self.pending = None
        # 2. a turn nobody will react to is complete as it stands
        if kind != "user":
            own["valence"] = own["sensor_valence"]
            ready = own if ready is None else ready
            if ready is not own:
                self._remember(own)
        else:
            self.pending = own
        self.pending_path.write_text(json.dumps(self.pending) if self.pending else "null")
        if ready is not None:
            self._remember(ready)
        return ready or {"valence": None, "pending": True, "signals": sig,
                         "sensor_valence": own["sensor_valence"]}

    def _remember(self, rec: dict) -> None:
        if rec.get("valence") is None:
            rec["valence"] = rec.get("sensor_valence", 0.0)
        with self.valence_log.open("a") as f:
            f.write(json.dumps(rec, ensure_ascii=False, default=str) + "\n")
        self._accumulate(rec["valence"])

    def flush_pending(self, force: bool = False) -> dict | None:
        """Nobody reacted. A turn waits `hold_seconds` for a reaction before it is felt on sensors
        alone: silence is neutral, but only after we have really waited for it."""
        if self.pending is None:
            return None
        if not force and time.time() - self.pending.get("ts", 0) < self.hold_seconds:
            return None
        rec = self.pending
        self.pending = None
        self.pending_path.write_text("null")
        self._remember(rec)
        return rec

    # ------------------------------------------------------------------ per-decision credit
    def credit(self, turn_messages: list[dict], turn_valence: float, use_judge: bool = True) -> list[tuple[int, float, str]]:
        """Spread a turn's valence over the tool calls that produced it: each call is judged by what came
        back from it, plus a share of how the turn ended, discounted the further back it is."""
        calls = [(i, m) for i, m in enumerate(turn_messages) if m["role"] == "assistant" and m.get("tool_calls")]
        out = []
        n = len(calls)
        for rank, (i, m) in enumerate(calls):
            results = [t for t in turn_messages[i + 1:i + 1 + len(m["tool_calls"])] if t["role"] == "tool"]
            local, tag = 0.0, "ok"
            for r in results:
                c = r.get("content", "")
                if sensors.is_timeout(c):
                    local += sensors.WEIGHTS["timeout"]; tag = "timeout"
                elif sensors.is_repeat_note(c):
                    local += sensors.WEIGHTS["repeat"]; tag = "repeat"
                elif sensors.is_error(c):
                    local += sensors.WEIGHTS["tool_error"]; tag = "error"
                else:
                    local += 0.1; tag = "ok" if tag == "ok" else tag
            if tag == "ok" and rank > 0 and out and out[-1][2] in ("error", "timeout"):
                local += sensors.WEIGHTS["recovered"]
                tag = "recovered"
            if use_judge and getattr(self.judge, "available", False) and results and tag in ("ok", "recovered"):
                args = m["tool_calls"][0]["function"].get("arguments", {})
                cmd = args.get("command") or json.dumps(args, ensure_ascii=False)
                u = self.judge.outcome(cmd[:300], results[0].get("content", ""))
                if u is not None:
                    local += u
                    tag = "judged" if tag == "ok" else tag
            share = 0.6 ** (n - 1 - rank)                     # the last call carries most of the outcome
            spill = turn_valence * share
            if tag in ("error", "timeout", "repeat") and spill > 0:
                spill = 0.0                                   # a failure inside a good turn is still a failure
            out.append((i, round(local + spill, 3), tag))
        return out

    # ------------------------------------------------------------------ drives
    def drives(self, steps_since_night: int, sleep_every: int) -> dict:
        self._decay()
        pressure = min(1.0, steps_since_night / max(1, sleep_every)) * 0.7 + min(1.0, self.state["pain"] / 5.0) * 0.3
        return {"sleep_pressure": round(pressure, 3), "pain": round(self.state["pain"], 3),
                "pleasure": round(self.state["pleasure"], 3)}
