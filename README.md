# Groow

Groow is a small language model whose weights are **not frozen**. Every
conversation turn is a gradient step on its own parameters, and it has tools to
learn on purpose: memorise a lesson until it can recite it, quiz itself, play
games whose rules reward or punish it, sleep (merge recent learning into its
long-term weights), and grow its own capacity. Everything persists to disk, so
the Groow you talk to tomorrow is not the one you talked to today.

Runs on one GPU (built and tested on a Tesla V100S 32 GB) on top of
**Qwen3-4B-Instruct-2507** (dense 4B, 262k native context, Apache-2.0).

## Quick start

```bash
git clone git@github.com:Marlinski/Groow.git && cd Groow
uv venv --python 3.12 .venv && . .venv/bin/activate && uv pip install -e .   # the CLI (UI, chat, ask, status)

groow start      # wakes Groow in its body (Docker). First time: builds the body, creates ./home, gives birth.
groow ui         # fullscreen UI; also groow chat, groow ask "…", curl localhost:7373/status
groow stop       # sleep. ./birth is the same thing as a plain script (start|stop|logs|shell|status|rebuild).
```

The **body** is a read-only Docker image (CUDA, Python, the runtime); the
**home** is `./home`, a volume owned by you, the only writable place in
Groow's world. Groow is an unprivileged user there: no `apt`, no `sudo`, but a
shell, Nix and `uv` to install anything locally, and everything it is or grows
(weights, memory, identity, skills, workspace, packages) survives restarts,
rebuilds and moves. One directory is Groow. The birth script and the body's
entrypoint are the only startup code, both outside Groow's reach: no
certificate in the home means birth, otherwise it just wakes up.

Without Docker, or on purpose:

```bash
groow start --nosandbox   # runs on this host, as you: its shell in your home, its state in ~/.groow
                          # (or in state/ when run from a checkout with a groow.json). Needs the cu126 torch wheels:
uv pip install --index-url https://download.pytorch.org/whl/cu126 torch
```

There is no sandbox in that mode: `run_shell` is your shell. Fine for
development, not for leaving it alone overnight.

Groow is a **daemon**. One process owns the GPU and the state; clients speak
HTTP: `POST /ask` for a single query, `GET /events` (Server-Sent Events) for
the run loop, `WS /ws` for interactive UIs (see `docs/gateway.html`, or
`curl -N localhost:7373/events`). The UI shows the conversation, inner thoughts, the
identity card with its live age, a status line, and a small creature whose
animation follows Groow's mood.

Inside the UI or the line client:

| you type | what happens |
| --- | --- |
| any message | Groow answers, calling tools when useful, then takes one gradient step on the exchange |
| `/good` `/bad` | rate the last answer: replay it with extra weight, or push it down (unlikelihood) |
| `/sleep` | a night: replay the day, internalise the identity, probe for drift, merge the overlay into the base (also automatic every `sleep_every_steps`) |
| `/sense` | a curiosity pass now: Groow reads the news through its tools and learns sourced facts (also automatic after `sense_idle_minutes` of silence) |
| `/identity` `/inbox [clear]` | its self-description and how internalised it is; questions it left for its mentor |
| `/thoughts [all]` `/thought read|pause|resume|kill <id>` `/skill …` `/incidents` `/news` `/play <game>` | the same operations Groow runs as `groow …` in its shell |
| `/learn off` `/reasoning on` | pause passive learning; enable Qwen3 thinking mode |
| `/tools` `/stats` `/restart` `/quit` | list tools, learning report, clear the conversation, rebuild the session, stop the daemon |

Scripts can skip the UI entirely: `groow ask "…"`, or `curl -X POST localhost:7373/ask -d '{"text":"…"}'`.

One-shot commands on the host (no daemon running; they load their own copy of the model; `--state` picks the state directory):

```bash
groow memorize --title "Loire" --text "The Loire is the longest river in France."
groow quiz "How long is the Loire?" --expected "It is the longest river in France."
groow train | training   # consume pending training samples now | what is pending per set
groow hippocampus        # extract facts from the journal into the training set now
groow learn "q" "a" --source S   # teach a fact directly (urgent, drilled at once); groow quiz "q" --expected A
groow sleep --replay 30  # a night now; --force merges even if probes drifted
groow rollback           # undo the last night (state/base.prev)
groow identity           # self-description + internalisation loss
groow doctor             # static health check: skills re-checked in sandboxes, incidents, state
groow chat --safe        # start in safe mode
groow probe              # drift on 8 fixed questions vs. birth
groow stats              # learning report
groow consolidate        # bare merge, no replay
groow grow --rank 64     # more plasticity, function-preserving
groow tools              # basic tool schemas
```

