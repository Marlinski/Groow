"""groow: a model that learns by rewriting its own weights.

  groow init                       download the base model into state/
  groow chat                       talk to it (it learns from every turn)
  groow memorize --title T --text "..." | --file lesson.txt
  groow quiz "question" [--expected "answer"]
  groow play tictactoe --rounds 10
  groow sense [--items N]          read the news and learn from it (curiosity pass)
  groow sleep [--replay N]         replay, internalise identity, probe, merge overlay into base
  groow rollback                   undo the last night
  groow identity                   show the self-description and how internalised it is
  groow probe | stats | consolidate | grow --rank 64 | tools
"""
from __future__ import annotations

import argparse
import asyncio
import json
import sys
from pathlib import Path

from rich.console import Console
from rich.panel import Panel

from .config import Config

console = Console()

SAFE_MODE_PROMPT = """You are Groow, running in SAFE MODE because the normal session crashed. Skills are unloaded, passive learning and curiosity are off. Your job now is repair, not conversation: call read_incidents to see the traceback, read_skill / list_skills to inspect what you wrote, fix a skill with draft_skill (tests must pass) and install_skill, or disable_skill if it cannot be saved. If the fault is in the core harness rather than a skill, write a propose_patch and ask_mentor. When you are done, say exactly: REPAIRED. Marlinski, your mentor, is watching.

Incident: {incident}"""


