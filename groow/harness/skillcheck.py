"""Skill checker: runs in a *fresh subprocess* so a bad skill cannot hurt the live process.

    python -m groow.harness.skillcheck path/to/skill.py [protected,names,...]

Prints one JSON report. Exit code 0 = the skill may be installed.
"""
from __future__ import annotations

import importlib.util
import json
import sys
import traceback

sys.dont_write_bytecode = True   # drafts change faster than mtime resolution; never trust a .pyc


EXAMPLE = '''"""Text tools: reverse text."""

def reverse_text(text: str) -> dict:
    """Reverse the characters of a text.

    Args:
        text: the text to reverse
    """
    return {"reversed": text[::-1]}

TESTS = [("reverse_text", {"text": "abc"}, {"reversed": "cba"})]
'''


def auto_register(mod, reg, group: str) -> list[str]:
    """Skill format is forgiving: if the module defines register(reg), call it;
    otherwise every public module-level function with a docstring becomes a tool."""
    import inspect
    if callable(getattr(mod, "register", None)):
        mod.register(reg)
    else:
        for name, fn in vars(mod).items():
            if name.startswith("_") or not inspect.isfunction(fn) or fn.__module__ != mod.__name__:
                continue
            if name in ("register",) or not (fn.__doc__ or "").strip():
                continue
            reg.tool(fn, group=group)
    return list(reg.tools)


def check(path: str, protected: set[str]) -> dict:
    report: dict = {"path": path, "ok": False, "tools": [], "tests": [], "errors": []}
    try:
        spec = importlib.util.spec_from_file_location("groow_skill_under_test", path)
        if spec is None or spec.loader is None:
            report["errors"].append("not a Python file")
            return report
        mod = importlib.util.module_from_spec(spec)
        sys.modules[spec.name] = mod
        spec.loader.exec_module(mod)                       # import errors surface here
    except Exception:
        report["errors"].append("import failed:\n" + traceback.format_exc(limit=6))
        report["example_of_a_valid_skill"] = EXAMPLE
        return report
    reg = _registry_class()()
    try:
        auto_register(mod, reg, "skill")
    except Exception:
        report["errors"].append("registering tools failed:\n" + traceback.format_exc(limit=6))
        return report
    if not reg.tools:
        report["errors"].append("no tools found: define module-level functions with a docstring (or a register(reg) function)")
    for name, spec_ in reg.tools.items():
        if name in protected:
            report["errors"].append(f"tool name {name!r} collides with a core tool")
        if not spec_.description or len(spec_.description) < 12:
            report["errors"].append(f"tool {name!r} needs a real docstring (what it does, Args:)")
        report["tools"].append(spec_.schema()["function"])
    for i, t in enumerate(getattr(mod, "TESTS", []) or []):
        try:
            tool, args, expected = t
            out = json.loads(reg.call(tool, args))
            ok = _subset(expected, out) if isinstance(expected, dict) else (expected == out.get("result", out))
            report["tests"].append({"tool": tool, "args": args, "ok": ok, "got": out if not ok else None})
            if not ok:
                report["errors"].append(f"test {i} failed: {tool}({args}) expected {expected}, got {out}")
        except Exception:
            report["errors"].append(f"test {i} crashed:\n" + traceback.format_exc(limit=4))
    if not getattr(mod, "TESTS", None):
        report["errors"].append("add at least one test: TESTS = [(tool_name, {args}, {expected subset of result})]")
    report["ok"] = not report["errors"]
    if not report["ok"]:
        report["example_of_a_valid_skill"] = EXAMPLE
    return report


def _registry_class():
    """Load registry.py by path so the check does not import the harness package (and torch)."""
    here = __import__("pathlib").Path(__file__).with_name("registry.py")
    spec = importlib.util.spec_from_file_location("groow_registry_standalone", here)
    mod = importlib.util.module_from_spec(spec)
    sys.modules[spec.name] = mod          # dataclasses need the module registered
    spec.loader.exec_module(mod)
    return mod.ToolRegistry


def _subset(expected: dict, got: dict) -> bool:
    return isinstance(got, dict) and all(k in got and got[k] == v for k, v in expected.items())


if __name__ == "__main__":
    prot = set(sys.argv[2].split(",")) if len(sys.argv) > 2 and sys.argv[2] else set()
    r = check(sys.argv[1], prot)
    print(json.dumps(r, ensure_ascii=False, default=str))
    sys.exit(0 if r["ok"] else 1)
