"""The harness: everything between a message and a finished turn.

    registry   ToolRegistry: Python functions -> tool schemas + safe dispatch
    builtins   the substrate: shell, read
    selftools  learn, quiz, ask_mentor
    mindtools  think (main); focus, finish (inner thoughts)
    sensetools news headlines for `groow news` and curiosity
    skills     SkillManager: tools Groow writes for itself (draft -> sandbox check -> install -> rollback/quarantine)
    skillcheck the subprocess checker a draft must pass
    loop       Harness: generate -> parse tool calls -> execute -> loop; Hooks (on_message journals every message)
"""
from .registry import ToolRegistry, ToolSpec
from .builtins import make_substrate_tools, make_builtin_tools, page_text
from .selftools import make_self_tools
from .sensetools import news_headlines
from .mindtools import make_main_mind_tools, make_thought_tools
from .skills import SkillManager
from .loop import Harness, Hooks, TurnResult, parse_generation

__all__ = ["ToolRegistry", "ToolSpec", "make_substrate_tools", "make_builtin_tools", "page_text", "make_self_tools",
           "news_headlines", "make_main_mind_tools", "make_thought_tools", "SkillManager", "Harness", "Hooks",
           "TurnResult", "parse_generation"]
