"""The harness: everything between a message and a finished turn.

    registry   ToolRegistry: Python functions -> tool schemas + safe dispatch
    builtins   the substrate: shell
    selftools  ask
    mindtools  think (main); focus, finish (inner thoughts)
    sensetools news headlines for the curiosity pipeline
    skills     SkillManager: tools Groow writes for itself
    skillcheck the subprocess checker a draft must pass
    loop       Harness: generate -> tool calls -> results -> ...; Hooks

Imported lazily: a turn process pulls in the registry and the tools without ever loading torch.
"""
import importlib
from typing import TYPE_CHECKING

_WHERE = {
    "ToolRegistry": "registry", "ToolSpec": "registry",
    "make_substrate_tools": "builtins", "make_builtin_tools": "builtins", "page_text": "builtins",
    "make_self_tools": "selftools", "news_headlines": "sensetools",
    "make_main_mind_tools": "mindtools", "make_thought_tools": "mindtools", "make_think_tool": "mindtools",
    "SkillManager": "skills",
    "Harness": "loop", "Hooks": "loop", "TurnResult": "loop", "parse_generation": "loop",
}

if TYPE_CHECKING:  # pragma: no cover
    from .registry import ToolRegistry, ToolSpec
    from .loop import Harness, Hooks, TurnResult, parse_generation


def __getattr__(name):
    if name in _WHERE:
        return getattr(importlib.import_module(f".{_WHERE[name]}", __name__), name)
    raise AttributeError(name)


__all__ = list(_WHERE)
