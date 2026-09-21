# The files

What never travels. These are the durable, the large, and the things a person should be able to
read with `cat`.

All of them live under the state directory. When the core runs as root they are owned by root
and readable, not writable, by the mind. Every write is made by filling a temporary file next
to the target, flushing it, and renaming it over, so a reader sees either the old content or
the new and never a truncation.

## The conversation

`state/main/YYYY-MM-DD_HHMMSS.jsonl`, rotating at a thousand records or at midnight, oldest
first by name. One JSON object per line, timestamp first, fsynced as it is written.

```json
{"ts":1789755947.44,"role":"user","content":"how many files are here?","kind":"user","turn":"01789755947441-0004"}
{"ts":1789755949.03,"role":"assistant","content":"","tool_calls":[{"name":"shell","arguments":{"command":"ls | wc -l"}}],"turn":"01789755947441-0004"}
{"ts":1789755949.41,"role":"tool","name":"shell","content":"25","turn":"01789755947441-0004"}
{"ts":1789755951.02,"role":"assistant","content":"25.","turn":"01789755947441-0004"}
```

`turn` is what lets a turn that went badly be left out of the window later. `kind` appears on
incoming messages and says what woke the mind.

Only the core writes this. The mind asks, and the core decides.

## The queue

`state/inbox/new/` and `state/inbox/cur/`, one file per waiting signal. The name is the
sort key, so the next thing to handle is the first name in order:

```
{priority}-{microseconds:016}-{pid:05}{seq:05}-{kind}.json
0-1789755947441234-1234500001-user.json
```

The body is `{"priority":0,"kind":"user","text":"…","ts":…,"meta":{},"attempts":0}`.

Delivery is at least once. A signal being handled is moved to `cur/` and moved back if the
process dies, because handling an alarm twice is survivable and losing a person's message is
not. `attempts` counts failures; after three the message is set down with a line in the
conversation.

## Questions, alarms, thoughts

- `state/mentor_inbox.jsonl` — one record per question: `id`, `ts`, `question`, `context`,
  `status` (`open`, `answered`, `expired`, `dropped`), `resolved_ts`, `answer`.
- `state/schedule.json` — an array of alarms: `id`, `text`, `when`, `repeat_s`,
  `repeat_daily`, `enabled`, `created`, `by`, `last_fired`, `fires`.
- `state/thoughts/<id>.json` — one inner thought: its goal, status, steps, budget, summary,
  its own message history, and the process running it. Unknown fields are carried through
  untouched, so an older core cannot destroy a newer one's data.

## The samples

`state/training/<set>.jsonl`, append-only, fsynced. Two kinds:

```json
{"ts":…,"kind":"sft","messages":[…],"weights":{"user":0.5,"assistant":1.0},"turn":"…","by":"hippocampus"}
{"ts":…,"kind":"pg","prompt":[…],"completion":"…","reward":0.15,"group":"turns","tags":["completed"],"by":"hippocampus"}
```

`state/training/cursor.json` records how far each set has been consumed, `{"actions": 27}`,
meaning the index of the next unread line. The brain is the only writer of the cursor, because
it is the only thing that consumes samples.

`group` matters: samples in a group are normalised against each other, so a group of one has an
advantage of zero and teaches nothing. Conversation turns share the group `turns` and are
measured against how turns have been going; games keep a group per round, because there the
same position really was played several ways.

## The statistics

`state/groow.db`, SQLite, mode 0600, root only, including its write-ahead files. Turns and
their durations, tool calls and their failures, what each turn felt like, the lifecycle of
each question, learning passes, counters.

Nothing in it is irreplaceable: it could be rebuilt by replaying the conversation. That is what
makes it safe to keep it fast, and it is unreadable to the mind on purpose, because how it is
being scored is not its business.

## The unchangeable

`state/birth.json`, written once and made read-only to everyone, root included. Id, name, the
moment of birth, lineage, hardware, mentor, version. No code path anywhere rewrites it and the
mind has no tool that could.

`state/identity.md` is the opposite: plain text the mind may rewrite, within limits, and which
is folded into every prompt.
