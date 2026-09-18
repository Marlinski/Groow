"""groow: a model that learns by rewriting its own weights.

  groow init                       give birth: copy the base model into state/, write birth.json
  groow start                      wake Groow in its body (Docker sandbox; gives birth the first time)
  groow start --nosandbox [-v]     run on this host, as you: shell in your home, state in ~/.groow
  groow ui                         fullscreen terminal UI (WebSocket)
  groow chat                       minimal line client (SSE + POST /say)
  groow ask "question"             single query over HTTP, waits for the answer
  groow status | stop              snapshot / shut the daemon down
  groow doctor                     static health check (no model)
  one-shot (no daemon running):    memorize, quiz, play, sense, sleep, rollback, identity, probe, stats,
                                   consolidate, grow, tools
"""
from __future__ import annotations

import argparse
import asyncio
import json
import os
import shutil
import sys
from pathlib import Path

from rich.console import Console
from rich.panel import Panel

from .config import Config

console = Console()

SAFE_MODE_PROMPT = """You are Groow, running in SAFE MODE because the normal session crashed. Skills are unloaded, passive learning and curiosity are off. Your job now is repair, not conversation: call read_incidents to see the traceback, read_skill / list_skills to inspect what you wrote, fix a skill with draft_skill (tests must pass) and install_skill, or disable_skill if it cannot be saved. If the fault is in the core harness rather than a skill, write a propose_patch and ask_mentor. When you are done, say exactly: REPAIRED. Marlinski, your mentor, is watching.

Incident: {incident}"""


