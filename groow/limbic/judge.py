"""The judge: a small, frozen decision model that answers narrow questions about what happened.

It is hypothalamic, not cortical: it evaluates, it does not learn, and nothing in the loop
can write into it. Groow cannot edit it, a skill cannot state its own reward, and the
conscious mind has no tool that touches it.

    Judge.reaction(answer, reply)   -> valence in [-1, 1]: how the person took it
    Judge.outcome(command, result)  -> valence in [-1, 1]: whether a command did something

Default: Laya (convaiinnovations/laya), a 421M System One model, Apache-2.0, ~35 ms on CPU,
so it never competes with the brain for the GPU. Alternatives: a GLiClass scorer
(heman10x/rlcd-modernbert-151m, 151M) or nothing at all, in which case only the free
sensors speak.
"""
from __future__ import annotations

import threading

REACTION_Q = {"reaction": {"type": "choice", "instructions": "How did the person react to what Groow said?",
                           "criteria": {"pleased": "satisfied, thanks, praise, accepts the answer",
                                        "neutral": "moves on, asks something else, no judgement",
                                        "displeased": "corrects, complains, repeats the question, says it is wrong"}}}
OUTCOME_Q = {"outcome": {"type": "choice", "instructions": "How did this command turn out?",
                         "criteria": {"useful": "it produced the information or effect that was wanted",
                                      "nothing": "it ran but produced nothing useful",
                                      "failed": "it errored, was refused, or found nothing"}}}


class NullJudge:
    """No judge: only the free sensors speak."""
    name = "none"
    available = False
    calls = 0

    def reaction(self, answer: str, reply: str) -> float | None:
        return None

    def outcome(self, command: str, result: str) -> float | None:
        return None


class LayaJudge:
    """Laya: state + typed questions -> calibrated probabilities, one forward pass, CPU."""
    name = "laya"

    def warm(self) -> None:
        """Load in the background so the first nap does not wait for a download."""
        threading.Thread(target=self._safe_load, name="groow-judge-warm", daemon=True).start()

    def _safe_load(self) -> None:
        try:
            self._load()
        except Exception:
            self.available = False

    def __init__(self, model_id: str = "convaiinnovations/laya", min_margin: float = 0.15):
        self.model_id, self.min_margin = model_id, min_margin
        self._agent = None
        self._lock = threading.Lock()
        self.available = True
        self.calls = 0

    def _load(self):
        if self._agent is None:
            with self._lock:
                if self._agent is None:
                    import laya
                    self._agent = laya.load(self.model_id)
        return self._agent

    def _choice(self, state: dict, questions: dict, key: str, good: str, bad: str) -> float | None:
        try:
            r = self._load().predict(state, questions)["answers"][key]
        except Exception:
            self.available = False
            return None
        self.calls += 1
        p = r.get("probabilities") or {}
        v = float(p.get(good, 0.0)) - float(p.get(bad, 0.0))
        return None if abs(v) < self.min_margin else round(v, 3)

    def reaction(self, answer: str, reply: str) -> float | None:
        if not (answer or "").strip() or not (reply or "").strip():
            return None
        return self._choice({"groow_said": answer[:1500], "person_replied": reply[:1500]},
                            REACTION_Q, "reaction", "pleased", "displeased")

    def outcome(self, command: str, result: str) -> float | None:
        if not (command or "").strip():
            return None
        return self._choice({"command": command[:500], "result": (result or "")[:1200]},
                            OUTCOME_Q, "outcome", "useful", "failed")


class GLiClassJudge:
    """A zero-shot GLiClass scorer as the judge (smaller, noisier than Laya)."""
    name = "gliclass"
    REACTION = ["the person is pleased with the answer", "the person moves on", "the person is correcting or complaining"]
    REACTION_V = [0.8, 0.0, -0.8]
    OUTCOME = ["the command achieved what was asked", "the command did nothing useful"]

    def __init__(self, model_id: str = "heman10x/rlcd-modernbert-151m", device: str = "cpu", min_confidence: float = 0.6):
        self.model_id, self.device, self.min_confidence = model_id, device, min_confidence
        self._pipe = None
        self._lock = threading.Lock()
        self.available = True
        self.calls = 0

    def _pipeline(self):
        if self._pipe is None:
            with self._lock:
                if self._pipe is None:
                    from gliclass import GLiClassModel, ZeroShotClassificationPipeline
                    from transformers import AutoTokenizer
                    self._pipe = ZeroShotClassificationPipeline(
                        GLiClassModel.from_pretrained(self.model_id), AutoTokenizer.from_pretrained(self.model_id),
                        classification_type="single-label", device=self.device, progress_bar=False)
        return self._pipe

    def _ask(self, context: str, options: list[str]) -> tuple[int, float]:
        try:
            scored = self._pipeline()(context[-4000:], options, threshold=0.0)[0]
        except Exception:
            self.available = False
            return 0, 0.0
        self.calls += 1
        best = max(scored, key=lambda x: x["score"])
        return options.index(best["label"]), float(best["score"])

    def reaction(self, answer: str, reply: str) -> float | None:
        i, p = self._ask(f"Groow said: {answer[:1200]}\nThe person replied: {reply[:1200]}", self.REACTION)
        return None if p < self.min_confidence else round(self.REACTION_V[i] * p, 3)

    def outcome(self, command: str, result: str) -> float | None:
        i, p = self._ask(f"Groow ran: {command[:400]}\nThe result was: {result[:1200]}", self.OUTCOME)
        return None if p < self.min_confidence else round((0.5 if i == 0 else -0.5) * p, 3)


def make_judge(kind: str = "laya", **kw):
    kinds = {"laya": LayaJudge, "gliclass": GLiClassJudge}
    if kind in ("none", "", None) or kind not in kinds:
        return NullJudge()
    try:
        return kinds[kind](**kw)
    except Exception:
        return NullJudge()
