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
| `/thoughts [all]` `/skills` `/incidents` | inner thoughts; skills; recent incidents |
| `/learn off` `/reasoning on` | pause passive learning; enable Qwen3 thinking mode |
| `/tools` `/stats` `/reset` `/restart` `/quit` | list tools, learning report, clear the conversation, rebuild the session, stop the daemon |

Scripts can skip the UI entirely: `groow ask "…"`, or `curl -X POST localhost:7373/ask -d '{"text":"…"}'`.

One-shot commands on the host (no daemon running; they load their own copy of the model; `--state` picks the state directory):

```bash
groow memorize --title "Loire" --text "The Loire is the longest river in France."
groow quiz "How long is the Loire?" --expected "It is the longest river in France."
groow play tictactoe --rounds 20
groow play arithmetic --rounds 10
groow sense --items 6    # curiosity pass (add --pipeline for the fixed drill instead of tools)
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

```
   you ──▶ ┌────────────── harness ──────────────┐ ──▶ answer
           │ generate → <tool_call> → run tool →  │
           │ <tool_response> → generate → …       │
           └──────────────┬───────────────────────┘
                          │ after every turn
                          ▼
              ┌──────────────────────┐   rehearsal   ┌───────────────┐
              │ learner.passive()    │◀──────────────│ episodic      │
              │ 1 SFT step, weights  │── store turn ─▶│ memory (jsonl)│
              │ by role + 2 replays  │               └───────────────┘
              └──────────┬───────────┘
                         ▼
   ┌─────────────────────────────────────────────────────────┐
   │ brain = frozen fp16 base  +  plastic LoRA overlay (fp32) │
   │ sft_step · pg_step · consolidate (merge) · grow_rank     │
   └─────────────────────────────────────────────────────────┘
