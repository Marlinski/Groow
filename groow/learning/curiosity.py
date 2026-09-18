"""Curiosity: what Groow does when nobody is talking to it.

It senses the world (news feeds), and for each new item:
  1. asks itself the question a curious person would ask about it (generated,
     but grounded in the item text),
  2. drills the answer, which is the *source* text with its date and origin,
     never its own paraphrase, so hallucinations are not trained in,
  3. files the item as a dated, sourced lesson so `recall` can find it.
Then it drills one "what is in the news today" digest, and checks the probes.
"""
from __future__ import annotations

import time
from datetime import date

from ..brain import build_sample
from ..config import Config
from ..memory import Memory
from ..senses import NewsSense, NewsItem
from .learner import Learner, REHEARSAL_SYSTEM

ANSWER_WEIGHTS = {"user": 0.0, "assistant": 1.0, "system": 0.0, "tool": 0.0}


IMPULSE = ("(No one is talking to you right now; this is your own idle time, not a human message.) "
           "Look at what is happening in the world: run `groow news --items {n}` with shell, pick the items that seem most "
           "important or most interesting to you, read the ones worth reading with `web <link>` in shell, and for each one call learn "
           "with a precise question and an answer in the source's own words, including the date and the source. "
           "Then write two or three sentences about what you learned today.")


class Curiosity:
    def __init__(self, learner: Learner, memory: Memory, cfg: Config, news: NewsSense | None = None):
        self.learner, self.memory, self.cfg = learner, memory, cfg
        self.brain = learner.brain
        self.news = news or NewsSense(cfg.state, feeds=cfg.feeds or None)
        self.last_tick = 0.0

    def impulse_text(self, max_items: int | None = None) -> str:
        return IMPULSE.format(n=max_items or self.cfg.sense_items)

    async def explore(self, harness, max_items: int | None = None, on_progress=None) -> dict:
        """Agentic pass: Groow reads the news through its own tools inside a normal
        harness turn. Falls back to the fixed pipeline if it learned nothing."""
        t0 = time.time()
        n = max_items or self.cfg.sense_items
        turn = await harness.turn(IMPULSE.format(n=n))
        learned = turn.tools_used.count("learn")
        result = {"mode": "agentic", "tools_used": turn.tools_used, "facts_learned": learned,
                  "note": turn.final_text[:600], "seconds": round(time.time() - t0, 1)}
        if learned == 0:
            result["fallback"] = self.tick(max_items, on_progress=on_progress)
        self.last_tick = time.time()
        self.memory.log("explore", facts=learned, tools=len(turn.tools_used), step=self.brain.meta["steps"])
        return result

    # ------------------------------------------------------------------ pieces
    def _question_for(self, item: NewsItem) -> str:
        msgs = [{"role": "system", "content": "You turn news items into one short, specific question that the item "
                                              "answers. Reply with the question only."},
                {"role": "user", "content": f"News item ({item.source}, {item.published}):\n{item.text}"}]
        q = self.brain.generate(msgs, max_new_tokens=48, temperature=0.0, enable_thinking=False).strip().splitlines()
        q = q[0].strip() if q else ""
        if len(q) < 8 or "?" not in q:
            q = f"What happened regarding: {item.title}?"
        return q

    @staticmethod
    def _answer_for(item: NewsItem) -> str:
        return f"{item.summary} (Source: {item.source}, {item.published}.)"

    def _sample(self, question: str, answer: str):
        msgs = [{"role": "system", "content": REHEARSAL_SYSTEM},
                {"role": "user", "content": question}, {"role": "assistant", "content": answer}]
        return build_sample(self.brain.tok, msgs, ANSWER_WEIGHTS, max_len=self.cfg.train_max_len)

    # ------------------------------------------------------------------ one pass
    def tick(self, max_items: int | None = None, on_progress=None) -> dict:
        """One sensing-and-learning pass. Bounded: at most `max_items` items,
        `sense_passes` gradient steps each, plus one digest and one probe."""
        t0 = time.time()
        items, errors = self.news.fetch(max_items or self.cfg.sense_items)
        learned = []
        for it in items:
            q = self._question_for(it)
            a = self._answer_for(it)
            s = self._sample(q, a)
            before = self.brain.sample_loss(s)
            for _ in range(self.cfg.sense_passes):
                self.brain.sft_step([s] + self.learner._rehearsal(1))
            after = self.brain.sample_loss(s)
            self.memory.add_lesson(f"news {it.published}: {it.title}", a,
                                   {"kind": "news", "source": it.source, "link": it.link, "question": q,
                                    "loss_before": round(before, 3), "loss_after": round(after, 3)})
            learned.append({"source": it.source, "title": it.title[:90], "question": q,
                            "loss": [round(before, 2), round(after, 2)]})
            if on_progress:
                on_progress(f"{it.source}: {it.title[:70]}  loss {before:.2f} → {after:.2f}")
        digest = None
        if items:
            today = date.today().isoformat()
            lines = "\n".join(f"- {it.title} ({it.source}, {it.published})" for it in items)
            s = self._sample(f"What is in the news today, {today}?", f"Headlines I read on {today}:\n{lines}")
            for _ in range(2):
                self.brain.sft_step([s] + self.learner._rehearsal(1))
            digest = round(self.brain.sample_loss(s), 3)
            self.news.mark_seen(items)
        probe = self.learner.probe() if items else None
        self.last_tick = time.time()
        result = {"items": len(items), "learned": learned, "digest_loss": digest, "feed_errors": errors,
                  "probe_mean_loss": probe["mean_loss"] if probe else None,
                  "probe_baseline": probe["baseline_mean"] if probe else None,
                  "seconds": round(time.time() - t0, 1)}
        self.memory.log("sense", items=len(items), digest_loss=digest, feed_errors=len(errors),
                        probe=result["probe_mean_loss"], step=self.brain.meta["steps"])
        if items:
            self.brain.save()
        return result
