# GROOW RUNTIME SPECIFICATION (read from source, Python package `groow`, /home/ubuntu/Work/github/groow)

Everything below is literal from the source. Line references are to the files as read. Items marked **[AMBIGUOUS]** / **[DEAD CODE]** / **[BUG?]** are flagged rather than guessed.

---

## 0. Process topology (context for the rest)

- **Daemon** (`/home/ubuntu/Work/github/groow/groow/gateway/daemon.py`): one long-lived process. Owns the GPU, the tokenizer, the model, the state, and the HTTP/SSE/WS server. Does **not** run the agent loop.
- **Turn process** (`/home/ubuntu/Work/github/groow/groow/turn.py`, launched as `python -m groow.cli turn --signal '<json>'`): one signal → one process → one `Harness.turn()` → JSON-lines on stdout → exit. Never loads torch.
- **Thought process** (`python -m groow.cli think <id>`): same shape, loops `Harness.turn()` until finish/budget/pause.
- Turn/thought processes reach the daemon over HTTP only: `POST /complete` (the single model call), `POST /op`, `POST /emit` (`/home/ubuntu/Work/github/groow/groow/remote.py`).

---

## 1. `/home/ubuntu/Work/github/groow/groow/harness/loop.py` — the turn algorithm

### 1.1 Regexes (exact)

```python
TOOL_CALL_RE = re.compile(r"<tool_call>\s*(\{.*?\})\s*</tool_call>", re.DOTALL)
THINK_RE     = re.compile(r"<think>(.*?)</think>", re.DOTALL)
```

### 1.2 `parse_generation(raw) -> (reasoning, content, calls)`

Exact algorithm:
1. `reasoning = ""`. `THINK_RE.search(raw)`; if it matches: `reasoning = group(1).strip()` and `raw = raw[m.end():]` (everything before and including the first `</think>` is discarded). Only the **first** `<think>` block is honoured.
2. For every `TOOL_CALL_RE.finditer(raw)` match: `json.loads(group(1))`. On `JSONDecodeError` → skip silently. If the parsed object is a `dict` and has key `"name"`:
   - `args = obj.get("arguments", {}) or {}`; if `args` is a `str`, try `json.loads(args)`, on failure `args = {}`.
   - append `{"type": "function", "function": {"name": obj["name"], "arguments": args}}`.
   Note: the regex is non-greedy `\{.*?\}` — **a tool-call JSON containing a nested closing `}` before its own end will only capture up to the first `}`** and then fail to parse (silently dropped). **[BUG?/AMBIGUOUS for a Rust port: to match Python exactly, replicate the lazy-`{...}` semantics, not a balanced-brace parser.]**
3. `content = TOOL_CALL_RE.sub("", raw)` (all well-formed tool_call blocks removed).
4. Unterminated-call recovery: if `"<tool_call>"` still occurs in `content`: split once on it → `head, tail`; `tail = tail.replace("</tool_call>", "").strip()`; try `json.loads(tail)`; if dict with `"name"`, append a call `{"type":"function","function":{"name":…,"arguments": obj.get("arguments", {}) or {}}}` (no str-decoding of arguments here, unlike step 2). `content = head`.
5. `content = content.strip()`; if `content in ("}", "})", "]", "}}")` → `content = ""`.
6. Return `(reasoning, content, calls)`.

### 1.3 Dataclasses

```python
@dataclass
class Hooks:
    on_message:     Callable[[dict], None] | None = None
    on_text:        Callable[[str], None] | None = None
    on_tool_call:   Callable[[str, dict], None] | None = None
    on_tool_result: Callable[[str, dict, str], None] | None = None
    after_turn:     Callable[[TurnResult], None] | None = None
    on_error:       Callable[[str, Exception], None] | None = None
```

```python
@dataclass
class TurnResult:
    user_text: str
    context: list[dict]       # conversation before this turn, incl. system prompt
    messages: list[dict]      # the turn itself: user, assistant(s), tool(s)
    final_text: str
    tools_used: list[str] = []
    rounds: int = 0
    seconds: float = 0.0
    interrupted: bool = False
    flags: list[str] = []     # "repeat", "tool_error", "exhausted"
    extra: dict = {}
```

### 1.4 `Harness` construction

`Harness(brain, tools: ToolRegistry, cfg: Config, system_prompt: str, hooks=None, name="main")`.
`self.history = [{"role": "system", "content": system_prompt}]`. `self.turns = []`. `self.turn_kind = "user"`.
`reset()` restores history to just the system message.

### 1.5 `async turn(user_text, should_stop=None)` — the exact algorithm

1. `t0 = time.time()`; `context = list(self.history)` (snapshot **before** the turn); `user_msg = {"role":"user","content":user_text}`; `turn_start = len(self.history)`; append `user_msg` to history; fire `on_message(user_msg)` (via `_journal`, exceptions swallowed); `turn_msgs = [user_msg]`; `tools_used = []`; `seen_calls = {}`; `flags = []`; `final=""`, `rounds=0`.
2. `for rounds in range(1, cfg.max_tool_rounds + 1):` — **default `max_tool_rounds = 10`** (`config.py`). "Round" = one generation + the execution of all tool calls it emitted.
   1. `self._fit_context(turn_start)`.
   2. `turn_start = len(self.history) - len(turn_msgs)` (recomputed every round so trimming stays consistent).
   3. `raw = await self.brain.complete(self.history, tools=self.tools.schemas(), on_text=self.hooks.on_text, should_stop=should_stop)`.
      - On `Interrupted` (from `groow/errors.py`): `del self.history[turn_start:]` (roll back the whole turn) and **return immediately**
        `TurnResult(user_text, context, [], "", tools_used, rounds, round(time.time()-t0, 2), True)` — i.e. `messages=[]`, `final_text=""`, `interrupted=True`, `flags=[]`, `after_turn` **not** fired, result **not** appended to `self.turns`.
      - `Interrupted` is caught **only** around `brain.complete`. An `Interrupted` raised anywhere else propagates.
   4. `reasoning, content, calls = parse_generation(raw)`.
   5. Build `msg = {"role":"assistant","content":content}`; add `"reasoning_content": reasoning` if non-empty; add `"tool_calls": calls` if any. Append to history, `_journal(msg)`, append to `turn_msgs`. `final = content`.
   6. **Turn ends** (`break`) if `not calls`.
   7. If `flags.count("repeat") >= 2`: `final = final or "(I got stuck repeating the same tool call and stopped.)"`, `flags.append("exhausted")`, `break`. (So two prior repeat-detections in this same turn end it; checked *after* the generation of the current round.)
   8. For each call `c` in `calls`, in order:
      - `name, args = c["function"]["name"], c["function"]["arguments"]`.
      - **Shell aliasing**: if `name not in self.tools.tools` **and** `"shell" in self.tools.tools` **and** `self._is_command(name)`:
        `argv = " ".join(str(v) for v in (args.values() if isinstance(args, dict) else [args]))`; then `name = "shell"`, `args = {"command": f"{orig_name} {argv}".strip()}`; `c["function"]` is mutated in place (so the journalled assistant message shows the rewritten call).
      - `on_tool_call(name, args)` fires (exceptions **not** caught here).
      - `key = name + json.dumps(args, sort_keys=True, ensure_ascii=False)`.
      - If `key in seen_calls`: `flags.append("repeat")` and
        `result = json.dumps({"note": "you already made this exact call in this turn; here is the same result again. Do something different next, or answer.", "previous_result": seen_calls[key][:1500]}, ensure_ascii=False)`
        (the tool is **not** re-run; `seen_calls` is not updated).
      - Else: `spec = self.tools.tools.get(name)`; `gpu = bool(spec and spec.executor == "gpu")`; `executor = self.brain.server.gpu if gpu else None`; `fut = loop.run_in_executor(executor, self.tools.call, name, args)`.
        - GPU tools: `await fut` with **no timeout**.
        - Non-GPU tools: `await asyncio.wait_for(fut, timeout=cfg.tool_timeout)` (`tool_timeout = 120`). On `asyncio.TimeoutError`:
          `result = json.dumps({"error": f"tool {name} did not finish within {cfg.tool_timeout}s; it may still be running in the background. Do not call it again with the same arguments."})`
          (the underlying thread is *not* cancelled). Note: on timeout `seen_calls[key] = result` is still executed (it follows the try/except).
        - `seen_calls[key] = result`; if `result.lstrip().startswith('{"error"')` → `flags.append("tool_error")`.
        - Note an unknown tool that is not a command yields `ToolRegistry.call`'s `{"error": "unknown tool …"}` — same tool_error path, never an exception.
      - `tools_used.append(name)`; `on_tool_result(name, args, result)`; `tmsg = {"role":"tool","content":result}` appended to history and `turn_msgs`; `_journal({**tmsg, "name": name, "args": args})` (the journal record carries name/args, the model history does **not**).
   9. Loop continues to the next round.