class App:
    """Wires the organs together: brain + generation server, memory, learner, identity, tools,
    the main harness (the conscious thread), inner thoughts, sleep and curiosity."""

    def __init__(self, cfg: Config, safe_mode: bool = False, incident: dict | None = None):
        import transformers
        transformers.logging.set_verbosity_error()
        transformers.logging.disable_progress_bar()
        from .brain import Brain, GenServer, ServedBrain
        from .memory import Memory
        from .learning import Learner, SleepPolicy, Curiosity, Identity
        from .senses import NewsSense
        from .mind import InputQueue, ThoughtManager, Priority
        from .harness import (Harness, Hooks, ToolRegistry, make_builtin_tools, make_self_tools, make_sense_tools,
                              make_main_mind_tools, make_thought_tools, SkillManager, make_skill_tools)

        self.cfg = cfg
        with console.status("[dim]waking up the brain...[/dim]"):
            self.brain = Brain(cfg).load()
        self.server = GenServer(self.brain, max_batch=cfg.gen_max_batch)
        self.memory = Memory(cfg.state)
        self.learner = Learner(self.brain, self.memory, cfg)
        self.identity = Identity(cfg, self.memory)
        self.news = NewsSense(cfg.state, feeds=cfg.feeds or None)
        self.sleep = SleepPolicy(self.learner, self.memory, cfg, self.identity)
        self.curiosity = Curiosity(self.learner, self.memory, cfg, self.news)
        self.queue = InputQueue()
        self.learning_enabled = cfg.passive_learning
        self.last_episode: str | None = None

        # tool sets ----------------------------------------------------------------
        self._basic = make_builtin_tools(Path(cfg.workspace_dir), cfg.python_timeout, cfg.allow_python)
        self._self = make_self_tools(self.learner, self.identity)
        self._sense = make_sense_tools(self.learner, self.news, cfg.sense_passes)
        self.thoughts = ThoughtManager(cfg.state, self.queue, self._thought_harness, self.memory,
                                       reminder_every=cfg.thought_reminder_every,
                                       learn_from_thoughts=cfg.learn_from_thoughts, learner=self.learner,
                                       max_concurrent=cfg.max_thoughts)
        self._mind = make_main_mind_tools(self.thoughts, self.memory)
        core_names = set(self._basic.names()) | set(self._self.names()) | set(self._sense.names()) | set(self._mind.names())
        self.skills = SkillManager(cfg.state, protected=core_names | {"draft_skill", "install_skill", "list_skills", "read_skill",
                                   "disable_skill", "rollback_skill", "read_incidents", "propose_patch", "focus", "finish"},
                                   memory=self.memory, check_timeout=cfg.skill_check_timeout)
        self._skilltools = make_skill_tools(self.skills, full=True)
        self._skilltools_ro = make_skill_tools(self.skills, full=False)
        self.safe_mode = safe_mode
        if cfg.skills_enabled and not safe_mode:
            r = self.skills.load_all()
            if r["quarantined"]:
                console.print(f"   [red]quarantined skills that failed to load: {r['quarantined']}[/red]")
        self.tools = ToolRegistry()
        self.refresh_tools()
        self.skills.on_change = self.refresh_tools       # only once the registry exists
        for r in (self._basic, self._self, self._sense, self._mind, self._skilltools):
            r.on_progress = lambda m: console.print(f"   [dim]{m}[/dim]")
        self._Harness, self._Hooks, self._ServedBrain, self._make_thought_tools, self._ToolRegistry = (
            Harness, Hooks, ServedBrain, make_thought_tools, ToolRegistry)

        # the conscious thread's harness: priority 0, streams to the console -----------
        prompt = self.identity.system_prompt()
        if safe_mode:
            self.learning_enabled = False
            prompt = SAFE_MODE_PROMPT.format(incident=json.dumps(incident or {}, ensure_ascii=False)[:3000])
        self.harness = Harness(ServedBrain(self.brain, self.server, 0), self.tools, cfg, prompt,
                               Hooks(on_text=lambda t: console.print(t, end="", highlight=False, markup=False),
                                     on_tool_result=lambda n, a, r: console.print(
                                         f"\n   [yellow]⚙ {n}[/yellow]({_short(a)}) → [dim]{_short(r, 300)}[/dim]"),
                                     after_turn=self._after_turn), name="main")

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

    # ---- inner thoughts: own harness, restricted tools, priority 2, quiet ------------
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
        return self._Harness(self._ServedBrain(self.brain, self.server, 2), reg, self.cfg, thought.history[0]["content"],
                             self._Hooks(on_tool_result=lambda n, a, r: console.print(
                                 f"   [dim]· thought {thought.id}: {n}({_short(a, 60)})[/dim]")),
                             name=f"thought-{thought.id}")

    # ---- learning after every main turn (runs on the GPU executor) ------------------
    def _after_turn(self, turn) -> None:
        if not self.learning_enabled or not turn.messages:
            return
        info = self.learner.passive(turn.context, turn.messages, turn.tools_used)
        self.last_episode = info["episode"]
        line = f"↺ learned · loss {info['loss']:.3f} · {info['learnable_tokens']} tokens · step {self.brain.meta['steps']}"
        if info.get("probe"):
            line += f" · probe {info['probe']['mean_loss']:.3f} (birth {info['probe']['baseline_mean']:.3f})"
        console.print(f"\n   [dim]{line}[/dim]")
        if self.brain.meta["steps"] % 10 == 0:
            self.brain.save()

    # ---- nights ------------------------------------------------------------------------
    async def maybe_sleep(self, force: bool = False) -> bool:
        why = "requested" if force else self.sleep.should_sleep()
        if not why:
            return False
        paused = await self.thoughts.pause_all()
        console.print(f"   [dim]… a night is due ({why}); {paused} thought(s) paused; sleeping[/dim]")
        r = await self.server.run_gpu(lambda: self.sleep.sleep(on_progress=lambda m: console.print(f"   [dim]{m}[/dim]"),
                                                               force=force))
        console.print(f"   [green]slept[/green]: {r['outcome']} · replay {r['replay_steps']} steps · "
                      f"probe {r['probe_before']:.3f}→{r['probe_after']:.3f}"
                      + (f" · identity {r['internalize']['identity_loss_before']}→{r['internalize']['identity_loss_after']}"
                         if r.get('internalize') else ""))
        self.harness.system_prompt = self.identity.system_prompt()
        self.harness.history[0] = {"role": "system", "content": self.harness.system_prompt}
        return True

    def show_inbox(self) -> None:
        p = self.memory.dir / "mentor_inbox.jsonl"
        if not p.exists():
            return
        qs = [json.loads(l) for l in p.read_text().splitlines() if l.strip()]
        open_qs = [q for q in qs if not q.get("answered")]
        if open_qs:
            console.print(f"[yellow]Groow left {len(open_qs)} question(s) for you:[/yellow]")
            for q in open_qs[-5:]:
                console.print(f"   • {q['question']}" + (f"  [dim]({q['context'][:80]})[/dim]" if q.get('context') else ""))
            console.print("[dim]answer in the chat; /inbox clear marks them answered[/dim]")


def _short(x, n: int = 120) -> str:
    s = x if isinstance(x, str) else json.dumps(x, ensure_ascii=False)
    return s if len(s) <= n else s[:n] + "…"


