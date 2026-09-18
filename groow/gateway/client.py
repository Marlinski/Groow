"""Client side of the protocol (aiohttp): HTTP for single queries, SSE or
WebSocket for the event stream. Used by `groow chat`, `groow status`,
`groow stop`, `groow ask` and the Textual UI."""
from __future__ import annotations

import asyncio
import json
from pathlib import Path
from typing import AsyncIterator

import aiohttp

from .protocol import decode


class Client:
    def __init__(self, base_url: str):
        self.base = base_url.rstrip("/")
        self._session: aiohttp.ClientSession | None = None
        self._ws: aiohttp.ClientWebSocketResponse | None = None

    async def __aenter__(self) -> "Client":
        self._session = aiohttp.ClientSession(timeout=aiohttp.ClientTimeout(total=None, connect=5))
        return self

    async def __aexit__(self, *exc) -> None:
        await self.close()

    async def close(self) -> None:
        if self._ws and not self._ws.closed:
            await self._ws.close()
        if self._session:
            await self._session.close()

    @property
    def session(self) -> aiohttp.ClientSession:
        if self._session is None:
            self._session = aiohttp.ClientSession(timeout=aiohttp.ClientTimeout(total=None, connect=5))
        return self._session

    # ------------------------------------------------------------------ single queries
    async def hello(self) -> dict:
        async with self.session.get(self.base + "/hello") as r:
            return await r.json()

    async def status(self) -> dict:
        async with self.session.get(self.base + "/status") as r:
            return await r.json()

    async def say(self, text: str) -> dict:
        path = "/command" if text.startswith("/") else "/say"
        async with self.session.post(self.base + path, json={"text": text}) as r:
            return await r.json()

    async def ask(self, text: str, timeout: float = 600) -> dict:
        async with self.session.post(self.base + "/ask", json={"text": text, "timeout": timeout}) as r:
            return await r.json()

    # ------------------------------------------------------------------ streams
    async def events(self, replay: int = 0) -> AsyncIterator[dict]:
        """Server-Sent Events."""
        async with self.session.get(self.base + f"/events?replay={replay}") as r:
            buf = ""
            async for chunk in r.content.iter_any():
                buf += chunk.decode("utf-8", errors="replace")
                while "\n\n" in buf:
                    block, buf = buf.split("\n\n", 1)
                    for line in block.splitlines():
                        if line.startswith("data:"):
                            ev = decode(line[5:].strip())
                            if ev:
                                yield ev

    async def ws_events(self, replay: int = 120) -> AsyncIterator[dict]:
        """WebSocket: events in; use ws_send() to talk while iterating."""
        self._ws = await self.session.ws_connect(self.base.replace("http", "ws", 1) + f"/ws?replay={replay}", heartbeat=20)
        async for msg in self._ws:
            if msg.type == aiohttp.WSMsgType.TEXT:
                ev = decode(msg.data)
                if ev:
                    yield ev
            elif msg.type in (aiohttp.WSMsgType.CLOSED, aiohttp.WSMsgType.ERROR):
                break

    async def ws_send(self, text: str) -> None:
        if self._ws is None or self._ws.closed:
            raise ConnectionError("websocket not open")
        await self._ws.send_str(json.dumps({"cmd": "command" if text.startswith("/") else "say", "text": text}))


def base_url(cfg_or_state) -> str:
    """The daemon's URL: from state/groow.url if the daemon wrote one, else the config."""
    state = Path(getattr(cfg_or_state, "state", cfg_or_state))
    p = state / "groow.url"
    if p.exists():
        return p.read_text().strip()
    host = getattr(cfg_or_state, "api_host", "127.0.0.1")
    port = getattr(cfg_or_state, "api_port", 7373)
    return f"http://{'127.0.0.1' if host == '0.0.0.0' else host}:{port}"