3. `else:` (the `for` ran to completion without `break`, i.e. `max_tool_rounds` generations all produced tool calls): `flags.append("exhausted")`; `final = final or "(I ran out of steps before answering.)"`.
   **"Exhausted" therefore has exactly two producers**: round budget consumed, or ≥2 `repeat` flags.
4. `result = TurnResult(user_text, context, turn_msgs, final, tools_used, rounds, round(time.time()-t0, 2), flags=flags)`; appended to `self.turns`.
5. If `hooks.after_turn`: `await loop.run_in_executor(self.brain.server.gpu, hooks.after_turn, result)`. On any exception: `result.extra["after_turn_error"] = f"{type(e).__name__}: {e}"` and `on_error("after_turn", e)`.
   In a turn process `brain.server` is `_NoExecutor` with `gpu = None` (default executor), and `after_turn` is **never set** by `turn.py` — learning happens in the daemon instead (`Daemon.after_turn`).
6. Return `result`.

`turn_sync(text)` = `asyncio.run(self.turn(text))`.

### 1.6 Hook firing order per round

`on_text` (streamed chunks, during generation, only if the brain streams) → `on_message(assistant msg)` → per call: `on_tool_call` → `on_tool_result` → `on_message(tool msg + name/args)`. `on_message(user msg)` fires once at the very start. `after_turn` once at the end. `on_error` only for an `after_turn` failure.
`_journal` wraps `on_message` in `try/except Exception: pass`; the other hooks are **not** guarded.

### 1.7 `_fit_context(turn_start)` and the `remote` flag

```python
if getattr(self.brain, "remote", False):
    return
```
`RemoteBrain.remote = True` (class attribute, `groow/remote.py`), so **inside a turn/thought process context fitting is entirely skipped**; the daemon trims what it is sent (`Daemon._trim`, §5.6). For a local `ServedBrain`/`Brain` there is no `remote` attribute → `False` → fitting runs:

```
budget = cfg.max_seq_len - cfg.max_new_tokens      # 32768 - 768 = 32000
while turn_start > 1:
    text = brain.prompt_text(history, tools=tools.schemas())
    if len(brain.tok(text, add_special_tokens=False)["input_ids"]) <= budget: return
    older  = trim_messages(history[:turn_start], max(0, turn_start - 3))
    dropped = turn_start - len(older)
    history = older + history[turn_start:]
    turn_start -= dropped
    if dropped == 0: return
```
The current turn (`history[turn_start:]`) is never cut. `trim_messages` is `groow/brain/chatfmt.py` (§8.4).

### 1.8 `_is_command(name)`

```
re.match(r"^[a-z][a-z0-9_-]{0,30}$", name)  must match, else False
home  = os.path.expanduser(cfg.home_dir or "~")
paths = f"{home}/.local/bin" + os.pathsep + f"{home}/.nix-profile/bin" + os.pathsep + os.environ["PATH"]
return name == "groow" or shutil.which(name, path=paths) is not None
```

---

## 2. `/home/ubuntu/Work/github/groow/groow/harness/registry.py` — schema derivation

### 2.1 Type mapping

```python
_TYPE_MAP = {str:"string", int:"integer", float:"number", bool:"boolean", list:"array", dict:"object"}
```
`_json_type(tp)`:
- `Optional[X]` / `X | None` → `_json_type(first non-None arg)`; no args → `{"type":"string"}`.
- `list[X]` / `tuple[X,…]` → `{"type":"array","items": _json_type(X)}` (no inner arg → `items {"type":"string"}`).
- `dict[...]` → `{"type":"object"}` (no `additionalProperties`).
- anything else → `{"type": _TYPE_MAP.get(tp, "string")}` (unknown annotations degrade to `"string"`).
Missing annotation → treated as `str`.

### 2.2 Docstring parsing (`_parse_docstring`)

`inspect.cleandoc(doc)` → split once on `re.split(r"\n\s*Args?:\s*\n", doc, maxsplit=1)`. `summary = parts[0].strip()` (the **whole** text before `Args:`, multi-line, newlines preserved). In the Args block, per line: `re.match(r"\s*(\w+)\s*(?:\([^)]*\))?\s*:\s*(.*)", line)` and `not line.startswith("        ")` (8 spaces) starts a new arg; otherwise a non-blank line is appended to the current arg with a single space separator.

### 2.3 `_spec_from_function`

For every parameter except `*args`/`**kwargs`, in declaration order:
- `prop = _json_type(hints.get(pname, str))`
- if documented → `prop["description"] = arg_docs[pname]`
- if no default → appended to `required`; else `prop["default"] = p.default` (the raw Python value, JSON-serialized later).
`ToolSpec.name = name or fn.__name__`; `description = description or summary or fn.__name__`; `parameters = {"type":"object","properties":props,"required":required}` (`required` is always present, possibly `[]`).

### 2.4 Exact schema handed to the model

`ToolSpec.schema()` → and `ToolRegistry.schemas(groups=None)` returns a list of these:

```json
{
  "type": "function",
  "function": {
    "name": "shell",
    "description": "Run a bash command in your home and return its output. …",
    "parameters": {
      "type": "object",
      "properties": {
        "command": {"type": "string", "description": "the command line, run with bash -lc"},
        "timeout": {"type": "integer", "description": "seconds before the process is killed (max 600)", "default": 120}
      },
      "required": ["command"]
    }
  }
}
```
Order of tools = insertion order of `self.tools` dict.

### 2.5 Dispatch (`ToolRegistry.call(name, args) -> str`)

- Unknown name → `json.dumps({"error": f"unknown tool {name!r}", "available": [...names...]})`. (Python `!r` quoting: `unknown tool 'foo'`.)
- `_coerce_args`: unknown keys are **dropped silently**; coercion by declared type: `integer` → `int(v)` unless `v` is a bool; `number` → `float(v)`; `boolean` from a string → `v.strip().lower() in ("1","true","yes","on")`; `string` from non-str → `json.dumps(v)` for dict/list else `str(v)`. `TypeError/ValueError` → `ValueError(f"argument {k!r} should be {want}, got {v!r}")`. Then missing required → `ValueError(f"missing required argument(s): {missing}")`. Either is returned as `json.dumps({"error": str(e), "expected": spec.parameters})`.
- Executes `spec.fn(**kwargs)`; **any** exception → `{"error": f"{type(e).__name__}: {e}"}`. `spec.calls += 1`, `spec.total_seconds += elapsed` always.
- Non-dict/list results are wrapped as `{"result": value}`.
- Serialized with `json.dumps(result, ensure_ascii=False, default=str)`.
- `stats()` → `{name: {"calls": n, "seconds": round(s,1)}}` for tools with `calls > 0`.

`ToolSpec.executor` is `"io"` by default; `"gpu"` marks tools that must run on the GPU executor (§1.5). `group` defaults `"general"`; skill tools get `group = f"skill:{name}"`.

---

## 3. Tool sets

Assembled in `turn.py::build_tools(cfg, state, kind, thought_id="", safe=False)`:

- always: `make_substrate_tools(home, cfg.allow_shell)` → `shell` (group `substrate`) when `allow_shell` is true.
- `kind == "thought"` → `make_thought_tools(...)` → `focus`, `finish` (group `thought`).
- otherwise (main) → `make_self_tools(state, put=lambda q,c: daemon.op("ask", question=q, context=c))` → `ask` (group `self`), and `make_think_tool(lambda goal, max_steps=8: daemon.op("think", goal=goal, max_steps=max_steps))` → `think` (group `mind`).
- if `cfg.skills_enabled and not safe`: `SkillManager(state, protected=set(reg.names()), home=home).load_all()` then `reg.include(sk.registry)` — **skill tools are added to both main turns and inner thoughts** (the module docstrings claiming a thought only has shell/focus/finish are out of date).

So:

**(a) Main conscious turn**: `shell`, `ask`, `think`, + installed skill tools.
**(b) Inner thought**: `shell`, `focus`, `finish`, + installed skill tools.
(Safe mode: `shell`, `ask`, `think` only.)

`make_main_mind_tools(thoughts, memory)` exists in `mindtools.py` with an in-process `think` (calls `thoughts.spawn`) but is **not used by `turn.py`** — only the daemon-side `App` may use it. **[DEAD/LEGACY in the turn path.]**

### 3.1 `shell` (builtins.py, group `substrate`)