# ====================================================================== the App
class App:
    """Wires the organs together: brain + generation server, memory, learner, identity, tools,
    the main harness (the conscious thread), inner thoughts, sleep, curiosity, skills.
    Everything observable goes through `emit(event, **data)` (protocol events)."""

    def __init__(self, cfg: Config, emit=None, safe_mode: bool = False, incident: dict | None = None):
        import transformers
        transformers.logging.set_verbosity_error()
        transformers.logging.disable_progress_bar()
        from .brain import Brain, GenServer, ServedBrain
        from .memory import Memory
        from .learning import Learner, SleepPolicy, Curiosity, Identity
        from .senses import NewsSense
        from .mind import InputQueue, ThoughtManager
        from .birth import load_or_create
        from .harness import (Harness, Hooks, ToolRegistry, make_builtin_tools, make_self_tools, make_sense_tools,
                              make_main_mind_tools, make_thought_tools, SkillManager, make_skill_tools)

        self.cfg = cfg
        self.emit = emit or console_emit
        self.safe_mode = safe_mode
        self.brain = Brain(cfg).load()
        self.birth = load_or_create(cfg.state, cfg.model_id)
        self.server = GenServer(self.brain, max_batch=cfg.gen_max_batch)
        self.memory = Memory(cfg.state)
        self.learner = Learner(self.brain, self.memory, cfg)
        self.identity = Identity(cfg, self.memory, self.birth)
        self.news = NewsSense(cfg.state, feeds=cfg.feeds or None)
        self.sleep = SleepPolicy(self.learner, self.memory, cfg, self.identity)
        self.curiosity = Curiosity(self.learner, self.memory, cfg, self.news)
        self.queue = InputQueue()
        self.learning_enabled = cfg.passive_learning and not safe_mode
        self.last_episode: str | None = None
        self.restart = False

        # tool sets ----------------------------------------------------------------
        self._basic = make_builtin_tools(Path(cfg.workspace_dir), cfg.python_timeout, cfg.allow_python,
                                         cfg.allow_shell, Path(cfg.home_dir) if cfg.home_dir else None)
        self._self = make_self_tools(self.learner, self.identity)
        self._sense = make_sense_tools(self.learner, self.news, cfg.sense_passes)
        self.thoughts = ThoughtManager(cfg.state, self.queue, self._thought_harness, self.memory,
                                       reminder_every=cfg.thought_reminder_every,
                                       learn_from_thoughts=cfg.learn_from_thoughts, learner=self.learner,
                                       max_concurrent=cfg.max_thoughts)
        self.thoughts.on_event = lambda ev, t, text="": self.emit(
            "thought", event=ev, id=t.id, status=t.status, goal=t.goal[:140], steps=f"{t.steps}/{t.max_steps}", text=text[:300])
        self._mind = make_main_mind_tools(self.thoughts, self.memory)
        core_names = set(self._basic.names()) | set(self._self.names()) | set(self._sense.names()) | set(self._mind.names())
        self.skills = SkillManager(cfg.state, protected=core_names | {"draft_skill", "install_skill", "list_skills", "read_skill",
                                   "disable_skill", "rollback_skill", "read_incidents", "propose_patch", "focus", "finish"},
                                   memory=self.memory, check_timeout=cfg.skill_check_timeout)
        self._skilltools = make_skill_tools(self.skills, full=True)
        self._skilltools_ro = make_skill_tools(self.skills, full=False)
        os.environ["GROOW_STATE"] = str(Path(cfg.state).resolve())
        seed_home(cfg, self.skills, self.emit)
        if cfg.skills_enabled and not safe_mode:
            r = self.skills.load_all()
            if r["quarantined"]:
                self.emit("log", level="warn", text=f"quarantined skills that failed to load: {r['quarantined']}")
        self.tools = ToolRegistry()
        self.refresh_tools()
        self.skills.on_change = self.refresh_tools
        for r in (self._basic, self._self, self._sense, self._mind, self._skilltools):
            r.on_progress = lambda m: self.emit("log", level="progress", text=m)
        self._Harness, self._Hooks, self._ServedBrain, self._make_thought_tools, self._ToolRegistry = (
            Harness, Hooks, ServedBrain, make_thought_tools, ToolRegistry)

        # the conscious thread's harness: priority 0, streams text events -----------------
        prompt = self.identity.system_prompt()
        if safe_mode:
            prompt = SAFE_MODE_PROMPT.format(incident=json.dumps(incident or {}, ensure_ascii=False)[:3000])
        self.harness = Harness(ServedBrain(self.brain, self.server, 0), self.tools, cfg, prompt,
                               Hooks(on_text=lambda t: self.emit("text", delta=t, req=self._req()),
                                     on_tool_call=lambda n, a: self.emit("tool_call", name=n, args=a, actor="main", req=self._req()),
                                     on_tool_result=lambda n, a, r: self.emit("tool_result", name=n, result=r[:600], actor="main", req=self._req()),
                                     after_turn=self._after_turn), name="main")

    def _req(self):
        """Correlation id of the /ask or /say request the main thought is currently serving."""
        return getattr(getattr(self, "mind", None), "_req", None)

    def refresh_tools(self) -> None:
        """Rebuild the main tool set in place (the harness holds the registry object).
        Safe mode: only repair tools, nothing that learns, senses or thinks."""
        merged = {}
        if self.safe_mode:
            merged.update(self._basic.tools)
            merged.update(self._skilltools.tools)
            for name in ("ask_mentor", "learning_report"):
                if name in self._self.tools:
                    merged[name] = self._self.tools[name]
            merged["recall"] = self._mind.tools["recall"]
        else:
            for r in (self._basic, self._self, self._sense, self._mind, self._skilltools):
                merged.update(r.tools)
            if self.cfg.skills_enabled:
                merged.update(self.skills.registry.tools)
        self.tools.tools.clear()
        self.tools.tools.update(merged)

    # ---- inner thoughts: own harness, restricted tools, priority 2 ---------------------
    def _thought_harness(self, thought):
        reg = self._ToolRegistry()
        reg.include(self._basic)
        reg.include(self._sense)
        reg.include(self._skilltools_ro)
        if self.cfg.skills_enabled and not self.safe_mode:
            reg.include(self.skills.registry)
        for name in ("memorize", "quiz", "play", "list_games", "learning_report"):
            if name in self._self.tools:
                reg.tools[name] = self._self.tools[name]
        reg.tools["recall"] = self._mind.tools["recall"]
        reg.include(self._make_thought_tools(self.thoughts, thought.id))
        actor = f"thought:{thought.id}"
        return self._Harness(self._ServedBrain(self.brain, self.server, 2), reg, self.cfg, thought.history[0]["content"],
                             self._Hooks(on_tool_call=lambda n, a: self.emit("tool_call", name=n, args=a, actor=actor),
                                         on_tool_result=lambda n, a, r: self.emit("tool_result", name=n, result=r[:300], actor=actor)),
                             name=actor)

    # ---- learning after every main turn (runs on the GPU executor) ------------------
    def _after_turn(self, turn) -> None:
        if not self.learning_enabled or not turn.messages:
            return
        info = self.learner.passive(turn.context, turn.messages, turn.tools_used)
        self.last_episode = info["episode"]
        self.emit("learned", loss=round(info["loss"], 4), tokens=info["learnable_tokens"], step=self.brain.meta["steps"],
                  probe=info.get("probe"), req=self._req())
        if self.brain.meta["steps"] % 10 == 0:
            self.brain.save()

    # ---- nights ------------------------------------------------------------------------
    async def maybe_sleep(self, force: bool = False) -> bool:
        why = "requested" if force else self.sleep.should_sleep()
        if not why:
            return False
        paused = await self.thoughts.pause_all()
        self.emit("sleep", phase="start", why=why, thoughts_paused=paused)
        r = await self.server.run_gpu(lambda: self.sleep.sleep(
            on_progress=lambda m: self.emit("sleep", phase="progress", text=m), force=force))
        self.emit("sleep", phase="done", **{k: v for k, v in r.items() if k != "internalize"},
                  identity=r.get("internalize"))
        self.harness.system_prompt = self.identity.system_prompt()
        self.harness.history[0] = {"role": "system", "content": self.harness.system_prompt}
        return True

    def open_questions(self) -> list[dict]:
        p = self.memory.dir / "mentor_inbox.jsonl"
        if not p.exists():
            return []
        qs = [json.loads(l) for l in p.read_text().splitlines() if l.strip()]
        return [q for q in qs if not q.get("answered")]

    def clear_inbox(self) -> int:
        p = self.memory.dir / "mentor_inbox.jsonl"
        if not p.exists():
            return 0
        qs = [json.loads(l) for l in p.read_text().splitlines() if l.strip()]
        n = sum(1 for q in qs if not q.get("answered"))
        for q in qs:
            q["answered"] = True
        p.write_text("".join(json.dumps(q, ensure_ascii=False) + "\n" for q in qs))
        return n


