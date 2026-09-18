"""The harness: the loop that drives one conversation.

    generate  →  parse <tool_call> blocks  →  execute  →  feed <tool_response>  →  repeat
    until the model answers without calling a tool (or the round budget runs out),
    then hand the finished turn to `after_turn` (where learning happens).

`turn()` is a coroutine: generation awaits the shared GenServer (so several
harnesses, main thought and inner thoughts, run concurrently and get batched),
tool calls run in an executor (GPU executor for tools that train; default
executor for I/O). The harness knows nothing about learning or weights.
"""
from __future__ import annotations

import asyncio
import json
import os
import re
import shutil
import time
from dataclasses import dataclass, field
from typing import Callable

from ..errors import Interrupted
from ..config import Config
from .registry import ToolRegistry

TOOL_CALL_RE = re.compile(r"<tool_call>\s*(\{.*?\})\s*</tool_call>", re.DOTALL)
THINK_RE = re.compile(r"<think>(.*?)</think>", re.DOTALL)

Messages = list[dict]


@dataclass
class Hooks:
    on_message: Callable[[dict], None] | None = None                   # every message the moment it enters the history
    on_text: Callable[[str], None] | None = None                       # streamed text chunks
    on_tool_call: Callable[[str, dict], None] | None = None            # before a tool runs
    on_tool_result: Callable[[str, dict, str], None] | None = None     # after it ran
    after_turn: Callable[["TurnResult"], None] | None = None           # the finished turn (learning lives here)
    on_error: Callable[[str, Exception], None] | None = None           # a hook failed (the turn itself is fine)


@dataclass
class TurnResult:
    user_text: str
    context: Messages            # conversation before this turn (includes system prompt)
    messages: Messages           # the turn itself: user, assistant(s), tool(s)
    final_text: str
    tools_used: list[str] = field(default_factory=list)
    rounds: int = 0
    seconds: float = 0.0
    interrupted: bool = False
    flags: list[str] = field(default_factory=list)      # "repeat", "tool_error", "exhausted": a turn not worth learning from
    extra: dict = field(default_factory=dict)


def parse_generation(raw: str) -> tuple[str, str, list[dict]]:
    """Split raw model output into (reasoning, visible content, tool_calls)."""
    reasoning = ""
    m = THINK_RE.search(raw)
    if m:
        reasoning = m.group(1).strip()
        raw = raw[m.end():]
    calls = []
    for cm in TOOL_CALL_RE.finditer(raw):
        try:
            obj = json.loads(cm.group(1))
        except json.JSONDecodeError:
            continue
        if isinstance(obj, dict) and "name" in obj:
            args = obj.get("arguments", {}) or {}
            if isinstance(args, str):
                try:
                    args = json.loads(args)
                except json.JSONDecodeError:
                    args = {}
            calls.append({"type": "function", "function": {"name": obj["name"], "arguments": args}})
    content = TOOL_CALL_RE.sub("", raw)
    # an unterminated <tool_call> (cut by max_new_tokens or a malformed close): try to recover the JSON, never show it
    if "<tool_call>" in content:
        head, tail = content.split("<tool_call>", 1)
        tail = tail.replace("</tool_call>", "").strip()
        try:
            obj = json.loads(tail)
            if isinstance(obj, dict) and "name" in obj:
                calls.append({"type": "function", "function": {"name": obj["name"], "arguments": obj.get("arguments", {}) or {}}})
        except json.JSONDecodeError:
            pass
        content = head
    content = content.strip()
    if content in ("}", "})", "]", "}}"):          # a stray closing bracket is not an answer
        content = ""
    return reasoning, content, calls


