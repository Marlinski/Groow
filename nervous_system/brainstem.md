# The brainstem

What the creature is doing, and what each thing that happens to it means given that.

Everything above the brainstem decides what to think. The brainstem decides whether it is in
any condition to think at all — awake or asleep, free or already busy — and what each stimulus
means in that condition. In an animal it is the organ that keeps the sleep and waking cycle and
gates what reaches the rest of the brain, which is exactly the job here.

It is `rust/crates/core/src/brainstem.rs`, and it is one pure function. It holds no journal, no
queue and no clock, so every rule in it is a table you can read and a test you can write.

## The states

| | |
|---|---|
| `waking` | the weights are loading. Nothing can happen yet |
| `listening` | nothing to do, and the conversation is warm |
| `idle` | nothing to do, and nobody has said anything for a while |
| `thinking` | in a turn, waiting for the brain |
| `working` | in a turn, waiting for a command of its own |
| `napping` | a short learning pass between turns |
| `sleeping` | the long pass, ending in the merge that makes it permanent |
| `stopping` | asked to stop. Nothing new begins |

## The stimuli

Everything that can happen to it, in one list, so that "what can happen here" is a question
with an answer you can read rather than one you have to go and find.

`brain` · `arrived` · `asked` · `turn began` · `tool called` · `tool returned` · `turn ended` ·
`sleep began` · `sleep ended` · `nudged` · `spoke` · `stop`

## The reflexes

What the body should do about a stimulus. The state decides, the core carries it out, and the
core decides nothing of its own.

| | |
|---|---|
| `nothing` | beyond whatever the state changed to |
| `begin` | take the next thing in the queue and open a turn for it |
| `rest(n)` | not now; look again in n seconds |
| `halt` | put everything down |

## Why it is written this way

Nothing asks the brainstem what state it is in and then decides. It says what happened, and
the state it was in at that moment decides what becomes of it. The rule for "a message arrived
while it was asleep" is written once, in the state that was asleep, rather than spread across
every place that has to ask whether it is asleep.

That is also what keeps the answers honest. The word the creature shows you and the rule it
actually follows are the same function, so it cannot say it is listening while something holds
your message back, or say it is asleep while a turn runs.

Two things it deliberately does not do. It never wakes early: a message arriving during a
learning pass waits, because interrupting the pass half way would leave the weights in a state
nobody chose, and the queue is on disk so nothing is lost by waiting. And it starts every
process in `waking`, whatever the last one was doing — the durable facts live outside it, in
the queue on disk and the counters in the database, and the live state is rebuilt from the
first few stimuli rather than restored.
