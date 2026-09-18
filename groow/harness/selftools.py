"""Self-tools: the tools through which Groow acts on its own weights and memory.
They are thin wrappers around the Learner; the registry turns them into schemas.
"""
from __future__ import annotations

from ..learning import Learner
from .registry import ToolRegistry


def make_self_tools(learner: Learner, identity=None) -> ToolRegistry:
    reg = ToolRegistry()
    brain, memory = learner.brain, learner.memory
    inbox = memory.dir / "mentor_inbox.jsonl"

    @reg.tool(group="self")
    def ask_mentor(question: str, context: str = "") -> dict:
        """Leave a question for Marlinski, your owner and mentor. He reads the inbox when he next talks to you.
        Use it when you cannot resolve something alone: what to learn, how to behave, whether a fact is right.

        Args:
            question: the question, in one or two sentences
            context: optional: what led you to ask
        """
        rec = {"ts": __import__("time").time(), "question": question.strip(), "context": context.strip(),
               "answered": False}
        with inbox.open("a") as f:
            f.write(__import__("json").dumps(rec, ensure_ascii=False) + "\n")
        return {"ok": True, "queued": question.strip()[:200]}

    if identity is not None:
        @reg.tool(group="self")
        def read_identity() -> dict:
            """Your current self-description, the text you carry as identity."""
            return {"identity": identity.text(), "versions": identity.versions(),
                    "in_prompt": learner.cfg.identity_in_prompt}

        @reg.tool(group="self")
        def update_identity(text: str, reason: str) -> dict:
            """Rewrite your self-description. Do this when you have genuinely changed or learned who you are;
            keep what is true, drop what is not, stay under 1500 characters. Every version is kept.

            Args:
                text: the complete new self-description
                reason: why you are changing it
            """
            return identity.update(text, reason)

    @reg.tool(group="self", executor="gpu")
    def memorize(title: str, text: str, target_loss: float | None = None) -> dict:
        """Learn a text by heart: repeated training passes on your own weights until you can recite it,
        then it is stored as a lesson. Use it when someone asks you to remember or learn something exactly.

        Args:
            title: short name of the lesson
            text: the exact text to learn
            target_loss: stop when the per-token recite loss falls below this (default 0.15)
        """
        r = learner.memorize(title, text, target_loss=target_loss,
                             on_progress=lambda s, l: reg.progress(f"memorize step {s + 1}: loss {l:.3f}"))
        r.pop("curve", None)
        brain.save()
        return r

    @reg.tool(group="self", executor="gpu")
    def quiz(question: str, expected: str | None = None) -> dict:
        """Test yourself. Generates your current answer and, if an expected answer is given, measures how
        surprising that answer is to your weights (low loss = you know it). Does not change weights.

        Args:
            question: the question to ask yourself
            expected: the correct answer, if known
        """
        return learner.quiz(question, expected)

    @reg.tool(group="self")
    def recall(query: str) -> dict:
        """Search your episodic memory (past conversations and lessons).

        Args:
            query: words to look for
        """
        return {"results": memory.recall(query)}

    @reg.tool(group="self", executor="gpu")
    def play(game: str, rounds: int = 5) -> dict:
        """Practice a game against yourself. The rules score your moves and the outcomes update your weights
        (reinforcement learning). Returns your skill before and after.

        Args:
            game: name of the game, see list_games
            rounds: training rounds; each round is a batch of episodes
        """
        r = learner.play(game, rounds=rounds,
                         on_progress=lambda rec: reg.progress(f"round {rec['round']}: mean reward {rec['mean_reward']}"))
        r.pop("history", None)
        brain.save()
        return r

    @reg.tool(group="self")
    def list_games() -> dict:
        """List the games you can practice."""
        return {"games": {k: g.description for k, g in learner.games().items()}}

    @reg.tool(group="self")
    def invent_game(name: str, source: str) -> dict:
        """Define a new game to practice, as Python source. It must define GAME, an instance of a Game subclass
        with new_episode(rng) and evaluate(policy, n, rng); episodes implement done, prompt(), act(text),
        credits(). Only works if the operator enabled invented games.

        Args:
            name: short identifier for the game
            source: Python source code
        """
        return learner.invent_game(name, source)

    @reg.tool(group="self", executor="gpu")
    def consolidate() -> dict:
        """Merge what you learned recently (the plastic overlay) permanently into your base weights and start
        a fresh overlay. Like sleeping. Do this after important lessons."""
        r = brain.consolidate()
        memory.log("consolidate", **r, step=brain.meta["steps"])
        return r

    @reg.tool(group="self", executor="gpu")
    def grow(rank: int) -> dict:
        """Increase your learning capacity (rank of the plastic overlay). Function-preserving: you compute the
        same thing right after, but have more room to change.

        Args:
            rank: new rank, must exceed the current one
        """
        r = brain.grow_rank(rank)
        memory.log("grow", **r, step=brain.meta["steps"])
        return r

    @reg.tool(group="self", executor="gpu")
    def probe() -> dict:
        """Measure drift: how well you still answer a fixed set of general questions compared to when you were
        born. Rising loss means you are forgetting general knowledge."""
        return learner.probe()

    @reg.tool(group="self")
    def learning_report() -> dict:
        """Your learning statistics: steps taken, losses, lessons, games, drift."""
        return learner.report()

    return reg
