"""groow: a model that learns by rewriting its own weights.

  groow start                      wake Groow in its body (Docker sandbox; gives birth the first time)
  groow start --nosandbox [-v]     run on this host, as you (state in ~/.groow, or state/ in a checkout)
  groow ui | chat | ask "…" | say  fullscreen UI (WebSocket) | line client (SSE) | one question over HTTP | drop a message
  groow status | stop | doctor     snapshot | sleep | static health check
  groow debug step [--say "…"]     run one pass of the run loop here, printing every event

  operations on the running Groow (also what Groow runs in its own shell):
  groow thoughts | thought read|pause|resume|kill <id> | skill check|install|… <name> | training | stats
  groow identity | inbox [--clear] | incidents | patch … | remind "..." --in 2h | schedule [cancel <id>]
  mentor only: groow learn "q" "a" | quiz "q" [--expected A] | train | hippocampus | sleep | probe | grow --rank N | rollback | consolidate
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
note = Console(stderr=True)      # progress and fallback notes: never mixed into the JSON on stdout

SAFE_MODE_PROMPT = """You are Groow, running in SAFE MODE because the normal session crashed. Skills are unloaded, passive learning and curiosity are off. Your job now is repair, not conversation. With shell: `groow incidents` shows the traceback; skills are files in state/skills (installed) and state/skills/_quarantine; fix one and `groow skill check <file>` then `groow skill install <name>`, or leave it quarantined. If the fault is in the core rather than a skill, `groow patch …` and ask. When you are done, say exactly: REPAIRED. Marlinski, your mentor, is watching.