Signature: `shell(command: str, timeout: int = 120) -> dict`. Docstring **verbatim** (this is the model-facing description; the summary keeps its embedded newlines/indentation as produced by `inspect.cleandoc`):

> Run a bash command in your home and return its output. Your home persists; the rest of the system is
> read-only and you are not root (no apt, no sudo). Your files: state/ (traces in state/main/, thoughts in
> state/thoughts/, lessons, skills, recipes, identity.md), workspace/. Your commands: `web <url>` (a page as
> text), `news`, `groow play <game>`, `groow thoughts`, `groow thought pause|resume|kill <id>`,
> `groow skill check|install <name>`, `groow stats`, `groow identity`, `groow inbox`. Install software locally:
> `nix profile install nixpkgs#<pkg>`, `uv pip install …`. Long jobs: `nohup … &` and check later.
> Read `cat state/recipes/*.md` when unsure.

Args docs: `command: the command line, run with bash -lc`; `timeout: seconds before the process is killed (max 600)`.

Behaviour: `subprocess.run(["bash","-lc",command], cwd=home, capture_output=True, text=True, timeout=min(int(timeout),600), env={**os.environ, "HOME": str(home)})`.
Return on success: `{"returncode": int, "stdout": stdout[-8000:], "stderr": stderr[-3000:], "cwd": str(home)}`.
On `TimeoutExpired`: `{"error": f"timed out after {timeout}s", "hint": "run it in the background with nohup … & and poll"}`.
`home = Path(home or Path.home()).resolve()`.

`page_text(url, timeout=15, max_chars=6000)` also lives in builtins.py but is **not registered as a tool** (used by senses/skills): returns `{"url","title","chars","text","truncated"}`.

### 3.2 `ask` (selftools.py, group `self`)

`ask(question: str, context: str = "") -> dict`. Docstring verbatim:

> Leave a question for Marlinski, your owner and mentor; he reads the inbox when he next talks to you.
> For what you cannot resolve alone: what to learn, how to behave, whether a fact is right, anything that
> needs root or a change to your body. His attention is limited: only a few questions can be open at once,
> asking when they are full drops the oldest unanswered one, and a question nobody answers expires.

Args: `question: the question, in one or two sentences`; `context: optional: what led you to ask`.
Implementation in a turn process: `daemon.op("ask", question=…, context=…)` → daemon `op_ask` → `app.inbox.add(...)` and emits an `inbox` event. Without a `put` callable it writes `Inbox(state).add(...)` directly.

### 3.3 `think` (mindtools.py `make_think_tool`, group `mind`)

`think(goal: str, max_steps: int = 8) -> dict`. Docstring verbatim:

> Start an inner thought: a separate line of reasoning that works toward a goal in the background,
> in its own process, with its own shell. It cannot talk to anyone; it reports back to you with focus
> and finish, and you get a reminder every few steps. Its trace is state/thoughts/<id>.json;
> `groow thoughts` lists them, `groow thought pause|resume|kill <id>` manages them. Use it for anything
> that takes several steps and does not need the person waiting.

Args: `goal: what the thought should achieve, concretely`; `max_steps: budget in steps (default 8, max 60)`.
Returns `spawn(goal, max_steps)` = `POST /op {"op":"think", ...}` → `Daemon.spawn_thought` → the thought brief, or `{"error": "already N thoughts running; pause or kill one first"}`.

(The unused `make_main_mind_tools` variant has a near-identical docstring, differing in one clause: “… in the background with its own tools (shell, read, learn, quiz).” and “Its trace is in state/thoughts/<id>.json”.)

### 3.4 `focus` / `finish` (mindtools.py `make_thought_tools`, group `thought`)

`focus(message: str) -> dict`:
> Send a message to the main thought now (it decides what to do with it; it may tell the person).

Args: `message: what the main thought should know, concisely`.
→ `daemon.op("thought", action="focus", id=tid, text=message)` → `ThoughtManager.focus` → pushes a `Priority.FOCUS` signal of kind `"focus"` with `meta.thought = id`; returns `{"ok": true, "delivered_to": "main thought"}`.

`finish(summary: str) -> dict`:
> End this thought and report the result to the main thought.

Args: `summary: what was found or done, concisely`.
→ `daemon.op("thought", action="finish", id=tid, text=summary)` → marks the thought `done` with the summary, pushes a `Priority.FOCUS` signal of kind `"thought_done"`; returns `{"ok": true, "thought": "<id>", "status": "done"}`.

### 3.5 `sensetools.py`

No tools. Exports `news_headlines(news: NewsSense, max_items=8) -> dict` used by the curiosity pipeline and re-exports `page_text`:
```json
{"date_today": "2026-09-18",
 "items": [{"source": "…", "date": "…", "title": "…", "summary": "…(≤400 chars)", "link": "…"}],
 "feed_errors": []}
```

### 3.6 Skill tools currently installed (`state/skills/*.py`)

`auto_register` (`harness/skillcheck.py`): if the module defines `register(reg)` it is called; otherwise **every public module-level function defined in that module, with a non-empty docstring, that is not named `register`/`main` and is not referenced by `CLI`**, becomes a tool.

With the current `state/skills`: `arithmetic.py`, `clock.py`, `news.py`, `tictactoe.py`, `web.py` expose **no tools** (only CLI commands `arithmetic`, `remind`, `schedule`, `news`, `tictactoe`, `web` written into `~/.local/bin`). The tools are:

- `list_recipes() -> dict` (recipes.py): "List your recipes: short notes on how your home, your shell, installing software, skills, learning, your mind and your mentor work. Read one with read_recipe(name)." → `{"recipes":[{"name","title"}…],"where": "<state>/recipes"}`.
- `read_recipe(name: str) -> dict`: "Read one recipe in full." Arg `name: the recipe name from list_recipes, e.g. installing`. → `{"found":true,"name","text"}` or `{"found":false,"name","available":[…]}`.
- `write_recipe(name: str, text: str) -> dict`: "Write or update a recipe (markdown). Use it when you learned how to do something in your home that you will need again." Args `name: lowercase name, letters, digits, dashes`; `text: the full markdown text`.
- `reverse_text(text: str) -> dict` (text_tools.py): "Reverse the characters of a text." Arg `text: the text to reverse`. → `{"reversed": "…"}`.

**[AMBIGUOUS/dynamic]**: the skill set is user/agent-mutable at runtime; a Rust port must treat the tool table as dynamic.

---

## 4. `/home/ubuntu/Work/github/groow/groow/turn.py` — one turn process

### 4.1 `FRAMES` verbatim

```python
FRAMES = {
    "focus": "[inner thought {thought} says] {text}",
    "thought_done": "[inner thought {thought} finished] {text}",
    "reminder": "[reminder, no reply needed] {text}. You may `groow thought read <id>` it, pause it, or ignore this.",
    "alarm": "[an alarm you set earlier] {text}",
    "note": "[a note you left yourself earlier] {text}",
    "expired": "[no answer came] {text} Your mentor's attention is limited; ask less, and ask what matters.",
    "idle": "{text}",
}
```
An identical dict exists in `groow/mind/mind.py` lines 18-26 but is **never referenced there** — **[DEAD CODE in mind.py; the framing is applied in turn.py only.]**

### 4.2 `emit`

```python
sys.stdout.write(json.dumps({"ev": ev, "t": time.time(), **data}, ensure_ascii=False, default=str) + "\n"); flush()
```
Every event line therefore has `ev` (string) and `t` (float epoch seconds) plus the payload keys below.

### 4.3 `run_turn(cfg, signal)` lifecycle

Input `signal`: `{"kind": str, "text": str, "meta": {...}}` (kind defaults `"user"`).
1. `state = Path(cfg.state)`; `kind = signal.get("kind","user")`; `req = (signal.get("meta") or {}).get("req")`; `text = signal.get("text","")`.
2. `framed = text` if `kind == "user"` else `FRAMES.get(kind, "{text}").format(text=text, thought=(signal.get("meta") or {}).get("thought", "?"))`.
3. System prompt: if env `GROOW_SAFE` set → `SAFE_PROMPT.format(incident=os.environ.get("GROOW_INCIDENT","")[:2000])` (verbatim text in the file, lines 43-45, ending "When you are done, say exactly: REPAIRED.\n\nIncident: {incident}"); else `Identity(cfg, _NoMemory(state)).system_prompt()`.
4. `tools = build_tools(cfg, state, "main", safe=bool(GROOW_SAFE))`.
5. `h = make_harness(cfg, state, prompt, tools, "main", req=req)`: `RemoteBrain(state, priority=0, actor="main", req=req)`; hooks as in §4.5.
6. `h.history = [system] + restore_window(state)`; `h.turn_kind = kind`.
7. `h.hooks.on_message` is **replaced** by `lambda m: (_journal(journal, m, kind), emit("message", role=m.get("role"), content=(m.get("content") or "")[:400]))` — journal record keeps only keys `role, content, tool_calls, name, args` (+`kind` for user messages), appended to `Journal(state/"main", max_lines=1000)`.
8. `emit("turn_start", ...)`, `await h.turn(framed)`, `emit("turn_end", ...)`, `emit("turn_done", ...)`.
9. Returns `{"final": result.final_text, "flags": result.flags}`.