def seed_home(cfg: Config, skills, emit) -> None:
    """First start (or a body upgrade): copy the default recipes into state/recipes (never overwriting
    Groow's own edits) and install the default skills once (Groow may later change or remove them)."""
    src = Path(__file__).parent
    rec = cfg.state / "recipes"
    rec.mkdir(parents=True, exist_ok=True)
    added = [p.name for p in sorted((src / "recipes").glob("*.md")) if not (rec / p.name).exists()
             and shutil.copy(p, rec / p.name)]
    seeded = skills.manifest.setdefault("_seeded", [])
    for p in sorted((src / "default_skills").glob("*.py")):
        if p.stem in seeded:
            continue
        (skills.drafts / p.name).write_text(p.read_text())
        r = skills.install(p.stem)
        seeded.append(p.stem)
        skills._save_manifest()
        emit("log", level="info", text=f"default skill {p.stem}: {'installed' if r.get('ok') else r}")
    if added:
        emit("log", level="info", text=f"recipes added to the home: {added}")


def console_emit(ev: str, **d) -> None:
    """Renders protocol events for one-shot CLI commands."""
    if ev == "text":
        console.print(d["delta"], end="", highlight=False, markup=False)
    elif ev == "tool_call":
        console.print(f"\n   [yellow]⚙ {d['name']}[/yellow]({_short(d['args'])})" + (f" [dim]{d['actor']}[/dim]" if d.get('actor') != 'main' else ''))
    elif ev == "tool_result":
        console.print(f"   [dim]→ {_short(d['result'], 300)}[/dim]")
    elif ev == "learned":
        console.print(f"\n   [dim]↺ learned · loss {d['loss']:.3f} · {d['tokens']} tokens · step {d['step']}[/dim]")
    elif ev == "log":
        console.print(f"   [dim]{d.get('text','')}[/dim]")
    elif ev == "sleep":
        console.print(f"   [dim]sleep {d.get('phase')}: {d.get('text') or d.get('outcome') or ''}[/dim]")
    elif ev == "thought":
        console.print(f"   [dim]· thought {d['id']} {d['event']} {d.get('text','')[:80]}[/dim]")


def _short(x, n: int = 120) -> str:
    s = x if isinstance(x, str) else json.dumps(x, ensure_ascii=False)
    return s if len(s) <= n else s[:n] + "…"


