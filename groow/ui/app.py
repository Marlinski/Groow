"""The fullscreen terminal UI (Textual). Connects to the daemon's socket and
renders: the conversation, inner thoughts, the identity card, a status bar and
the creature. Type to talk; slash commands go to the daemon too.

Palette: a night garden. Deep blue-black ground, phosphor mint for Groow,
warm amber for the human, violet for inner thoughts, sky for tools.
"""
from __future__ import annotations

import asyncio
import json
import re
import time
from pathlib import Path

from rich.markup import escape
from rich.text import Text
from textual.app import App, ComposeResult
from textual.containers import Horizontal, Vertical, VerticalScroll
from textual.widgets import Input, Static

from ..gateway import Client
from .creature import CAPTIONS, frame

_HIDE = re.compile(r"<tool_call>.*?(</tool_call>|$)|<think>.*?(</think>|$)", re.DOTALL)


def visible(raw: str) -> str:
    """What a person should see of a streamed generation: no tool-call JSON, no think block."""
    return _HIDE.sub("", raw).strip()


MINT, AMBER, VIOLET, SKY, DIM, ROSE, MOSS = "#7ee8c8", "#f2c97d", "#b89cff", "#6fb7ff", "#5c6b7a", "#ff7b72", "#4e9a7a"


class Bubble(Static):
    """One message in the conversation."""


class ChatLog(VerticalScroll):
    def add(self, who: str, text: str, css: str) -> Bubble:
        b = Bubble(Text(text), classes=f"bubble {css}")
        b.border_title = who
        self.mount(b)
        self.scroll_end(animate=False)
        return b


class Creature(Static):
    mood = "idle"
    tick = 0

    def on_mount(self) -> None:
        self.set_interval(0.45, self.animate)

    def animate(self) -> None:
        self.tick += 1
        t = Text(frame(self.mood, self.tick), style=MINT)
        t.append(f"\n {CAPTIONS.get(self.mood, self.mood)}", style=DIM)
        self.update(t)


class IdCard(Static):
    birth: dict = {}
    status: dict = {}
    identity_version = 1

    def on_mount(self) -> None:
        self.set_interval(1.0, self.render_card)

    def render_card(self) -> None:
        b, s = self.birth, self.status
        if not b:
            self.update(Text("connecting…", style=DIM))
            return
        born = time.time() - b["born"]
        d, r = divmod(int(born), 86400)
        h, r = divmod(r, 3600)
        m, sec = divmod(r, 60)
        age = f"{d}d {h:02d}:{m:02d}:{sec:02d}"
        t = Text()
        t.append(f"{b['name']}\n", style=f"bold {MINT}")
        t.append("id       ", style=DIM); t.append(f"{b['id']}\n")
        t.append("born     ", style=DIM); t.append(f"{b['born_text']}\n")
        t.append("age      ", style=DIM); t.append(f"{age}\n", style=AMBER)
        t.append("lineage  ", style=DIM); t.append(f"{b['lineage'].split('/')[-1]}\n")
        t.append("body     ", style=DIM); t.append(f"{b['hardware']}\n")
        t.append("mentor   ", style=DIM); t.append(f"{b['mentor']}\n")
        t.append("self     ", style=DIM); t.append(f"identity v{self.identity_version}\n")
        if s:
            t.append("nights   ", style=DIM); t.append(f"{s.get('nights', 0)}   ")
            t.append("steps ", style=DIM); t.append(f"{s.get('steps', 0)}\n")
            t.append("skills   ", style=DIM); t.append(", ".join(s.get("skills", [])) or "none")
        self.update(t)


class Thoughts(Static):
    rows: dict[str, dict] = {}

    def upsert(self, ev: dict) -> None:
        self.rows[ev["id"]] = ev
        self.render_rows()

    def sync(self, listing: list[dict]) -> None:
        for t in listing:
            self.rows.setdefault(t["id"], {}).update({"id": t["id"], "status": t["status"], "goal": t["goal"],
                                                      "steps": t["steps"]})
        self.render_rows()

    def render_rows(self) -> None:
        t = Text()
        live = [r for r in self.rows.values() if r.get("status") in ("running", "paused")]
        done = [r for r in self.rows.values() if r.get("status") not in ("running", "paused")][-3:]
        if not live and not done:
            t.append("no inner thoughts\n", style=DIM)
        for r in live + done:
            mark = {"running": "◉", "paused": "◌", "done": "✓", "killed": "✗"}.get(r.get("status"), "·")
            style = VIOLET if r.get("status") == "running" else DIM
            t.append(f"{mark} {r['id']} ", style=style)
            t.append(f"{r.get('steps', '')}\n", style=DIM)
            t.append(f"  {r.get('goal', '')[:60]}\n", style=style)
            if r.get("text"):
                t.append(f"  {r['text'][:90]}\n", style=DIM)
        self.update(t)


