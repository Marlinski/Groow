# The nervous system

Every way the parts of Groow talk to each other, written down in one place.

There are four parts and three channels. The rule that explains the shape is that the part
which owns a thing is the only part that touches it: the core owns the state, the brain owns
the GPU, and the mind owns nothing at all.

| | |
|---|---|
| [socket.md](socket.md) | the Unix socket between the core and everything that is not the brain |
| [brain.md](brain.md) | the loopback HTTP to the process that holds the weights |
| [files.md](files.md) | the formats nobody sends over a wire |

```
      a window
          │ I
          ▼
 mind ──I──► CORE ──II──► brain ◄──II── passes
                │              ▲
              III            III
                ▼              │
              the state ───────┘
```

## Who may speak to whom

- The **mind** speaks only to the core. It has no address for the brain and no handle on the
  state. Generation is relayed by the core, which is how the core can stream tokens to a
  window, count what a turn cost, and refuse a turn that is no longer current.
- The **core** speaks to the mind, to windows, and to the brain.
- The **passes** speak to the brain and write files. They never speak to the core; the core
  starts them and waits.
- A **skill** playing in the background calls the brain directly, at the lowest priority,
  because a game is not a turn and must never come before a person.

## What each channel is for

**The socket** carries anything that needs to know who is asking. The kernel puts the peer's
real user id on the connection, so authority is decided before a byte is read and nothing a
caller claims about itself can grant it anything.

**The loopback** carries work for the GPU. One queue, one worker, three kinds of job:
generate, train, consolidate. A request arriving during a training step waits its turn rather
than being refused.

**The files** carry everything large and everything durable. They are also what makes the
privilege split real: root owns them and the mind can read but not write them, which is a
stronger guarantee than a protocol could give, because editing is not something that happens
by message.

## Keeping this honest

The op table in [socket.md](socket.md) is checked against the code: a test in the `groow-proto`
crate parses it and fails if an op exists in one and not the other, or if the roles disagree.
A specification nobody checks becomes fiction within a month.