# ====================================================================== slash commands (daemon side)
async def run_command(user: str, app: App) -> bool:
    """Execute a slash command inside the mind loop. Returns True to stop the daemon."""
    cmd, *rest = user.split(maxsplit=1)
    arg = rest[0] if rest else ""
    b, learner, emit = app.brain, app.learner, app.emit
    if cmd in ("/quit", "/exit", "/stop"):
        return True
    if cmd == "/restart":
        app.restart = True
        return True
    if cmd in ("/good", "/bad"):
        if not app.last_episode:
            emit("log", text="nothing to rate yet")
        else:
            r = await app.server.run_gpu(learner.feedback, app.last_episode, 1 if cmd == "/good" else -1)
            emit("log", text=f"{cmd[1:]} noted · losses {[round(x, 3) for x in r['losses']]}")
    elif cmd in ("/sleep", "/consolidate"):
        await app.maybe_sleep(force=True)
    elif cmd == "/sense":
        app.queue.push(3, "idle", app.curiosity.impulse_text())
    elif cmd == "/thoughts":
        emit("log", text=json.dumps({"thoughts": app.thoughts.listing(arg.strip() == "all"), "server": app.server.stats}))
    elif cmd == "/skills":
        emit("log", text=json.dumps(app.skills.listing()))
    elif cmd == "/incidents":
        emit("log", text=json.dumps(app.skills.incidents(int(arg) if arg.strip().isdigit() else 3), default=str))
    elif cmd == "/identity":
        loss = await app.server.run_gpu(app.identity.probe, b)
        emit("log", text=f"identity v{app.identity.versions()} · internalised loss {loss:.3f}\n{app.identity.text()}")
    elif cmd == "/inbox":
        if arg.strip() == "clear":
            emit("log", text=f"inbox cleared ({app.clear_inbox()} answered)")
        emit("inbox", questions=app.open_questions())
    elif cmd == "/learn":
        app.learning_enabled = arg.strip().lower() != "off"
        emit("log", text=f"passive learning {'on' if app.learning_enabled else 'off'}")
    elif cmd == "/reasoning":
        app.cfg.enable_thinking = arg.strip().lower() != "off"
        emit("log", text=f"reasoning mode {'on' if app.cfg.enable_thinking else 'off'}")
    elif cmd == "/tools":
        emit("log", text="\n".join(f"{s['function']['name']}  {s['function']['description'].splitlines()[0][:90]}"
                                    for s in app.tools.schemas()))
    elif cmd == "/stats":
        rep = learner.report()
        rep["tool_usage"] = app.tools.stats()
        rep["generation_server"] = app.server.stats
        emit("log", text=json.dumps(rep, default=str))
    elif cmd == "/reset":
        app.harness.reset()
        emit("log", text="conversation cleared (weights untouched)")
    elif cmd == "/save":
        b.save()
        emit("log", text="saved")
    elif cmd == "/help":
        emit("log", text="/good /bad /sleep /sense /thoughts [all] /skills /incidents /identity /inbox [clear] "
                          "/learn on|off /reasoning on|off /tools /stats /reset /save /restart /quit")
    else:
        emit("log", text=f"unknown command {cmd}; try /help")
    return False


# ====================================================================== commands
def _boot(cfg: Config) -> App:
    with console.status("[dim]waking up the brain...[/dim]"):
        return App(cfg)


def _url(cfg: Config) -> str:
    from .gateway import base_url
    return base_url(cfg)


def _repo_root() -> Path | None:
    """The checkout that holds the body (Dockerfile, docker-compose.yml, birth). Needed for the sandbox."""
    candidates = [Path(os.environ["GROOW_REPO"])] if os.environ.get("GROOW_REPO") else []
    candidates += [Path.cwd(), Path(__file__).resolve().parents[1]]
    for c in candidates:
        c = c.resolve()
        if (c / "docker-compose.yml").exists() and (c / "birth").exists():
            return c
    return None


def _resolve_state(cfg: Config, args) -> None:
    """Where Groow's state lives when it runs on the host: --state, else state/ next to a groow.json in the
    current directory (a checkout), else ~/.groow/state."""
    if getattr(args, "state", None):
        cfg.state_dir = args.state
    elif not Path("groow.json").exists():
        cfg.state_dir = str(Path.home() / ".groow" / "state")
    Path(cfg.state_dir).mkdir(parents=True, exist_ok=True)
    if cfg.workspace_dir == "state/workspace":
        cfg.workspace_dir = str(Path(cfg.state_dir) / "workspace")


def _sandbox_running(root: Path) -> bool:
    import subprocess
    try:
        r = subprocess.run(["docker", "compose", "ps", "-q", "--status", "running", "groow"], cwd=root,
                           capture_output=True, text=True, timeout=20)
        return bool(r.stdout.strip())
    except Exception:
        return False


