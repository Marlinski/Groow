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
import json
import os
import sys
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
        self.waiters: dict[str, asyncio.Future] = {}        # /ask correlation id -> future(turn_end event)
        self.req_events: dict[str, list] = {}
        self.mood = "idle"
        self._night = False
        self.children: set = set()
        self.turn_running = False
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
        elif ev in ("learned", "felt"):
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
        feeling = a.limbic.mood() if getattr(a, "limbic", None) else {}
        return {"mood": self.mood, "feeling": feeling, "weights_busy": a.brain.busy, "body": os.environ.get("GROOW_BODY", "host"),
                "home": str(Path(self.cfg.home_dir).expanduser() if self.cfg.home_dir else Path.home()),
                "state": str(Path(self.cfg.state).resolve()),
                "steps": b["steps"], "nights": b["consolidations"], "rank": b["rank"],
                "tokens_seen": b.get("tokens_seen", 0), "age": a.birth.age_text(), "safe_mode": self.safe_mode,
                "learning": a.learning_enabled, "thoughts": a.thoughts.listing(False),
                "thoughts_running": len(a.thoughts.running()), "queue": len(a.queue),
                "server": a.server.stats, "gpu_gb": a.brain.status()["gpu_memory_gb"],
                "clients": len(self.subscribers), "identity_version": a.identity.versions(),
                "skills": a.skills.installed(), "handled": self.mind.handled if self.mind else 0}

    def replay(self, n: int) -> list[dict]:
        """Recent conversation as UI events, rebuilt from the main journal (the single durable trace)."""
        out = []
        for rec in self.app.journal.tail(n):
            role, t = rec.get("role"), rec.get("ts")
            if role == "user":
                who = "user" if rec.get("kind", "user") in ("user", "command") else "signal"
                out.append({"ev": "turn_start", "t": t, "who": who, "kind": rec.get("kind", "user"), "text": rec.get("content", ""), "replay": True})
            elif role == "assistant":
                if rec.get("content"):
                    out.append({"ev": "text", "t": t, "delta": rec["content"], "replay": True})
                for c in rec.get("tool_calls") or []:
                    out.append({"ev": "tool_call", "t": t, "name": c["function"]["name"], "args": c["function"]["arguments"],
                                "actor": "main", "replay": True})
                if not rec.get("tool_calls"):
                    out.append({"ev": "turn_end", "t": t, "final": rec.get("content", ""), "replay": True})
            elif role == "tool":
                out.append({"ev": "tool_result", "t": t, "name": rec.get("name", ""), "result": (rec.get("content") or "")[:600],
                            "actor": "main", "replay": True})
        return out

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

    async def h_complete(self, request: web.Request) -> web.Response:
        """The only model call. A turn process sends its conversation (and its tools); the daemon owns
        the tokenizer, so it trims to fit, generates, and streams the text out as events if asked."""
        body = await request.json()
        convs = body.get("messages") or []
        if isinstance(convs, dict) or (convs and isinstance(convs[0], dict)):
            convs = [convs]
        if not convs or len(convs) > 32:
            raise web.HTTPBadRequest(text='{"error": "messages: a conversation or a list of at most 32"}',
                                     content_type="application/json")
        tools = body.get("tools")
        max_new = int(body.get("max_new_tokens") or self.cfg.max_new_tokens)
        temperature = body.get("temperature")
        thinking = body.get("enable_thinking")
        stream = body.get("stream_as") or None
        if body.get("trim"):
            convs = [self._trim(c, tools, max_new) for c in convs]
        on_text = None
        if stream:
            on_text = lambda t: self.emit("text", delta=t, actor=stream.get("actor", "main"), req=stream.get("req"))
        try:
            texts = await asyncio.gather(*[
                self.app.server.complete(c, tools, priority=int(body.get("priority", 2)), max_new_tokens=max_new,
                                         temperature=temperature, enable_thinking=thinking,
                                         on_text=on_text if i == 0 else None)
                for i, c in enumerate(convs)])
        except Exception as e:
            return web.json_response({"interrupted": True, "reason": f"{type(e).__name__}: {e}"}, dumps=encode)
        return web.json_response({"completions": list(texts)}, dumps=encode)

    def _trim(self, messages: list, tools, max_new: int) -> list:
        """Drop the oldest exchanges until the rendered prompt leaves room to answer."""
        from ..brain import trim_messages
        b = self.app.brain
        budget = self.cfg.max_seq_len - max_new
        msgs = list(messages)
        while len(msgs) > 3:
            text = b.prompt_text(msgs, tools=tools)
            if len(b.tok(text, add_special_tokens=False)["input_ids"]) <= budget:
                break
            head = msgs[:1] if msgs and msgs[0]["role"] == "system" else []
            msgs = head + trim_messages(msgs[len(head):], max(2, len(msgs) - len(head) - 3))
        return msgs

    async def h_emit(self, request: web.Request) -> web.Response:
        """A process asks for an event to be broadcast (it has no connection to the UIs itself)."""
        body = await request.json()
        ev = body.pop("ev", "log")
        self.emit(ev, **body)
        return web.json_response({"ok": True})

    async def h_op(self, request: web.Request) -> web.Response:
        """Operations on the running Groow (what `groow …` commands in its shell call)."""
        from ..ops import run_op
        body = await request.json()
        name = body.get("op") or ""
        r = await run_op(self.app, name, body.get("args") or {})
        return web.json_response(r, dumps=encode, status=200 if "error" not in r else 400)

    async def h_events(self, request: web.Request) -> web.StreamResponse:
        replay = int(request.query.get("replay", "60"))   # number of journal messages to rebuild from
        resp = web.StreamResponse(headers={"Content-Type": "text/event-stream", "Cache-Control": "no-cache",
                                           "X-Accel-Buffering": "no"})
        await resp.prepare(request)
        q: asyncio.Queue = asyncio.Queue()
        self.subscribers.add(q)
        try:
            await resp.write(sse(self.hello()))
            for e in self.replay(replay) if replay else []:
                await resp.write(sse(e))
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
                for e in self.replay(replay) if replay else []:
                    await ws.send_str(encode(e))
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

    # ------------------------------------------------------------------ processes
    def _child_env(self) -> dict:
        env = dict(os.environ)
        if self.safe_mode:
            env["GROOW_SAFE"] = "1"
            env["GROOW_INCIDENT"] = json.dumps(self.incident or {}, default=str)[:3000]
        env["GROOW_STATE"] = str(Path(self.cfg.state).resolve())
        env["GROOW_URL"] = self.urlfile.read_text().strip() if self.urlfile.exists() else \
            f"http://127.0.0.1:{self.cfg.api_port}"
        env["PYTHONUNBUFFERED"] = "1"
        return env

    async def _run_child(self, args: list[str], on_event, timeout: float = 1800.0) -> int:
        """Fire a process, read its events off stdout as they come, return its exit code."""
        proc = await asyncio.create_subprocess_exec(
            sys.executable, "-m", "groow.cli", *args, cwd=str(Path(self.cfg.state).parent),
            env=self._child_env(), stdout=asyncio.subprocess.PIPE, stderr=asyncio.subprocess.PIPE)
        self.children.add(proc)

        async def pump():
            async for line in proc.stdout:
                try:
                    ev = json.loads(line)
                except json.JSONDecodeError:
                    continue
                try:
                    on_event(ev)
                except Exception as e:
                    self.emit("log", level="warn", text=f"event handling failed: {type(e).__name__}: {e}")

        try:
            await asyncio.wait_for(asyncio.gather(pump(), proc.wait()), timeout)
        except asyncio.TimeoutError:
            proc.kill()
            self.emit("log", level="error", text=f"a {args[0]} process ran past {timeout:.0f}s and was killed")
        except asyncio.CancelledError:
            proc.kill()
            raise
        finally:
            self.children.discard(proc)
            err = (await proc.stderr.read())[-2000:].decode(errors="replace") if proc.stderr else ""
            if proc.returncode not in (0, None) and err.strip():
                self.emit("log", level="error", text=f"{args[0]} exited {proc.returncode}: {err.strip()[-400:]}")
                self.app.skills.record_incident(f"child_{args[0]}", err)
        return proc.returncode or 0

    async def run_turn(self, signal: dict) -> None:
        """One signal, one process. Its events are broadcast as they arrive; its finished turn is
        handed to the limbic system and the hippocampus, which is where the GPU work happens."""
        done: dict = {}

        def on_event(ev: dict):
            if ev.get("ev") == "turn_done":
                done.update(ev)
                return
            if ev.get("ev") == "message":
                return                                  # journalled by the process itself
            self.emit(ev.pop("ev"), **{k: v for k, v in ev.items() if k != "t"})

        self.turn_running = True
        try:
            await self._run_child(["turn", "--signal", json.dumps(signal, default=str)], on_event,
                                  timeout=self.cfg.turn_timeout)
        finally:
            self.turn_running = False
        if done:
            await self.after_turn(done)

    async def after_turn(self, done: dict) -> None:
        """The GPU side of a turn: feel it, prepare samples, learn, then decide about a night."""
        app = self.app
        if not app.learning_enabled or not done.get("messages"):
            return
        try:
            prepared = await self.app.server.run_gpu(
                lambda: app.hippocampus.nap(done["messages"], done.get("context") or [], done.get("flags") or [],
                                            kind=done.get("kind", "user"), user_text=done.get("user_text", ""),
                                            final_text=done.get("final", "")))
            r = await self.app.server.run_gpu(lambda: app.trainer.consume(max_samples=self.cfg.nap_max_samples))
        except Exception as e:
            self.emit("log", level="error", text=f"nap failed: {type(e).__name__}: {e}")
            return
        self.emit("felt", valence=prepared.get("valence"), pending=prepared.get("pending", False),
                  mood=app.limbic.mood(), consumed=r.get("consumed", 0), sets=r.get("sets"))
        if app.brain.meta["steps"] % 10 == 0:
            app.brain.save()

    async def run_thought(self, thought_id: str) -> None:
        def on_event(ev: dict):
            if ev.get("ev") == "thought_step":
                app = self.app
                app.memory.add_episode([], ev.get("messages") or [], ev.get("tools_used") or [],
                                       kind=f"thought:{thought_id}")
                return
            self.emit(ev.pop("ev"), **{k: v for k, v in ev.items() if k != "t"})
        await self._run_child(["think", thought_id], on_event, timeout=self.cfg.thought_timeout)

    def spawn_thought(self, goal: str, max_steps: int = 8) -> dict:
        """Called through /op by a turn process's `think` tool."""
        app = self.app
        running = app.thoughts.running(refresh=True)
        if len(running) >= self.cfg.max_thoughts:
            return {"error": f"already {self.cfg.max_thoughts} thoughts running; pause or kill one first"}
        brief = app.thoughts.spawn_record(goal, max_steps)
        self.loop.create_task(self.run_thought(brief["id"]))
        self.emit("thought", event="spawn", id=brief["id"], status="running", goal=goal[:140],
                  steps=brief["steps"], text="")
        return brief

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
        app.daemon = self
        app.brain.on_busy = lambda op: self.emit("weights", busy=op is not None, op=op or "")
        app.queue.bind(loop)
        app.thoughts.bind(loop)
        idle_s = self.cfg.sense_idle_minutes * 60 if (self.cfg.curiosity and self.cfg.sense_idle_minutes > 0
                                                       and not self.safe_mode) else 0
        self.mind = mind = Mind(app.queue, on_idle_text=app.curiosity.impulse_text,
                                idle_seconds=idle_s, maybe_sleep=app.maybe_sleep, emit=self.emit,
                                run_command=lambda line: run_command(line, app))
        mind.run_turn = self.run_turn
        mind.on_error = lambda kind, tb: app.skills.record_incident(kind, tb)
        mind.idle_nap = app.idle_nap
        mind.schedule = app.schedule
        app.mind = mind
        if app.thoughts.listing(False) and not self.safe_mode:
            app.queue.push(3, "reminder", "you carried paused thoughts over from your last session: "
                           + "; ".join(f"{t['id']} ({t['status']}): {t['goal'][:60]}" for t in app.thoughts.listing(False)))

        web_app = web.Application(client_max_size=2**20)
        web_app.add_routes([web.get("/hello", self.h_hello), web.get("/status", self.h_status),
                            web.post("/say", self.h_say), web.post("/command", self.h_command),
                            web.post("/ask", self.h_ask), web.post("/op", self.h_op), web.post("/complete", self.h_complete), web.post("/emit", self.h_emit),
                            web.get("/events", self.h_events), web.get("/ws", self.h_ws),
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
            for child in list(self.children):
                child.terminate()
            await app.server.stop()
            app.brain.save()
            await runner.cleanup()
            self.urlfile.unlink(missing_ok=True)
            self.pidfile.unlink(missing_ok=True)
            app.server.gpu.shutdown(wait=False, cancel_futures=True)
            print("groow: asleep (state saved)", flush=True)
            threading.Timer(2.0, lambda: os._exit(0)).start()