```

**Two speeds of memory.** The *overlay* (LoRA rank 32 on every linear layer,
about 66M parameters for the 4B) is trained continuously and cheaply. The
*base* (4B parameters, fp16) changes only at consolidation, when the overlay
is merged into it exactly (a LoRA delta folds into the dense weight with no
change in function) and a blank overlay is opened. Until then, deleting
`state/plastic/` is a complete undo.

**Passive learning.** After each turn the exchange is rendered with the model's
own chat template and trained on with per-role weights: user 0.5 (absorb what
people say), assistant 1.0 (reinforce its own habits and tool use), tool output
0. Two older episodes are replayed in the same step, preferring turns rated
good and never those rated bad.

**Active learning.** Tools the model calls on itself. `memorize` repeats a
lesson until the recite loss is under a target. `quiz` measures the loss of an
expected answer, so knowing is a number. `play` runs a batch of self-play
episodes, lets the rules score every decision, normalises rewards into
advantages and takes one policy-gradient step; a quarter of moves are random
legal ones so a collapsed policy still explores. `probe` watches for drift.

**Sleep.** A night has phases: *replay* the day's episodes and lessons for a
bounded number of steps (strengthen and interleave before anything becomes
permanent), *internalise* the identity (below), *probe* for drift and abort
the night if the probes got much worse, then *merge* the overlay into the base
exactly, keeping the previous base for `groow rollback`. Nights come
automatically every `sleep_every_steps` learning steps.

**Curiosity.** When nobody has talked for `sense_idle_minutes`, Groow gets an
internal impulse, not a human message, and reads the news through its own
tools: `news_headlines` (RSS feeds, dated and sourced), `read_article`, then
`learn_fact` for each item worth keeping. `learn_fact` drills a question to an
answer in the *source's* wording with its date, so what gets trained in is
grounded text, never Groow's paraphrase. Everything is filed as a lesson that
`recall` can find. A fixed pipeline (`--pipeline`) does the same without the
model's judgement and is the fallback when the agentic pass learns nothing.

**Identity.** There is no rigid system prompt. `state/identity.md` starts from
a short seed (including that Marlinski is its owner and mentor, to be asked when
stuck) and Groow may rewrite it with `update_identity`; every version is kept.
Each night, *context distillation* moves the identity from text into weights:
answers generated with the identity in the prompt are trained on without it.
`groow identity` reports the loss of the self-description when asked "who are
you?" with no system prompt at all. When that is low enough, set
`identity_in_prompt: false` and the prompt is gone; the personality stays.
Questions Groow cannot resolve go to `ask_mentor`; you see the inbox when you
next open the chat.

**Mind.** Groow has one conscious thread: a single rolling conversation fed
by a priority queue of signals. Your messages come first, then messages from
inner thoughts (`focus`, finished), then reminders that a thought is running,
then the idle impulse. It is the only thing that speaks to you or to the mentor.
With `think(goal, max_steps)` it spawns **inner thoughts**: separate
conversations that run concurrently as coroutines with a restricted toolset
(they can read, learn facts, memorise, play, recall; they cannot talk to anyone
or rewrite the identity). Main indexes them with `list_thoughts`,
`read_thought`, `pause_thought`, `resume_thought`, `kill_thought`. All
generation goes through one in-process server that batches concurrent requests
into a single forward pass; a pending user message preempts a thought batch per
token, so you never wait for background thinking. Every main turn and every
thought step is an episode that `recall` can read back in full.

**Body and home.** In Docker, Groow runs as a non-root user with a read-only
image (the body: CUDA, Python, the `groow` package, rebuilt from the
Dockerfile) and a persistent home volume (everything it is and everything it
grows: weights, memory, identity, skills, workspace, Nix profile, venvs).
`run_shell` gives it a real shell in that home. It cannot escape it, cannot
alter its own runtime under `/opt/venv`, cannot become root; it can download
binaries, build things, and install anything nixpkgs or PyPI has. On a bare
host the same tools run as you, so keep the shell tool for the container.

**Recipes.** The body ships short notes written for Groow, not for you:
its home layout, installing software with Nix or `uv`, using the shell,
writing a skill, how it learns, its mind, its mentor. On first start they are
copied into `state/recipes/` and a default skill (`list_recipes`,
`read_recipe`, `write_recipe`) is installed, so Groow can read them when
unsure and rewrite them when it learns better. They are its notes, not the
body's: upgrades add new recipes but never overwrite edited ones.

**Self-extension.** Groow writes its own commands. A *skill* is a Python
file in `state/skills/` that registers tools and ships its own `TESTS`.
`draft_skill` checks it in a fresh subprocess (imports, schemas, no collision
with core tools, tests pass, timeout for runaway code); `install_skill`
hot-loads it and keeps the previous version for `rollback_skill`;
`disable_skill` quarantines it. The core package is off limits: for that Groow
files a `propose_patch` for you to review. A supervisor around the chat turns
tool errors and per-signal exceptions into incidents, quarantines a skill whose
traceback caused a crash, and otherwise restarts in **safe mode** (skills off,
learning off, curiosity off, repair tools on, the incident in the prompt).
`groow doctor` runs the static checks without loading the model. See
`docs/extension.html`.

**Growth.** `grow` consolidates and attaches a wider overlay. The new part
starts at zero, so the function is unchanged at t=0. The same zero-init
principle extends to widening MLPs, inserting identity layers and bolting on a
vision encoder (see Roadmap).

## Repository map

```
groow/
  config.py            Config dataclass ⇄ groow.json
  brain/               the neural substrate
    model.py           Brain: load, generate, generate_batch, sft_step, pg_step,
                       consolidate, grow_rank, save/load; fp16 base + fp32 LoRA
    chatfmt.py         render conversations with the chat template; per-role loss masks
  memory/
    episodic.py        Memory: episodes, lessons, learning log, drift probes, recall
  learning/
    learner.py         Learner: passive, feedback, memorize, quiz, play, probe, report
    sleep.py           SleepPolicy: replay, internalise, drift check, merge, rollback, when to sleep
    curiosity.py       Curiosity: idle behaviour (agentic news reading, pipeline fallback)
    identity.py        Identity: seed, self-edit, context distillation into weights
  senses/
    news.py            NewsSense: RSS/Atom -> dated, sourced items; remembers what it has seen
  games/               rule systems that score actions without a human
    base.py            Game / Episode protocol (+ optional explore() for exploration)
    tictactoe.py       self-play, rewards from the rules only
    arithmetic.py      mental arithmetic at three levels
    __init__.py        registry, loading and validating invented games
  mind/                the conscious thread and its inner thoughts
    signals.py         Priority, Signal, InputQueue (thread-safe, asyncio)
    thoughts.py        Thought, ThoughtManager: concurrent coroutine run-loops with own traces
    mind.py            Mind: the scheduler loop; frames signals into the main conversation
  harness/             everything between a user message and a finished turn
    registry.py        ToolRegistry: Python function → JSON schema, safe dispatch
    builtins.py        calculator, current_time, list/read/write_file, run_python, run_shell (bash in the home)
    selftools.py       memorize, quiz, recall, play, list_games, invent_game,
                       consolidate, grow, probe, learning_report, ask_mentor,
                       read_identity, update_identity
    sensetools.py      news_headlines, read_article, learn_fact
    mindtools.py       think / list / read / pause / resume / kill thoughts, recall; focus / finish
    skills.py          SkillManager: draft → sandbox check → install → rollback / quarantine; incidents; patches
    skillcheck.py      the subprocess checker (torch-free) a draft must pass
    skilltools.py      draft_skill, install_skill, list/read/disable/rollback_skill, read_incidents, propose_patch
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
  default_skills/      skills installed on first start (recipes: list_recipes, read_recipe, write_recipe)
  cli.py               App wiring + commands (init, start, ui, chat, status, stop, doctor, one-shots)
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
- Tic-tac-toe self-play went from 100 % illegal moves to 0 % illegal and a
  positive score against a random player in 12 rounds (about 3 minutes on the
  1.7B). Two-digit arithmetic went from 31 % to 92 % in 6 rounds. Part of that
  is learning the answer format.
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
