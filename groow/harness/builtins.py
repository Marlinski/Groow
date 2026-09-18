"""Basic tools every agent needs: a calculator, a clock, a scratch workspace on
disk, and a Python runner. Nothing here touches the model's weights.

All file tools are confined to one workspace directory (state/workspace by
default). The Python runner executes in a subprocess with a timeout, so a
runaway script cannot take the loop down with it.
"""
from __future__ import annotations

import ast
import datetime as dt
import operator
import subprocess
import sys
from pathlib import Path

from .registry import ToolRegistry

_OPS = {ast.Add: operator.add, ast.Sub: operator.sub, ast.Mult: operator.mul, ast.Div: operator.truediv,
        ast.FloorDiv: operator.floordiv, ast.Mod: operator.mod, ast.Pow: operator.pow, ast.USub: operator.neg,
        ast.UAdd: operator.pos}


def _safe_eval(node):
    if isinstance(node, ast.Expression):
        return _safe_eval(node.body)
    if isinstance(node, ast.Constant) and isinstance(node.value, (int, float)):
        return node.value
    if isinstance(node, ast.BinOp) and type(node.op) in _OPS:
        return _OPS[type(node.op)](_safe_eval(node.left), _safe_eval(node.right))
    if isinstance(node, ast.UnaryOp) and type(node.op) in _OPS:
        return _OPS[type(node.op)](_safe_eval(node.operand))
    raise ValueError(f"unsupported expression element: {ast.dump(node)[:40]}")


def make_builtin_tools(workspace: Path, python_timeout: int = 30, allow_python: bool = True) -> ToolRegistry:
    ws = Path(workspace).resolve()
    ws.mkdir(parents=True, exist_ok=True)
    reg = ToolRegistry()

    def _inside(rel: str) -> Path:
        p = (ws / rel).resolve()
        if ws not in p.parents and p != ws:
            raise ValueError(f"path escapes the workspace: {rel}")
        return p

    @reg.tool(group="basic")
    def calculator(expression: str) -> dict:
        """Evaluate an arithmetic expression exactly (+ - * / // % ** and parentheses).

        Args:
            expression: for example "17 * 23 + 4" or "2 ** 10"
        """
        value = _safe_eval(ast.parse(expression, mode="eval"))
        return {"expression": expression, "value": value}

    @reg.tool(group="basic")
    def current_time() -> dict:
        """Current date and time on this machine."""
        now = dt.datetime.now().astimezone()
        return {"iso": now.isoformat(timespec="seconds"), "weekday": now.strftime("%A"),
                "human": now.strftime("%A %d %B %Y, %H:%M %Z")}

    @reg.tool(group="basic")
    def list_files(subdir: str = "") -> dict:
        """List files in your workspace directory.

        Args:
            subdir: optional sub-directory inside the workspace
        """
        root = _inside(subdir or ".")
        files = sorted(str(p.relative_to(ws)) for p in root.rglob("*") if p.is_file())
        return {"workspace": str(ws), "files": files[:200], "count": len(files)}

    @reg.tool(group="basic")
    def read_file(path: str, max_chars: int = 8000) -> dict:
        """Read a text file from your workspace.

        Args:
            path: path relative to the workspace
            max_chars: truncate the content after this many characters
        """
        p = _inside(path)
        text = p.read_text(errors="replace")
        return {"path": path, "chars": len(text), "content": text[:max_chars], "truncated": len(text) > max_chars}

    @reg.tool(group="basic")
    def write_file(path: str, content: str, append: bool = False) -> dict:
        """Write (or append) text to a file in your workspace. Creates parent folders.

        Args:
            path: path relative to the workspace
            content: the text to write
            append: add to the end instead of replacing
        """
        p = _inside(path)
        p.parent.mkdir(parents=True, exist_ok=True)
        with p.open("a" if append else "w") as f:
            f.write(content)
        return {"path": path, "bytes": p.stat().st_size}

    if allow_python:
        @reg.tool(group="basic")
        def run_python(code: str, timeout: int = python_timeout) -> dict:
            """Run a Python snippet in a fresh subprocess (cwd = your workspace) and return stdout/stderr.

            Args:
                code: the Python source to execute
                timeout: seconds before the process is killed
            """
            try:
                r = subprocess.run([sys.executable, "-c", code], cwd=ws, capture_output=True, text=True,
                                   timeout=min(int(timeout), 300))
            except subprocess.TimeoutExpired:
                return {"error": f"timed out after {timeout}s"}
            return {"returncode": r.returncode, "stdout": r.stdout[-6000:], "stderr": r.stderr[-3000:]}

    return reg