def cmd_init(cfg: Config, args) -> None:
    import transformers
    transformers.logging.set_verbosity_error()
    from .brain import Brain
    cfg.state.mkdir(parents=True, exist_ok=True)
    if not Path("groow.json").exists():
        cfg.save()
    with console.status(f"[dim]copying {cfg.model_id} into {cfg.state}/base ...[/dim]"):
        Brain(cfg).initialize(force=args.force)
    app = App(cfg)
    r = app.learner.probe()
    console.print(f"[green]born.[/green] {app.brain.total_parameters()/1e9:.2f}B parameters, "
                  f"{app.brain.trainable_parameters()/1e6:.1f}M plastic. baseline probe loss {r['mean_loss']:.3f}")
    app.brain.save()


def cmd_chat(cfg: Config, args) -> None:
    """Supervisor: run the mind; on a crash, blame a skill and quarantine it, or fall back to safe mode.
    Two crashes in safe mode end the session with a report for the mentor."""
    import traceback
    from .harness import SkillManager
    safe_mode, safe_crashes, incident = args.safe, 0, None
    while True:
        try:
            app = App(cfg, safe_mode=safe_mode, incident=incident)
            if args.no_learn:
                app.learning_enabled = False
            outcome = asyncio.run(_chat(app))
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
                    console.print("[red]crashed twice in safe mode; giving up. Incident log: state/incidents.jsonl[/red]")
                    raise SystemExit(1)
            safe_mode = True
            console.print("[yellow]entering safe mode: skills off, learning off, repair tools on[/yellow]")


async def _chat(app: App) -> None:
    from .mind import Mind
    cfg, b = app.cfg, app.brain
    console.print(Panel.fit(
        f"[bold]Groow[/bold] · {cfg.model_id} · step {b.meta['steps']} · {b.meta['consolidations']} nights "
        f"· rank {b.meta['rank']} · {len(app.tools.names())} tools · identity v{app.identity.versions()} · "
        f"{len(app.thoughts.listing(False))} thought(s) carried over\n"
        "[dim]/good /bad  rate the last answer   /sleep   /sense   /thoughts   /skills   /incidents   /identity   /inbox\n"
        "/learn on|off   /reasoning on|off   /tools   /stats   /reset   /restart   /quit[/dim]"
        + ("\n[red]SAFE MODE[/red]" if app.safe_mode else "")))
    app.show_inbox()
    loop = asyncio.get_running_loop()
    app.queue.bind(loop)
    app.thoughts.bind(loop)
    idle_s = cfg.sense_idle_minutes * 60 if (cfg.curiosity and cfg.sense_idle_minutes > 0) else 0
    mind = Mind(app.harness, app.queue, app.thoughts, on_idle_text=app.curiosity.impulse_text, idle_seconds=idle_s,
                maybe_sleep=app.maybe_sleep, say=lambda t: console.print(f"[bold magenta]{t}[/bold magenta]", end="")
                if t.startswith("groow") else console.print(f"[dim]{t}[/dim]", end=""),
                run_command=lambda line: _command(line, app))
    app.mind = mind
    if app.thoughts.listing(False) and not app.safe_mode:
        app.queue.push(3, "reminder", "you carried paused thoughts over from your last session: "
                       + "; ".join(f"{t['id']} ({t['status']}): {t['goal'][:60]}" for t in app.thoughts.listing(False)))

    async def read_stdin():
        loop = asyncio.get_running_loop()
        while mind.alive:
            line = await loop.run_in_executor(None, sys.stdin.readline)
            if not line:                       # EOF
                mind.push_user("/quit")
                return
            if line.strip():
                mind.push_user(line.strip())

    reader = asyncio.get_running_loop().create_task(read_stdin())
    console.print("[bold cyan]you>[/bold cyan] ", end="")
    app.restart = False
    try:
        await mind.run()
        return "restart" if app.restart else "quit"
    finally:
        reader.cancel()
        for t in app.thoughts.running():
            app.thoughts.pause(t.id)
        await app.thoughts.wait_idle(10)
        await app.server.stop()
        b.save()
        console.print("[dim]saved.[/dim]")


