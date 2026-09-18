"""The wire protocol between the Groow daemon and any client: HTTP + SSE + WebSocket.

Base URL: http://127.0.0.1:7373 (groow.json: api_host / api_port; the daemon
also writes it to state/groow.url).

Single queries (HTTP):
  GET  /hello                      birth card, identity, model, tools, status, inbox
  GET  /status                     live snapshot (mood, steps, nights, thoughts, queue, server stats ...)
  POST /ask      {"text": "..."}   queue a human message and wait for that turn: {"final", "tools_used", "seconds", "events"}
  POST /say      {"text": "..."}   queue a human message, return at once
  POST /command  {"text": "/..."}  queue a slash command
  POST /complete {"messages": [conv or [convs]], "max_new_tokens", "temperature"}  raw completions for skills (self-play, drills)
  POST /op       {"op": "train", "args": {...}}   run an operation on the running Groow (play, sleep, probe, stats,
                                   thoughts, thought, skill, identity, inbox, incidents, patch, feedback); what `groow …` commands call
  GET  /events                     Server-Sent Events stream of the run loop (event: <type>, data: <json>)
                                   ?replay=N sends the last N events first (default 60)
Interactive UI (WebSocket):
  WS   /ws                         server -> client: one JSON event per message (hello, replay, then live)
                                   client -> server: {"cmd": "say"|"command"|"status", "text": "..."}

Event types (both SSE and WS carry the same JSON objects):
  hello        first message: birth card, identity, config summary, tools
  status       periodic snapshot: steps, nights, thoughts, queue, server stats, mode, mood
  turn_start   {who: "user"|"signal", text, kind, req?}   the main thought starts handling something
  text         {delta, req?}                              streamed assistant text
  turn_end     {final, tools_used, seconds, req?}
  tool_call    {name, args, actor: "main"|"thought:<id>"}
  tool_result  {name, result, actor}
  learned      {loss, tokens, step, probe?}
  thought      {id, status, goal, steps, event: spawn|step|focus|done|paused|resumed|killed, text?}
  sleep        {phase: start|progress|done, ...}
  weights      {busy: bool, op}                          the weights are being changed: no inference until done (a nap)
  inbox        {questions: [...]}
  log          {level, text}
  bye          the daemon is shutting down
`req` is the correlation id of a /ask or /say request, present on the events of that turn.
"""
from __future__ import annotations

import json
import time
from typing import Any


def encode(obj: dict) -> str:
    return json.dumps(obj, ensure_ascii=False, default=str)


def decode(text: bytes | str) -> dict | None:
    try:
        return json.loads(text)
    except (json.JSONDecodeError, TypeError):
        return None


def event(ev: str, **data: Any) -> dict:
    return {"ev": ev, "t": time.time(), **data}


def sse(e: dict) -> bytes:
    return f"event: {e['ev']}\ndata: {encode(e)}\n\n".encode()


MOODS = ("idle", "listening", "thinking", "tooling", "speaking", "learning", "reading", "dreaming", "napping", "sleeping", "repair")
