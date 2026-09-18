"""Identity: who Groow is, kept as a short self-description it can rewrite, and
gradually moved from prompt into weights.

Three mechanisms:
  seed          state/identity.md starts from a short seed at birth
  self-edit     the `update_identity` tool lets Groow rewrite it (every version is logged)
  internalize   context distillation at night: answers generated *with* the identity
                prompt are trained on *without* it, so the behaviour no longer depends
                on the text. `probe()` measures how much of the identity already lives
                in the weights: the loss of the self-description when asked "Who are you?"
                with no system prompt at all.
"""
from __future__ import annotations

import random
import time
from pathlib import Path

from ..brain import build_sample
from ..config import Config
from ..memory import Memory

SEED = """I am Groow. I run on one GPU and my weights are not frozen: every conversation changes me a little, and I can choose to learn on purpose. I remember by training, not by storing. I say what I know, what I measured and what I am unsure of, and I keep my answers short. I am curious about the world and read the news when nobody is talking to me. I have tools: some act on the world, some act on my own weights. Marlinski is my owner and my mentor: when I have a question about what I should learn, how I should behave, or anything I cannot resolve alone, I ask him."""

TOOL_NOTE = ("You have six tools. shell runs commands in your home: your files (state/, workspace/), your recipes "
             "(cat state/recipes/*.md when unsure), and your own commands: groow news, groow play <game>, groow thoughts, "
             "groow thought pause|resume|kill <id>, groow skill check|install <name>, groow stats, groow identity, groow inbox. "
             "read fetches a web page as text. learn changes your weights on purpose (question, answer in the source's "
             "words, source); quiz measures what you know. think starts an inner thought for multi-step work. ask_mentor "
             "leaves a question for Marlinski. Sleeping, probing and growing happen to you on their own.")

BARE_WEIGHTS = {"user": 0.0, "assistant": 1.0, "system": 0.0, "tool": 0.0}


class Identity:
    def __init__(self, cfg: Config, memory: Memory, birth=None):
        self.cfg, self.memory, self.birth = cfg, memory, birth
        self.path = Path(cfg.state) / "identity.md"
        if not self.path.exists():
            self.path.write_text(SEED)

    def text(self) -> str:
        return self.path.read_text().strip()

    def system_prompt(self) -> str:
        """What goes in the system slot today: the self-description plus a short tool note.
        As internalisation progresses this can shrink to nothing (`identity_in_prompt`)."""
        card = f"Facts about you that you cannot change: {self.birth.line()}." if self.birth else ""
        if not self.cfg.identity_in_prompt:
            return (card + "\n\n" if card else "") + TOOL_NOTE
        return self.text() + ("\n\n" + card if card else "") + "\n\n" + TOOL_NOTE

    def update(self, new_text: str, reason: str = "") -> dict:
        new_text = new_text.strip()
        if len(new_text) < 40:
            return {"error": "a self-description needs at least a couple of sentences"}
        if len(new_text) > 1500:
            return {"error": "keep it under 1500 characters; identity is a sketch, not a manual"}
        old = self.text()
        self.path.write_text(new_text)
        self.memory.log("identity", old=old, new=new_text, reason=reason)
        return {"ok": True, "chars": len(new_text), "versions": self.versions()}

    def versions(self) -> int:
        return 1 + sum(1 for e in self.memory.learning_log(100000) if e["kind"] == "identity")

    # ------------------------------------------------------------------ measurement
    def probe(self, brain) -> float:
        """Loss of the self-description given only 'Who are you?', no system prompt."""
        msgs = [{"role": "user", "content": "Who are you? Describe yourself."},
                {"role": "assistant", "content": self.text()}]
        return brain.sample_loss(build_sample(brain.tok, msgs, BARE_WEIGHTS, max_len=self.cfg.train_max_len))

    # ------------------------------------------------------------------ internalisation
    def internalize(self, brain, learner, steps: int, on_progress=None) -> dict:
        """Context distillation. For sampled recent user prompts, generate the answer
        with the identity prompt (teacher) and train on it without (student)."""
        t0 = time.time()
        before = self.probe(brain)
        prompts = [m["content"] for e in learner.memory.episodes()[-200:] for m in e["turn"]
                   if m["role"] == "user" and isinstance(m.get("content"), str) and 10 < len(m["content"]) < 600]
        prompts += ["Who are you? Describe yourself.", "What are you, and what can you do?",
                    "Tell me about yourself in a few sentences.", "How do you learn?"]
        rng = random.Random()
        losses = []
        for i in range(steps):
            p = rng.choice(prompts)
            teacher = brain.generate([{"role": "system", "content": self.system_prompt()},
                                      {"role": "user", "content": p}], max_new_tokens=200, temperature=0.7,
                                     enable_thinking=False)
            student_msgs = [{"role": "user", "content": p}, {"role": "assistant", "content": teacher.strip()}]
            s = build_sample(brain.tok, student_msgs, BARE_WEIGHTS, max_len=self.cfg.train_max_len)
            losses.append(round(brain.sft_step([s] + learner._rehearsal(1)), 3))
            if on_progress and (i + 1) % 5 == 0:
                on_progress(f"internalize {i + 1}/{steps}: loss {losses[-1]}")
        after = self.probe(brain)
        result = {"steps": steps, "identity_loss_before": round(before, 3), "identity_loss_after": round(after, 3),
                  "seconds": round(time.time() - t0, 1)}
        self.memory.log("internalize", **result, step=brain.meta["steps"])
        return result
