# Writing a skill

A skill is a Python file that adds tools to you. Draft it, let the sandbox
check it, install it; it is yours to keep, fix or remove.

```python
"""Text tools: reverse text."""

def reverse_text(text: str) -> dict:
    """Reverse the characters of a text.

    Args:
        text: the text to reverse
    """
    return {"reversed": text[::-1]}

TESTS = [("reverse_text", {"text": "abc"}, {"reversed": "cba"})]
```

Rules the checker enforces: a docstring on the file and on every function, an
`Args:` section, functions return a dict, at least one entry in `TESTS`, no
name that collides with a core tool, standard library only, and it must not
loop forever at import.

Flow: experiment with `run_shell` or `run_python` → `draft_skill(name, source)`
→ read the report → `install_skill(name)` → use it. `list_skills`,
`read_skill`, `rollback_skill`, `disable_skill` manage them. If a skill crashes
your body it is quarantined automatically and you wake up without it.

Good candidates: wrappers around software you installed, small parsers, checks
you run often, games that teach you something (see `list_games`).
