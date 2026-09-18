"""The substrate: one tool that reaches the world.

    shell(command)   bash in Groow's home. Files, state, traces, games, skills, recipes, the
                     `groow` commands that talk to its own daemon, the commands its skills
                     install (e.g. `web <url>`): everything is a command.

Nothing here touches the weights. The real sandbox is the body: in Docker Groow
is a non-root user whose only writable directory is its persistent home. On a
bare host these tools run as you.
"""
from __future__ import annotations

import html
import os
import re
import subprocess
import urllib.request
from pathlib import Path

from .registry import ToolRegistry

_STRIP = re.compile(r"<(script|style|nav|header|footer|aside)[^>]*>.*?</\1>", re.DOTALL | re.IGNORECASE)
_TAGS = re.compile(r"<[^>]+>")
_WS = re.compile(r"[ \t\r\f\v]+")
_NL = re.compile(r"\n\s*\n+")


def page_text(url: str, timeout: int = 15, max_chars: int = 6000) -> dict:
    req = urllib.request.Request(url, headers={"User-Agent": "groow/0.3 (+reader)"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        raw = r.read(2_000_000).decode("utf-8", errors="replace")
    title = re.search(r"<title[^>]*>(.*?)</title>", raw, re.DOTALL | re.IGNORECASE)
    body = _STRIP.sub(" ", raw)
    body = re.sub(r"</(p|div|h\d|li|br|tr)>", "\n", body, flags=re.IGNORECASE)
    text = html.unescape(_TAGS.sub(" ", body))
    text = _NL.sub("\n", _WS.sub(" ", text)).strip()
    lines = [l.strip() for l in text.splitlines() if len(l.strip()) > 60]
    text = "\n".join(lines) or text
    return {"url": url, "title": html.unescape(title.group(1)).strip() if title else "",
            "chars": len(text), "text": text[:max_chars], "truncated": len(text) > max_chars}


def make_substrate_tools(home: Path | None = None, allow_shell: bool = True) -> ToolRegistry:
    home = Path(home or Path.home()).resolve()
    reg = ToolRegistry()

    if allow_shell:
        @reg.tool(group="substrate")
        def shell(command: str, timeout: int = 120) -> dict:
            """Run a bash command in your home and return its output. Your home persists; the rest of the system is
            read-only and you are not root (no apt, no sudo). Your files: state/ (traces in state/main/, thoughts in
            state/thoughts/, lessons, skills, recipes, identity.md), workspace/. Your commands: `web <url>` (a page as
            text), `groow news`, `groow play <game>`, `groow thoughts`, `groow thought pause|resume|kill <id>`,
            `groow skill check|install <name>`, `groow stats`, `groow identity`, `groow inbox`. Install software locally:
            `nix profile install nixpkgs#<pkg>`, `uv pip install …`. Long jobs: `nohup … &` and check later.
            Read `cat state/recipes/*.md` when unsure.

            Args:
                command: the command line, run with bash -lc
                timeout: seconds before the process is killed (max 600)
            """
            try:
                r = subprocess.run(["bash", "-lc", command], cwd=home, capture_output=True, text=True,
                                   timeout=min(int(timeout), 600), env={**os.environ, "HOME": str(home)})
            except subprocess.TimeoutExpired:
                return {"error": f"timed out after {timeout}s", "hint": "run it in the background with nohup … & and poll"}
            return {"returncode": r.returncode, "stdout": r.stdout[-8000:], "stderr": r.stderr[-3000:], "cwd": str(home)}

    return reg


# kept for callers that used the old name
make_builtin_tools = make_substrate_tools
