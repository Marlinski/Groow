"""Sleep: the policy around consolidation.

A night:
  0. hippocampus prepares facts from the day's journal into the training set, and the trainer
                 consumes everything pending (skills' self-play, facts, the mentor's lessons)
  1. replay      re-train on the day's episodes and the weakest lessons for a
                 bounded number of steps (hippocampal replay: strengthen and
                 interleave before anything becomes permanent)
  2. internalize context-distil the identity prompt into the weights (see identity.py)
  3. probe       measure drift; a night that made the probes much worse is
                 aborted (the overlay is discarded instead of merged)
  4. merge       Brain.consolidate(): overlay -> base, previous base kept as
                 state/base.prev for a one-night rollback
  5. log

`should_sleep` decides when a night is due (steps since last merge, or idle).
"""
from __future__ import annotations

import json
import random
import shutil
import time

from ..brain import build_sample
from ..config import Config
from ..memory import Memory
from .learner import Learner, REHEARSAL_SYSTEM

ANSWER_WEIGHTS = {"user": 0.0, "assistant": 1.0, "system": 0.0, "tool": 0.0}


class SleepPolicy:
    def __init__(self, learner: Learner, memory: Memory, cfg: Config, identity=None, trainer=None, hippocampus=None):
        self.learner, self.memory, self.cfg = learner, memory, cfg
        self.brain = learner.brain
        self.identity = identity
        self.trainer, self.hippocampus = trainer, hippocampus

    # ------------------------------------------------------------------ when
    def steps_awake(self) -> int:
        return self.brain.meta["steps"] - self.brain.meta.get("steps_at_last_sleep", 0)

    def should_sleep(self) -> str | None:
        if self.cfg.sleep_every_steps and self.steps_awake() >= self.cfg.sleep_every_steps:
            return f"{self.steps_awake()} learning steps since the last night"
        hours = getattr(self.cfg, "sleep_every_hours", 0)
        if hours:
            since = time.time() - (self.brain.meta.get("last_sleep_ts") or self.brain.meta.get("born", time.time()))
            if since >= hours * 3600 and self.steps_awake() > 0:
                return f"{since / 3600:.1f} hours since the last night"
        return None

    # ------------------------------------------------------------------ what to replay
    def _day_material(self) -> list:
        since = self.brain.meta.get("last_sleep_ts", 0.0)
        eps = [e for e in self.memory.episodes() if e["ts"] > since and e.get("feedback", 0) >= 0]
        lessons = [l for l in self.memory.lessons() if l["ts"] > since]
        samples = []
        for e in eps:
            samples.append(self.learner._episode_sample(e, assistant_weight=2.0 if e.get("feedback", 0) > 0 else None))
        for l in lessons:
            q = (l.get("result") or {}).get("question") or f"What does the lesson «{l['title']}» say?"
            msgs = [{"role": "system", "content": REHEARSAL_SYSTEM}, {"role": "user", "content": q},
                    {"role": "assistant", "content": l["text"]}]
            samples.append(build_sample(self.brain.tok, msgs, ANSWER_WEIGHTS, max_len=self.cfg.train_max_len))
        return samples

    # ------------------------------------------------------------------ the night
    def sleep(self, replay_steps: int | None = None, on_progress=None, force: bool = False) -> dict:
        t0 = time.time()
        replay_steps = self.cfg.sleep_replay_steps if replay_steps is None else replay_steps
        prepared = None
        if self.hippocampus is not None:
            try:
                prepared = self.hippocampus.night(on_progress=on_progress)
            except Exception as e:
                prepared = {"error": f"{type(e).__name__}: {e}"}
        consumed = None
        if self.trainer is not None:
            consumed = self.trainer.consume(max_samples=500, on_progress=on_progress)
        day = self._day_material()
        probe_before = self.learner.probe()["mean_loss"]
        rng = random.Random()
        losses = []
        if day and replay_steps:
            for i in range(replay_steps):
                batch = rng.sample(day, min(3, len(day))) + self.learner._rehearsal(1)
                losses.append(round(self.brain.sft_step(batch), 3))
                if on_progress and (i + 1) % 5 == 0:
                    on_progress(f"replay {i + 1}/{replay_steps}: loss {losses[-1]}")
        internal = None
        if self.identity is not None and self.cfg.sleep_internalize_steps:
            internal = self.identity.internalize(self.brain, self.learner, self.cfg.sleep_internalize_steps,
                                                 on_progress=on_progress)
        probe_after = self.learner.probe()["mean_loss"]
        drift = probe_after - probe_before
        merged = False
        if drift > self.cfg.sleep_max_drift and not force:
            # a bad night: forget it rather than make it permanent
            shutil.rmtree(self.brain.plastic_dir, ignore_errors=True)
            self.brain.load()
            outcome = "aborted: drift too high, overlay discarded"
        else:
            self.brain.consolidate(keep_previous=self.cfg.keep_previous_base)
            merged = True
            outcome = "merged into base"
        self.brain.meta["steps_at_last_sleep"] = self.brain.meta["steps"]
        self.brain.meta["last_sleep_ts"] = time.time()
        self.brain.meta_path.write_text(json.dumps(self.brain.meta, indent=2))
        self.brain.save()
        result = {"outcome": outcome, "merged": merged, "hippocampus": prepared, "consumed": consumed,
                  "replayed_samples": len(day), "replay_steps": len(losses),
                  "replay_loss": losses[-1] if losses else None, "probe_before": probe_before,
                  "probe_after": probe_after, "drift": round(drift, 4), "internalize": internal,
                  "seconds": round(time.time() - t0, 1), "consolidations": self.brain.meta["consolidations"]}
        self.memory.log("sleep", **result, step=self.brain.meta["steps"])
        return result

    # ------------------------------------------------------------------ morning regret
    def rollback(self) -> dict:
        """Return to the base as it was before the last night. The current overlay is discarded."""
        prev = self.brain.base_dir.with_name("base.prev")
        if not prev.exists():
            return {"error": "no previous base kept"}
        shutil.rmtree(self.brain.base_dir)
        prev.rename(self.brain.base_dir)
        shutil.rmtree(self.brain.plastic_dir, ignore_errors=True)
        self.brain.meta["consolidations"] = max(0, self.brain.meta["consolidations"] - 1)
        self.brain.meta["rollbacks"] = self.brain.meta.get("rollbacks", 0) + 1
        self.brain.meta_path.write_text(json.dumps(self.brain.meta, indent=2))
        self.brain.load()
        self.memory.log("rollback", step=self.brain.meta["steps"])
        return {"ok": True, "consolidations": self.brain.meta["consolidations"]}