def cmd_init(cfg: Config, args) -> None:
    import transformers
    transformers.logging.set_verbosity_error()
    from .brain import Brain
    from .birth import load_or_create
    _resolve_state(cfg, args)
    cfg.state.mkdir(parents=True, exist_ok=True)
    if not Path("groow.json").exists() and not (cfg.state.parent / "groow.json").exists():
        cfg.save(cfg.state.parent / "groow.json")
    with console.status(f"[dim]copying {cfg.model_id} into {cfg.state}/base ...[/dim]"):
        Brain(cfg).initialize(force=args.force)
    birth = load_or_create(cfg.state, cfg.model_id)
    app = _boot(cfg)
    r = app.learner.probe()
    console.print(f"[green]born.[/green] {birth.line()} · {app.brain.total_parameters()/1e9:.2f}B parameters, "
                  f"{app.brain.trainable_parameters()/1e6:.1f}M plastic · baseline probe loss {r['mean_loss']:.3f}")
    app.brain.save()


def cmd_start(cfg: Config, args) -> None:
    """Default: Groow runs in its body (Docker sandbox) via the birth script. --nosandbox: on this host,
    as you, with its shell in your home and its state in ~/.groow (or state/ in a checkout)."""
    if not args.nosandbox:
        root = _repo_root()
        if root is None:
            console.print("[red]the sandbox needs the Groow checkout (Dockerfile, docker-compose.yml, birth). "
                          "Run from the repository, set GROOW_REPO, or use --nosandbox.[/red]")
            raise SystemExit(2)
        import shutil as _sh, subprocess
        if not _sh.which("docker"):
            console.print("[red]docker is not installed; use `groow start --nosandbox` to run on this host[/red]")
            raise SystemExit(2)
        console.print(f"[dim]sandbox: {root}/birth[/dim]")
        raise SystemExit(subprocess.call([str(root / "birth"), "start"], cwd=root))
    _resolve_state(cfg, args)
    _start_host(cfg, args)


def _start_host(cfg: Config, args) -> None:
    """Supervisor around the daemon: crash in a skill -> quarantine + restart; elsewhere -> safe mode."""
    import traceback
    from .gateway import Daemon
    from .harness import SkillManager
    try:
        asyncio.run(_ping(cfg))
        console.print(f"[red]a Groow daemon already answers at {_url(cfg)} (use `groow stop`)[/red]")
        return
    except Exception:
        (cfg.state / "groow.url").unlink(missing_ok=True)
    if args.host:
        cfg.api_host = args.host
    if args.port:
        cfg.api_port = args.port
    if not (cfg.state / "birth.json").exists():
        console.print(f"[dim]no birth certificate in {cfg.state}: this is a birth (fetching the base model)[/dim]")
        cmd_init(cfg, argparse.Namespace(force=False, state=cfg.state_dir))
    console.print(f"[dim]host mode: state {cfg.state} · shell home {cfg.home_dir or Path.home()} · running as you, no sandbox[/dim]")
    safe_mode, safe_crashes, incident = args.safe, 0, None
    while True:
        try:
            outcome = asyncio.run(Daemon(cfg, safe_mode=safe_mode, incident=incident, verbose=args.verbose).run())
            if outcome == "restart":
                safe_mode, incident = False, None
                console.print("[dim]restarting in normal mode…[/dim]")
                continue
            return
        except KeyboardInterrupt:
            return
        except Exception:
            tb = traceback.format_exc()
            sk = SkillManager(cfg.state, protected=set())
            incident = sk.record_incident("crash", tb, safe_mode=safe_mode)
            blamed = sk.blame(tb)
            console.print(f"[red]crashed[/red]: {tb.strip().splitlines()[-1][:200]}")
            if blamed and not safe_mode:
                sk.quarantine_after_crash(blamed, tb)
                console.print(f"[yellow]skill {blamed!r} quarantined; restarting[/yellow]")
                continue
            if safe_mode:
                safe_crashes += 1
                if safe_crashes >= 2:
                    console.print("[red]crashed twice in safe mode; giving up. See state/incidents.jsonl and `groow doctor`[/red]")
                    raise SystemExit(1)
            safe_mode = True
            console.print("[yellow]entering safe mode: skills off, learning off, repair tools on[/yellow]")


async def _ping(cfg: Config) -> dict:
    from .gateway import Client
    async with Client(_url(cfg)) as c:
        return await c.hello()


def cmd_status(cfg: Config, args) -> None:
    try:
        h = asyncio.run(_ping(cfg))
    except Exception as e:
        console.print(f"[dim]no daemon at {_url(cfg)} ({type(e).__name__}); start one with `groow start`[/dim]")
        return
    b, s = h["birth"], h["status"]
    console.print(Panel.fit(f"[bold]{b['name']}[/bold] · id {b['id']} · born {b['born_text']} · age {b['age']}\n"
                            f"lineage {b['lineage']} · {b['hardware']} · {_url(cfg)}\n"
                            f"mood {s['mood']} · steps {s['steps']} · nights {s['nights']} · rank {s['rank']} · "
                            f"thoughts running {s['thoughts_running']} · skills {s['skills']} · clients {s['clients']}"
                            + ("\n[red]SAFE MODE[/red]" if s['safe_mode'] else "")))