Settings live in `groow.json` (created by `init`): model id, LoRA rank, learning
rate, per-role loss weights, rehearsal count, exploration rate, the prompt
budget (`max_seq_len`, 32k) and the training sample length (`train_max_len`,
4k: each learning step backpropagates through at most this much of the
conversation, taking the tail), and whether the model may run Python or define
its own games.

## Mental model

Groow is an organism with five organs, each a directory in this repository. The
full tour with diagrams is `docs/anatomy.html`.

```
   person ──▶ senses: shell · think · ask ──▶ logs (journal, activity log)
                  ▲                                  │
                  │                                  ▼
            brain: fp16 base              limbic: free sensors + frozen judge
            + plastic LoRA overlay                   │ valence
                  ▲                                  ▼
                  └──── trainer ◀──── hippocampus: logs → training sets
                     (nap, night)
```

**The body.** A read-only container; a home (`./home`) that is the whole of
Groow and survives everything. Unprivileged: no `apt`, no `sudo`, no way to
touch its own runtime. It installs what it needs locally with Nix or `uv`.

**The senses.** Three tools and nothing else: `shell` (its home, its files, the
commands its skills install, the `groow …` commands that reach its own daemon),
`think` (an inner thought, concurrent, with its own history), `ask` (the
mentor's inbox). Everything a sense does is journalled to `state/main/` as it
happens.

**The limbic system.** Nothing in the thinking part writes its own reward, so
there is no tool to rate a turn and a skill cannot state its own score. Free
sensors read the transcript: a command that failed, a call that timed out, the
same call twice, a turn with no answer, a fix after a failure, the person
having to say it again. A small **frozen judge** covers the ambiguous part:
[Laya](https://huggingface.co/convaiinnovations/laya) (421M, Apache-2.0, ~35 ms
on CPU), a System One model in the [Jev](https://typesafe.ai/) mould, answers
"how did the person react?" and "did this command do anything?" as calibrated
probabilities. Valence is the signed gap. Approval arrives with the *next*
message, so a turn is held and learned from one turn late. Pain and pleasure
accumulate with a half-life, and that mood is what the creature in the UI shows.

**The brain.** A frozen fp16 base (Qwen3-4B-Instruct-2507) plus a plastic LoRA
overlay, the only thing that moves. A generation server batches the main
thought and its inner thoughts and preempts background work when you speak. At
night the overlay merges into the base exactly.

**A turn is a process.** The daemon is small: it owns the GPU, the clock and
the files, and holds no conversation. When a signal arrives (a message, an
alarm, an inner thought reporting back, the idle impulse) it fires
`groow turn`: a short-lived process that rebuilds the conversation from the
journal, loads the skills, runs one exchange through the harness, prints what
happened as JSON lines, and exits. Generation goes back to the daemon over
HTTP, the only place the model lives. The process keeps no state and starts in
about 0.2 s, because it never imports torch. An inner thought is the same thing
with a restricted toolset and no way to speak: `groow think <id>`, one process
per thought, several at once. A lockfile makes the single conscious thread a
fact rather than a habit, and `groow debug step [--say "…"]` runs one pass by
hand with every event printed.

**The hippocampus.** Reads the logs and the valence, writes training sets; the
trainer is the only thing that produces gradients. A nap after every turn (the
exchange weighted by how it felt, every tool call credited), idle naps for what
skills logged, and a night for the facts stated during the day, then replay,
identity distillation, drift probe, merge. Because credit lands on tool calls,
getting better at the shell or at an API discovered this morning is the same
mechanism as learning a fact.

## Repository map

```
groow/
  config.py            Config dataclass ⇄ groow.json
  brain/               the neural substrate
    model.py           Brain: load, generate, generate_batch, sft_step, pg_step,
                       consolidate, grow_rank, save/load; fp16 base + fp32 LoRA
    chatfmt.py         render conversations with the chat template; per-role loss masks
  limbic/              how it felt
    sensors.py         free signals from the transcript (errors, timeouts, repeats, recovery, restatement)
    judge.py           the frozen judge: Laya (default), a GLiClass scorer, or none
    limbic.py          Limbic: valence per turn, credit per tool call, pain/pleasure, drives
  memory/
    episodic.py        Memory: episodes, lessons, learning log, drift probes, recall
    journal.py         Journal: append-only rotating trace (state/main/, one file per day, 1000 lines)
  learning/
    learner.py         Learner: passive, feedback, quiz, probe, report; learn for the mentor
    trainingset.py     TrainingSets: state/training/<set>.jsonl, cursor of what was consumed
    trainer.py         Trainer: the one place gradients come from (SFT and grouped policy-gradient steps)
    hippocampus.py     Hippocampus: logs -> training sets (turns, tool-call credit, activity log, night facts)
    sleep.py           SleepPolicy: replay, internalise, drift check, merge, rollback, when to sleep
    curiosity.py       Curiosity: idle behaviour (agentic news reading, pipeline fallback)
    identity.py        Identity: seed, self-edit, context distillation into weights
  senses/
    news.py            NewsSense: RSS/Atom -> dated, sourced items; remembers what it has seen
  turn.py              one turn, one process: rebuild from files, run one exchange, exit
  remote.py            how a turn process talks to the daemon (generation, ops, events)
  mind/                the scheduler and the records it keeps
    clock.py           Schedule: alarms and periodic tasks (state/schedule.json)
    lock.py            Conscious: one conscious turn at a time (state/conscious.lock)
    inbox.py           Inbox: the mentor's attention as a budget, with expiry and outcomes
    signals.py         Priority, Signal, Mailbox (the input queue on disk)
    thoughts.py        Thought, ThoughtManager: concurrent coroutine run-loops with own traces
    mind.py            Mind: the scheduler loop; frames signals into the main conversation
  harness/             everything between a user message and a finished turn
    registry.py        ToolRegistry: Python function → JSON schema, safe dispatch
    builtins.py        the substrate: shell (bash in the home)
    selftools.py       ask
    sensetools.py      news headlines for `news` and the curiosity pipeline
    mindtools.py       think (main); focus, finish (inner thoughts)
    skills.py          SkillManager: draft → sandbox check → install → rollback / quarantine; incidents; patches
    skillcheck.py      the subprocess checker (torch-free) a draft must pass
    loop.py            Harness (async): generate → parse tool calls → execute → loop; Hooks
  brain/server.py      GenServer: completions-style request queue, batching, main-thought preemption
  gateway/             the daemon and its protocol
    protocol.py        event shapes; HTTP routes, SSE framing, WebSocket messages
    daemon.py          Daemon (aiohttp): owns App + Mind, /ask /say /command /status /events /ws, mood
    client.py          Client: hello / status / ask / say, SSE and WebSocket streams
  ui/                  the fullscreen terminal UI (Textual)
    app.py             conversation, inner thoughts, identity card, status bar
    creature.py        the sprout: animation frames per mood
  birth.py             the birth certificate (state/birth.json, written once, read-only)
  recipes/             notes for Groow (home, installing, shell, skills, learning, mind, mentor), seeded into state/recipes
  default_skills/      installed on first start: recipes, web, news, clock (remind/schedule), tictactoe, arithmetic
  ops.py               the operations table (play, sleep, probe, news, thoughts, skill, …) used by /op, slash commands and the CLI
  cli.py               App wiring + commands (init, start, ui, chat, ask, status, stop, doctor, operations)
birth                  host script: create the home, build the body, wake Groow (idempotent)
docker/entrypoint.sh   the body waking up: Nix into the home, birth if no certificate, then the command
state/                 runtime, created by init (gitignored); inside the container it is /home/groow/state
  base/  base.prev/    consolidated weights (HF format), and last night's for rollback
  plastic/             current overlay + optimizer state
  identity.md          the self-description; mentor_inbox.jsonl: questions for you
  birth.json           id, birth time, lineage, body, mentor: facts Groow cannot change
  groow.url groow.pid  the daemon's URL and pid while it runs
  thoughts/            inner thought traces (*.json), paused across sessions
  skills/              Groow's own tools (_drafts, _versions, _quarantine, manifest.json)
  recipes/             its notes, seeded from the body, editable by Groow
  incidents.jsonl      crashes and failed loads with tracebacks; patches/: proposed core changes
  senses/              news items seen and learned
  main/                the conversation journal: every message, one JSONL file per day, rotated
  mailbox/             the input queue on disk (new/, cur/)
  schedule.json        alarms Groow (or you) set: when, what, how often
  conscious.lock       the pid of the turn process holding the conscious thread, while one runs
  log/activity.jsonl   what skills log when they play in bulk; read by the hippocampus
  limbic/              valence.jsonl (how each turn felt), state.json (mood), held.json (turns awaiting a reaction)
  training/            training sets prepared by the hippocampus, waiting for the trainer (<set>.jsonl, cursor.json)
  episodes.jsonl lessons.jsonl learning_log.jsonl probes.json
  workspace/           the only directory file tools may touch
  games/               invented games (*.py)
```

Dependency direction is one way: `harness` and `learning` depend on `brain`,
`memory` and `senses`; `mind` depends on `harness`; `brain` depends on nothing
but torch/transformers/peft; `cli` wires everything. Detailed mechanics with
diagrams are in `docs/` (open `docs/index.html`). The harness is deliberately independent of learning: it
knows messages, tools, budgets and hooks, and the learner subscribes to the
`after_turn` hook. That is also the seam through which Groow could later edit
its own harness: it is a few hundred lines of plain Python with no framework
underneath.

### Adding a tool

```python
@registry.tool(group="basic")
def word_count(text: str) -> dict:
    """Count words in a text.

    Args:
        text: the text to count
    """
    return {"words": len(text.split())}
```

The schema comes from the signature and docstring. Errors are returned to the
model as JSON, never raised into the loop.

### Adding a game

Subclass `Game` and `Episode` in `groow/games/` (about 60 lines, see
`arithmetic.py`), register it in `groow/games/__init__.py`. Or let Groow write
one with `invent_game` after setting `"allow_invented_games": true`.

## Hardware notes

- V100 is fp16 only. Base weights are fp16, overlay master weights fp32, mixed
  precision with a gradient scaler, gradient checkpointing. Peak memory for
  the 4B with rank 32 is around 10 GB; generation is the bottleneck, not training.
- No FlashAttention: it needs Ampere (sm_80) or newer and the V100 is sm_70.
  PyTorch falls back to the memory-efficient SDPA kernel, which keeps attention
  memory linear in sequence length but is slower on long prompts. That, not the
  model's 262k window, is why the prompt budget defaults to 32k.
- Docker: the GPU is passed with a CDI device (`nvidia.com/gpu=all`). After a
  driver upgrade the spec goes stale; regenerate it with
  `sudo nvidia-ctk cdi generate --output=/var/run/cdi/nvidia.yaml`. The
  container runs as your uid (`./birth` passes it), so `./home` is simply yours.
- No quantisation by design: only bitsandbytes NF4/int8 run on Volta, both are
  slower than fp16 here, and merging the overlay into a quantised base is lossy.
  Memory is not the constraint at 4B; quantise only to try a 14B+ base.
- CUDA 13 dropped Volta: install torch from the `cu126` index.
- Other bases: `groow --model Qwen/Qwen3-1.7B init` for a faster, smaller
  brain, `Qwen/Qwen3-8B` (16 GB fp16) for a bigger one that still trains without
  quantisation. Qwen3.5 small models (hybrid linear attention, vision tower,
  bf16 kernels) are not a good fit for this card yet.

## What to expect

- Memorising a sentence takes ~15 steps. Common words stick first; a rare
  proper noun may still come out wrong at the default target loss of 0.15.
  The intended fix is the loop Groow can run itself: memorize → quiz → memorize.
- Tic-tac-toe self-play (`tictactoe play`, then a nap) went from 100 % illegal
  moves to 0 % illegal and a positive score against a random player in 12 rounds
  on the 1.7B. Two-digit arithmetic went from 31 % to 92 % in 6 rounds. Part of
  that is learning the answer format.
- Passive learning trains on Groow's own answers, which entrenches mistakes as
  well as successes. `/bad`, the rehearsal policy and the probes exist for that.

## Roadmap: toward a dynamic model

1. ~~Automatic sleep with replay and rollback~~ done. Next: a drift alarm that triggers a night early.
2. ~~Idle curiosity~~ done as news sensing. Next: more senses (a folder it watches, a calendar, a camera) and letting it choose feeds.
3. Weaning: drop `identity_in_prompt` automatically once the internalisation loss is low, and measure personality stability across nights.
4. Net2Net MLP widening and identity-initialised new layers as tools.
5. Vision: frozen image encoder + zero-initialised projector, trained on rule-scored visual drills.
6. Several overlays with a router (skill slots).
7. ~~Groow proposing patches to its own harness~~ done as `propose_patch`. Next: apply proposals to a copy in a container, run `groow doctor` and a scripted conversation, promote if green.
