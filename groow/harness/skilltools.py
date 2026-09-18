"""Skill tools: how Groow extends its own command set, and how it repairs it."""
from __future__ import annotations

from .registry import ToolRegistry
from .skills import SkillManager

SKILL_HOWTO = ("A skill is one Python file: a one-line docstring, then plain functions with type hints and a docstring "
               "that has an Args: section (each function becomes a tool; return a dict), then "
               "TESTS = [(function_name, {args}, {expected subset of the returned dict})] with at least one test. "
               "Standard library only. Example:\n"
               '"""Text tools."""\n\ndef reverse_text(text: str) -> dict:\n    """Reverse the characters of a text.\n\n'
               '    Args:\n        text: the text to reverse\n    """\n    return {"reversed": text[::-1]}\n\n'
               'TESTS = [("reverse_text", {"text": "abc"}, {"reversed": "cba"})]')


def make_skill_tools(skills: SkillManager, full: bool = True) -> ToolRegistry:
    reg = ToolRegistry()

    @reg.tool(group="skills")
    def draft_skill(name: str, source: str) -> dict:
        f"""Write (or rewrite) a new skill as Python source and check it in a sandbox: imports, register(reg),
        tool schemas, no collision with core tools, and its own TESTS must pass. Nothing is installed yet.
        {SKILL_HOWTO}

        Args:
            name: lowercase identifier, e.g. word_tools
            source: the full Python source of the skill file
        """
        return skills.draft(name, source)

    draft_skill.__doc__ = draft_skill.__doc__  # (f-string docstrings are not literal; set explicitly below)
    reg.tools["draft_skill"].description = ("Write (or rewrite) a new skill as Python source and check it in a sandbox: imports, "
                                            "register(reg), tool schemas, no collision with core tools, and its own TESTS must pass. "
                                            "Nothing is installed yet. " + SKILL_HOWTO)

    @reg.tool(group="skills")
    def list_skills() -> dict:
        """Your installed skills (tools you wrote for yourself), drafts awaiting install, and quarantined ones."""
        return skills.listing()

    @reg.tool(group="skills")
    def read_skill(name: str) -> dict:
        """Read a skill's source (installed, draft or quarantined) and its manifest entry.

        Args:
            name: the skill name
        """
        return skills.read(name)

    @reg.tool(group="skills")
    def read_incidents(last: int = 3) -> dict:
        """Recent incidents: crashes, failed skill loads, with tracebacks. Read this before fixing yourself.

        Args:
            last: how many recent incidents to return
        """
        return {"incidents": skills.incidents(last)}

    if full:
        @reg.tool(group="skills")
        def install_skill(name: str) -> dict:
            """Install a checked draft: its tools become available to you immediately (and to new inner thoughts).
            The previous version, if any, is kept for rollback_skill.

            Args:
                name: the draft's name
            """
            return skills.install(name)

        @reg.tool(group="skills")
        def disable_skill(name: str, reason: str = "") -> dict:
            """Disable an installed skill (moved to quarantine, tools removed now).

            Args:
                name: the skill name
                reason: why
            """
            return skills.disable(name, reason)

        @reg.tool(group="skills")
        def rollback_skill(name: str) -> dict:
            """Reinstall the previous version of a skill.

            Args:
                name: the skill name
            """
            return skills.rollback(name)

        @reg.tool(group="skills")
        def propose_patch(path: str, description: str, patch: str) -> dict:
            """Propose a change to your own core code (the groow/ package). You cannot apply it yourself: it is
            filed for Marlinski to review. Use it when a limitation is in the harness rather than in a skill.

            Args:
                path: file inside the groow/ package, e.g. groow/harness/loop.py
                description: what the change does and why
                patch: a unified diff, or the full new content of the function(s) concerned
            """
            return skills.propose_patch(path, description, patch)

    return reg