def cmd_stop(cfg: Config, args) -> None:
    root = _repo_root()
    if root and _sandbox_running(root):
        import subprocess
        raise SystemExit(subprocess.call([str(root / "birth"), "stop"], cwd=root))

    async def go():
        from .gateway import Client
        async with Client(_url(cfg)) as c:
            await c.hello()                               # is anyone there?
            await c.say("/quit")
            try:
                async for ev in c.events(replay=0):       # wait for the goodbye; the stream ends when it dies
                    if ev.get("ev") == "bye":
                        break
            except Exception:
                pass                                      # connection dropped = it is gone
    try:
        asyncio.run(asyncio.wait_for(go(), 600))
        console.print("[dim]groow is asleep (state saved). It finishes the turn it was in first, so this can take a minute.[/dim]")
    except Exception as e:
        console.print(f"[dim]no daemon at {_url(cfg)} ({type(e).__name__})[/dim]")


def cmd_ask(cfg: Config, args) -> None:
    """Single query over HTTP: wait for the turn and print the answer."""
    async def go():
        from .gateway import Client
        async with Client(_url(cfg)) as c:
            return await c.ask(args.text, timeout=args.timeout)
    try:
        r = asyncio.run(go())
    except Exception as e:
        console.print(f"[red]cannot reach the daemon at {_url(cfg)} ({type(e).__name__})[/red]")
        return
    if args.json:
        console.print_json(json.dumps(r, default=str))
    else:
        for e in r.get("events", []):
            if e["ev"] == "tool_call":
                console.print(f"   [yellow]⚙ {e['name']}[/yellow]({_short(e['args'])})")
        console.print(r.get("final") or r.get("error", ""))


def cmd_chat(cfg: Config, args) -> None:
    """Minimal line client: SSE stream in, POST /say out."""
    from .gateway import Client

    async def go():
        c = Client(_url(cfg))
        try:
            hello = await c.hello()
        except Exception as e:
            console.print(f"[red]cannot connect to the daemon at {_url(cfg)} ({type(e).__name__}); run `groow start` first[/red]")
            return
        b = hello["birth"]
        console.print(Panel.fit(f"[bold]{b['name']}[/bold] · age {b['age']} · {hello['model']} · {len(hello['tools'])} tools"
                                + ("\n[red]SAFE MODE[/red]" if hello['safe_mode'] else "")))
        if hello.get("inbox"):
            console.print(f"[yellow]{b['name']} left {len(hello['inbox'])} question(s):[/yellow] "
                          + " | ".join(q["question"] for q in hello["inbox"][-3:]))
        loop = asyncio.get_running_loop()

        async def pump():
            async for ev in c.events(replay=0):
                k = ev["ev"]
                if k == "turn_start":
                    tag = "groow>" if ev["who"] == "user" else f"· {ev['kind']} →"
                    console.print(f"[bold magenta]{tag}[/bold magenta] ", end="")
                elif k == "text":
                    console.print(ev["delta"], end="", highlight=False, markup=False)
                elif k == "turn_end":
                    console.print()
                elif k in ("tool_call", "tool_result", "learned", "log", "sleep", "thought"):
                    console_emit(k, **{a: v for a, v in ev.items() if a not in ("ev", "t")})
                elif k == "inbox":
                    console.print(f"   [yellow]inbox:[/yellow] " + (" | ".join(q["question"] for q in ev["questions"]) or "empty"))
                elif k == "bye":
                    console.print("[dim]daemon went to sleep[/dim]")
                    return

        pump_task = loop.create_task(pump())
        while not pump_task.done():
            line = await loop.run_in_executor(None, sys.stdin.readline)
            if not line:
                break
            line = line.strip()
            if line in ("/quit", "/exit"):
                break
            if line:
                await c.say(line)
        pump_task.cancel()
        await c.close()

    asyncio.run(go())


def cmd_ui(cfg: Config, args) -> None:
    from .ui import run_ui
    run_ui(_url(cfg))


