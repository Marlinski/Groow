"""Mind tools.

For the main thought: spawn and index inner thoughts, and read any past
conversation in full (`recall`: main turns, inner-thought steps, lessons).
For an inner thought: `focus` (post to the main thought) and `finish`.
"""
from __future__ import annotations

from ..memory import Memory
from .registry import ToolRegistry


def make_main_mind_tools(thoughts, memory: Memory) -> ToolRegistry:
    reg = ToolRegistry()

    @reg.tool(group="mind")
    def think(goal: str, max_steps: int = 12) -> dict:
        """Start an inner thought: a separate line of reasoning that works toward a goal in the background,
        step by step, with tools (reading, learning facts, memorising, playing). It cannot talk to anyone; it
        reports back to you with focus() and finish(). You get a reminder every few steps. Use it for anything
        that takes several steps and does not need the person waiting.

        Args:
            goal: what the thought should achieve, concretely
            max_steps: budget in steps (default 12, max 60)
        """
        return thoughts.spawn(goal, max_steps)

    @reg.tool(group="mind")
    def list_thoughts(include_finished: bool = False) -> dict:
        """Your inner thoughts: running, paused and (optionally) finished ones, with their ids.

        Args:
            include_finished: also list done and killed thoughts
        """
        return {"thoughts": thoughts.listing(include_finished)}

    @reg.tool(group="mind")
    def read_thought(thought_id: str, last_n: int = 10) -> dict:
        """Introspect an inner thought: its status, summary and the last steps of its trace.

        Args:
            thought_id: id from list_thoughts (a prefix is enough)
            last_n: how many recent messages of its trace to show
        """
        return thoughts.trace(thought_id, last_n)

    @reg.tool(group="mind")
    def pause_thought(thought_id: str) -> dict:
        """Pause a running inner thought (it keeps its trace and can be resumed).

        Args:
            thought_id: id from list_thoughts
        """
        return thoughts.pause(thought_id)

    @reg.tool(group="mind")
    def resume_thought(thought_id: str) -> dict:
        """Resume a paused inner thought.

        Args:
            thought_id: id from list_thoughts
        """
        return thoughts.resume(thought_id)

    @reg.tool(group="mind")
    def kill_thought(thought_id: str, reason: str = "") -> dict:
        """Terminate an inner thought for good.

        Args:
            thought_id: id from list_thoughts
            reason: why
        """
        return thoughts.kill(thought_id, reason)

    @reg.tool(group="mind")
    def recall(query: str = "", episode_id: str = "", last: int = 0, kind: str = "") -> dict:
        """Read past conversations in full. Give a query to search all traces (main conversation and inner
        thoughts), an episode_id to read one exchange verbatim, or last=N for the N most recent exchanges.
        Lessons you learned are searched too.

        Args:
            query: words to search for
            episode_id: id (or prefix) of one exchange to read verbatim
            last: number of most recent exchanges to return
            kind: filter: "chat" for your own conversations, "thought" for inner thoughts, "" for all
        """
        if episode_id:
            ep = memory.episode(episode_id)
            return memory.transcript(ep, 6000) if ep else {"error": f"no episode {episode_id}"}
        if last:
            return {"episodes": [memory.transcript(e, 1500) for e in memory.recent_episodes(int(last), kind or None)]}
        if query:
            eps = memory.search_episodes(query, 5)
            if kind:
                eps = [e for e in eps if e.get("kind", "chat").startswith(kind)]
            return {"episodes": [memory.transcript(e, 1500) for e in eps],
                    "lessons": [r for r in memory.recall(query, 5) if r["type"] == "lesson"]}
        return {"error": "give a query, an episode_id or last=N"}

    return reg


def make_thought_tools(thoughts, thought_id: str) -> ToolRegistry:
    reg = ToolRegistry()

    @reg.tool(group="thought")
    def focus(message: str) -> dict:
        """Send a message to the main thought now (it decides what to do with it; it may tell the person).

        Args:
            message: what the main thought should know, concisely
        """
        return thoughts.focus(thought_id, message)

    @reg.tool(group="thought")
    def finish(summary: str) -> dict:
        """End this thought and report the result to the main thought.

        Args:
            summary: what was found or done, concisely
        """
        return thoughts.finish(thought_id, summary)

    return reg
