"""The wire protocol between the Groow daemon and any UI.

Transport: a Unix domain socket (state/groow.sock), newline-delimited JSON.
Client -> daemon:   {"cmd": "say", "text": "..."}          a human message
                    {"cmd": "command", "text": "/sleep"}   a slash command
                    {"cmd": "status"}                       ask for a snapshot now
Daemon -> clients:  {"ev": <type>, "t": <unix time>, ...}  broadcast to every connected client

Event types (the UI only needs to know these):
  hello        first message: birth card, identity, config summary, tools
  status       periodic snapshot: steps, nights, thoughts, queue, server stats, mode, mood
  turn_start   {who: "user"|"signal", text, kind}    the main thought starts handling something
  text         {delta}                              streamed assistant text
  turn_end     {final, tools_used, seconds}
  tool_call    {name, args, actor: "main"|"thought:<id>"}
  tool_result  {name, result, actor}
  learned      {loss, tokens, step, probe?}
  thought      {id, status, goal, steps, event: spawn|step|focus|done|paused|resumed|killed, text?}
  signal       {kind, text}                         a queued signal being handled (focus, reminder, idle)
  sleep        {phase: start|progress|done, ...}
  inbox        {questions: [...]}
  log          {level, text}                        anything else worth showing
  bye          the daemon is shutting down
"""
from __future__ import annotations

import json
import time
from typing import Any


def encode(obj: dict) -> bytes:
    return (json.dumps(obj, ensure_ascii=False, default=str) + "\n").encode()


def decode(line: bytes | str) -> dict | None:
    try:
        return json.loads(line)
    except (json.JSONDecodeError, TypeError):
        return None


def event(ev: str, **data: Any) -> dict:
    return {"ev": ev, "t": time.time(), **data}


MOODS = ("idle", "listening", "thinking", "tooling", "speaking", "learning", "reading", "dreaming", "sleeping", "repair")