class StatusBar(Static):
    def show(self, s: dict) -> None:
        t = Text()
        t.append(f" {s.get('mood', '')} ", style=f"bold {MINT}")
        t.append(f"· queue {s.get('queue', 0)} · thoughts {s.get('thoughts_running', 0)} · "
                 f"gen batches {s.get('server', {}).get('batches', 0)} (preempted {s.get('server', {}).get('preempted', 0)}) · "
                 f"gpu {s.get('gpu_gb', 0)} GB · rank {s.get('rank', '')} · "
                 f"learning {'on' if s.get('learning') else 'off'} · clients {s.get('clients', 0)}", style=DIM)
        if s.get("safe_mode"):
            t.append("  SAFE MODE", style=f"bold {ROSE}")
        self.update(t)


class GroowUI(App):
    TITLE = "groow"
    CSS = f"""
    Screen {{ background: #0b1016; color: #d7dde3; layout: vertical; }}
    #body {{ height: 1fr; }}
    #main {{ width: 3fr; height: 100%; }}
    #side {{ width: 34; min-width: 30; height: 100%; border-left: solid #1e2a33; }}
    ChatLog {{ padding: 0 1; height: 1fr; }}
    #input {{ height: 3; margin: 0 0; }}
    .bubble {{ margin: 0 0 1 0; padding: 0 1; border: round #1e2a33; border-title-color: {DIM}; }}
    .user {{ border: round {AMBER} 40%; border-title-color: {AMBER}; }}
    .groow {{ border: round {MINT} 40%; border-title-color: {MINT}; }}
    .signal {{ border: round {VIOLET} 40%; border-title-color: {VIOLET}; color: #b9c2cb; }}
    .tool {{ border: none; color: {SKY}; padding: 0 2; margin: 0 0 0 0; }}
    .sys {{ border: none; color: {DIM}; padding: 0 2; }}
    Input {{ border: tall {MOSS}; background: #0f161d; color: #e6ecf1; }}
    Input:focus {{ border: tall {MINT}; }}
    Creature {{ height: 8; padding: 0 1; content-align: left middle; }}
    IdCard {{ padding: 0 1; border-top: solid #1e2a33; height: auto; }}
    #thoughts-title, #chat-title {{ color: {DIM}; padding: 0 1; height: 1; }}
    Thoughts {{ padding: 0 1; border-top: solid #1e2a33; height: 1fr; }}
    StatusBar {{ dock: bottom; height: 1; background: #0f161d; }}
    """
    BINDINGS = [("ctrl+q", "quit", "quit"), ("ctrl+l", "clear", "clear log")]

    def __init__(self, base_url: str):
        super().__init__()
        self.base_url = base_url
        self.client: Client | None = None
        self.current: Bubble | None = None
        self.current_text = ""

    def compose(self) -> ComposeResult:
        with Horizontal(id="body"):
            with Vertical(id="main"):
                yield Static(" conversation", id="chat-title")
                yield ChatLog(id="chat")
            with Vertical(id="side"):
                yield Creature(id="creature")
                yield IdCard(id="card")
                yield Static(" inner thoughts", id="thoughts-title")
                yield Thoughts(id="thoughts")
        yield Input(placeholder="talk to groow · /help for commands · ctrl+q to leave", id="input")
        yield StatusBar(id="status")

    async def on_mount(self) -> None:
        self.query_one(Input).focus()
        self.run_worker(self.pump(), exclusive=True)

    # ------------------------------------------------------------------ events in
    async def pump(self) -> None:
        chat = self.query_one(ChatLog)
        while True:
            self.client = Client(self.base_url)
            try:
                async for ev in self.client.ws_events(replay=120):
                    self.handle(ev)
                chat.add("ui", "daemon went away; reconnecting…", "sys")
            except Exception as e:
                chat.add("ui", f"no daemon at {self.base_url} ({type(e).__name__}); start one with `groow start` (retrying)", "sys")
            finally:
                await self.client.close()
                self.client = None
            await asyncio.sleep(3)

    def handle(self, ev: dict) -> None:
        k = ev["ev"]
        chat = self.query_one(ChatLog)
        card = self.query_one(IdCard)
        creature = self.query_one(Creature)
        if k == "hello":
            card.birth = ev["birth"]
            card.status = ev["status"]
            card.identity_version = ev["status"].get("identity_version", 1)
            self.query_one(StatusBar).show(ev["status"])
            self.query_one(Thoughts).sync(ev["status"].get("thoughts", []))
            chat.add("ui", f"connected · {ev['model']} · {len(ev['tools'])} tools" + (" · SAFE MODE" if ev["safe_mode"] else ""), "sys")
            if ev.get("inbox"):
                chat.add("inbox", "\n".join("• " + q["question"] for q in ev["inbox"][-5:]) + "\n(answer here; /inbox clear when done)", "signal")
            return
        if k == "status":
            card.status = ev
            card.identity_version = ev.get("identity_version", card.identity_version)
            creature.mood = ev.get("mood", creature.mood)
            self.query_one(StatusBar).show(ev)
            self.query_one(Thoughts).sync(ev.get("thoughts", []))
            return
        replay = ev.get("replay", False)
        if k == "turn_start":
            if ev["who"] == "user":
                chat.add("you", ev["text"], "user")
            else:
                chat.add(f"signal · {ev['kind']}", ev["text"][:400], "signal")
            self.current = None
            self.current_text = ""
            creature.mood = "thinking"
        elif k == "text":
            if replay:
                return
            self.current_text += ev["delta"]
            shown = visible(self.current_text)
            if not shown:
                creature.mood = "thinking"
                return
            if self.current is None:
                self.current = chat.add("groow", "", "groow")
            self.current.update(Text(shown))
            chat.scroll_end(animate=False)
            creature.mood = "speaking"
        elif k == "turn_end":
            final = visible(ev.get("final") or "")
            if self.current is None and final:
                chat.add("groow", final, "groow")
            elif self.current is not None and final:
                self.current.update(Text(final))
            self.current = None
            creature.mood = "listening"
        elif k == "tool_call":
            actor = "" if ev.get("actor") == "main" else f" [{ev.get('actor')}]"
            chat.add("", f"⚙ {ev['name']}({_short(ev['args'], 100)}){actor}", "tool")
            creature.mood = "tooling"
        elif k == "tool_result":
            if ev.get("actor") == "main":
                chat.add("", f"  ↳ {_short(ev['result'], 220)}", "sys")
        elif k == "learned":
            chat.add("", f"↺ learned · loss {ev['loss']:.3f} · {ev['tokens']} tokens · step {ev['step']}", "sys")
            creature.mood = "learning"
        elif k == "thought":
            self.query_one(Thoughts).upsert(ev)
            if ev["event"] in ("spawn", "focus", "done", "killed"):
                chat.add(f"thought {ev['id']}", f"{ev['event']}: {ev.get('text') or ev.get('goal')}", "signal")
        elif k == "sleep":
            msg = ev.get("text") or ev.get("outcome") or ev.get("why") or ""
            chat.add("night", f"{ev['phase']} {msg}", "signal")
            creature.mood = "sleeping" if ev["phase"] != "done" else "listening"
        elif k == "inbox":
            chat.add("inbox", "\n".join("• " + q["question"] for q in ev["questions"]) or "empty", "signal")
        elif k == "log":
            chat.add("", ev.get("text", "")[:2000], "sys")
        elif k == "bye":
            chat.add("ui", "groow went to sleep (daemon stopped)", "sys")
            creature.mood = "sleeping"

    # ------------------------------------------------------------------ input
    async def on_input_submitted(self, msg: Input.Submitted) -> None:
        text = msg.value.strip()
        msg.input.value = ""
        if not text:
            return
        if text in ("/quit", "/exit"):
            self.exit()
            return
        if self.client is None:
            self.query_one(ChatLog).add("ui", "not connected", "sys")
            return
        try:
            await self.client.ws_send(text)
        except Exception as e:
            self.query_one(ChatLog).add("ui", f"send failed: {e}", "sys")

    def action_clear(self) -> None:
        self.query_one(ChatLog).remove_children()


def _short(x, n: int) -> str:
    s = x if isinstance(x, str) else json.dumps(x, ensure_ascii=False)
    return s if len(s) <= n else s[:n] + "…"


def run_ui(base_url: str) -> None:
    GroowUI(base_url).run()
