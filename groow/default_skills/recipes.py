"""Recipes: notes about your home, your tools and how you learn. They live in state/recipes and are yours to improve."""
import os
from pathlib import Path


def _dir() -> Path:
    return Path(os.environ.get("GROOW_STATE", "state")) / "recipes"


def list_recipes() -> dict:
    """List your recipes: short notes on how your home, your shell, installing software, skills, learning,
    your mind and your mentor work. Read one with read_recipe(name)."""
    d = _dir()
    out = []
    for p in sorted(d.glob("*.md")):
        first = next((l.strip("# ").strip() for l in p.read_text().splitlines() if l.strip()), p.stem)
        out.append({"name": p.stem, "title": first})
    return {"recipes": out, "where": str(d)}


def read_recipe(name: str) -> dict:
    """Read one recipe in full.

    Args:
        name: the recipe name from list_recipes, e.g. installing
    """
    p = _dir() / f"{name}.md"
    if not p.exists():
        return {"found": False, "name": name, "available": [q.stem for q in sorted(_dir().glob('*.md'))]}
    return {"found": True, "name": name, "text": p.read_text()}


def write_recipe(name: str, text: str) -> dict:
    """Write or update a recipe (markdown). Use it when you learned how to do something in your home
    that you will need again.

    Args:
        name: lowercase name, letters, digits, dashes
        text: the full markdown text
    """
    safe = "".join(c for c in name.lower() if c.isalnum() or c == "-")[:40]
    if not safe:
        return {"error": "bad name"}
    d = _dir()
    d.mkdir(parents=True, exist_ok=True)
    (d / f"{safe}.md").write_text(text)
    return {"ok": True, "name": safe, "chars": len(text)}


TESTS = [("read_recipe", {"name": "this-recipe-does-not-exist"}, {"found": False})]