`restore_window(state, n=30)`: reads the last `n*3` journal records; keeps user records whose `kind` ∈ {`user`,`command`} and assistant records that have content and **no** tool_calls; then keeps only adjacent user/assistant pairs; returns the last `n - n%2` (=30) messages. Tool messages and tool-calling assistant turns are never restored.

### 4.4 Events emitted by a main turn process (exact payload keys)

- `turn_start`: keys `who` (`"user"` if kind=="user" else `"signal"`), `kind`, `text` (the **unframed** text), `req`.
```json
{"ev":"turn_start","t":1758182400.12,"who":"user","kind":"user","text":"hello","req":"9f2a4c1b"}
```
- `message` (once per history message): keys `role`, `content` (truncated to 400 chars).
```json
{"ev":"message","t":1758182400.30,"role":"assistant","content":"Let me check."}
```
- `tool_call`: keys `name`, `args`, `actor`, `req`.
```json
{"ev":"tool_call","t":1758182401.02,"name":"shell","args":{"command":"ls state"},"actor":"main","req":"9f2a4c1b"}
```
- `tool_result`: keys `name`, `result` (JSON string truncated to 600 chars), `actor`, `req`. (No `args`.)
```json
{"ev":"tool_result","t":1758182401.44,"name":"shell","result":"{\"returncode\": 0, \"stdout\": \"main\\nskills\\n\", \"stderr\": \"\", \"cwd\": \"/home/groow\"}","actor":"main","req":"9f2a4c1b"}
```
- `turn_end`: keys `who`, `kind`, `final`, `tools_used`, `seconds`, `req`.
```json
{"ev":"turn_end","t":1758182402.10,"who":"user","kind":"user","final":"Hi!","tools_used":["shell"],"seconds":1.98,"req":"9f2a4c1b"}
```
- `turn_done` (consumed by the daemon, never rebroadcast): keys `kind`, `user_text` (the **framed** text), `final`, `flags`, `messages` (full turn messages incl. tool contents), `context` (history before the turn), `tools_used`.
```json
{"ev":"turn_done","t":1758182402.11,"kind":"user","user_text":"hello","final":"Hi!","flags":[],
 "messages":[{"role":"user","content":"hello"},{"role":"assistant","content":"Hi!"}],
 "context":[{"role":"system","content":"You are Groow…"}],"tools_used":[]}
```
- `log`: keys `level`, `text` — emitted by `main_turn` when the conscious lock is held:
```json
{"ev":"log","t":1758182400.01,"level":"warn","text":"another conscious turn is running (pid 1234); not starting a second"}
```
and by `run_thought` when the id is unknown: `{"ev":"log","t":…,"level":"error","text":"no thought abc123"}`.

`main_turn(cfg, signal)` acquires `Conscious(cfg.state)` (a pid lock). If not acquired it emits the warn `log` and returns `{"error": "conscious thread busy"}` without running. Otherwise `asyncio.run(run_turn(...))`, lock released in `finally`.

### 4.5 `make_harness` hooks (before `run_turn` overrides `on_message`)

```python
Hooks(on_message = lambda m: emit("message", **{k:v for k,v in m.items() if k != "reasoning_content"}),
      on_text    = (lambda t: None) if kind == "main" else None,
      on_tool_call   = lambda n,a: emit("tool_call", name=n, args=a, actor=actor, req=req),
      on_tool_result = lambda n,a,r: emit("tool_result", name=n, result=r[:600], actor=actor, req=req))
```
The `on_text` sentinel matters: it is a **no-op function** for `kind == "main"` purely so that `RemoteBrain.complete` sees `on_text is not None` and sets `stream_as` in the `/complete` payload — the actual streaming to clients is done by the daemon, not by the turn process. For thoughts `on_text is None` → no `stream_as` → no `text` events.

### 4.6 `run_thought(cfg, thought_id)` — how it differs

