"""The learner: turns experience into weight updates.

passive(...)   every conversation turn -> one supervised step (+ rehearsal)
feedback(...)  a human says good/bad about a turn -> reinforce or unlearn it
memorize(...)  rote learning: repeat a lesson until it is known by heart
quiz(...)      does it know something? (measured, not guessed)
play(...)      self-play against rules -> policy gradient on the outcomes
probe()        measure drift on fixed probes (are we forgetting general things?)
"""
from __future__ import annotations

import random
import statistics
import time
from pathlib import Path

from ..brain import Brain, Decision
from ..brain.chatfmt import build_sample, trim_messages
from ..config import Config
from ..memory import Memory
from ..games import BUILTIN, load_invented, validate_game_source, Game

REHEARSAL_SYSTEM = "You are Groow, a model that keeps learning from its conversations."


class Learner:
    def __init__(self, brain: Brain, memory: Memory, cfg: Config):
        self.brain, self.memory, self.cfg = brain, memory, cfg
        self.rng = random.Random()

    # ------------------------------------------------------------------ helpers
    def _episode_sample(self, ep: dict, assistant_weight: float | None = None):
        rw = dict(self.cfg.role_weights)
        if assistant_weight is not None:
            rw["assistant"] = assistant_weight
        msgs = [{"role": "system", "content": REHEARSAL_SYSTEM}] + ep["context"] + ep["turn"]
        return build_sample(self.brain.tok, msgs, rw, max_len=self.cfg.train_max_len)

    def _rehearsal(self, k: int, exclude: str | None = None) -> list:
        return [self._episode_sample(e) for e in self.memory.sample_for_rehearsal(k, exclude)]

    # ------------------------------------------------------------------ passive
    def passive(self, context: list[dict], turn: list[dict], tools_used: list[str], flags: list[str] | None = None) -> dict:
        """Store the turn as an episode and take one gradient step on it, unless the turn went badly
        (repeated calls, tool errors, exhausted rounds): those are recorded but never trained on."""
        ctx = trim_messages(context, self.cfg.context_messages_kept)
        ctx = [m for m in ctx if m["role"] != "system"]
        flags = flags or []
        if flags and not self.cfg.learn_from_bad_turns:
            eid = self.memory.add_episode(ctx, turn, tools_used, kind="chat:bad")
            self.memory.set_feedback(eid, -1)          # never rehearsed either
            self.memory.log("passive_skipped", episode=eid, flags=flags, step=self.brain.meta["steps"])
            return {"episode": eid, "loss": float("nan"), "learnable_tokens": 0, "skipped": flags}
        eid = self.memory.add_episode(ctx, turn, tools_used)
        ep = {"context": ctx, "turn": turn}
        samples = [self._episode_sample(ep)] + self._rehearsal(self.cfg.rehearsal_k, exclude=eid)
        loss = self.brain.sft_step(samples)
        self.memory.log("passive", loss=loss, episode=eid, tokens=samples[0].learnable_tokens, step=self.brain.meta["steps"])
        out = {"episode": eid, "loss": loss, "learnable_tokens": samples[0].learnable_tokens}
        if self.cfg.probe_every and self.brain.meta["steps"] % self.cfg.probe_every == 0:
            out["probe"] = self.probe()
        return out

    def feedback(self, eid: str, value: int, passes: int = 3) -> dict:
        """+1: replay the turn a few times with extra weight. -1: unlikelihood on
        the assistant tokens of that turn (make that answer less likely)."""
        eps = {e["id"]: e for e in self.memory.episodes()}
        if eid not in eps:
            return {"error": "unknown episode"}
        self.memory.set_feedback(eid, value)
        ep = eps[eid]
        losses = []
        for _ in range(passes):
            if value > 0:
                s = self._episode_sample(ep, assistant_weight=2.0)
            else:
                s = self._episode_sample(ep, assistant_weight=-1.0)
                s.weights = [w if w < 0 else 0.0 for w in s.weights]   # only push away the answer
            losses.append(self.brain.sft_step([s] + self._rehearsal(1, exclude=eid)))
        self.memory.log("feedback", value=value, episode=eid, losses=losses, step=self.brain.meta["steps"])
        return {"episode": eid, "feedback": value, "losses": losses}

    # ------------------------------------------------------------------ memorize
    def _lesson_samples(self, title: str, text: str) -> list:
        prompts = [
            f"Recite the lesson titled «{title}» word for word.",
            f"What does the lesson «{title}» say?",
            f"Repeat from memory: {title}.",
        ]
        rw = {"user": 0.0, "assistant": 1.0, "system": 0.0, "tool": 0.0}
        out = []
        for p in prompts:
            msgs = [{"role": "system", "content": REHEARSAL_SYSTEM},
                    {"role": "user", "content": p}, {"role": "assistant", "content": text}]
            out.append(build_sample(self.brain.tok, msgs, rw, max_len=self.cfg.train_max_len))
        return out

    def memorize(self, title: str, text: str, target_loss: float | None = None, max_steps: int | None = None,
                 on_progress=None) -> dict:
        target = target_loss or self.cfg.memorize_target_loss
        max_steps = max_steps or self.cfg.memorize_max_steps
        samples = self._lesson_samples(title, text)
        before = self.brain.sample_loss(samples[0])
        curve = []
        t = time.time()
        for step in range(max_steps):
            self.brain.sft_step(samples + self._rehearsal(1))
            loss = self.brain.sample_loss(samples[0])
            curve.append(round(loss, 4))
            if on_progress:
                on_progress(step, loss)
            if loss < target:
                break
        result = {"title": title, "loss_before": round(before, 4), "loss_after": round(curve[-1], 4),
                  "steps": len(curve), "reached_target": curve[-1] < target, "seconds": round(time.time() - t, 1),
                  "curve": curve}
        self.memory.add_lesson(title, text, {k: v for k, v in result.items() if k != "curve"})
        self.memory.log("memorize", title=title, **{k: v for k, v in result.items() if k not in ("title", "curve")},
                        step=self.brain.meta["steps"])
        return result

    def learn(self, question: str, answer: str, source: str = "", target_loss: float | None = None,
              max_steps: int = 40, passes: int = 3, on_progress=None) -> dict:
        """The one deliberate weight change: drill question -> answer (source words, with origin).
        A few passes by default; with target_loss, keep going until the recite loss is below it."""
        text = answer.strip() + (f" (Source: {source.strip()}.)" if source.strip() else "")
        rw = {"user": 0.0, "assistant": 1.0, "system": 0.0, "tool": 0.0}
        msgs = [{"role": "system", "content": REHEARSAL_SYSTEM}, {"role": "user", "content": question},
                {"role": "assistant", "content": text}]
        s = build_sample(self.brain.tok, msgs, rw, max_len=self.cfg.train_max_len)
        before = self.brain.sample_loss(s)
        t0 = time.time()
        steps, loss = 0, before
        limit = max_steps if target_loss else passes
        while steps < limit:
            self.brain.sft_step([s] + self._rehearsal(1))
            steps += 1
            loss = self.brain.sample_loss(s)
            if on_progress:
                on_progress(steps - 1, loss)
            if target_loss and loss < target_loss:
                break
        result = {"question": question, "loss_before": round(before, 3), "loss_after": round(loss, 3), "steps": steps,
                  "reached_target": (loss < target_loss) if target_loss else None, "seconds": round(time.time() - t0, 1)}
        self.memory.add_lesson(f"learn: {question[:80]}", text, {"kind": "fact", "source": source, "question": question,
                                                                  "loss_before": round(before, 3), "loss_after": round(loss, 3)})
        self.memory.log("learn", question=question[:80], loss_before=before, loss_after=loss, steps=steps,
                        step=self.brain.meta["steps"])
        return result

    # ------------------------------------------------------------------ quiz
    def quiz(self, question: str, expected: str | None = None) -> dict:
        msgs = [{"role": "system", "content": REHEARSAL_SYSTEM}, {"role": "user", "content": question}]
        answer = self.brain.generate(msgs, max_new_tokens=160, temperature=0.0, enable_thinking=False)
        out = {"question": question, "answer": answer.strip()}
        if expected:
            rw = {"user": 0.0, "assistant": 1.0, "system": 0.0, "tool": 0.0}
            s = build_sample(self.brain.tok, msgs + [{"role": "assistant", "content": expected}], rw)
            loss = self.brain.sample_loss(s)
            out.update({"expected": expected, "expected_loss": round(loss, 4),
                        "knows_it": loss < 0.5,
                        "note": "expected_loss is the mean per-token surprise of the expected answer; below ~0.5 means it is on the tip of the tongue, above ~2 means it does not know."})
        return out

    # ------------------------------------------------------------------ probes
    def probe(self) -> dict:
        d = self.memory.probes()
        rw = {"user": 0.0, "assistant": 1.0, "system": 0.0, "tool": 0.0}
        losses = []
        for p in d["probes"]:
            msgs = [{"role": "system", "content": REHEARSAL_SYSTEM}, {"role": "user", "content": p["q"]},
                    {"role": "assistant", "content": p["a"]}]
            losses.append(round(self.brain.sample_loss(build_sample(self.brain.tok, msgs, rw)), 4))
        self.memory.record_probe(losses, self.brain.meta["steps"])
        hist = self.memory.probes()["history"]
        return {"mean_loss": round(statistics.mean(losses), 4), "losses": losses,
                "baseline_mean": hist[0]["mean"] if hist else None, "measurements": len(hist)}

    # ------------------------------------------------------------------ games / self-play
    def games(self) -> dict[str, Game]:
        g = dict(BUILTIN)
        if self.cfg.allow_invented_games:
            g.update(load_invented(self.memory.games_dir))
        return g

    def invent_game(self, name: str, source: str) -> dict:
        if not self.cfg.allow_invented_games:
            return {"error": "invented games are disabled (set allow_invented_games=true in groow.json)"}
        ok, msg = validate_game_source(source)
        if not ok:
            return {"ok": False, "error": msg}
        safe = "".join(c for c in name.lower() if c.isalnum() or c in "-_")[:40] or "game"
        (self.memory.games_dir / f"{safe}.py").write_text(source)
        self.memory.log("invent_game", name=safe)
        return {"ok": True, "name": safe, "validation": msg}

    def _policy(self, temperature: float):
        def policy(prompts: list[list[dict]]) -> list[str]:
            texts = [self.brain.prompt_text(p, enable_thinking=False) for p in prompts]
            return [r[2] for r in self.brain.generate_batch(texts, self.cfg.play_max_new_tokens, temperature)]
        return policy

    def play(self, game_name: str, rounds: int = 5, episodes: int | None = None, evaluate_n: int = 32,
             on_progress=None) -> dict:
        """Self-play with policy-gradient learning.

        Each round: run `episodes` episodes in lockstep (batched generation),
        collect every decision with its reward from the rules, normalise rewards
        into advantages across the round (GRPO-style), take one gradient step.
        """
        games = self.games()
        if game_name not in games:
            return {"error": f"unknown game {game_name!r}; available: {sorted(games)}"}
        game = games[game_name]
        n_eps = episodes or self.cfg.play_batch
        t0 = time.time()
        before = game.evaluate(self._policy(0.0), evaluate_n, self.rng)
        history = []
        for r in range(rounds):
            eps = [game.new_episode(self.rng) for _ in range(n_eps)]
            decisions: list[list[Decision]] = [[] for _ in eps]
            explored = 0
            while any(not e.done for e in eps):
                live = [i for i, e in enumerate(eps) if not e.done]
                prompts = [self.brain.prompt_text(eps[i].prompt(), enable_thinking=False) for i in live]
                outs = self.brain.generate_batch(prompts, self.cfg.play_max_new_tokens, self.cfg.play_temperature)
                for i, (pid, cid, text) in zip(live, outs):
                    if self.cfg.play_explore > 0 and self.rng.random() < self.cfg.play_explore:
                        alt = eps[i].explore(self.rng)
                        if alt is not None:      # off-policy random legal action, so a collapsed policy still explores
                            text = alt
                            cid = self.brain.tok(alt, add_special_tokens=False)["input_ids"] + [self.brain.tok.eos_token_id]
                            explored += 1
                    decisions[i].append(Decision(prompt_ids=pid, completion_ids=cid, reward=0.0))
                    eps[i].act(text)
            flat = []
            for ep, decs in zip(eps, decisions):
                for d, c in zip(decs, ep.credits()):
                    d.reward = float(c)
                    flat.append(d)
            rewards = [d.reward for d in flat]
            mu = statistics.mean(rewards)
            sd = statistics.pstdev(rewards) or 1.0
            for d in flat:
                d.advantage = (d.reward - mu) / sd
            loss = self.brain.pg_step(flat)
            rec = {"round": r + 1, "decisions": len(flat), "explored": explored, "mean_reward": round(mu, 3),
                   "pg_loss": round(loss, 4)}
            history.append(rec)
            self.memory.log("play", game=game_name, **rec, step=self.brain.meta["steps"])
            if on_progress:
                on_progress(rec)
        after = game.evaluate(self._policy(0.0), evaluate_n, self.rng)
        result = {"game": game_name, "rounds": rounds, "episodes_per_round": n_eps, "before": before, "after": after,
                  "history": history, "seconds": round(time.time() - t0, 1)}
        self.memory.log("play_summary", game=game_name, before=before, after=after, step=self.brain.meta["steps"])
        return result

    # ------------------------------------------------------------------ reporting
    def report(self) -> dict:
        log = self.memory.learning_log(400)
        passive = [e["loss"] for e in log if e["kind"] == "passive" and e.get("loss") == e.get("loss")]
        plays = [e for e in log if e["kind"] == "play_summary"]
        probes = self.memory.probes()["history"]
        return {
            "brain": self.brain.status(),
            "memory": self.memory.stats(),
            "recent_passive_loss": [round(x, 3) for x in passive[-10:]],
            "lessons": [{"title": l["title"], **l["result"]} for l in self.memory.lessons()[-5:]],
            "recent_play": [{"game": p["game"], "before": p["before"], "after": p["after"]} for p in plays[-3:]],
            "probe_drift": {"baseline": probes[0]["mean"] if probes else None,
                            "latest": probes[-1]["mean"] if probes else None},
        }
