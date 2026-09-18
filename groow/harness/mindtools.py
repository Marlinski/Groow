"""Mind tools.

    main thought:  think(goal, max_steps)   spawn an inner thought
    inner thought: focus(message)           speak to the main thought now
                   finish(summary)          end and report
Listing, reading, pausing and killing thoughts are files and `groow thought …` commands in the shell.
"""
from __future__ import annotations

from .registry import ToolRegistry


def make_main_mind_tools(thoughts, memory=None) -> ToolRegistry:
    reg = ToolRegistry()

    @reg.tool(group="mind")
    def think(goal: str, max_steps: int = 8) -> dict:
        """Start an inner thought: a separate line of reasoning that works toward a goal in the background with
        its own tools (shell, read, learn, quiz). It cannot talk to anyone; it reports back to you with focus and
        finish, and you get a reminder every few steps. Its trace is in state/thoughts/<id>.json; `groow thoughts`
        lists them, `groow thought pause|resume|kill <id>` manages them. Use it for anything that takes several
        steps and does not need the person waiting.

        Args:
            goal: what the thought should achieve, concretely
            max_steps: budget in steps (default 8, max 60)
        """
        return thoughts.spawn(goal, max_steps)

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
