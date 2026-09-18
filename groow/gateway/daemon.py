"""The daemon: `groow start` runs the mind and serves it over HTTP.

One process owns the GPU and the state. Clients use plain HTTP for single
queries (/hello, /status, /ask, /say, /command), Server-Sent Events for the
run-loop stream (/events) and a WebSocket for interactive UIs (/ws).

Events are produced from several threads (the GPU executor emits `learned`,
tools emit progress), so `emit` hops onto the event loop and fans out through
per-subscriber queues; each connection has one writer task draining its queue.

The supervisor around it (see cli.cmd_start) handles crashes: a skill in the
traceback is quarantined and the daemon restarts; anything else restarts in
safe mode.
"""
from __future__ import annotations

import asyncio
import collections
import json
import os
import threading
import time
import uuid
from pathlib import Path

from aiohttp import web, WSMsgType

from ..config import Config
from .protocol import encode, event, sse

LEARN_TOOLS = {"memorize", "learn_fact", "play", "quiz", "consolidate", "grow", "probe"}
READ_TOOLS = {"news_headlines", "read_article", "recall", "read_thought", "read_skill", "read_incidents"}


class Daemon:
    def __init__(self, cfg: Config, safe_mode: bool = False, incident: dict | None = None, verbose: bool = False):
        self.cfg, self.safe_mode, self.incident, self.verbose = cfg, safe_mode, incident, verbose
        self.urlfile = Path(cfg.state) / "groow.url"
        self.pidfile = Path(cfg.state) / "groow.pid"
        self.subscribers: set[asyncio.Queue] = set()
        self.history: collections.deque = collections.deque(maxlen=600)   # text deltas are coalesced per turn
        self.waiters: dict[str, asyncio.Future] = {}        # /ask correlation id -> future(turn_end event)
        self.req_events: dict[str, list] = {}
        self.mood = "idle"
        self._night = False
        self.app = None
        self.mind = None
        self.loop: asyncio.AbstractEventLoop | None = None
        self.outcome = "quit"
        self._stopping = asyncio.Event() if False else None

    # ------------------------------------------------------------------ events out (thread-safe)
    def emit(self, ev: str, **data) -> None:
        e = event(ev, **data)
        if self.loop is None:
            return
        if threading.current_thread() is threading.main_thread():
            self._fanout(e)
        else:
            self.loop.call_soon_threadsafe(self._fanout, e)

    def _fanout(self, e: dict) -> None:
        self._update_mood(e)
        if e["ev"] == "text":
            # keep one coalesced text entry per turn in the history (a copy: live queues hold the original)
            last = self.history[-1] if self.history else None
            if last is not None and last["ev"] == "text" and last.get("req") == e.get("req"):
                last["delta"] += e["delta"]
            else:
                self.history.append({**e})
        elif e["ev"] != "status":
            self.history.append(e)
        req = e.get("req")
        if req in self.req_events:
            self.req_events[req].append(e)
            if e["ev"] == "turn_end" and req in self.waiters and not self.waiters[req].done():
                self.waiters[req].set_result(e)
        for q in list(self.subscribers):
            if q.qsize() > 2000:                 # a client that never reads is dropped
                self.subscribers.discard(q)
                continue
            q.put_nowait(e)
        if self.verbose and e["ev"] not in ("text", "status"):
            print(encode(e)[:300], flush=True)

    def _update_mood(self, e: dict) -> None:
        ev = e["ev"]
        if ev == "turn_start":
            self.mood = "reading" if e.get("kind") == "idle" else "thinking"
        elif ev == "text":
            self.mood = "speaking"
        elif ev == "tool_call":
            n = e.get("name", "")
            self.mood = "learning" if n in LEARN_TOOLS else "reading" if n in READ_TOOLS else "tooling"
        elif ev == "weights":
            if e.get("busy"):
                self.mood = "sleeping" if self._night else "napping"
            elif self.mood in ("napping",):
                self.mood = "listening"
        elif ev == "learned":
            self.mood = "learning" if self.mood != "napping" else self.mood
        elif ev == "turn_end":
            self.mood = "listening"
        elif ev == "sleep":
            self._night = e.get("phase") != "done"
            self.mood = "sleeping" if self._night else "listening"
        elif ev == "thought" and e.get("event") in ("spawn", "step") and self.mood in ("idle", "listening"):
            self.mood = "dreaming"
        if self.safe_mode:
            self.mood = "repair"

    def status(self) -> dict:
        a = self.app
        b = a.brain.meta
        if self.mood == "listening" and self.mind and time.time() - self.mind.last_human > 60 and not a.thoughts.running():
            self.mood = "idle"
        return {"mood": self.mood, "weights_busy": a.brain.busy, "body": os.environ.get("GROOW_BODY", "host"),
                "home": str(Path(self.cfg.home_dir).expanduser() if self.cfg.home_dir else Path.home()),
                "state": str(Path(self.cfg.state).resolve()),
                "steps": b["steps"], "nights": b["consolidations"], "rank": b["rank"],
                "tokens_seen": b.get("tokens_seen", 0), "age": a.birth.age_text(), "safe_mode": self.safe_mode,
                "learning": a.learning_enabled, "thoughts": a.thoughts.listing(False),
                "thoughts_running": len(a.thoughts.running()), "queue": len(a.queue),
                "server": a.server.stats, "gpu_gb": a.brain.status()["gpu_memory_gb"],
                "clients": len(self.subscribers), "identity_version": a.identity.versions(),
                "skills": a.skills.installed(), "handled": self.mind.handled if self.mind else 0}

    def hello(self) -> dict:
        a = self.app
        return event("hello", birth=a.birth.card(), identity=a.identity.text(), model=self.cfg.model_id,
                     tools=sorted(a.tools.names()), safe_mode=self.safe_mode, status=self.status(),
                     inbox=a.open_questions(), url=self.urlfile.read_text().strip() if self.urlfile.exists() else "")

    # ------------------------------------------------------------------ HTTP routes
    async def h_hello(self, request: web.Request) -> web.Response:
        return web.json_response(self.hello(), dumps=encode)

    async def h_status(self, request: web.Request) -> web.Response:
        return web.json_response(event("status", **self.status()), dumps=encode)

    async def _enqueue(self, request: web.Request, force_command: bool = False) -> tuple[str, str]:
        body = await request.json()
        text = (body.get("text") or "").strip()
        if not text:
            raise web.HTTPBadRequest(text='{"error": "text is required"}', content_type="application/json")
        if force_command and not text.startswith("/"):
            text = "/" + text
        req = uuid.uuid4().hex[:8]
        self.mind.push_user(text, req=req)
        return text, req

    async def h_say(self, request: web.Request) -> web.Response:
        text, req = await self._enqueue(request)
        return web.json_response({"queued": True, "req": req, "queue": len(self.app.queue)})

    async def h_command(self, request: web.Request) -> web.Response:
        body = await request.json()
        text = (body.get("text") or "").strip()
        if text in ("/quit", "/stop", "/exit", "/restart"):
            self.mind.stop(restart=(text == "/restart"))
            return web.json_response({"stopping": True, "restart": text == "/restart"})
        text, req = await self._enqueue(request, force_command=True)
        return web.json_response({"queued": True, "req": req})

    async def h_ask(self, request: web.Request) -> web.Response:
        body = await request.json()
        timeout = float(body.get("timeout") or 600)
        text = (body.get("text") or "").strip()
        if not text:
            raise web.HTTPBadRequest(text='{"error": "text is required"}', content_type="application/json")
        req = uuid.uuid4().hex[:8]
        fut = self.loop.create_future()
        self.waiters[req] = fut
        self.req_events[req] = []
        self.mind.push_user(text, req=req)
        try:
            end = await asyncio.wait_for(fut, timeout)
        except asyncio.TimeoutError:
            return web.json_response({"error": "timeout", "req": req, "events": self.req_events.get(req, [])},
                                     status=504, dumps=encode)
        finally:
            self.waiters.pop(req, None)
            evs = self.req_events.pop(req, [])
        return web.json_response({"req": req, "final": end.get("final", ""), "tools_used": end.get("tools_used", []),
                                  "seconds": end.get("seconds"), "events": [e for e in evs if e["ev"] != "text"]}, dumps=encode)

    async def h_events(self, request: web.Request) -> web.StreamResponse:
        replay = int(request.query.get("replay", "60"))
        resp = web.StreamResponse(headers={"Content-Type": "text/event-stream", "Cache-Control": "no-cache",
                                           "X-Accel-Buffering": "no"})
        await resp.prepare(request)
        q: asyncio.Queue = asyncio.Queue()
        self.subscribers.add(q)
        try:
            await resp.write(sse(self.hello()))
            for e in list(self.history)[-replay:] if replay else []:
                await resp.write(sse({**e, "replay": True}))
            while True:
                try:
                    e = await asyncio.wait_for(q.get(), timeout=15)
                except asyncio.TimeoutError:
                    await resp.write(b": keepalive\n\n")
                    continue
                await resp.write(sse(e))
                if e["ev"] == "bye":
                    break
        except (ConnectionResetError, asyncio.CancelledError):
            pass
        finally:
            self.subscribers.discard(q)
        return resp

    async def h_ws(self, request: web.Request) -> web.WebSocketResponse:
        ws = web.WebSocketResponse(heartbeat=20)
        await ws.prepare(request)
        replay = int(request.query.get("replay", "200"))
        q: asyncio.Queue = asyncio.Queue()
        self.subscribers.add(q)

        async def writer():
            try:
                await ws.send_str(encode(self.hello()))
                for e in list(self.history)[-replay:] if replay else []:
                    await ws.send_str(encode({**e, "replay": True}))
                while True:
                    e = await q.get()
                    await ws.send_str(encode(e))
                    if e["ev"] == "bye":
                        await ws.close()
                        return
            except (ConnectionResetError, asyncio.CancelledError):
                pass

        wtask = self.loop.create_task(writer())
        try:
            async for msg in ws:
                if msg.type != WSMsgType.TEXT:
                    continue
                try:
                    m = json.loads(msg.data)
                except json.JSONDecodeError:
                    continue
                cmd, text = m.get("cmd"), (m.get("text") or "").strip()
                if cmd == "command" and text in ("/quit", "/stop", "/exit", "/restart"):
                    self.mind.stop(restart=(text == "/restart"))
                elif cmd in ("say", "command") and text:
                    self.mind.push_user(text if cmd == "say" or text.startswith("/") else "/" + text)
                elif cmd == "status":
                    await ws.send_str(encode(event("status", **self.status())))
        finally:
            wtask.cancel()
            self.subscribers.discard(q)
        return ws

    async def _status_loop(self) -> None:
        while True:
            await asyncio.sleep(2.0)
            try:
                self.emit("status", **self.status())
            except Exception as e:
                self.emit("log", level="warn", text=f"status failed: {e}")

    # ------------------------------------------------------------------ run
    async def run(self) -> str:
        from ..cli import App, run_command
        from ..mind import Mind
        self.loop = loop = asyncio.get_running_loop()
        print(f"groow: waking up: loading {self.cfg.model_id} into the GPU (~30 s), then memory, skills, senses…", flush=True)
        self.app = app = App(self.cfg, emit=self.emit, safe_mode=self.safe_mode, incident=self.incident)
        app.brain.on_busy = lambda op: self.emit("weights", busy=op is not None, op=op or "")
        app.queue.bind(loop)
        app.thoughts.bind(loop)
        idle_s = self.cfg.sense_idle_minutes * 60 if (self.cfg.curiosity and self.cfg.sense_idle_minutes > 0
                                                       and not self.safe_mode) else 0
        self.mind = mind = Mind(app.harness, app.queue, app.thoughts, on_idle_text=app.curiosity.impulse_text,
                                idle_seconds=idle_s, maybe_sleep=app.maybe_sleep, emit=self.emit,
                                run_command=lambda line: run_command(line, app))
        mind.on_error = lambda kind, tb: app.skills.record_incident(kind, tb)
        app.mind = mind
        if app.thoughts.listing(False) and not self.safe_mode:
            app.queue.push(3, "reminder", "you carried paused thoughts over from your last session: "
                           + "; ".join(f"{t['id']} ({t['status']}): {t['goal'][:60]}" for t in app.thoughts.listing(False)))

        web_app = web.Application(client_max_size=2**20)
        web_app.add_routes([web.get("/hello", self.h_hello), web.get("/status", self.h_status),
                            web.post("/say", self.h_say), web.post("/command", self.h_command),
                            web.post("/ask", self.h_ask), web.get("/events", self.h_events), web.get("/ws", self.h_ws),
                            web.get("/", self.h_hello)])
        runner = web.AppRunner(web_app, access_log=None, shutdown_timeout=2.0)
        await runner.setup()
        site = web.TCPSite(runner, self.cfg.api_host, self.cfg.api_port)
        await site.start()
        shown_host = "127.0.0.1" if self.cfg.api_host == "0.0.0.0" else self.cfg.api_host
        url = f"http://{shown_host}:{self.cfg.api_port}"
        self.urlfile.write_text(url)
        self.pidfile.write_text(str(os.getpid()))
        status_task = loop.create_task(self._status_loop())
        self.emit("log", level="info", text=f"{app.birth.name} is awake ({'safe mode' if self.safe_mode else 'normal'}) at {url}")
        print(f"groow: awake · {url} · pid {os.getpid()} · {'SAFE MODE' if self.safe_mode else 'normal'}", flush=True)
        try:
            await mind.run()
            self.outcome = "restart" if (getattr(app, "restart", False) or mind.restart_requested) else "quit"
            return self.outcome
        finally:
            status_task.cancel()
            self.emit("bye")
            # whatever happens below, the process ends: CUDA and executor threads are known to linger
            threading.Timer(20.0, lambda: os._exit(0)).start()
            await asyncio.sleep(0.3)                      # let writers flush the goodbye
            for t in app.thoughts.running():
                app.thoughts.pause(t.id)
            await app.thoughts.wait_idle(10)
            await app.server.stop()
            app.brain.save()
            await runner.cleanup()
            self.urlfile.unlink(missing_ok=True)
            self.pidfile.unlink(missing_ok=True)
            app.server.gpu.shutdown(wait=False, cancel_futures=True)
            print("groow: asleep (state saved)", flush=True)
            threading.Timer(2.0, lambda: os._exit(0)).start()
