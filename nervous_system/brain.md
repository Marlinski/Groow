# The loopback

Between anything that needs the GPU and the one process that owns it.

- **Where** `http://127.0.0.1:7374` by default, `brain_port` in the configuration.
- **Encoding** JSON in, newline-delimited JSON out, so a long piece of work is not silence.
- **Who** anyone who can reach loopback. There are no credentials here, which is why the socket
  and not this is where authority is decided.

Three kinds of work, one queue, one worker, because there is one GPU. Lower priority goes
first: a conscious turn at 0, an inner thought at 2, a background game at 4, training at 8. A
request arriving during a training step waits its turn rather than being refused.

## POST /generate

```json
{"messages":[{"role":"user","content":"…"}],
 "tools":[{"name":"shell","description":"…","parameters":{…}}],
 "max_new_tokens":768,"temperature":0.7,"top_p":0.8,"top_k":20,
 "enable_thinking":false,"priority":0}
```

Answers with zero or more deltas and then exactly one terminal line:

```json
{"delta":"The capital"}
{"done":true,"text":"The capital of France is Paris.","tokens":7,"seconds":1.08}
```

The full `text` is authoritative; the deltas exist so a person can watch it appear. A dropped
delta cannot corrupt what gets journalled. On failure the terminal line is `{"error":"…"}`.

Trimming happens on this side, because this is the side with the tokenizer and therefore the
only side that knows what fits.

## POST /train

```json
{"max_samples":8,"urgent_only":false}
```

Answers with progress lines while it works, then the trainer's report:

```json
{"progress":"policy step on actions:turns: 27 decisions, mean reward +0.15, loss -0.031"}
{"done":true,"report":{"consumed":8,"sft_steps":3,"pg_steps":1,"last_loss":2.2,"seconds":9.7}}
```

The samples themselves are never sent. They are files, and this process reads them from a
cursor; see [files.md](files.md).

## POST /consolidate

No body. Merges the overlay into the base weights and opens a blank one. Nothing reloads
afterwards: the process that merged them is the one that serves, so the merged weights are what
it answers from, from the next request onward.

## POST /probe

No body. Scores the held-out probes in `state/probes.json` and records the measurement.
Reads the weights; never changes them.

```json
{"mean_loss":0.83,"losses":[0.4,1.2,…],"baseline_mean":0.79,"measurements":14}
```

These questions are never trained on, which is the only reason the number means anything. A
night takes this before and after its training and compares the two; see
[learning](../docs/learning.html).

## POST /discard

No body. Throws the overlay away and opens a blank one on the base, which never moved.
Everything practised since the last merge is gone.

```json
{"discarded":true,"passes_lost":7,"discards":1}
```

This is what a failed gate does, and it is why no base needs archiving: the base on disk is
already the checkpoint, so undoing a night costs a day and no disk at all.

## POST /reload

Picks up weights that changed on disk underneath it. Only needed when something other than this
process changed them; a consolidation does not need it.

## GET /health

```json
{"ok":true,"model":"Qwen/Qwen3-4B-Instruct-2507","busy":true,"doing":"train",
 "waiting":2,"generated":132,"trained":8,"steps":212}
```

`doing` is `generate`, `train`, `consolidate`, `probe`, `discard` or empty. `waiting` is the queue depth.