async def _command(user: str, app: App) -> bool:
    cmd, *rest = user.split(maxsplit=1)
    arg = rest[0] if rest else ""
    b, learner = app.brain, app.learner
    if cmd in ("/quit", "/exit"):
        return True
    if cmd == "/restart":
        app.restart = True
        return True
    if cmd == "/skills":
        console.print_json(json.dumps(app.skills.listing()))
    elif cmd == "/incidents":
        console.print_json(json.dumps(app.skills.incidents(int(arg) if arg.strip().isdigit() else 3), default=str))
    elif cmd in ("/good", "/bad"):
        if not app.last_episode:
            console.print("[dim]nothing to rate yet[/dim]")
        else:
            r = await app.server.run_gpu(learner.feedback, app.last_episode, 1 if cmd == "/good" else -1)
            console.print(f"   [dim]{cmd[1:]} noted · losses {[round(x, 3) for x in r['losses']]}[/dim]")
    elif cmd in ("/sleep", "/consolidate"):
        await app.maybe_sleep(force=True)
    elif cmd == "/sense":
        app.queue.push(3, "idle", app.curiosity.impulse_text())
    elif cmd == "/thoughts":
        for t in app.thoughts.listing(arg.strip() == "all"):
            console.print(f"   [yellow]{t['id']}[/yellow] {t['status']:7s} {t['steps']:>6s}  {t['goal'][:80]}"
                          + (f"  [dim]{t['summary'][:80]}[/dim]" if t['summary'] else ""))
        console.print(f"   [dim]generation server: {app.server.stats}[/dim]")
    elif cmd == "/identity":
        loss = await app.server.run_gpu(app.identity.probe, b)
        console.print(Panel(app.identity.text(), title=f"identity v{app.identity.versions()} · "
                            f"internalised loss {loss:.3f} (lower = more in the weights)"))
    elif cmd == "/inbox":
        p = app.memory.dir / "mentor_inbox.jsonl"
        if arg.strip() == "clear" and p.exists():
            qs = [json.loads(l) for l in p.read_text().splitlines() if l.strip()]
            for q in qs:
                q["answered"] = True
            p.write_text("".join(json.dumps(q, ensure_ascii=False) + "\n" for q in qs))
            console.print("   [dim]inbox cleared[/dim]")
        else:
            app.show_inbox()
    elif cmd == "/learn":
        app.learning_enabled = arg.strip().lower() != "off"
        console.print(f"   passive learning {'on' if app.learning_enabled else 'off'}")
    elif cmd == "/reasoning":
        app.cfg.enable_thinking = arg.strip().lower() != "off"
        console.print(f"   reasoning mode {'on' if app.cfg.enable_thinking else 'off'}")
    elif cmd == "/tools":
        for s_ in app.tools.schemas():
            f = s_["function"]
            console.print(f"   [yellow]{f['name']}[/yellow]  [dim]{f['description'].splitlines()[0][:100]}[/dim]")
    elif cmd == "/stats":
        rep = learner.report()
        rep["tool_usage"] = app.tools.stats()
        rep["generation_server"] = app.server.stats
        rep["thoughts"] = app.thoughts.listing(True)
        console.print_json(json.dumps(rep, default=str))
    elif cmd == "/reset":
        app.harness.reset()
        console.print("   [dim]conversation cleared (weights untouched)[/dim]")
    elif cmd == "/save":
        b.save()
        console.print("   [dim]saved[/dim]")
    else:
        console.print("[dim]unknown command[/dim]")
    console.print("[bold cyan]you>[/bold cyan] ", end="")
    return False


def cmd_memorize(cfg: Config, args) -> None:
    app = App(cfg)
    text = Path(args.file).read_text() if args.file else args.text
    r = app.learner.memorize(args.title, text, target_loss=args.target, max_steps=args.max_steps,
                             on_progress=lambda s, l: console.print(f"   step {s+1:3d}  loss {l:.4f}"))
    r.pop("curve")
    console.print_json(json.dumps(r))
    app.brain.save()


def cmd_quiz(cfg: Config, args) -> None:
    app = App(cfg)
    console.print_json(json.dumps(app.learner.quiz(args.question, args.expected)))


def cmd_play(cfg: Config, args) -> None:
    app = App(cfg)
    r = app.learner.play(args.game, rounds=args.rounds, episodes=args.episodes, evaluate_n=args.eval_n,
                         on_progress=lambda rec: console.print(
                             f"   round {rec['round']:3d}  decisions {rec['decisions']:4d}  explored {rec['explored']:3d}  "
                             f"mean reward {rec['mean_reward']:+.3f}  pg loss {rec['pg_loss']:+.4f}"))
    r.pop("history")
    console.print_json(json.dumps(r))
    app.brain.save()


def cmd_probe(cfg: Config, args) -> None:
    console.print_json(json.dumps(App(cfg).learner.probe()))


