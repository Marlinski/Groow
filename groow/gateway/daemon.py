"""The daemon: `groow start` runs the mind and serves events on a Unix socket.

One process owns the GPU and the state. Any number of UIs connect, each gets a
`hello` (birth card, identity, tools), the recent event history, then the live
stream. Human messages and slash commands arrive as commands and are pushed
into the mind's priority queue like any other signal.

The supervisor around it (see cli.cmd_start) handles crashes: a skill in the
traceback is quarantined and the daemon restarts; anything else restarts in
safe mode.
"""
from __future__ import annotations

import asyncio
import collections
import json
import os
import time
from pathlib import Path

from ..config import Config
from .protocol import decode, encode, event

LEARN_TOOLS = {"memorize", "learn_fact", "play", "quiz", "consolidate", "grow", "probe"}
READ_TOOLS = {"news_headlines", "read_article", "recall", "read_thought", "read_skill", "read_incidents"}


class Daemon:
    def __init__(self, cfg: Config, safe_mode: bool = False, incident: dict | None = None, verbose: bool = False):
        self.cfg, self.safe_mode, self.incident, self.verbose = cfg, safe_mode, incident, verbose
        self.sock = Path(cfg.state) / "groow.sock"
        self.pidfile = Path(cfg.state) / "groow.pid"
        self.clients: set[asyncio.StreamWriter] = set()
        self.history: collections.deque = collections.deque(maxlen=300)
        self.mood = "idle"
        self._mood_until = 0.0
        self.app = None
        self.mind = None
        self.outcome = "quit"

    # ------------------------------------------------------------------ events out
    def emit(self, ev: str, **data) -> None:
        e = event(ev, **data)
        self._update_mood(e)
        if ev != "status":
            self.history.append(e)
        payload = encode(e)
        dead = []
        for w in self.clients:
            try:
                if w.is_closing():
                    dead.append(w)
                    continue
                w.write(payload)
            except Exception:
                dead.append(w)
        for w in dead:
            self.clients.discard(w)
        if self.verbose and ev not in ("text", "status"):
            print(json.dumps(e, ensure_ascii=False, default=str)[:300], flush=True)

    def _update_mood(self, e: dict) -> None:
        ev = e["ev"]
        now = time.time()
        if ev == "turn_start":
            self.mood = "reading" if e.get("kind") == "idle" else "thinking"
        elif ev == "text":
            self.mood = "speaking"
        elif ev == "tool_call":
            n = e.get("name", "")
            self.mood = "learning" if n in LEARN_TOOLS else "reading" if n in READ_TOOLS else "tooling"
        elif ev == "learned":
            self.mood, self._mood_until = "learning", now + 1.5
        elif ev == "turn_end":
            self.mood = "listening"
        elif ev == "sleep":
            self.mood = "sleeping" if e.get("phase") != "done" else "listening"
        elif ev == "thought" and e.get("event") in ("spawn", "step") and self.mood in ("idle", "listening"):
            self.mood = "dreaming"
        if self.safe_mode:
            self.mood = "repair"

    def status(self) -> dict:
        a = self.app
        b = a.brain.meta
        if self.mood in ("listening",) and time.time() - self.mind.last_human > 60 and not self.app.thoughts.running():
            self.mood = "idle"
        return {"mood": self.mood, "steps": b["steps"], "nights": b["consolidations"], "rank": b["rank"],
                "tokens_seen": b.get("tokens_seen", 0), "age": a.birth.age_text(), "safe_mode": self.safe_mode,
                "learning": a.learning_enabled, "thoughts": a.thoughts.listing(False),
                "thoughts_running": len(a.thoughts.running()), "queue": len(a.queue),
                "server": a.server.stats, "gpu_gb": a.brain.status()["gpu_memory_gb"],
                "clients": len(self.clients), "identity_version": a.identity.versions(),
                "skills": a.skills.installed(), "handled": self.mind.handled if self.mind else 0}

    def hello(self) -> dict:
        a = self.app
        return event("hello", birth=a.birth.card(), identity=a.identity.text(), model=self.cfg.model_id,
                     tools=sorted(a.tools.names()), safe_mode=self.safe_mode, status=self.status(),
                     inbox=a.open_questions())

    # ------------------------------------------------------------------ clients
    async def handle_client(self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter) -> None:
        self.clients.add(writer)
        try:
            try:
                writer.write(encode(self.hello()))
                for e in list(self.history)[-120:]:
                    writer.write(encode({**e, "replay": True}))
                await writer.drain()
            except Exception:
                pass                      # a client that only wants to send (groow stop) may not read
            while True:
                line = await reader.readline()
                if not line:
                    break
                msg = decode(line)
                if not msg:
                    continue
                cmd, text = msg.get("cmd"), (msg.get("text") or "").strip()
                if cmd in ("say", "command") and text:
                    self.mind.push_user(text)
                elif cmd == "status":
                    writer.write(encode(event("status", **self.status())))
                    await writer.drain()
        except (ConnectionResetError, asyncio.IncompleteReadError, BrokenPipeError):
            pass
        finally:
            self.clients.discard(writer)
            writer.close()

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
        self.app = app = App(self.cfg, emit=self.emit, safe_mode=self.safe_mode, incident=self.incident)
        loop = asyncio.get_running_loop()
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
        if self.sock.exists():
            self.sock.unlink()
        server = await asyncio.start_unix_server(self.handle_client, path=str(self.sock))
        os.chmod(self.sock, 0o600)
        self.pidfile.write_text(str(os.getpid()))
        status_task = loop.create_task(self._status_loop())
        self.emit("log", level="info", text=f"{app.birth.name} is awake ({'safe mode' if self.safe_mode else 'normal'}), "
                                             f"socket {self.sock}")
        print(f"groow: awake · socket {self.sock} · pid {os.getpid()} · {'SAFE MODE' if self.safe_mode else 'normal'}", flush=True)
        try:
            await mind.run()
            self.outcome = "restart" if getattr(app, "restart", False) else "quit"
            return self.outcome
        finally:
            status_task.cancel()
            self.emit("bye")
            for t in app.thoughts.running():
                app.thoughts.pause(t.id)
            await app.thoughts.wait_idle(10)
            await app.server.stop()
            app.brain.save()
            server.close()
            for w in list(self.clients):
                w.close()
            self.sock.unlink(missing_ok=True)
            self.pidfile.unlink(missing_ok=True)
            app.server.gpu.shutdown(wait=False, cancel_futures=True)
            print("groow: asleep (state saved)", flush=True)
            import threading
            code = 0
            threading.Timer(5.0, lambda: os._exit(3 if self.outcome == "restart" else code)).start()
