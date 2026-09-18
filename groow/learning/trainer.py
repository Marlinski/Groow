"""The trainer: the one place gradients come from, fed by training sets.

    consume(urgent_only)  take pending samples: SFT samples become weighted steps (with rehearsal),
                          reinforcement samples are grouped, advantages normalised within the group,
                          and become policy-gradient steps. Consumed lines are marked in the cursor.
The trainer does not know what a game, a news item or a conversation is: only samples.
"""
from __future__ import annotations

import statistics
import time
from collections import defaultdict

from ..brain import Brain, Decision, build_sample
from ..config import Config
from ..memory import Memory
from .learner import Learner
from .trainingset import TrainingSets

DEFAULT_WEIGHTS = {"user": 0.0, "assistant": 1.0, "system": 0.0, "tool": 0.0}


class Trainer:
    def __init__(self, brain: Brain, memory: Memory, learner: Learner, sets: TrainingSets, cfg: Config):
        self.brain, self.memory, self.learner, self.sets, self.cfg = brain, memory, learner, sets, cfg

    def _sft_sample(self, s: dict):
        weights = {**DEFAULT_WEIGHTS, **(s.get("weights") or {})}
        return build_sample(self.brain.tok, s["messages"], weights, max_len=self.cfg.train_max_len)

    def consume(self, urgent_only: bool = False, max_samples: int = 64, on_progress=None) -> dict:
        t0 = time.time()
        pending = self.sets.pending(limit=max_samples, urgent_only=urgent_only)
        if not pending:
            return {"consumed": 0, "seconds": 0.0}
        report = {"consumed": 0, "sft_steps": 0, "pg_steps": 0, "drilled": [], "sets": defaultdict(int), "last_loss": None}
        # --- supervised: batches of up to 3 samples + 1 rehearsal each; urgent ones drilled to their target
        sft = [(n, i, s) for n, i, s in pending if s.get("kind", "sft") == "sft"]
        batch = []
        for n, i, s in sft:
            try:
                sample = self._sft_sample(s)
            except Exception:
                continue
            if s.get("target_loss"):
                before = self.brain.sample_loss(sample)
                loss, steps = before, 0
                while steps < 40 and loss >= float(s["target_loss"]):
                    self.brain.sft_step([sample] + self.learner._rehearsal(1))
                    steps += 1
                    loss = self.brain.sample_loss(sample)
                report["sft_steps"] += steps
                report["drilled"].append({"set": n, "loss_before": round(before, 3), "loss_after": round(loss, 3), "steps": steps,
                                          "source": s.get("source", "")})
                if on_progress:
                    on_progress(f"drilled {n}: {before:.2f} -> {loss:.2f} in {steps} steps")
            else:
                batch.append(sample)
                if len(batch) == 3:
                    report["last_loss"] = self.brain.sft_step(batch + self.learner._rehearsal(1))
                    report["sft_steps"] += 1
                    batch = []
            report["sets"][n] += 1
        if batch:
            report["last_loss"] = self.brain.sft_step(batch + self.learner._rehearsal(1))
            report["sft_steps"] += 1
        # --- reinforcement: group -> normalised advantages -> one policy-gradient step per group
        pg = [(n, i, s) for n, i, s in pending if s.get("kind") == "pg"]
        groups: dict[str, list] = defaultdict(list)
        for n, i, s in pg:
            groups[f"{n}:{s.get('group', i)}"].append((n, s))
        for gid, items in groups.items():
            rewards = [float(s["reward"]) for _, s in items]
            mu, spread = statistics.mean(rewards), statistics.pstdev(rewards)
            # Every sample in the group scored the same, so there is nothing to prefer: the
            # advantages would all be zero and the step would change nothing. Say so and move
            # on rather than spending a backward pass on it.
            if spread == 0.0 or len(items) < 2:
                for n, _ in items:
                    report["sets"][n] += 1
                report.setdefault("skipped_groups", []).append(
                    {"group": gid, "decisions": len(items), "reward": round(mu, 3), "why": "nothing to compare"})
                if on_progress:
                    on_progress(f"skipped {gid}: {len(items)} decision(s) all at {mu:+.2f}, nothing to learn from")
                continue
            sd = spread
            decisions = []
            for n, s in items:
                prompt_ids = self.brain.tok(self.brain.prompt_text(s["prompt"], enable_thinking=False), add_special_tokens=False)["input_ids"]
                comp_ids = self.brain.tok(s["completion"], add_special_tokens=False)["input_ids"] + [self.brain.tok.eos_token_id]
                decisions.append(Decision(prompt_ids=prompt_ids, completion_ids=comp_ids, reward=float(s["reward"]),
                                          advantage=(float(s["reward"]) - mu) / sd))
                report["sets"][n] += 1
            loss = self.brain.pg_step(decisions)
            report["pg_steps"] += 1
            if on_progress:
                on_progress(f"policy step on {gid}: {len(items)} decisions, mean reward {mu:+.2f}, loss {loss:+.3f}")
        # --- mark consumed
        for n in {n for n, _, _ in pending}:
            self.sets.mark(n, max(i for m, i, _ in pending if m == n))
        report["consumed"] = len(pending)
        report["sets"] = dict(report["sets"])
        report["seconds"] = round(time.time() - t0, 1)
        self.memory.log("train", **{k: v for k, v in report.items() if k != "drilled"}, drilled=len(report["drilled"]),
                        step=self.brain.meta["steps"])
        return report