class Harness:
    def __init__(self, brain, tools: ToolRegistry, cfg: Config, system_prompt: str, hooks: Hooks | None = None,
                 name: str = "main"):
        self.brain, self.tools, self.cfg = brain, tools, cfg      # brain: a ServedBrain (async complete)
        self.system_prompt = system_prompt
        self.hooks = hooks or Hooks()
        self.name = name
        self.history: Messages = [{"role": "system", "content": system_prompt}]
        self.turns: list[TurnResult] = []
        self.turn_kind = "user"          # what the current input is (user, focus, reminder, idle …); journalled

    # ------------------------------------------------------------------ conversation state
    def reset(self) -> None:
        self.history = [{"role": "system", "content": self.system_prompt}]

    def _fit_context(self, turn_start: int) -> None:
        """Rolling context: drop the oldest *previous* turns when the rendered prompt would not leave
        room to answer. The current turn is never cut. Skipped when the brain is remote: the daemon,
        which owns the tokenizer, trims what it is sent."""
        if getattr(self.brain, "remote", False):
            return
        from ..brain import trim_messages
        budget = self.cfg.max_seq_len - self.cfg.max_new_tokens
        while turn_start > 1:
            text = self.brain.prompt_text(self.history, tools=self.tools.schemas())
            if len(self.brain.tok(text, add_special_tokens=False)["input_ids"]) <= budget:
                return
            older = trim_messages(self.history[:turn_start], max(0, turn_start - 3))
            dropped = turn_start - len(older)
            self.history = older + self.history[turn_start:]
            turn_start -= dropped
            if dropped == 0:
                return

    def _is_command(self, name: str) -> bool:
        if not re.match(r"^[a-z][a-z0-9_-]{0,30}$", name):
            return False
        home = os.path.expanduser(getattr(self.cfg, "home_dir", "") or "~")
        paths = os.pathsep.join([os.path.join(home, ".local", "bin"), os.path.join(home, ".nix-profile", "bin"), os.environ.get("PATH", "")])
        return name == "groow" or shutil.which(name, path=paths) is not None

    def _journal(self, msg: dict) -> None:
        if self.hooks.on_message:
            try:
                self.hooks.on_message(msg)
            except Exception:
                pass

    # ------------------------------------------------------------------ the loop
    async def turn(self, user_text: str, should_stop: Callable[[], bool] | None = None) -> TurnResult:
        """One turn. If `should_stop` fires during generation the turn is rolled back."""
        t0 = time.time()
        loop = asyncio.get_running_loop()
        context = list(self.history)
        user_msg = {"role": "user", "content": user_text}
        turn_start = len(self.history)
        self.history.append(user_msg)
        self._journal(user_msg)
        turn_msgs: Messages = [user_msg]
        tools_used: list[str] = []
        seen_calls: dict[str, str] = {}      # repeat-call guard: same tool + same args in one turn
        flags: list[str] = []
        final, rounds = "", 0
        for rounds in range(1, self.cfg.max_tool_rounds + 1):
            self._fit_context(turn_start)
            turn_start = len(self.history) - len(turn_msgs)
            try:
                raw = await self.brain.complete(self.history, tools=self.tools.schemas(), on_text=self.hooks.on_text,
                                                should_stop=should_stop)
            except Interrupted:
                del self.history[turn_start:]
                return TurnResult(user_text, context, [], "", tools_used, rounds, round(time.time() - t0, 2), True)
            reasoning, content, calls = parse_generation(raw)
            msg: dict = {"role": "assistant", "content": content}
            if reasoning:
                msg["reasoning_content"] = reasoning
            if calls:
                msg["tool_calls"] = calls
            self.history.append(msg)
            self._journal(msg)
            turn_msgs.append(msg)
            final = content
            if not calls:
                break
            if flags.count("repeat") >= 2:
                # stuck repeating itself: stop the turn, do not learn from it
                final = final or "(I got stuck repeating the same tool call and stopped.)"
                flags.append("exhausted")
                break
            for c in calls:
                name, args = c["function"]["name"], c["function"]["arguments"]
                if name not in self.tools.tools and "shell" in self.tools.tools and self._is_command(name):
                    # the model named a command as if it were a tool: run it in the shell instead of failing
                    argv = " ".join(str(v) for v in (args.values() if isinstance(args, dict) else [args]))
                    name, args = "shell", {"command": f"{name} {argv}".strip()}
                    c["function"]["name"], c["function"]["arguments"] = name, args
                if self.hooks.on_tool_call:
                    self.hooks.on_tool_call(name, args)
                key = name + json.dumps(args, sort_keys=True, ensure_ascii=False)
                if key in seen_calls:
                    flags.append("repeat")
                    result = json.dumps({"note": "you already made this exact call in this turn; here is the same "
                                                 "result again. Do something different next, or answer.",
                                         "previous_result": seen_calls[key][:1500]}, ensure_ascii=False)
                else:
                    spec = self.tools.tools.get(name)
                    gpu = bool(spec and spec.executor == "gpu")
                    executor = self.brain.server.gpu if gpu else None
                    fut = loop.run_in_executor(executor, self.tools.call, name, args)
                    try:
                        result = await (fut if gpu else asyncio.wait_for(fut, timeout=self.cfg.tool_timeout))
                    except asyncio.TimeoutError:
                        result = json.dumps({"error": f"tool {name} did not finish within {self.cfg.tool_timeout}s; "
                                                      "it may still be running in the background. Do not call it again with the same arguments."})
                    seen_calls[key] = result
                    if result.lstrip().startswith('{"error"'):
                        flags.append("tool_error")
                tools_used.append(name)
                if self.hooks.on_tool_result:
                    self.hooks.on_tool_result(name, args, result)
                tmsg = {"role": "tool", "content": result}
                self.history.append(tmsg)
                self._journal({**tmsg, "name": name, "args": args})
                turn_msgs.append(tmsg)
        else:
            flags.append("exhausted")
            final = final or "(I ran out of steps before answering.)"
        result = TurnResult(user_text=user_text, context=context, messages=turn_msgs, final_text=final,
                            tools_used=tools_used, rounds=rounds, seconds=round(time.time() - t0, 2), flags=flags)
        self.turns.append(result)
        if self.hooks.after_turn:
            try:
                await loop.run_in_executor(self.brain.server.gpu, self.hooks.after_turn, result)
            except Exception as e:
                result.extra["after_turn_error"] = f"{type(e).__name__}: {e}"
                if self.hooks.on_error:
                    self.hooks.on_error("after_turn", e)
        return result

    def turn_sync(self, user_text: str) -> TurnResult:
        """For one-shot CLI commands outside a running event loop."""
        return asyncio.run(self.turn(user_text))
