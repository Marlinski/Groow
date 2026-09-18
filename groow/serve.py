"""The brain, as a service.

The core is in Rust and owns the state; this owns the GPU. It answers two things:
whether it is up, and one generation at a time, streamed token by token.

Generations are serialised because there is one model on one card, and they are
ordered by priority: the conscious turn goes before an inner thought, which goes
before anything running in the background. Trimming happens here rather than in
the core because this is the side that has the tokenizer and therefore the only
side that knows how much actually fits.

    python -m groow.serve --port 7374
"""
from __future__ import annotations

import argparse
import asyncio
import json
import time
from dataclasses import dataclass, field
from pathlib import Path

from aiohttp import web

from .config import Config

# Lower goes first.
MAIN, THOUGHT, BACKGROUND = 0, 2, 4


@dataclass(order=True)
class Job:
    priority: int
    seq: int
    payload: dict = field(compare=False)
    out: asyncio.Queue = field(compare=False)
    loop: asyncio.AbstractEventLoop = field(compare=False)


class Server:
    def __init__(self, cfg: Config):
        self.cfg = cfg
        self.brain = None
        self.queue: asyncio.PriorityQueue[Job] = asyncio.PriorityQueue()
        self.seq = 0
        self.busy = False
        self.generated = 0

    # ---------------------------------------------------------------- model
    def load(self) -> None:
        from .brain.model import Brain

        self.brain = Brain(self.cfg).load()

    def fits(self, messages: list[dict], tools: list[dict]) -> list[dict]:
        """Drop the oldest exchanges until the prompt fits, keeping the system message.

        The system message is who it is, so it is never dropped; whole exchanges go
        rather than halves, so the window never opens on a dangling answer.
        """
        budget = self.cfg.max_seq_len - self.cfg.max_new_tokens - 64
        if budget <= 0 or not messages:
            return messages
        tok = self.brain.tok
        head = [m for m in messages[:1] if m.get("role") == "system"]
        rest = messages[len(head):]

        def size(ms):
            try:
                return len(tok(self.brain.prompt_text(ms, tools=tools or None))["input_ids"])
            except Exception:
                # If the template cannot render, let the model complain rather than
                # silently sending something different from what was asked for.
                return 0

        while rest and size(head + rest) > budget:
            # Take two at a time so a question never loses its answer.
            drop = 2 if len(rest) > 2 else 1
            rest = rest[drop:]
        return head + rest

    # ---------------------------------------------------------------- worker
    async def work(self) -> None:
        while True:
            job = await self.queue.get()
            self.busy = True
            try:
                await asyncio.get_running_loop().run_in_executor(None, self.run, job)
            except Exception as e:  # a failed generation must not take the server down
                self.send(job, {"error": f"{type(e).__name__}: {e}"})
            finally:
                self.busy = False
                self.send(job, None)
                self.queue.task_done()

    def send(self, job: Job, item) -> None:
        job.loop.call_soon_threadsafe(job.out.put_nowait, item)

    def run(self, job: Job) -> None:
        """Generate on the model thread, pushing pieces back as they appear."""
        p = job.payload
        started = time.time()
        messages = self.fits(p.get("messages") or [], p.get("tools") or [])
        pieces: list[str] = []

        def on_text(t: str) -> None:
            pieces.append(t)
            self.send(job, {"delta": t})

        text = self.brain.generate(
            messages,
            tools=p.get("tools") or None,
            on_text=on_text,
            max_new_tokens=int(p.get("max_new_tokens") or self.cfg.max_new_tokens),
            temperature=float(p.get("temperature", self.cfg.temperature)),
            top_p=float(p.get("top_p", self.cfg.top_p)),
            top_k=int(p.get("top_k", self.cfg.top_k)),
            enable_thinking=bool(p.get("enable_thinking", self.cfg.enable_thinking)),
        )
        if text is None:
            text = "".join(pieces)
        self.generated += 1
        self.send(job, {
            "done": True,
            "text": text,
            "tokens": len(self.brain.tok(text)["input_ids"]) if text else 0,
            "seconds": round(time.time() - started, 3),
        })

    # ---------------------------------------------------------------- routes
    async def h_health(self, request: web.Request) -> web.Response:
        return web.json_response({
            "ok": self.brain is not None,
            "model": self.cfg.model_id,
            "busy": self.busy,
            "waiting": self.queue.qsize(),
            "generated": self.generated,
        })

    async def h_reload(self, request: web.Request) -> web.Response:
        """Pick up weights that changed underneath us.

        A night merges the overlay into the base on disk. Without this the running brain would
        keep answering from the copy it loaded hours ago, and the night would appear to have
        done nothing at all. Generation is serialised through the queue, so this waits for the
        current one to finish rather than swapping the model out from under it.
        """
        while self.busy or self.queue.qsize():
            await asyncio.sleep(0.2)
        try:
            await asyncio.get_running_loop().run_in_executor(None, self.load)
        except Exception as e:
            return web.json_response({"ok": False, "error": f"{type(e).__name__}: {e}"}, status=500)
        return web.json_response({"ok": True, "model": self.cfg.model_id, "reloaded": True})

    async def h_generate(self, request: web.Request) -> web.StreamResponse:
        try:
            payload = await request.json()
        except Exception:
            return web.json_response({"error": "that was not JSON"}, status=400)
        if not payload.get("messages"):
            return web.json_response({"error": "there was nothing to generate from"}, status=400)

        resp = web.StreamResponse(headers={"Content-Type": "application/x-ndjson"})
        await resp.prepare(request)

        self.seq += 1
        job = Job(
            priority=int(payload.get("priority", MAIN)),
            seq=self.seq,
            payload=payload,
            out=asyncio.Queue(),
            loop=asyncio.get_running_loop(),
        )
        await self.queue.put(job)

        while True:
            item = await job.out.get()
            if item is None:
                break
            try:
                await resp.write((json.dumps(item, ensure_ascii=False) + "\n").encode())
            except (ConnectionResetError, asyncio.CancelledError):
                # The core went away mid-generation. Let the model finish quietly.
                break
        await resp.write_eof()
        return resp

    def app(self) -> web.Application:
        app = web.Application(client_max_size=64 * 1024 * 1024)
        app.add_routes([
            web.get("/health", self.h_health),
            web.post("/generate", self.h_generate),
            web.post("/reload", self.h_reload),
        ])
        app.on_startup.append(lambda _: self._start())
        return app

    async def _start(self) -> None:
        asyncio.create_task(self.work())


def main() -> None:
    ap = argparse.ArgumentParser(description="the brain, as a service")
    ap.add_argument("--config", default="groow.json")
    ap.add_argument("--state", default=None)
    ap.add_argument("--host", default="127.0.0.1")
    ap.add_argument("--port", type=int, default=7374)
    args = ap.parse_args()

    cfg = Config.load(Path(args.config))
    if args.state:
        cfg.state_dir = args.state
    s = Server(cfg)
    print(f"loading {cfg.model_id} from {cfg.state}…", flush=True)
    s.load()
    print(f"the brain is listening on http://{args.host}:{args.port}", flush=True)
    web.run_app(s.app(), host=args.host, port=args.port, print=None)


if __name__ == "__main__":
    main()