def cmd_stats(cfg: Config, args) -> None:
    console.print_json(json.dumps(App(cfg).learner.report(), default=str))


def cmd_consolidate(cfg: Config, args) -> None:
    app = App(cfg)
    r = app.brain.consolidate(keep_previous=cfg.keep_previous_base)
    app.memory.log("consolidate", **r, step=app.brain.meta["steps"])
    console.print_json(json.dumps(r))


def cmd_sleep(cfg: Config, args) -> None:
    app = App(cfg)
    r = app.sleep.sleep(replay_steps=args.replay, force=args.force,
                        on_progress=lambda m: console.print(f"   [dim]{m}[/dim]"))
    console.print_json(json.dumps(r, default=str))


def cmd_rollback(cfg: Config, args) -> None:
    app = App(cfg)
    console.print_json(json.dumps(app.sleep.rollback()))


def cmd_sense(cfg: Config, args) -> None:
    app = App(cfg)
    if args.pipeline or cfg.curiosity_mode != "agentic":
        r = app.curiosity.tick(args.items, on_progress=lambda m: console.print(f"   [dim]{m}[/dim]"))
    else:
        console.print("[bold magenta]groow>[/bold magenta] ", end="")
        r = asyncio.run(app.curiosity.explore(app.harness, args.items, on_progress=lambda m: console.print(f"   [dim]{m}[/dim]")))
        console.print()
    console.print_json(json.dumps(r, default=str))
    app.brain.save()


def cmd_identity(cfg: Config, args) -> None:
    app = App(cfg)
    console.print(Panel(app.identity.text(), title=f"identity v{app.identity.versions()} · internalised loss "
                        f"{app.identity.probe(app.brain):.3f} (lower = more in the weights)"))


def cmd_grow(cfg: Config, args) -> None:
    app = App(cfg)
    r = app.brain.grow_rank(args.rank)
    app.memory.log("grow", **r, step=app.brain.meta["steps"])
    console.print_json(json.dumps(r))


def cmd_doctor(cfg: Config, args) -> None:
    """Static health check without loading the model: skills re-checked in sandboxes, incidents, state integrity."""
    from .harness import SkillManager, ToolRegistry, make_builtin_tools
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
    for must in ("base/config.json", "identity.md", "probes.json"):
        present = (cfg.state / must).exists()
        ok &= present
        console.print(f"   state/{must}: {'ok' if present else 'MISSING'}")
    console.print("[green]healthy[/green]" if ok else "[red]problems found[/red]")


def cmd_tools(cfg: Config, args) -> None:
    """List tool schemas without loading the model."""
    from .harness import ToolRegistry, make_builtin_tools
    reg = ToolRegistry()
    reg.include(make_builtin_tools(Path(cfg.workspace_dir), cfg.python_timeout, cfg.allow_python))
    console.print("[dim]basic tools (self-tools need a loaded brain; see `groow chat` then /tools):[/dim]")
    console.print_json(json.dumps(reg.schemas()))


def main(argv=None) -> None:
    p = argparse.ArgumentParser(prog="groow", description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    p.add_argument("--config", default="groow.json")
    p.add_argument("--model", help="override model id (e.g. Qwen/Qwen3-1.7B) before init")
    sub = p.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("init"); s.add_argument("--force", action="store_true"); s.set_defaults(fn=cmd_init)
    s = sub.add_parser("chat"); s.add_argument("--no-learn", action="store_true")
    s.add_argument("--safe", action="store_true", help="start in safe mode (skills off, repair tools on)"); s.set_defaults(fn=cmd_chat)
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
    s = sub.add_parser("sleep"); s.add_argument("--replay", type=int); s.add_argument("--force", action="store_true")
    s.set_defaults(fn=cmd_sleep)
    sub.add_parser("rollback").set_defaults(fn=cmd_rollback)
    s = sub.add_parser("sense"); s.add_argument("--items", type=int); s.add_argument("--pipeline", action="store_true")
    s.set_defaults(fn=cmd_sense)
    sub.add_parser("identity").set_defaults(fn=cmd_identity)
    sub.add_parser("tools").set_defaults(fn=cmd_tools)
    s = sub.add_parser("grow"); s.add_argument("--rank", type=int, required=True); s.set_defaults(fn=cmd_grow)
    args = p.parse_args(argv)
    cfg = Config.load(args.config)
    if args.model:
        cfg.model_id = args.model
    args.fn(cfg, args)


if __name__ == "__main__":
    main()
