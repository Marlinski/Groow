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
import re
import time
from dataclasses import dataclass, field
from typing import Callable

from ..brain import Interrupted, trim_messages
from ..config import Config
from .registry import ToolRegistry

TOOL_CALL_RE = re.compile(r"<tool_call>\s*(\{.*?\})\s*</tool_call>", re.DOTALL)
THINK_RE = re.compile(r"<think>(.*?)</think>", re.DOTALL)

Messages = list[dict]


@dataclass
class Hooks:
    on_text: Callable[[str], None] | None = None                       # streamed text chunks
    on_tool_call: Callable[[str, dict], None] | None = None            # before a tool runs
    on_tool_result: Callable[[str, dict, str], None] | None = None     # after it ran
    after_turn: Callable[["TurnResult"], None] | None = None           # the finished turn (learning lives here)


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
    content = TOOL_CALL_RE.sub("", raw).strip()
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

    # ------------------------------------------------------------------ conversation state
    def reset(self) -> None:
        self.history = [{"role": "system", "content": self.system_prompt}]

    def _fit_context(self, turn_start: int) -> None:
        """Rolling context: drop the oldest *previous* turns when the rendered prompt
        would not leave room to answer. The current turn is never cut."""
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

    # ------------------------------------------------------------------ the loop
    async def turn(self, user_text: str, should_stop: Callable[[], bool] | None = None) -> TurnResult:
        """One turn. If `should_stop` fires during generation the turn is rolled back."""
        t0 = time.time()
        loop = asyncio.get_running_loop()
        context = list(self.history)
        user_msg = {"role": "user", "content": user_text}
        turn_start = len(self.history)
        self.history.append(user_msg)
        turn_msgs: Messages = [user_msg]
        tools_used: list[str] = []
        seen_calls: dict[str, str] = {}      # repeat-call guard: same tool + same args in one turn
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
            turn_msgs.append(msg)
            final = content
            if not calls:
                break
            for c in calls:
                name, args = c["function"]["name"], c["function"]["arguments"]
                if self.hooks.on_tool_call:
                    self.hooks.on_tool_call(name, args)
                key = name + json.dumps(args, sort_keys=True, ensure_ascii=False)
                if key in seen_calls:
                    result = json.dumps({"note": "you already made this exact call in this turn; here is the same "
                                                 "result again. Do something different next.",
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
                tools_used.append(name)
                if self.hooks.on_tool_result:
                    self.hooks.on_tool_result(name, args, result)
                tmsg = {"role": "tool", "content": result}
                self.history.append(tmsg)
                turn_msgs.append(tmsg)
        result = TurnResult(user_text=user_text, context=context, messages=turn_msgs, final_text=final,
                            tools_used=tools_used, rounds=rounds, seconds=round(time.time() - t0, 2))
        self.turns.append(result)
        if self.hooks.after_turn:
            await loop.run_in_executor(self.brain.server.gpu, self.hooks.after_turn, result)
        return result

    def turn_sync(self, user_text: str) -> TurnResult:
        """For one-shot CLI commands outside a running event loop."""
        return asyncio.run(self.turn(user_text))
