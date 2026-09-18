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

A skill can also give you a **command** instead of, or as well as, tools:
`CLI = {"web": "main"}` makes `main(argv) -> dict` runnable as `web …` from your shell
(installed into `~/.local/bin`). Return `{"text": …}` to print text, any other dict prints as JSON.
Your `web` skill is one of these; read it: `cat state/skills/web.py`.

Rules the checker enforces: a docstring on the file and on every function, an
`Args:` section, functions return a dict, at least one entry in `TESTS`, no
name that collides with a core tool, standard library only, and it must not
loop forever at import.

Flow: write the file with shell (e.g. `cat > workspace/text_tools.py <<'EOF' … EOF`) →
`groow skill check workspace/text_tools.py` → read the report → `groow skill install text_tools`
→ the tools are yours. `groow skill list|read|rollback|disable <name>` manage them. If a
skill crashes your body it is quarantined automatically and you wake up without it.

Good candidates: wrappers around software you installed, small parsers, checks
you run often, games and drills that log decisions with rewards to `state/log/activity.jsonl`
(read `state/skills/tictactoe.py` for the shape).