Incident: {incident}"""


# ====================================================================== the App
class App:
    """The daemon's organs: the brain and its generation server, memory, the journal, the learner,
    the limbic system, the hippocampus and the trainer, the identity, the schedule, the thought
    records and the skills. It runs no conversation: a turn is a process (see groow/turn.py)."""

    def __init__(self, cfg: Config, emit=None, safe_mode: bool = False, incident: dict | None = None):
        import transformers
        transformers.logging.set_verbosity_error()
        transformers.logging.disable_progress_bar()
        from .brain import Brain, GenServer
        from .memory import Memory, Journal
        from .learning import Learner, SleepPolicy, Curiosity, Identity, TrainingSets, Trainer, Hippocampus
        from .limbic import Limbic
        from .senses import NewsSense
        from .mind import InputQueue, ThoughtManager, Schedule
        from .birth import load_or_create
        from .harness import ToolRegistry, SkillManager, make_substrate_tools, make_self_tools, make_main_mind_tools

        self.cfg = cfg
        self.emit = emit or console_emit
        self.safe_mode = safe_mode
        self.daemon = None                      # set by the daemon so ops can spawn processes
        self.brain = Brain(cfg).load()
        self.birth = load_or_create(cfg.state, cfg.model_id)
        self.server = GenServer(self.brain, max_batch=cfg.gen_max_batch)
        self.memory = Memory(cfg.state)
        self.journal = Journal(cfg.state / "main", max_lines=1000)
        self.learner = Learner(self.brain, self.memory, cfg)
        self.sets = TrainingSets(cfg.state / "training")
        self.trainer = Trainer(self.brain, self.memory, self.learner, self.sets, cfg)
        self.limbic = Limbic(cfg.state, judge_kind=cfg.judge, decay_halflife_s=cfg.mood_halflife_s)
        self.hippocampus = Hippocampus(self.brain, self.memory, self.journal, self.sets, cfg, limbic=self.limbic)
        self.hippocampus.person = self.birth.mentor
        self.identity = Identity(cfg, self.memory, self.birth)
        self.news = NewsSense(cfg.state, feeds=cfg.feeds or None)
        self.sleep = SleepPolicy(self.learner, self.memory, cfg, self.identity, trainer=self.trainer,
                                 hippocampus=self.hippocampus)
        self.curiosity = Curiosity(self.learner, self.memory, cfg, self.news)
        self.queue = InputQueue(cfg.state / "mailbox")
        self.schedule = Schedule(cfg.state)
        self.thoughts = ThoughtManager(cfg.state, self.queue, lambda t: None, self.memory,
                                       reminder_every=cfg.thought_reminder_every, learner=self.learner,
                                       max_concurrent=cfg.max_thoughts)
        self.thoughts.on_event = lambda ev, t, text="": self.emit(
            "thought", event=ev, id=t.id, status=t.status, goal=t.goal[:140],
            steps=f"{t.steps}/{t.max_steps}", text=text[:300])
        self.learning_enabled = cfg.passive_learning and not safe_mode
        self.last_episode: str | None = None
        self.restart = False
        os.environ["GROOW_STATE"] = str(Path(cfg.state).resolve())

        home = Path(cfg.home_dir).expanduser() if cfg.home_dir else Path.home()
        self.skills = SkillManager(cfg.state, protected={"shell", "think", "ask", "focus", "finish"},
                                   memory=self.memory, check_timeout=cfg.skill_check_timeout, home=home)
        seed_home(cfg, self.skills, self.emit)
        if cfg.skills_enabled and not safe_mode:
            r = self.skills.load_all()
            if r["quarantined"]:
                self.emit("log", level="warn", text=f"quarantined skills that failed to load: {r['quarantined']}")
        # a registry built only so /hello can say what a turn process will have
        self.tools = ToolRegistry()
        self.tools.include(make_substrate_tools(home, cfg.allow_shell))
        self.tools.include(make_self_tools(cfg.state))
        self.tools.include(make_main_mind_tools(self.thoughts, self.memory))
        if cfg.skills_enabled and not safe_mode:
            self.tools.include(self.skills.registry)

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
        return True

    async def idle_nap(self) -> dict:
        """Nobody is talking: digest the activity log and consume what is pending."""
        if not self.learning_enabled:
            return {"consumed": 0}
        self.hippocampus.digest_activity()
        self.hippocampus.flush()
        if not self.sets.pending(limit=1):
            return {"consumed": 0}
        r = await self.server.run_gpu(lambda: self.trainer.consume(max_samples=self.cfg.idle_nap_max_samples))
        if r.get("consumed"):
            self.emit("log", level="info", text=f"idle nap: {r['consumed']} samples consumed {r.get('sets')}")
        return r

    def open_questions(self) -> list[dict]:
        p = self.memory.dir / "mentor_inbox.jsonl"
        if not p.exists():
            return []
        return [q for q in (json.loads(l) for l in p.read_text().splitlines() if l.strip()) if not q.get("answered")]

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
    """First start (or a body upgrade): default recipes into state/recipes (never overwriting Groow's edits),
    default skills installed once."""
    src = Path(__file__).parent
    rec = cfg.state / "recipes"
    rec.mkdir(parents=True, exist_ok=True)
    added = [p.name for p in sorted((src / "recipes").glob("*.md")) if not (rec / p.name).exists()
             and shutil.copy(p, rec / p.name)]
    import hashlib
    seeded = skills.manifest.setdefault("_seeded", [])
    shas = skills.manifest.setdefault("_seeded_sha", {})
    for p in sorted((src / "default_skills").glob("*.py")):
        shipped = hashlib.sha1(p.read_bytes()).hexdigest()[:10]
        installed = skills.dir / f"{p.name}"
        if p.stem in seeded:
            if shas.get(p.stem) == shipped or not installed.exists():
                continue                      # unchanged, or Groow removed it: leave it alone
            if p.stem in shas and hashlib.sha1(installed.read_bytes()).hexdigest()[:10] != shas[p.stem]:
                continue                      # Groow edited it: its version wins
            what = "upgraded"
        else:
            what = "installed"
        (skills.drafts / p.name).write_text(p.read_text())
        r = skills.install(p.stem)
        if p.stem not in seeded:
            seeded.append(p.stem)
        shas[p.stem] = shipped
        skills._save_manifest()
        emit("log", level="info", text=f"default skill {p.stem}: {what if r.get('ok') else r}")
    if added:
        emit("log", level="info", text=f"recipes added to the home: {added}")


def console_emit(ev: str, **d) -> None:
    if ev == "text":
        console.print(d["delta"], end="", highlight=False, markup=False)
    elif ev == "tool_call":
        console.print(f"\n   [yellow]⚙ {d['name']}[/yellow]({_short(d['args'])})" + (f" [dim]{d['actor']}[/dim]" if d.get('actor') != 'main' else ''))
    elif ev == "tool_result":
        console.print(f"   [dim]→ {_short(d['result'], 300)}[/dim]")
    elif ev == "learned":
        console.print(f"\n   [dim]↺ learned · loss {d['loss']:.3f} · {d['tokens']} tokens · step {d['step']}[/dim]")
    elif ev in ("log", "sleep", "thought"):
        console.print(f"   [dim]{d.get('text') or d.get('phase') or d.get('event') or ''}[/dim]")


def _short(x, n: int = 120) -> str:
    s = x if isinstance(x, str) else json.dumps(x, ensure_ascii=False)
    return s if len(s) <= n else s[:n] + "…"


# ====================================================================== slash commands (daemon side)
async def run_command(user: str, app: App) -> bool:
    from .ops import run_op
    cmd, *rest = user.split(maxsplit=1)
    arg = (rest[0] if rest else "").strip()
    emit = app.emit
    if cmd in ("/quit", "/exit", "/stop"):
        return True
    if cmd == "/restart":
        app.restart = True
        return True
    if cmd in ("/good", "/bad"):
        r = await run_op(app, "feedback", {"value": 1 if cmd == "/good" else -1})
        emit("log", text=f"{cmd[1:]} noted · {r.get('losses', r.get('error'))}")
    elif cmd == "/sense":
        app.queue.push(3, "idle", app.curiosity.impulse_text())
    elif cmd == "/learn":
        app.learning_enabled = arg.lower() != "off"
        emit("log", text=f"passive learning {'on' if app.learning_enabled else 'off'}")
    elif cmd == "/reasoning":
        app.cfg.enable_thinking = arg.lower() != "off"
        emit("log", text=f"reasoning mode {'on' if app.cfg.enable_thinking else 'off'}")
    elif cmd == "/tools":
        emit("log", text="\n".join(f"{s['function']['name']}  {s['function']['description'].splitlines()[0][:90]}" for s in app.tools.schemas()))
    elif cmd == "/reset":
        emit("log", text="the window is rebuilt from the journal each turn; nothing to clear")
    elif cmd == "/inbox":
        r = await run_op(app, "inbox", {"clear": arg == "clear"})
        emit("inbox", questions=r.get("questions", []))
    elif cmd[1:] in ("sleep", "probe", "stats", "thoughts", "thought", "skill", "identity", "incidents", "train", "training", "hippocampus"):
        args = {}
        if cmd == "/thoughts":
            args = {"all": arg == "all"}
        elif cmd == "/thought" and arg:
            parts = arg.split()
            args = {"action": parts[0], "id": parts[1] if len(parts) > 1 else ""}
        elif cmd == "/skill" and arg:
            parts = arg.split()
            args = {"action": parts[0], "name": parts[1] if len(parts) > 1 else ""}
        r = await run_op(app, cmd[1:], args)
        emit("log", text=json.dumps(r, default=str)[:4000])
    elif cmd == "/help":
        emit("log", text="/good /bad /sleep /sense /train /training /hippocampus /thoughts [all] /thought <read|pause|resume|kill> <id> "
                          "/skill <list|read|check|install|disable|rollback> <name> /identity /inbox [clear] /incidents /stats "
                          "/learn on|off /reasoning on|off /tools /reset /restart /quit")
    else:
        emit("log", text=f"unknown command {cmd}; try /help")
    return False


# ====================================================================== helpers
def _url(cfg: Config) -> str:
    from .gateway import base_url
    return base_url(cfg)


def _repo_root() -> Path | None:
    candidates = [Path(os.environ["GROOW_REPO"])] if os.environ.get("GROOW_REPO") else []
    candidates += [Path.cwd(), Path(__file__).resolve().parents[1]]
    for c in candidates:
        c = c.resolve()
        if (c / "docker-compose.yml").exists() and (c / "birth").exists():
            return c
    return None


def _resolve_state(cfg: Config, args) -> None:
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


def _boot(cfg: Config) -> App:
    with console.status("[dim]waking up the brain...[/dim]"):
        return App(cfg)


async def _ping(cfg: Config) -> dict:
    from .gateway import Client
    async with Client(_url(cfg)) as c:
        return await c.hello()


def _daemon_op(cfg: Config, op: str, **args):
    """Run an op on the daemon if one answers; None otherwise."""
    async def go():
        import aiohttp
        async with aiohttp.ClientSession(timeout=aiohttp.ClientTimeout(total=None, connect=3)) as s:
            async with s.post(_url(cfg) + "/op", json={"op": op, "args": args}) as r:
                return await r.json()
    try:
        return asyncio.run(go())
    except Exception:
        return None


def _print(r) -> None:
    console.print_json(json.dumps(r, default=str, ensure_ascii=False))


# ====================================================================== lifecycle commands
def cmd_init(cfg: Config, args) -> None:
    import transformers
    transformers.logging.set_verbosity_error()
    from .brain import Brain
    from .birth import load_or_create
    _resolve_state(cfg, args)
    cfg.state.mkdir(parents=True, exist_ok=True)
    if not Path("groow.json").exists() and not (cfg.state.parent / "groow.json").exists():
        cfg.save(cfg.state.parent / "groow.json")
    if not (cfg.state / "base").exists() or args.force:
        console.print(f"birth: fetching the base model {cfg.model_id} from Hugging Face (~8 GB for a 4B model, once)…")
    Brain(cfg).initialize(force=args.force)
    console.print(f"birth: weights written to {cfg.state}/base (the working copy)")
    birth = load_or_create(cfg.state, cfg.model_id)
    console.print(f"birth: certificate written: id {birth.id}, born {birth.born_text()}")
    console.print("birth: loading the weights into the GPU and measuring baseline probes…")
    app = _boot(cfg)
    r = app.learner.probe()
    console.print(f"birth: [green]born.[/green] {birth.line()} · {app.brain.total_parameters()/1e9:.2f}B parameters, "
                  f"{app.brain.trainable_parameters()/1e6:.1f}M plastic · baseline probe loss {r['mean_loss']:.3f}")
    app.brain.save()


def cmd_start(cfg: Config, args) -> None:
    if not args.nosandbox:
        root = _repo_root()
        if root is None:
            console.print("[red]the sandbox needs the Groow checkout (Dockerfile, docker-compose.yml, birth). "
                          "Run from the repository, set GROOW_REPO, or use --nosandbox.[/red]")
            raise SystemExit(2)
        import subprocess
        if not shutil.which("docker"):
            console.print("[red]docker is not installed; use `groow start --nosandbox` to run on this host[/red]")
            raise SystemExit(2)
        console.print(f"[dim]sandbox: {root}/birth[/dim]")
        raise SystemExit(subprocess.call([str(root / "birth"), "start"], cwd=root))
    _resolve_state(cfg, args)
    _start_host(cfg, args)


def _start_host(cfg: Config, args) -> None:
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


def cmd_status(cfg: Config, args) -> None:
    try:
        h = asyncio.run(_ping(cfg))
    except Exception as e:
        console.print(f"[dim]no daemon at {_url(cfg)} ({type(e).__name__}); start one with `groow start`[/dim]")
        return
    b, s = h["birth"], h["status"]
    body = s.get("body", "host")
    where = f"[green]sandbox[/green] (body: read-only image, home {s.get('home')})" if body == "sandbox" \
        else f"[yellow]host, no sandbox[/yellow] (running as you, shell in {s.get('home')}, state {s.get('state')})"
    console.print(Panel.fit(f"[bold]{b['name']}[/bold] · id {b['id']} · born {b['born_text']} · age {b['age']}\n{where}\n"
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
            await c.hello()
            await c.say("/quit")
            try:
                async for ev in c.events(replay=0):
                    if ev.get("ev") == "bye":
                        break
            except Exception:
                pass
    try:
        asyncio.run(asyncio.wait_for(go(), 600))
        console.print("[dim]groow is asleep (state saved)[/dim]")
    except Exception as e:
        console.print(f"[dim]no daemon at {_url(cfg)} ({type(e).__name__})[/dim]")


def cmd_ask(cfg: Config, args) -> None:
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
        r.setdefault("final", ""); _print(r)
    else:
        for e in r.get("events", []):
            if e["ev"] == "tool_call":
                console.print(f"   [yellow]⚙ {e['name']}[/yellow]({_short(e['args'])})")
        console.print(r.get("final") or f"[red]{r.get('error', 'no answer')}[/red]")


def cmd_chat(cfg: Config, args) -> None:
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


def cmd_say(cfg: Config, args) -> None:
    """Send a message. From outside: over HTTP if Groow is awake, else dropped in the mailbox for when it
    wakes. From inside the body (Groow itself): a note to self, delivered to its own mailbox later."""
    _resolve_state(cfg, args)
    from .mind import Mailbox, Priority
    if os.environ.get("GROOW_SELF"):
        sig = Mailbox(cfg.state / "mailbox").push(Priority.FOCUS, "note", args.text)
        _print({"ok": True, "note_to_self": args.text[:200], "delivered": "after this turn"})
        return
    async def go():
        from .gateway import Client
        async with Client(_url(cfg)) as c:
            return await c.say(args.text)
    try:
        _print(asyncio.run(go()))
    except Exception:
        sig = Mailbox(cfg.state / "mailbox").push(Priority.USER, "user", args.text)
        console.print(f"[dim]groow is asleep; left in the mailbox: {sig.path.name}[/dim]")


def cmd_ui(cfg: Config, args) -> None:
    from .ui import run_ui
    run_ui(_url(cfg))


def cmd_turn(cfg: Config, args) -> None:
    """Internal: run one turn in this process (the daemon fires this). Events go to stdout."""
    from .turn import main_turn
    _resolve_state(cfg, args)
    main_turn(cfg, json.loads(args.signal))


def cmd_think(cfg: Config, args) -> None:
    """Internal: run one inner thought to its end in this process."""
    from .turn import main_thought
    _resolve_state(cfg, args)
    main_thought(cfg, args.id)


def cmd_debug(cfg: Config, args) -> None:
    """One pass of the run loop by hand: take the next signal (or the one you give) and run a turn
    here, printing every event. The daemon must be awake, because it owns the model."""
    from .turn import main_turn
    import asyncio as _a
    _resolve_state(cfg, args)
    if args.what != "step":
        console.print("[dim]usage: groow debug step [--say TEXT] [--kind user|alarm|idle|reminder][/dim]")
        return
    if args.say:
        signal = {"kind": args.kind or "user", "text": args.say, "meta": {}}
    else:
        from .mind import Mailbox
        sig = _a.run(Mailbox(cfg.state / "mailbox").pop(timeout=0.2))
        if sig is None:
            console.print("[dim]nothing in the mailbox; use --say \"…\" to make one up[/dim]")
            return
        signal = {"kind": sig.kind, "text": sig.text, "meta": sig.meta}
        Mailbox(cfg.state / "mailbox").ack(sig)
    console.print(f"[dim]one pass on: {signal['kind']} · {signal['text'][:80]}[/dim]")
    main_turn(cfg, signal)


def cmd_doctor(cfg: Config, args) -> None:
    from .harness import SkillManager
    _resolve_state(cfg, args)
    sk = SkillManager(cfg.state, protected=set())
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
    main = cfg.state / "main"
    if main.exists():
        files = sorted(main.glob("*.jsonl"))
        console.print(f"   journal: {len(files)} file(s), latest {files[-1].name if files else '-'}")
    console.print("[green]healthy[/green]" if ok else "[red]problems found[/red]")


# ====================================================================== operations (daemon first, then local)
def _op_command(op: str, build_args, local=None, needs_model: bool = True):
    """A CLI command that runs an op on the daemon if it answers, else `local(cfg, args)` if given.
    `needs_model` is False for operations that are only files (alarms, the inbox, listings)."""
    def cmd(cfg: Config, args) -> None:
        _resolve_state(cfg, args)
        kw = build_args(args)
        r = _daemon_op(cfg, op, **kw)
        if r is not None:
            _print(r)
            return
        if local is None:
            note.print(f"[dim]no daemon at {_url(cfg)}; this needs a running Groow (`groow start`)[/dim]")
            return
        note.print("[dim]groow is asleep; working on its files[/dim]" if not needs_model
                   else "[dim]no daemon; running locally (loads its own copy of the model)[/dim]")
        _print(local(cfg, args))
    return cmd


def _local_probe(cfg, args):
    return _boot(cfg).learner.probe()


def _local_stats(cfg, args):
    return _boot(cfg).learner.report()


def _local_sleep(cfg, args):
    app = _boot(cfg)
    return app.sleep.sleep(replay_steps=args.replay, force=args.force, on_progress=lambda m: console.print(f"   [dim]{m}[/dim]"))


def _local_identity(cfg, args):
    app = _boot(cfg)
    return {"identity": app.identity.text(), "versions": app.identity.versions(),
            "internalised_loss": round(app.identity.probe(app.brain), 3), "birth": app.birth.card()}


def _local_learn(cfg, args):
    app = _boot(cfg)
    r = app.learner.learn(args.question, args.answer, args.source or "", target_loss=args.target); app.brain.save(); return r


def _local_quiz(cfg, args):
    return _boot(cfg).learner.quiz(args.question, args.expected)


def _files_thoughts(cfg, args):
    from .mind import ThoughtManager
    from .memory import Memory
    tm = ThoughtManager(cfg.state, None, lambda t: None, Memory(cfg.state))
    return {"thoughts": tm.listing(args.all)}


def _files_skill(cfg, args):
    from .harness import SkillManager
    sk = SkillManager(cfg.state, protected=set())
    from .ops import op_skill
    class _A:  # minimal stand-in for the app
        skills = sk
    return op_skill(_A, action=args.action, name=args.name or "", path=args.path or "")


def _files_inbox(cfg, args):
    p = cfg.state / "mentor_inbox.jsonl"
    qs = [json.loads(l) for l in p.read_text().splitlines() if l.strip()] if p.exists() else []
    if args.clear:
        for q in qs:
            q["answered"] = True
        p.write_text("".join(json.dumps(q, ensure_ascii=False) + "\n" for q in qs))
    return {"questions": [q for q in qs if not q.get("answered")]}


def _files_incidents(cfg, args):
    from .harness import SkillManager
    return {"incidents": SkillManager(cfg.state, protected=set()).incidents(args.last)}


cmd_train = _op_command("train", lambda a: {"urgent_only": a.urgent, "max_samples": a.max}, lambda cfg, a: _boot(cfg).trainer.consume(max_samples=a.max))
cmd_training = _op_command("training", lambda a: {}, lambda cfg, a: {"sets": __import__("groow.learning", fromlist=["TrainingSets"]).TrainingSets(cfg.state / "training").counts()}, needs_model=False)
cmd_hippocampus = _op_command("hippocampus", lambda a: {}, lambda cfg, a: _boot(cfg).hippocampus.run())
cmd_probe = _op_command("probe", lambda a: {}, _local_probe)
cmd_stats = _op_command("stats", lambda a: {}, _local_stats)
cmd_sleep = _op_command("sleep", lambda a: {"force": True}, _local_sleep)
cmd_identity = _op_command("identity", lambda a: {}, _local_identity)
cmd_learn = _op_command("learn", lambda a: {"question": a.question, "answer": a.answer, "source": a.source or "", "target_loss": a.target or 0.0}, _local_learn)
cmd_quiz = _op_command("quiz", lambda a: {"question": a.question, "expected": a.expected or ""}, _local_quiz)
cmd_thoughts = _op_command("thoughts", lambda a: {"all": a.all}, _files_thoughts, needs_model=False)
cmd_thought = _op_command("thought", lambda a: {"action": a.action, "id": a.id, "last": a.last})
cmd_skill = _op_command("skill", lambda a: {"action": a.action, "name": a.name or "", "path": a.path or ""}, _files_skill, needs_model=False)
cmd_inbox = _op_command("inbox", lambda a: {"clear": a.clear}, _files_inbox, needs_model=False)
cmd_remind = _op_command("remind", lambda a: {"text": a.text, "when": a.when or "", "every": a.every or "",
                                              "by": "groow" if os.environ.get("GROOW_SELF") else "mentor"},
                         lambda cfg, a: __import__("groow.mind", fromlist=["Schedule"]).Schedule(cfg.state).add(
                             a.text, when=a.when or "", every=a.every or "",
                             by="groow" if os.environ.get("GROOW_SELF") else "mentor"),
                         needs_model=False)
cmd_schedule = _op_command("schedule", lambda a: {"action": a.action, "id": a.id or ""},
                           lambda cfg, a: (__import__("groow.mind", fromlist=["Schedule"]).Schedule(cfg.state).cancel(a.id)
                                           if a.action == "cancel" else
                                           __import__("groow.mind", fromlist=["Schedule"]).Schedule(cfg.state).listing()),
                           needs_model=False)
cmd_incidents = _op_command("incidents", lambda a: {"last": a.last}, _files_incidents, needs_model=False)
cmd_patch = _op_command("patch", lambda a: {"path": a.path, "description": a.description, "patch": Path(a.file).read_text()})


def cmd_rollback(cfg: Config, args) -> None:
    _resolve_state(cfg, args); _print(_boot(cfg).sleep.rollback())


def cmd_grow(cfg: Config, args) -> None:
    _resolve_state(cfg, args); app = _boot(cfg)
    r = app.brain.grow_rank(args.rank); app.memory.log("grow", **r, step=app.brain.meta["steps"]); _print(r)


def cmd_consolidate(cfg: Config, args) -> None:
    _resolve_state(cfg, args); app = _boot(cfg)
    r = app.brain.consolidate(keep_previous=cfg.keep_previous_base); app.memory.log("consolidate", **r, step=app.brain.meta["steps"]); _print(r)


# what Groow may not do to itself from inside its body (GROOW_SELF=1 there)
MENTOR_ONLY = {
    "init": "it would overwrite your base weights", "stop": "you would only reboot",
    "grow": "your capacity is Marlinski's decision", "rollback": "undoing a night is Marlinski's decision",
    "consolidate": "nights happen on their own", "sleep": "nights happen on their own", "probe": "probes run on their own",
    "ask": "asking yourself a question would wait on yourself; use a note (groow say) or think",
    "learn": "what you learn is decided by how things felt and by your mentor, not typed in",
    "quiz": "measuring yourself is the mentor's audit",
    "train": "naps and nights train you on their own", "hippocampus": "it runs at night on its own",
    "ui": "no terminal here", "chat": "you are the one being talked to",
}


# ====================================================================== argparse
def main(argv=None) -> None:
    p = argparse.ArgumentParser(prog="groow", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--config", default=None, help="config file (default: ./groow.json, else ~/.groow/groow.json)")
    p.add_argument("--model", help="override model id before init")
    p.add_argument("--state", help="state directory for host mode (default: state/ in a checkout, else ~/.groow/state)")
    sub = p.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("init"); s.add_argument("--force", action="store_true"); s.set_defaults(fn=cmd_init)
    s = sub.add_parser("start"); s.add_argument("--safe", action="store_true"); s.add_argument("--nosandbox", action="store_true")
    s.add_argument("-v", "--verbose", action="store_true"); s.add_argument("--host"); s.add_argument("--port", type=int); s.set_defaults(fn=cmd_start)
    sub.add_parser("stop").set_defaults(fn=cmd_stop)
    sub.add_parser("status").set_defaults(fn=cmd_status)
    sub.add_parser("chat").set_defaults(fn=cmd_chat)
    sub.add_parser("ui").set_defaults(fn=cmd_ui)
    s = sub.add_parser("say"); s.add_argument("text"); s.set_defaults(fn=cmd_say)
    sub.add_parser("doctor").set_defaults(fn=cmd_doctor)
    s = sub.add_parser("turn"); s.add_argument("--signal", required=True); s.set_defaults(fn=cmd_turn)
    s = sub.add_parser("think"); s.add_argument("id"); s.set_defaults(fn=cmd_think)
    s = sub.add_parser("debug"); s.add_argument("what", nargs="?", default="step"); s.add_argument("--say")
    s.add_argument("--kind"); s.set_defaults(fn=cmd_debug)
    s = sub.add_parser("ask"); s.add_argument("text"); s.add_argument("--timeout", type=float, default=600); s.add_argument("--json", action="store_true"); s.set_defaults(fn=cmd_ask)
    # operations
    s = sub.add_parser("train"); s.add_argument("--urgent", action="store_true"); s.add_argument("--max", type=int, default=64); s.set_defaults(fn=cmd_train)
    sub.add_parser("training").set_defaults(fn=cmd_training)
    sub.add_parser("hippocampus").set_defaults(fn=cmd_hippocampus)
    s = sub.add_parser("thoughts"); s.add_argument("--all", action="store_true"); s.set_defaults(fn=cmd_thoughts)
    s = sub.add_parser("thought"); s.add_argument("action", choices=["read", "pause", "resume", "kill"]); s.add_argument("id"); s.add_argument("--last", type=int, default=10); s.set_defaults(fn=cmd_thought)
    s = sub.add_parser("skill"); s.add_argument("action", choices=["list", "read", "check", "install", "disable", "rollback"])
    s.add_argument("name", nargs="?", help="skill name, or a path to a .py file for check/install"); s.add_argument("--path"); s.set_defaults(fn=_skill_dispatch)
    s = sub.add_parser("learn"); s.add_argument("question"); s.add_argument("answer"); s.add_argument("--source", default=""); s.add_argument("--target", type=float); s.set_defaults(fn=cmd_learn)
    s = sub.add_parser("quiz"); s.add_argument("question"); s.add_argument("--expected"); s.set_defaults(fn=cmd_quiz)
    sub.add_parser("stats").set_defaults(fn=cmd_stats)
    sub.add_parser("identity").set_defaults(fn=cmd_identity)
    s = sub.add_parser("inbox"); s.add_argument("--clear", action="store_true"); s.set_defaults(fn=cmd_inbox)
    s = sub.add_parser("remind", help="set an alarm for yourself: --in 30m | --at 18:30 | --every 2h | --every 'daily 06:30'")
    s.add_argument("text"); s.add_argument("--in", dest="when"); s.add_argument("--at", dest="at")
    s.add_argument("--every"); s.set_defaults(fn=lambda cfg, a: (setattr(a, "when", a.when or (f"at {a.at}" if a.at else "")), cmd_remind(cfg, a))[1])
    s = sub.add_parser("schedule"); s.add_argument("action", nargs="?", default="list", choices=["list", "cancel"])
    s.add_argument("id", nargs="?"); s.set_defaults(fn=cmd_schedule)
    s = sub.add_parser("incidents"); s.add_argument("--last", type=int, default=3); s.set_defaults(fn=cmd_incidents)
    s = sub.add_parser("patch"); s.add_argument("path"); s.add_argument("description"); s.add_argument("file"); s.set_defaults(fn=cmd_patch)
    # mentor only
    s = sub.add_parser("sleep"); s.add_argument("--replay", type=int); s.add_argument("--force", action="store_true"); s.set_defaults(fn=cmd_sleep)
    sub.add_parser("probe").set_defaults(fn=cmd_probe)
    s = sub.add_parser("grow"); s.add_argument("--rank", type=int, required=True); s.set_defaults(fn=cmd_grow)
    sub.add_parser("rollback").set_defaults(fn=cmd_rollback)
    sub.add_parser("consolidate").set_defaults(fn=cmd_consolidate)
    args = p.parse_args(argv)
    if os.environ.get("GROOW_SELF") and args.cmd in MENTOR_ONLY:
        console.print(f"[yellow]`groow {args.cmd}` is for your mentor, not for you: {MENTOR_ONLY[args.cmd]}[/yellow]")
        raise SystemExit(3)
    cfg_path = args.config or ("groow.json" if Path("groow.json").exists() else str(Path.home() / ".groow" / "groow.json"))
    cfg = Config.load(cfg_path)
    if args.model:
        cfg.model_id = args.model
    args.fn(cfg, args)


def _skill_dispatch(cfg: Config, args) -> None:
    """`groow skill check path/to/file.py` or `groow skill install name`."""
    if args.name and args.name.endswith(".py"):
        args.path, args.name = args.name, None
    cmd_skill(cfg, args)


if __name__ == "__main__":
    main()
