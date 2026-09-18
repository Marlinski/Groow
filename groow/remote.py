"""Talking to the daemon from a short-lived process.

A turn is a process: it starts, rebuilds itself from files, runs one exchange and exits.
It never loads the model. Generation goes to the daemon, which owns the GPU and the
tokenizer, and which therefore also trims the context it is sent.

    RemoteBrain.complete(messages, tools) -> text        POST /complete
    Daemon.op(name, **args) -> dict                      POST /op
"""
from __future__ import annotations

import json
import os
import urllib.error
import urllib.request
from pathlib import Path

from .errors import Interrupted


def daemon_url(state_dir: str | Path) -> str:
    p = Path(state_dir) / "groow.url"
    if p.exists():
        return p.read_text().strip()
    return os.environ.get("GROOW_URL", "http://127.0.0.1:7373")


def _post(url: str, payload: dict, timeout: float = 900.0) -> dict:
    req = urllib.request.Request(url, data=json.dumps(payload, default=str).encode(),
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=timeout) as r:
        return json.loads(r.read() or b"{}")


class Daemon:
    def __init__(self, state_dir: str | Path):
        self.url = daemon_url(state_dir)

    def op(self, name: str, **args) -> dict:
        try:
            return _post(f"{self.url}/op", {"op": name, "args": args})
        except urllib.error.HTTPError as e:
            try:
                return json.loads(e.read())
            except Exception:
                return {"error": f"HTTP {e.code}"}
        except Exception as e:
            return {"error": f"{type(e).__name__}: {e}"}

    def event(self, ev: str, **data) -> None:
        """Ask the daemon to broadcast an event (the UI sees it live)."""
        try:
            _post(f"{self.url}/emit", {"ev": ev, **data}, timeout=10)
        except Exception:
            pass


class RemoteBrain:
    """What a Harness holds inside a turn process. `complete` is the only model call."""
    remote = True

    def __init__(self, state_dir: str | Path, priority: int = 0, actor: str = "main", req: str | None = None):
        self.url = daemon_url(state_dir)
        self.priority, self.actor, self.req = priority, actor, req
        self.server = _NoExecutor()

    async def complete(self, messages, tools=None, on_text=None, max_new_tokens=None, temperature=None,
                       enable_thinking=None, should_stop=None) -> str:
        import asyncio
        payload = {"messages": messages, "tools": tools, "priority": self.priority, "trim": True,
                   "stream_as": {"actor": self.actor, "req": self.req} if on_text is not None else None}
        if max_new_tokens is not None:
            payload["max_new_tokens"] = max_new_tokens
        if temperature is not None:
            payload["temperature"] = temperature
        if enable_thinking is not None:
            payload["enable_thinking"] = enable_thinking
        try:
            r = await asyncio.to_thread(_post, f"{self.url}/complete", payload)
        except Exception as e:
            raise Interrupted(f"the daemon did not answer: {type(e).__name__}: {e}")
        if r.get("interrupted"):
            raise Interrupted(r.get("reason", "preempted"))
        texts = r.get("completions") or [""]
        return texts[0]


class _NoExecutor:
    """The Harness asks the brain's server for a GPU executor when a tool trains. In a turn
    process no tool trains, so every tool runs on the default executor."""
    gpu = None
