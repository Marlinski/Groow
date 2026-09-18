"""The harness: everything between a user message and a finished turn.

    registry   ToolRegistry: Python functions -> tool schemas + safe dispatch
    builtins   basic tools (calculator, clock, workspace files, python runner)
    selftools  tools acting on Groow's own weights and memory (via the Learner)
    sensetools news_headlines / read_article / learn_fact: perceiving the world, learning grounded facts
    mindtools  think / list / read / pause / resume / kill thoughts, recall transcripts; focus / finish for thoughts
    skills     SkillManager: tools Groow writes for itself (draft -> sandbox check -> install -> rollback/quarantine)
    skilltools draft_skill / install_skill / list / read / disable / rollback, read_incidents, propose_patch
    skillcheck the subprocess checker a draft must pass
    loop       Harness: generate -> tool calls -> results -> ... -> after_turn hook
"""
from .registry import ToolRegistry, ToolSpec
from .builtins import make_builtin_tools
from .selftools import make_self_tools
from .sensetools import make_sense_tools
from .mindtools import make_main_mind_tools, make_thought_tools
from .skills import SkillManager
from .skilltools import make_skill_tools
from .loop import Harness, Hooks, TurnResult, parse_generation

__all__ = ["ToolRegistry", "ToolSpec", "make_builtin_tools", "make_self_tools", "make_sense_tools", "make_main_mind_tools", "make_thought_tools", "SkillManager", "make_skill_tools", "Harness", "Hooks",
           "TurnResult", "parse_generation"]
