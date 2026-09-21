# The socket

Between the core and everything that is not the brain.

Defined by [proto/wire.proto](proto/wire.proto) and [proto/turn.proto](proto/turn.proto). Those
files are the specification; this page explains it. Both languages are generated from them, so
a field that exists on one side exists on the other.

- **Where** `<state>/core.sock`, a Unix domain socket. Mode 0660, owned by root, group of the
  agent user, so nobody else on the machine can open it.
- **Encoding** one JSON object per line, UTF-8, newline-terminated. Lines up to 8 MiB.
- **Who** decided from the peer's credentials, which the kernel puts on the connection. Nothing
  a caller says about itself is used.

## Frames

Every line is exactly one of these, tagged by `f`. An unknown tag is a parse error, not a
guess.

Every line is one `Frame`, which is a protobuf `oneof`: exactly one field is set, and the name
of that field says what it is.

| field | shape | meaning |
|---|---|---|
| `req` | `{"req":{"id":7,"op":"turn.claim","arg":{}}}` | a request, answered exactly once |
| `part` | `{"part":{"id":7,"data":{"delta":"hel"}}}` | a piece of an answer still in flight |
| `rep` | `{"rep":{"id":7,"ok":{…}}}` | the answer |
| `err` | `{"err":{"id":7,"code":"CODE_STALE_EPOCH","msg":"…"}}` | the refusal |
| `ev` | `{"ev":{"name":"tool_call","t":1789.5,"data":{…}}}` | something happened; nobody replies |
| `push` | `{"push":{"name":"turn_end","data":{…}}}` | the core telling a peer something unasked |

`id` is chosen by the caller and is unique within a connection. Exactly one `rep` or `err`
closes it; any number of `part` frames may precede them.

### Error codes

`CODE_UNKNOWN_OP`, `CODE_BAD_ARG`, `CODE_DENIED`, `CODE_STALE_EPOCH`, `CODE_NOT_FOUND`,
`CODE_IDLE`, `CODE_BUSY`, `CODE_CANCELLED`, `CODE_INTERNAL`. Codes are stable; messages are for
a person reading a log.

## Roles

| role | who | how it is decided |
|---|---|---|
| `mentor` | the owner | peer uid is 0, or the uid the core itself runs as |
| `agent` | the mind | peer uid is the configured agent user |
| `viewer` | a window | arrived over the loopback gateway, which carries no credentials |

Any other uid is refused outright rather than given a lesser role: an unexpected account on
this socket is a misconfiguration and should be loud.

## Ops

The least role that may call each one. This table is checked against the code; see the test in
`rust/crates/proto`.

| op | least role | what it does |
|---|---|---|
| `hello` | agent | identify; the reply states the role the core assigned |
| `status` | agent | a snapshot: its [state](brainstem.md), age, queue, turns, feeling |
| `watch` | viewer | follow the event stream on this connection, as `push` frames |
| `turn.claim` | agent | take the turn this process was spawned for |
| `turn.append` | agent | record one message in the conversation |
| `turn.end` | agent | close the turn out |
| `complete` | agent | generate; the core relays to the brain and answers with the text, what it cost and how long it took |
| `thought.claim` | agent | take the next step of an inner thought |
| `thought.append` | agent | record one message on that thought's own trace |
| `thought.end` | agent | close out one step |
| `think` | agent | start an inner thought |
| `thought` | agent | read, pause, resume, kill, focus or finish a thought |
| `recall` | viewer | read the conversation back, a page at a time |
| `schedule` | agent | list, add or cancel an alarm |
| `stats` | mentor | the measurements: learning passes, turns, feeling, tool health |
| `interrupt` | mentor | stop the turn that is running |
| `say` | viewer | speak to it |
| `command` | viewer | run a mentor command |
| `consolidate` | mentor | merge the overlay into the base |
| `train` | mentor | run a learning pass now |
| `quit` | mentor | stop the core |

Note that `agent` is not a rank above `viewer`. The roles are an explicit table: a window may
speak and watch, and must never be able to claim a turn or generate, whatever order the roles
are written in.

## The turn contract

`turn.claim` returns everything a turn needs, so the process that runs it holds nothing:

```json
{"turn":"01789755947441-0004","epoch":12,"kind":"SIGNAL_USER",
 "text":"how many files are here?",
 "framed":"how many files are here?",
 "system":"I am Groow…",
 "window":[{"role":"user","content":"…"},{"role":"assistant","content":"…"}],
 "max_rounds":10,"meta":{}}
```

Every later write carries `turn` and `epoch`. The core bumps the epoch whenever a turn ends or
is cancelled, and refuses anything stale, which is what makes re-running a turn either the same
turn or a clean refusal and never half of one.

`kind` is a `SignalKind`. Only `SIGNAL_USER` and `SIGNAL_COMMAND` reach the mind unframed; the
rest are wrapped in a sentence saying what woke it. On disk the journal writes the short form,
`user`, `alarm` and so on, because the learning passes have always parsed it that way.

The epoch is deliberately a 32-bit number: canonical protobuf JSON writes a 64-bit integer as a
string, and this one is read straight out of the JSON by things expecting a number.

## Events

Carried up as `ev` from a turn, fanned out to watchers as `push`. Names are stable:

`turn_start`, `turn_end`, `message`, `tool_call`, `tool_result`, `thought`,
`feeling`, `learned`, `log`, `status`.

`status` is sent whenever what it is doing changes, as well as when asked, so a window never
draws a state it has already left. What the states are and what moves between them is
[brainstem.md](brainstem.md).

`log` may be dropped when a watcher falls behind; the others may not. A watcher
that has stopped reading loses events and can never slow down a turn.