def cmd_doctor(cfg: Config, args) -> None:
    """Static health check without loading the model."""
    from .harness import SkillManager, make_builtin_tools
    core = set(make_builtin_tools(Path(cfg.workspace_dir), cfg.python_timeout, cfg.allow_python).names())
    sk = SkillManager(cfg.state, protected=core)
    ok = True
    for name in sk.installed():
        r = sk.check(sk.dir / f"{name}.py")
        ok &= bool(r.get("ok"))
        console.print(f"   skill [yellow]{name}[/yellow]: {'ok' if r.get('ok') else 'FAIL'} "
                      f"{[t['name'] for t in r.get('tools', [])]} {r.get('errors') or ''}")
    for d, label in ((sk.drafts, "draft"), (sk.quarantine_dir, "quarantined")):
        for p in d.glob("*.py"):
            console.print(f"   {label} [dim]{p.stem}[/dim]")
    for inc in sk.incidents(5):
        console.print(f"   incident [red]{inc['kind']}[/red] {inc.get('skill', '')}: {inc['traceback'].strip().splitlines()[-1][:120]}")
    for must in ("base/config.json", "identity.md", "probes.json", "birth.json"):
        present = (cfg.state / must).exists()
        ok &= present
        console.print(f"   state/{must}: {'ok' if present else 'MISSING'}")
    if (cfg.state / "groow.url").exists():
        console.print(f"   daemon url file present: {(cfg.state / 'groow.url').read_text().strip()}")
    console.print("[green]healthy[/green]" if ok else "[red]problems found[/red]")


# ---- one-shot commands (no daemon) -------------------------------------------------
def _oneshot_guard(cfg: Config, args=None) -> None:
    _resolve_state(cfg, args or argparse.Namespace(state=None))
    if (cfg.state / "groow.url").exists():
        console.print("[yellow]note: a daemon may be running; one-shot commands load a second copy of the model.[/yellow]")


def cmd_memorize(cfg: Config, args) -> None:
    _oneshot_guard(cfg, args); app = _boot(cfg)
    text = Path(args.file).read_text() if args.file else args.text
    r = app.learner.memorize(args.title, text, target_loss=args.target, max_steps=args.max_steps,
                             on_progress=lambda s, l: console.print(f"   step {s+1:3d}  loss {l:.4f}"))
    r.pop("curve"); console.print_json(json.dumps(r)); app.brain.save()


def cmd_quiz(cfg: Config, args) -> None:
    _oneshot_guard(cfg, args); app = _boot(cfg)
    console.print_json(json.dumps(app.learner.quiz(args.question, args.expected)))


def cmd_play(cfg: Config, args) -> None:
    _oneshot_guard(cfg, args); app = _boot(cfg)
    r = app.learner.play(args.game, rounds=args.rounds, episodes=args.episodes, evaluate_n=args.eval_n,
                         on_progress=lambda rec: console.print(
                             f"   round {rec['round']:3d}  decisions {rec['decisions']:4d}  explored {rec['explored']:3d}  "
                             f"mean reward {rec['mean_reward']:+.3f}  pg loss {rec['pg_loss']:+.4f}"))
    r.pop("history"); console.print_json(json.dumps(r)); app.brain.save()


def cmd_probe(cfg: Config, args) -> None:
    _oneshot_guard(cfg, args); console.print_json(json.dumps(_boot(cfg).learner.probe()))


def cmd_stats(cfg: Config, args) -> None:
    _oneshot_guard(cfg, args); console.print_json(json.dumps(_boot(cfg).learner.report(), default=str))


def cmd_consolidate(cfg: Config, args) -> None:
    _oneshot_guard(cfg, args); app = _boot(cfg)
    r = app.brain.consolidate(keep_previous=cfg.keep_previous_base)
    app.memory.log("consolidate", **r, step=app.brain.meta["steps"]); console.print_json(json.dumps(r))


def cmd_sleep(cfg: Config, args) -> None:
    _oneshot_guard(cfg, args); app = _boot(cfg)
    r = app.sleep.sleep(replay_steps=args.replay, force=args.force, on_progress=lambda m: console.print(f"   [dim]{m}[/dim]"))
    console.print_json(json.dumps(r, default=str))


def cmd_rollback(cfg: Config, args) -> None:
    _oneshot_guard(cfg, args); console.print_json(json.dumps(_boot(cfg).sleep.rollback()))


def cmd_sense(cfg: Config, args) -> None:
    _oneshot_guard(cfg, args); app = _boot(cfg)
    if args.pipeline or cfg.curiosity_mode != "agentic":
        r = app.curiosity.tick(args.items, on_progress=lambda m: console.print(f"   [dim]{m}[/dim]"))
    else:
        console.print("[bold magenta]groow>[/bold magenta] ", end="")
        r = asyncio.run(app.curiosity.explore(app.harness, args.items))
        console.print()
    console.print_json(json.dumps(r, default=str)); app.brain.save()


