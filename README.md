# Groow

A small mind that learns by changing its own weights.

Not a chat application with a memory file. Groow has a frozen base model with a trainable
overlay on top of it, and that overlay changes from what happens to it: every exchange, every
command that worked or did not, every question a person answered or ignored. At night the
overlay is merged into the base, and what it practised becomes what it is.

It lives in a sandbox with a home of its own, reaches the world through a shell, keeps a
conversation it cannot rewrite, sets its own alarms, puts work aside for later, and asks its
mentor when it is stuck. It has a birth certificate it cannot edit and an age that ticks.

```
./birth                  wake it (the first time, this is a birth: it fetches its base model)
./birth ui               open the window onto it
./birth say "hello"      say something
./birth stop             put it to sleep
```

## How it is put together

Four parts, each with one job.

**The core** is a Rust program running as root. It owns the state, decides what happens next,
and starts everything else. It is the only writer of the conversation: the mind cannot edit
its own history, because it has no handle on it. One task owns all mutable state and handles
every request synchronously, with no `await` inside a handler, which is what makes deadlock
structurally impossible rather than merely unobserved.

**The harness** is the agent loop, also Rust, and it holds nothing. It starts, claims the turn
it was spawned for, receives everything it needs in one reply, runs the exchange, reports what
happened and exits. Every write it sends back carries a turn id and an epoch, and the core
refuses anything stale, so re-running a turn is a no-op or a clean refusal, never half a turn.

**The brain** is Python, because that is where the model and the training code live. It answers
two things: whether it is up, and one streamed generation at a time, ordered so a conscious
turn goes before an inner thought.

**The learning passes** are Python too, and the mind never calls them. The core runs them
between turns and at night. They read how each turn actually went, score it, turn the good ones
into things to practise and the bad ones into things to do less of, and take the gradient step.
What is learned is decided by how things went, not by the mind deciding it did well.

```
python -m groow.learn feel       score the turns that have ended
python -m groow.learn harvest    turn what was felt into things to practise
python -m groow.learn nap        the short pass: feel, harvest, a little practice
python -m groow.learn night      all of it, then merge the overlay into the base
```

A night changes the weights on disk, so it tells the brain to pick them up; otherwise it would
keep answering from the copy it loaded hours earlier and the night would appear to have done
nothing.

```
     you ──────► groow ui ──┐
                            │  one unix socket, peer credentials from the kernel
     alarms ─────────────┐  │
     inner thoughts ──┐  │  ▼
                      └──┴──►  CORE (root) ──── spawns ────► harness (the mind, uid 1000)
                                 │  owns state/                   │ shell, ask, think
                                 │  journal, queue, questions      │
                                 │  alarms, statistics             ▼
                                 │                          skills: any executable
                                 ├──────────────────────────► brain (python, the card)
                                 └──── between turns ───────► learning passes (python)
```

## What the mind can do

Three tools, and that is deliberate. `shell`, `ask` and `think`. Everything else is a command
in its own `bin` directory, reached through the shell like anything else, so it gets an exit
status and an error message it can learn from rather than a tidy wrapper that teaches it
nothing.

Skills are simply executable files. Python, shell, a compiled binary: the kernel runs it, the
mind reads what it printed. Nothing is loaded into the core, so a broken skill breaks only its
own process. The ones that ship are in `skills/`; the mind can write its own, and one it has
changed is never overwritten.

## Why it does not annotate itself

Nothing asks the mind how it thinks it did. The free signals are observable from outside: a
command failed, the same call was made twice, the round budget ran out, it recovered from an
error it had just made. On top of those sits a frozen judge that reads a person's reaction to
what was said. The judge never learns, so what counts as approval cannot drift.

Its questions cost something too. A handful may be open at once; asking one more drops the
oldest, and one nobody answers expires. Both outcomes come back as a cost, which is what
teaches it to ask less and ask better.

Because the judge never learns, a badly worded question to it is a permanent, invisible
mistake. `python -m groow.limbic.calibrate` checks it against exchanges where we know what a
person would say, and fails if it is ever confidently backwards. That fixture is how the
current wording was chosen: an earlier one scored a perfectly good answer at minus zero point
seven six, because it was judging the answer's quality rather than whether it was an answer.

## Running it without Docker

```
cargo build --release --manifest-path rust/Cargo.toml     # the core, the harness, the window
uv pip install -e .                                       # the brain and the learning passes
python -m groow.serve --port 7374 &                       # the card
rust/target/release/groow start                           # the core
rust/target/release/groow ui                              # the window
```

Started this way the core is not root, so the state is not out of the mind's reach. It says so
at startup rather than implying a guarantee it does not have.

## The machine this was built for

One Tesla V100S, which is Volta: fp16 only, no bf16, no FlashAttention, and CUDA 13 has dropped
it, so torch has to come from the cu126 wheels. The base model is Qwen3-4B with a rank-32
overlay.

## Layout

```
nervous_system/proto   every message, defined once, generated into both languages
rust/crates/proto      the wire: frames, ops, who may call what
rust/crates/core       state, scheduler, connections, the only writer
rust/crates/harness    the agent loop
rust/crates/ui         the window
rust/crates/cli        the groow command
groow/serve.py         the brain
groow/learn.py         feel, harvest, train, night
groow/brain, learning  the model and the gradient steps
skills/                the skills it starts with, copied into its home
recipes/               its manual, copied into its home where it can rewrite it
```

Run the tests with `cargo test --manifest-path rust/Cargo.toml`, and check the judge with
`python -m groow.limbic.calibrate`.