1. `ThoughtManager(state, None, lambda t: None, _NoMemory(state))`; `t = tm.get(thought_id)`; unknown → `emit("log", level="error", text=f"no thought {id}")` and return `{"error": "unknown thought"}`.
2. `t.pid = os.getpid()`; `t.status = "running" if t.status in ("running","paused") else t.status`; `tm._save(t)`.
3. `tools = build_tools(cfg, state, "thought", thought_id=t.id)`.
4. `h = make_harness(cfg, state, t.history[0]["content"], tools, "thought", actor=f"thought:{t.id}")` → `RemoteBrain(priority=2)`, no streaming, `on_message` **not** overridden (so a thought emits full `message` events including `tool_calls`, `name`, `args`; it does **not** write the main journal).
5. `h.history = t.history` (the thought's persisted conversation, system prompt = `THOUGHT_SYSTEM.format(goal=…)` from `mind/thoughts.py`).
6. Loop `while t.status == "running" and t.steps < t.max_steps`:
   - prompt = `"Begin. Plan briefly, then take the first concrete step with your tools."` if `t.steps == 0` else `f"Step {t.steps+1} of {t.max_steps}. Continue toward the goal. If the goal is reached, call finish."`
   - `result = await h.turn(prompt)`; `Interrupted` → `break`. (Note: `Harness.turn` returns rather than raising on interruption, so this `except` catches only interruptions raised outside `brain.complete`.) **[AMBIGUOUS: an interrupted turn here still increments steps? No — `break` happens before increments only if Interrupted propagates; a `TurnResult(interrupted=True)` is treated as a normal step.]**
   - `t.steps += 1`; `t.tools_used += result.tools_used`; re-read status from disk via `tm.load_one(t.id)` (the daemon may have paused/killed/finished it) — copies `status` and `summary`; `tm._save(t)`.
   - emits two events per step (below).
7. After the loop: if still `running` and `steps >= max_steps` → `status = "done"`, `summary = summary or "step budget exhausted"`. `t.pid = None`; `tm._save(t)`. If `status == "done"` → emit the `thought`/`done` event.
8. Returns `{"steps": <steps this process ran>, "status": t.status}`.

Thought-process events:
- `message`, `tool_call`, `tool_result` as in §4.4 but with `actor = "thought:<id>"` and `req = null`; `message` carries all message keys except `reasoning_content`:
```json
{"ev":"message","t":1758182500.4,"role":"assistant","content":"","tool_calls":[{"type":"function","function":{"name":"shell","arguments":{"command":"ls"}}}]}
```
- `thought` step:
```json
{"ev":"thought","t":1758182502.0,"event":"step","id":"a1b2c3","status":"running","goal":"find the changelog","steps":"1/8","text":"I listed the directory…"}
```
(`goal` ≤140 chars, `text` = `result.final_text[:300]`, `steps` is the string `"<done>/<max>"`).
- `thought_step` (consumed by the daemon for episodic memory, not rebroadcast): keys `id`, `messages`, `tools_used`.
```json
{"ev":"thought_step","t":1758182502.0,"id":"a1b2c3","messages":[…],"tools_used":["shell"]}
```
- `thought` done: same key set as step with `"event":"done"`, `status` and `text = t.summary`.

`main_thought(cfg, id)` = `asyncio.run(run_thought(...))` — **no conscious lock**.

---

## 5. `/home/ubuntu/Work/github/groow/groow/gateway/daemon.py`

### 5.1 Route table (registered in `run()`)

| Method | Path | Handler |
|---|---|---|
| GET | `/hello` | `h_hello` |
| GET | `/` | `h_hello` |
| GET | `/status` | `h_status` |
| POST | `/say` | `h_say` |
| POST | `/command` | `h_command` |
| POST | `/ask` | `h_ask` |
| POST | `/op` | `h_op` |
| POST | `/complete` | `h_complete` |
| POST | `/emit` | `h_emit` |
| GET | `/events` | `h_events` (SSE) |
| GET | `/ws` | `h_ws` (WebSocket) |

`web.Application(client_max_size=2**20)` (1 MiB request cap), `AppRunner(access_log=None, shutdown_timeout=2.0)`, `TCPSite(cfg.api_host, cfg.api_port)` (default `127.0.0.1:7373`). On start the daemon writes `state/groow.url` (`http://<host>:<port>`, with `0.0.0.0` displayed as `127.0.0.1`) and `state/groow.pid`. Both are unlinked on shutdown. All JSON responses use `dumps=encode` (`json.dumps(obj, ensure_ascii=False, default=str)`) except `/say`, `/command`, `/emit` which use aiohttp's default dumps.

### 5.2 `GET /hello` and `GET /`

Response = `event("hello", …)`:
```json
{"ev":"hello","t":1758182400.0,
 "birth":{...},"identity":"…","model":"Qwen/Qwen3-4B",
 "tools":["ask","list_recipes","read_recipe","reverse_text","shell","think","write_recipe"],
 "safe_mode":false,"status":{...as /status…},"inbox":[...],"url":"http://127.0.0.1:7373"}
```
`tools` = `sorted(app.tools.names())`; `url` = contents of `state/groow.url` or `""`.

### 5.3 `GET /status`

Response = `event("status", **self.status())`:
```json
{"ev":"status","t":1758182400.0,"mood":"listening","feeling":{},"weights_busy":false,
 "body":"host","home":"/home/groow","state":"/srv/groow/state",
 "steps":128,"nights":3,"rank":32,"tokens_seen":940122,"age":"4 days",
 "safe_mode":false,"learning":true,"thoughts":[],"thoughts_running":0,"queue":0,
 "server":{"batches":12,"requests":30,"preempted":1,"max_batch_seen":3},
 "gpu_gb":7.8,"clients":2,"identity_version":1,"skills":["recipes","text_tools"],"handled":57}
```
Side effect: if `mood == "listening"` and no human for >60 s and no thought running → `mood = "idle"`.
Mood machine (`_update_mood`, applied to every emitted event): `turn_start` → `"reading"` if `kind=="idle"` else `"thinking"`; `text` → `"speaking"`; `tool_call` → `"learning"` if name ∈ `LEARN_TOOLS = {"memorize","learn_fact","play","quiz","consolidate","grow","probe"}`, `"reading"` if ∈ `READ_TOOLS = {"news_headlines","read_article","recall","read_thought","read_skill","read_incidents"}`, else `"tooling"`; `weights` busy → `"sleeping"` at night else `"napping"`, not busy and currently napping → `"listening"`; `learned`/`felt` → `"learning"` unless napping; `turn_end` → `"listening"`; `sleep` → `_night = phase != "done"`, mood `"sleeping"`/`"listening"`; `thought` with event ∈ {spawn, step} while mood ∈ {idle, listening} → `"dreaming"`. In safe mode mood is forced to `"repair"`.

### 5.4 `POST /say`, `POST /command`, `POST /ask`

`_enqueue(request, force_command)`: body `{"text": str}`; `text.strip()`; empty → **400** with body `{"error": "text is required"}` (content-type application/json). If `force_command` and text doesn't start with `/` → prefix `/`. `req = uuid.uuid4().hex[:8]`; `mind.push_user(text, req=req)`.

- `POST /say` request `{"text":"hello"}` → `{"queued": true, "req": "9f2a4c1b", "queue": 0}`.
- `POST /command` request `{"text":"/stats"}`:
  - If text ∈ `("/quit","/stop","/exit","/restart")` → `mind.stop(restart = text=="/restart")` and `{"stopping": true, "restart": false}`.
  - else enqueued as a command → `{"queued": true, "req": "9f2a4c1b"}` (no `queue` key).
- `POST /ask` request `{"text":"…","timeout":600}` (timeout optional, `float(body.get("timeout") or 600)`):
  - registers `waiters[req] = future` and `req_events[req] = []`, pushes the user message, awaits the future with `asyncio.wait_for(fut, timeout)`. The future is resolved by `_fanout` when a `turn_end` event carrying the same `req` passes through.
  - Success 200: `{"req": "9f2a4c1b", "final": "Hi!", "tools_used": ["shell"], "seconds": 1.98, "events": [ …all events of that req except ev=="text"… ]}`.
  - Timeout **504**: `{"error": "timeout", "req": "9f2a4c1b", "events": [ …everything collected so far, including text events… ]}`.
  - In both cases the waiter and the buffer are removed in `finally`.

### 5.5 `POST /complete` — the only model call

Request JSON:
```json
{"messages": [{"role":"system","content":"…"},{"role":"user","content":"hi"}],
 "tools": [ …tool schemas… ],
 "priority": 0,
 "trim": true,
 "stream_as": {"actor": "main", "req": "9f2a4c1b"},
 "max_new_tokens": 768,
 "temperature": 0.7,
 "enable_thinking": false}
```
Semantics:
- `convs = body.get("messages") or []`; if `convs` is a dict **or** its first element is a dict → wrap as `[convs]` (a single conversation). So the field is either one conversation or a list of conversations.
- `if not convs or len(convs) > 32` → **400** `{"error": "messages: a conversation or a list of at most 32"}`.
- `max_new = int(body.get("max_new_tokens") or cfg.max_new_tokens)` (768).
- `temperature`, `enable_thinking` passed through as-is (may be `None` → brain defaults `cfg.temperature=0.7`, `cfg.enable_thinking=False`).
- `stream = body.get("stream_as") or None`.
- If `body.get("trim")` truthy → each conversation goes through `_trim` (§5.6).
- `on_text` is set only when `stream` is truthy:
  `on_text = lambda t: self.emit("text", delta=t, actor=stream.get("actor","main"), req=stream.get("req"))`
  and is attached **only to conversation index 0** of the batch. This is how token deltas reach clients: the generation streamer (`_CallbackStreamer` in `brain/model.py`, `skip_prompt=True, skip_special_tokens=True`) calls it per decoded chunk, from the GPU thread; `emit` hops to the event loop with `call_soon_threadsafe` and fans out to every SSE/WS subscriber as:
```json
{"ev":"text","t":1758182401.7,"delta":"Hello","actor":"main","req":"9f2a4c1b"}
```
- All conversations are generated concurrently via `asyncio.gather(app.server.complete(...))` with `priority=int(body.get("priority", 2))`.
- Success 200: `{"completions": ["<raw assistant text>", …]}` (same order as input).
- Any exception (including `Interrupted` from preemption) → 200 with `{"interrupted": true, "reason": "Interrupted: preempted"}` — the client (`RemoteBrain`) turns this into `Interrupted`.

Note: streamed `text` events are emitted **even though the turn process discards them** (`on_text = lambda t: None`); the delta stream exists purely for UIs.

### 5.6 `_trim(messages, tools, max_new)`

```
budget = cfg.max_seq_len - max_new         # 32768 - max_new
msgs = list(messages)
while len(msgs) > 3:
    text = brain.prompt_text(msgs, tools=tools)
    if len(brain.tok(text, add_special_tokens=False)["input_ids"]) <= budget: break
    head = msgs[:1] if msgs[0]["role"] == "system" else []
    msgs = head + trim_messages(msgs[len(head):], max(2, len(msgs) - len(head) - 3))
return msgs
```
i.e. repeatedly drop ~3 of the oldest non-system messages until the rendered prompt fits, never going below 4 messages total. `trim_messages` additionally refuses to start the tail on a `tool` message or on an assistant message carrying `tool_calls` (§8.4).

### 5.7 `POST /emit`

Request: any JSON object; `ev = body.pop("ev", "log")`; the rest becomes the payload. Response `{"ok": true}`.
```json
// request
{"ev":"log","level":"info","text":"skill installed"}
// response
{"ok":true}
```

### 5.8 `POST /op`

Request `{"op": "<name>", "args": {...}}` (args optional). Runs `run_op(app, name, args)` (§7). Response = the op's dict, status **200** if it has no `"error"` key, **400** if it does.
```json
// request
{"op":"thoughts","args":{"all":false}}
// response 200
{"thoughts":[{"id":"a1b2c3","status":"running","goal":"find the changelog","steps":"2/8","summary":""}]}
// response 400
{"error":"unknown op 'nope'","available":["ask","feedback","hippocampus","identity","inbox","incidents","learn","patch","probe","quiz","remind","schedule","skill","sleep","stats","think","thought","thoughts","train","training"]}
```

### 5.9 `GET /events` — SSE framing

Query: `?replay=N` (default `60`; `0` disables replay). Headers: `Content-Type: text/event-stream`, `Cache-Control: no-cache`, `X-Accel-Buffering: no`.
Frame (`protocol.sse`): `f"event: {e['ev']}\ndata: {encode(e)}\n\n"` encoded UTF-8. Example bytes:
```
event: text
data: {"ev": "text", "t": 1758182401.7, "delta": "Hello", "actor": "main", "req": "9f2a4c1b"}

```
Sequence: first frame = the `hello` event; then `replay(N)` reconstructed events; then live events from a per-subscriber `asyncio.Queue`. Every 15 s without an event a keepalive comment `b": keepalive\n\n"` is written. The stream terminates after forwarding an event with `ev == "bye"`. Subscribers whose queue exceeds 2000 items are dropped silently.

`replay(n)` rebuilds from the main journal:
- user record → `{"ev":"turn_start","t":<ts>,"who":"user"|"signal","kind":<rec kind>,"text":<content>,"replay":true}` (`who` = `"user"` if kind ∈ {user, command} else `"signal"`).
- assistant record with content → `{"ev":"text","t":…,"delta":<content>,"replay":true}`; each tool call → `{"ev":"tool_call","t":…,"name":…,"args":…,"actor":"main","replay":true}`; if no tool_calls → `{"ev":"turn_end","t":…,"final":<content>,"replay":true}`.
- tool record → `{"ev":"tool_result","t":…,"name":…,"result":<content ≤600>,"actor":"main","replay":true}`.

### 5.10 `GET /ws` — WebSocket framing

`web.WebSocketResponse(heartbeat=20)`; query `?replay=N` default **200**.
Server→client: one **text** frame per event, body = `encode(event_dict)` (the same JSON object as SSE, without the `event:`/`data:` wrapper). Order: `hello`, then replay events, then live events. On an event with `ev == "bye"` the socket is closed.
Client→server: text frames of JSON `{"cmd": "say"|"command"|"status", "text": "…"}`; non-TEXT frames and unparsable JSON are ignored.
- `cmd == "command"` and text ∈ `("/quit","/stop","/exit","/restart")` → `mind.stop(restart=text=="/restart")`.
- `cmd ∈ ("say","command")` with non-empty text → `mind.push_user(text if cmd=="say" or text.startswith("/") else "/"+text)` — **note: no `req` correlation id is assigned on the WS path.**
- `cmd == "status"` → sends one `{"ev":"status", …}` frame immediately.
On disconnect the writer task is cancelled and the queue unsubscribed.

### 5.11 `_child_env` and `_run_child`

`_child_env()` = `dict(os.environ)` plus: `GROOW_SAFE="1"` and `GROOW_INCIDENT=json.dumps(incident)[:3000]` when in safe mode; `GROOW_STATE=<abs state>`; `GROOW_URL=<state/groow.url or http://127.0.0.1:{api_port}>`; `PYTHONUNBUFFERED="1"`.

`_run_child(args, on_event, timeout=1800.0)`:
```python
proc = await asyncio.create_subprocess_exec(sys.executable, "-m", "groow.cli", *args,
        cwd=str(Path(cfg.state).parent), env=self._child_env(),
        stdout=PIPE, stderr=PIPE)
```
- Registered in `self.children`.
- `pump()`: `async for line in proc.stdout`, `json.loads(line)`; unparsable lines skipped silently; `on_event(ev)`; an exception inside `on_event` emits `{"ev":"log","level":"warn","text":"event handling failed: <Type>: <msg>"}`.
- `await asyncio.wait_for(asyncio.gather(pump(), proc.wait()), timeout)`.
  - `TimeoutError` → `proc.kill()` and `log/error "a <args[0]> process ran past <timeout>s and was killed"`.
  - `CancelledError` → `proc.kill()` and re-raise.
- `finally`: discard from `children`; read up to the last 2000 bytes of stderr; if `returncode not in (0, None)` and stderr non-blank → emit `{"ev":"log","level":"error","text":"<args[0]> exited <rc>: <stderr tail ≤400>"}` and `app.skills.record_incident(f"child_{args[0]}", err)`.
- Returns `proc.returncode or 0`.

`run_turn(signal)`: `self.turn_running = True`; `_run_child(["turn", "--signal", json.dumps(signal, default=str)], on_event, timeout=cfg.turn_timeout)` (**900 s**). `on_event`:
- `ev == "turn_done"` → captured into `done`, **not** rebroadcast.
- `ev == "message"` → dropped (already journalled by the child).
- everything else → `self.emit(ev.pop("ev"), **{k:v for k,v in ev.items() if k != "t"})` — i.e. the daemon **re-stamps `t`** with its own time.
Afterwards, if `done` → `await self.after_turn(done)`.

`after_turn(done)`: skipped if `not app.learning_enabled or not done.get("messages")`. Else on the GPU executor: `app.hippocampus.nap(messages, context, flags, kind=…, user_text=…, final_text=…)` then `app.trainer.consume(max_samples=cfg.nap_max_samples)`. Failure → `log/error "nap failed: …"` and return. Success → emit
```json
{"ev":"felt","t":…,"valence":0.3,"pending":false,"mood":{...},"consumed":4,"sets":{"…":1}}
```
and `app.brain.save()` every 10 learning steps.

`run_thought(thought_id)`: `_run_child(["think", thought_id], on_event, timeout=cfg.thought_timeout)` (**3600 s**). `on_event` intercepts `thought_step` → `app.memory.add_episode([], messages, tools_used, kind=f"thought:{thought_id}")` and does not rebroadcast; everything else (including `message`, `tool_call`, `tool_result`, `thought`) is re-emitted.

`spawn_thought(goal, max_steps=8)`: refuses with `{"error": f"already {cfg.max_thoughts} thoughts running; pause or kill one first"}` when `len(app.thoughts.running(refresh=True)) >= cfg.max_thoughts` (4); else writes the record (`ThoughtManager.spawn_record`, id = `uuid4().hex[:6]`, `max_steps` clamped to `[1,60]`, system prompt `THOUGHT_SYSTEM.format(goal=…)`), schedules `run_thought` as a task, emits
```json
{"ev":"thought","t":…,"event":"spawn","id":"a1b2c3","status":"running","goal":"find the changelog","steps":"0/8","text":""}
```
and returns the brief.

### 5.12 Event fan-out internals

`emit(ev, **data)` builds `event(ev, **data)` = `{"ev":ev,"t":time.time(), **data}`. If called from the main thread → `_fanout` directly; otherwise `loop.call_soon_threadsafe(self._fanout, e)`. `_fanout` updates the mood, appends to `req_events[req]` if that req is being awaited, resolves the `/ask` future on `turn_end`, then `q.put_nowait(e)` for each subscriber (dropping any with `qsize() > 2000`), and prints `encode(e)[:300]` when `verbose` and `ev not in ("text","status")`.

Background: `_status_loop` emits a `status` event every 2.0 s. `housekeeping()` expires unanswered inbox questions and pushes one `expired` signal at priority 2 with `questions=[ids]`. `on_user(text)` runs `app.detect_answer(text)`.

Shutdown: `emit("bye")` (payload: only `ev` and `t`), thoughts paused, children `terminate()`d, server stopped, brain saved, url/pid files removed, hard `os._exit(0)` timers at 20 s and 2 s.

**[DEAD CODE]** `self._stopping = asyncio.Event() if False else None` → always `None`, never used.

---

## 6. `gateway/protocol.py` and `gateway/client.py`

### 6.1 protocol.py

```python
def encode(obj: dict) -> str:  return json.dumps(obj, ensure_ascii=False, default=str)
def decode(text) -> dict|None: json.loads or None on JSONDecodeError/TypeError
def event(ev, **data) -> dict: {"ev": ev, "t": time.time(), **data}
def sse(e) -> bytes:           f"event: {e['ev']}\ndata: {encode(e)}\n\n".encode()
MOODS = ("idle","listening","thinking","tooling","speaking","learning","reading","dreaming","napping","sleeping","repair")
```
The module docstring is the normative documentation of routes and event types; it lists event payloads: `hello`, `status`, `turn_start {who,text,kind,req?}`, `text {delta,req?}`, `turn_end {final,tools_used,seconds,req?}`, `tool_call {name,args,actor}`, `tool_result {name,result,actor}`, `learned {loss,tokens,step,probe?}`, `felt {valence,pending,mood,consumed,sets}`, `thought {id,status,goal,steps,event,text?}`, `sleep {phase,...}`, `weights {busy,op}`, `inbox {questions}`, `log {level,text}`, `bye`.
**[Docstring vs code discrepancies to flag]**: `tool_call`/`tool_result` also carry `req`; `turn_start`/`turn_end` also carry `kind`; the `learned` event is produced elsewhere (learning code), not in the files reviewed.

### 6.2 client.py

`Client(base_url)`; `aiohttp.ClientSession(timeout=ClientTimeout(total=None, connect=5))`; async context manager closing the WS then the session.
- `hello()` → `GET /hello` JSON.
- `status()` → `GET /status` JSON.
- `say(text)` → `POST /command` if `text.startswith("/")` else `POST /say`, body `{"text": text}`.
- `ask(text, timeout=600)` → `POST /ask` body `{"text": text, "timeout": timeout}`.
- `events(replay=0)` → `GET /events?replay=N`, streams `r.content.iter_any()`, buffers on `"\n\n"`, and for each block yields `decode(line[5:].strip())` for lines starting with `data:` (the `event:` line is ignored). Non-decodable payloads are skipped.
- `ws_events(replay=120)` → `ws_connect(base.replace("http","ws",1) + f"/ws?replay={N}", heartbeat=20)`, yields decoded TEXT frames, breaks on CLOSED/ERROR.
- `ws_send(text)` → `{"cmd": "command" if text.startswith("/") else "say", "text": text}`; raises `ConnectionError("websocket not open")` if no socket.
- `base_url(cfg_or_state)` → contents of `<state>/groow.url` if present, else `http://{127.0.0.1 if host=="0.0.0.0" else host}:{port}` from the config (defaults `127.0.0.1:7373`).

The **other** client is `groow/remote.py` (used by turn processes, blocking urllib, 900 s timeout): `Daemon.op(name, **args)` → `POST /op {"op":name,"args":args}` with HTTPError bodies parsed as JSON (falling back to `{"error": "HTTP <code>"}`) and any other exception → `{"error": "<Type>: <msg>"}`; `Daemon.event(ev, **data)` → `POST /emit` (10 s, failures ignored); `RemoteBrain.complete` builds
```json
{"messages": [...], "tools": [...], "priority": 0, "trim": true,
 "stream_as": {"actor": "main", "req": "9f2a4c1b"}}
```
(`stream_as` is `null` when `on_text is None`; `max_new_tokens`/`temperature`/`enable_thinking` are added only when not `None`), posts via `asyncio.to_thread`, raises `Interrupted("the daemon did not answer: …")` on transport failure, raises `Interrupted(reason)` when the response has `interrupted`, else returns `completions[0]` (or `""`). `RemoteBrain.complete` accepts `should_stop` and **ignores it** — cancellation inside a turn process is not possible; preemption is decided server-side.

---

## 7. `/home/ubuntu/Work/github/groow/groow/ops.py` — the ops table

Routing: `run_op(app, name, args)`; unknown → `{"error": f"unknown op {name!r}", "available": sorted(OPS)}`. `ASYNC_OPS = {"sleep"}` are awaited directly; all others run in `loop.run_in_executor(executor, …)` with `executor = app.server.gpu` when the name ∈ `GPU_OPS = {"train","hippocampus","probe","identity","learn","quiz","feedback"}`, else the default executor. `TypeError` → `{"error": f"bad arguments for {name}: {e}"}`; any other exception → `{"error": f"{type(e).__name__}: {e}"}`.

| op | args (defaults) | behaviour / return |
|---|---|---|
| `train` | `urgent_only=False`, `max_samples=64` | `app.trainer.consume(...)` with a progress callback emitting `log/progress` events, then `brain.save()`; returns the trainer report. |
| `training` | – | `{"sets": app.sets.counts(), "hippocampus_last": <ts>}` |
| `hippocampus` | – | `app.hippocampus.run(on_progress=…)` |
| `sleep` | `force=True` | async; `{"slept": <bool>}` |
| `probe` | – | `app.learner.probe()` |
| `stats` | – | `app.learner.report()` + `tool_usage`, `generation_server`, `thoughts`, `skills` |
| `thoughts` | `all=False` | `{"thoughts": app.thoughts.listing(all)}` |
| `think` | `goal=""`, `max_steps=8` | `app.daemon.spawn_thought(goal, int(max_steps))` |
| `thought` | `action="read"`, `id=""`, `last=10`, `text=""` | `focus`→`t.focus(id,text)`; `finish`→`t.finish(id,text)`; `resume`→`t.resume(id)` and, if it became running, schedule `daemon.run_thought`; `read`→`t.trace(id,last)`; `pause`→`t.pause(id)`; `kill`→`t.kill(id,"killed by command")`; unknown → `{"error": f"unknown action {action}; read|pause|resume|kill"}`. **[DEAD CODE: a second `if action == "resume"` branch after `pause` is unreachable.]** |
| `skill` | `action="list"`, `name=""`, `path=""` | `list`→`sk.listing()`; `read`→`sk.read(name)`; `check`→ draft from `path` or `_drafts/<name>.py`, missing → `{"error": f"no file {src}"}`, then `sk.draft(...)`; `install`→ optionally draft from `path` first (abort if the draft fails), then `sk.install(name)`; `disable`→`sk.disable(name,"disabled by command")`; `rollback`→`sk.rollback(name)`; unknown → `{"error": f"unknown action {action}; list|read|check|install|disable|rollback"}` |
| `identity` | – | `{"identity": text, "versions": …, "internalised_loss": round(...,3), "birth": card}` |
| `ask` | `question=""`, `context=""` | empty question → `{"error": "a question is needed"}`; else `app.inbox.add(...)`, emits `{"ev":"inbox","questions":[…],"asked":"<id>"}`, returns the inbox receipt. |
| `inbox` | `clear=False`, `answer=""`, `id=""`, `drop=""`, `all=False` | `clear`→`{"cleared": n, **listing(all)}`; `drop`→ resolve as "dropped" + listing; `id`+`answer`→ resolve as "answered" + listing; else listing. |
| `incidents` | `last=3` | `{"incidents": [...]}` |
| `patch` | `path=""`, `description=""`, `patch=""` | `app.skills.propose_patch(...)`; path must match `^groow/[A-Za-z0-9_/]+\.py$`. |
| `learn` | `question=""`, `answer=""`, `source=""`, `target_loss=0.0` | missing q or a → `{"error": "question and answer are required"}`; builds `[system REHEARSAL_SYSTEM, user question, assistant answer(+" (Source: X.)")]`, appends an urgent sft sample to set `facts` with `target_loss = target_loss or 0.2`, adds a lesson, `trainer.consume(urgent_only=True)`, `brain.save()`. |
| `quiz` | `question=""`, `expected=""` | `app.learner.quiz(question, expected or None)` |
| `remind` | `text=""`, `when=""`, `every=""`, `by="groow"` | `app.schedule.add(...)` |
| `schedule` | `action="list"`, `id=""` | `cancel`→`app.schedule.cancel(id)`; else `app.schedule.listing()` |
| `feedback` | `value=1` | no last episode → `{"error": "nothing to rate yet"}`; else `app.learner.feedback(app.last_episode, int(value))` |

Every op accepts and ignores extra kwargs (`**_`), so unknown args never error.

---

## 8. `groow/brain/server.py` and `groow/brain/chatfmt.py`

### 8.1 Request/response shape at the generation server

```python
async GenServer.complete(messages, tools=None, *, priority=2, max_new_tokens=None,
                         temperature=None, enable_thinking=None, on_text=None, should_stop=None) -> str
```
- `prompt = brain.prompt_text(messages, tools, enable_thinking)` → the rendered chat string (§8.3).
- A `_Req(priority, seq, prompt, max_new_tokens or cfg.max_new_tokens, cfg.temperature if temperature is None else temperature, on_text, future)` is appended to `_pending`, `stats["requests"] += 1`, `_wake.set()`.
- The caller polls `asyncio.wait_for(asyncio.shield(req.future), timeout=0.25)` in a loop; every 0.25 s it evaluates `should_stop()` and, if true, sets `req.cancelled = True` and raises `Interrupted`. `asyncio.CancelledError` also marks it cancelled.
- Returns the decoded assistant text (no special tokens).

Priorities used: **0 = the main conscious turn** (served alone, streaming, preempts), **2 = inner thoughts and generic `/complete` callers** (default `priority` in `h_complete` is `2`).

### 8.2 Scheduler / batching

- Worker loop: if nothing pending, wait on `_wake`. If no priority-0 request is pending, `await asyncio.sleep(gather_s)` (`gather_ms=60` → 0.06 s) so concurrent thoughts land in one batch.
- `_take()`: drop cancelled; `top = min(priority)`. If `top == 0` → take the single lowest-`seq` priority-0 request (batch of one, streaming allowed). Else take up to `max_batch` (`cfg.gen_max_batch = 8`) requests of that priority ordered by `seq`, restricted to those with the **same `max_new_tokens` and the same `temperature`** as the first.
- `should_stop` for the batch: priority > 0 → `has_urgent(prio) or all cancelled`; priority 0 → `all cancelled`. This is polled by `_InterruptCriteria` **every generated token**.
- The batch runs in `run_in_executor(self.gpu, self._run, batch, should_stop)`; `self.gpu` is a 1-worker `ThreadPoolExecutor` named `groow-gpu` — the only place CUDA work happens (training uses it too via `run_gpu`).
- `Interrupted` → the non-cancelled requests are pushed back onto `_pending` (`stats["preempted"] += 1`), nothing is consumed. Other exceptions are set on every future. Success → futures resolved in order; `stats["batches"] += 1`, `max_batch_seen` updated.
- `_run`: holds `brain._lock`; batch of 1 → `_generate_text(prompt, max_new, temp, on_text, should_stop)`; batch of n → `_generate_texts(prompts, batch[0].max_new_tokens, batch[0].temperature, should_stop)` (left padding, no streaming).

`ServedBrain(brain, server, priority)` proxies every other attribute to the real `Brain` and injects the default priority into `complete`.

### 8.3 Sampling parameters (`brain/model.py`)

```python
def _sampling(temperature, top_p, top_k):
    if temperature <= 0:
        return {"do_sample": False, "temperature": None, "top_p": None, "top_k": None}
    return {"do_sample": True, "temperature": temperature, "top_p": top_p, "top_k": top_k}
```
Chat generation always uses `cfg.top_p = 0.8` and `cfg.top_k = 20` with the request's temperature (default `cfg.temperature = 0.7`), `max_new_tokens` default `768`, `pad_token_id = tok.pad_token_id`, `use_cache=True`, tokenization with `add_special_tokens=False`, decoding with `skip_special_tokens=True` of the tokens after the prompt. Tokenizer `padding_side = "left"`. Streaming uses `_CallbackStreamer(TextStreamer, skip_prompt=True, skip_special_tokens=True)`, calling back on each finalized non-empty chunk. Interruption is a `StoppingCriteria` that returns a per-sequence bool tensor and sets `fired`; after `generate` returns, `Interrupted()` is raised.
(`generate_batch`, used for self-play/RL, differs: `_sampling(temperature, 1.0, 0)` — no nucleus/top-k filtering — and keeps the EOS token.)

### 8.4 Chat template and how tool schemas are rendered

```python
def render(tok, messages, tools=None, add_generation_prompt=False, enable_thinking=False) -> str:
    return tok.apply_chat_template(messages, tools=tools or None,
                                   add_generation_prompt=add_generation_prompt,
                                   enable_thinking=enable_thinking, tokenize=False)
```
`Brain.prompt_text(messages, tools, enable_thinking=None)` → `render(..., add_generation_prompt=True, enable_thinking=cfg.enable_thinking if None)`.

Format assumed everywhere in this repo (Qwen/ChatML):
- `IM_START = "<|im_start|>"`, `IM_END = "<|im_end|>"`, spans matched by `SPAN_RE = re.compile(r"<\|im_start\|>(\w+)\n(.*?)<\|im_end\|>", re.DOTALL)`.
- Tool **responses** are rendered by the Qwen template inside a `user` turn wrapped in `<tool_response>` — `chatfmt.build_sample` detects `role == "user" and body.lstrip().startswith("<tool_response>")` and re-labels it `tool`.
- Tool **calls** appear in assistant bodies as `<tool_call>{json}</tool_call>` — matching the loop's parser.

**[AMBIGUOUS — must be resolved from the model's own `chat_template.jinja`, not from this repo]**: the exact text the `tools=[…]` argument produces (the Qwen3 template emits a `# Tools` section inside the system message containing `<tools>` … one JSON object per line … `</tools>` plus the instruction to answer with `<tool_call>{"name": …, "arguments": …}</tool_call>`). This repo never formats tools itself; a Rust port must either embed the tokenizer's jinja template or reimplement it byte-for-byte, otherwise training and inference distributions diverge (the code comments explicitly require them to match).

Also note the `enable_thinking` kwarg is passed to `apply_chat_template` unconditionally; with `cfg.enable_thinking = False` (default) the Qwen3 template suppresses the `<think>` block, but `parse_generation` still strips one if present.

### 8.5 `build_sample` (training-side, for completeness)

Renders the conversation (no generation prompt), tokenizes with `return_offsets_mapping=True, add_special_tokens=False`, builds a character-weight array from `SPAN_RE` spans using `cfg.role_weights` (`user 0.5, assistant 1.0, tool 0.0, system 0.0, tool_call_only 0.2`), where:
- an assistant span whose body contains `<tool_call>` and whose visible text (after removing `<tool_call>…</tool_call>`, `<think>`, `</think>`, stripped) is empty gets the `tool_call_only` weight;
- assistant spans are weighted from `m.start(2)` to `m.end()` (**including `<|im_end|>`**, so the model learns to stop); other roles from `m.start(2)` to `m.end(2)`.
Tokens take `max(char_w[s:e])`; zero-length offsets get 0.0; if longer than `max_len` the **tail** is kept.

`trim_messages(messages, keep_last)`: if `len(messages) <= keep_last + 1` return a copy; else keep `messages[0]` if it is a system message, take the last `keep_last` of the rest, then drop leading messages while the first is a `tool` message or an assistant message with `tool_calls`.

---

## 9. Cross-cutting constants a port needs (`groow/config.py`)

`max_seq_len 32768`, `max_new_tokens 768`, `max_tool_rounds 10`, `tool_timeout 120`, `temperature 0.7`, `top_p 0.8`, `top_k 20`, `enable_thinking False`, `turn_timeout 900.0`, `thought_timeout 3600.0`, `max_thoughts 4`, `thought_reminder_every 3`, `gen_max_batch 8`, `nap_max_samples 8`, `skills_enabled True`, `allow_shell True`, `api_host "127.0.0.1"`, `api_port 7373`, `state_dir "state"`, `model_id "Qwen/Qwen3-4B"`. Config is loaded from `groow.json` in the cwd, only keys that already exist on the dataclass are applied.

Signal priorities (`groow/mind/signals.py`): `USER 0`, `FOCUS 1`, `REMINDER 2`, `IDLE 3`, `HOUSEKEEPING 4`. The mailbox is a maildir under `state/mailbox/{new,cur}`; `cur/*.json` left over from a crash is moved back to `new/` at startup. `Mind.handle` special-cases: `kind=="user"` starting with `/` → run as a slash command in-process (emits `turn_start`/`turn_end` with `kind:"command"`, `final:""`, `tools_used:[]`, `seconds:0`) and no turn process is spawned; `kind=="reminder"` while a FOCUS signal is queued → dropped; `kind ∈ ("housekeeping","noop")` → dropped.

---

## 10. Open questions / ambiguities to resolve before porting

1. **Tool-call JSON regex**: `\{.*?\}` is lazy and non-balanced. Arguments containing nested objects (e.g. `{"name":"x","arguments":{"a":1}}`) *do* parse because the lazy match still extends to the first `}` that is followed by `</tool_call>`… but only because of the anchoring `\s*</tool_call>`. Any `}` followed by whitespace and `</tool_call>` ends the match; a `</tool_call>`-free trailing block falls to the recovery path. A Rust port should replicate the regex semantics literally rather than using a JSON scanner.
2. **`enable_thinking` and `<think>`**: `parse_generation` strips everything up to and including the first `</think>`; if the model emits `<think>` without a closing tag, nothing is stripped and the reasoning leaks into `content`. Intentional?
3. **`should_stop` is dead in the deployed path** (`RemoteBrain` ignores it); it only works for in-process `ServedBrain`. Should the Rust turn runner support interruption at all?
4. **`after_turn` hook is never wired in `turn.py`**; learning is driven by the daemon from the `turn_done` event. The `Hooks.after_turn`/`on_error` machinery in `loop.py` is therefore unexercised in production.
5. **`mind.py::FRAMES` is dead** (duplicate of `turn.py::FRAMES`). Which is authoritative? They are currently byte-identical.
6. **WS `say`/`command` assigns no `req`**, so WS clients cannot correlate a turn; SSE/`/ask` clients can.
7. **`op_thought` has an unreachable duplicate `resume` branch**, and `action="focus"`/`"finish"` are accepted from `/op` (used by thought processes) but not advertised in the error string.
8. **Skill tools are exposed to inner thoughts** despite docstrings in `harness/__init__.py`, `mindtools.py` and `turn.py` saying a thought only gets the substrate plus focus/finish.
9. **`_run_child` default timeout 1800 s is never used** (callers always pass `turn_timeout`/`thought_timeout`).
10. **Exact rendered prompt for tool schemas** comes from the HF tokenizer's jinja chat template (Qwen3), not from this repo — the single biggest external dependency for a faithful Rust reimplementation.
11. `h_complete`'s conversation-vs-list detection (`isinstance(convs[0], dict)`) makes a batch whose first element is a dict indistinguishable from a single conversation; batches must therefore be `[[{…}], [{…}]]` and are correctly detected only because `convs[0]` is then a list.