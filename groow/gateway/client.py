"""Client side of the protocol: connect to the daemon's socket, send commands,
iterate events. Used by `groow chat` (line client), `groow status`, `groow stop`
and the Textual UI."""
from __future__ import annotations

import asyncio
from pathlib import Path
from typing import AsyncIterator

from .protocol import decode, encode


class Client:
    def __init__(self, socket_path: str | Path):
        self.path = str(socket_path)
        self.reader: asyncio.StreamReader | None = None
        self.writer: asyncio.StreamWriter | None = None

    async def connect(self, timeout: float = 5.0) -> "Client":
        self.reader, self.writer = await asyncio.wait_for(asyncio.open_unix_connection(self.path), timeout)
        return self

    async def send(self, cmd: str, **data) -> None:
        self.writer.write(encode({"cmd": cmd, **data}))
        await self.writer.drain()

    async def say(self, text: str) -> None:
        await self.send("command" if text.startswith("/") else "say", text=text)

    async def events(self) -> AsyncIterator[dict]:
        while True:
            line = await self.reader.readline()
            if not line:
                return
            ev = decode(line)
            if ev:
                yield ev

    async def close(self) -> None:
        if self.writer:
            self.writer.close()
            try:
                await self.writer.wait_closed()
            except Exception:
                pass


def default_socket(state_dir: str | Path) -> Path:
    return Path(state_dir) / "groow.sock"