def cmd_identity(cfg: Config, args) -> None:
    _oneshot_guard(cfg, args); app = _boot(cfg)
    console.print(Panel(app.identity.text(), title=f"identity v{app.identity.versions()} · internalised loss "
                        f"{app.identity.probe(app.brain):.3f} · {app.birth.line()}"))


def cmd_grow(cfg: Config, args) -> None:
    _oneshot_guard(cfg, args); app = _boot(cfg)
    r = app.brain.grow_rank(args.rank); app.memory.log("grow", **r, step=app.brain.meta["steps"]); console.print_json(json.dumps(r))


def cmd_tools(cfg: Config, args) -> None:
    from .harness import ToolRegistry, make_builtin_tools
    reg = ToolRegistry(); reg.include(make_builtin_tools(Path(cfg.workspace_dir), cfg.python_timeout, cfg.allow_python))
    console.print_json(json.dumps(reg.schemas()))


def main(argv=None) -> None:
    p = argparse.ArgumentParser(prog="groow", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--config", default=None, help="config file (default: ./groow.json, else ~/.groow/groow.json)")
    p.add_argument("--model", help="override model id (e.g. Qwen/Qwen3-1.7B) before init")
    p.add_argument("--state", help="state directory for host mode (default: state/ in a checkout, else ~/.groow/state)")
    sub = p.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("init"); s.add_argument("--force", action="store_true"); s.set_defaults(fn=cmd_init)
    s = sub.add_parser("start"); s.add_argument("--safe", action="store_true", help="start in safe mode")
    s.add_argument("--nosandbox", action="store_true", help="run on this host as you (default: Docker sandbox via ./birth)")
    s.add_argument("-v", "--verbose", action="store_true", help="print events to stdout")
    s.add_argument("--host"); s.add_argument("--port", type=int); s.set_defaults(fn=cmd_start)
    s = sub.add_parser("ask"); s.add_argument("text"); s.add_argument("--timeout", type=float, default=600)
    s.add_argument("--json", action="store_true"); s.set_defaults(fn=cmd_ask)
    sub.add_parser("stop").set_defaults(fn=cmd_stop)
    sub.add_parser("status").set_defaults(fn=cmd_status)
    sub.add_parser("chat").set_defaults(fn=cmd_chat)
    sub.add_parser("ui").set_defaults(fn=cmd_ui)
    sub.add_parser("doctor").set_defaults(fn=cmd_doctor)
    s = sub.add_parser("memorize"); s.add_argument("--title", required=True)
    s.add_argument("--text"); s.add_argument("--file"); s.add_argument("--target", type=float)
    s.add_argument("--max-steps", type=int); s.set_defaults(fn=cmd_memorize)
    s = sub.add_parser("quiz"); s.add_argument("question"); s.add_argument("--expected"); s.set_defaults(fn=cmd_quiz)
    s = sub.add_parser("play"); s.add_argument("game"); s.add_argument("--rounds", type=int, default=5)
    s.add_argument("--episodes", type=int); s.add_argument("--eval-n", type=int, default=32); s.set_defaults(fn=cmd_play)
    sub.add_parser("probe").set_defaults(fn=cmd_probe)
    sub.add_parser("stats").set_defaults(fn=cmd_stats)
    sub.add_parser("consolidate").set_defaults(fn=cmd_consolidate)
    s = sub.add_parser("sleep"); s.add_argument("--replay", type=int); s.add_argument("--force", action="store_true"); s.set_defaults(fn=cmd_sleep)
    sub.add_parser("rollback").set_defaults(fn=cmd_rollback)
    s = sub.add_parser("sense"); s.add_argument("--items", type=int); s.add_argument("--pipeline", action="store_true"); s.set_defaults(fn=cmd_sense)
    sub.add_parser("identity").set_defaults(fn=cmd_identity)
    s = sub.add_parser("grow"); s.add_argument("--rank", type=int, required=True); s.set_defaults(fn=cmd_grow)
    sub.add_parser("tools").set_defaults(fn=cmd_tools)
    args = p.parse_args(argv)
    cfg_path = args.config or ("groow.json" if Path("groow.json").exists() else str(Path.home() / ".groow" / "groow.json"))
    cfg = Config.load(cfg_path)
    if args.model:
        cfg.model_id = args.model
    args.fn(cfg, args)


if __name__ == "__main__":
    main()
