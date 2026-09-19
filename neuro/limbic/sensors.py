"""Sensors: the signals that need no model, read from what actually happened.

These are Groow's nociception. They are unambiguous, free, and they dominate the
judge wherever they fire: a command that exits non-zero hurt, whatever anyone thinks.

    tool_error     a command failed, a tool raised            pain
    truncated      the answer was cut off mid-thought         pain (small)
    timeout        a call did not come back in time           pain
    repeat         the same call made twice in one turn       pain (frustration)
    exhausted      the turn ran out of rounds without an answer   pain
    recovered      a success right after a failure            pleasure (competence)
    completed      an answer, no pain anywhere in the turn    pleasure (small)
    restated       the person had to say it again             pain (delayed, social)
    surprise       how unexpected the incoming words were     drive (curiosity), not valence
"""
from __future__ import annotations

import json
import re

WEIGHTS = {
    "tool_error": -0.5,
    "timeout": -0.7,
    "repeat": -0.6,
    "exhausted": -0.8,
    "truncated": -0.3,
    "recovered": 0.6,
    "completed": 0.15,
    "restated": -0.8,
}


def from_flags(flags: list[str]) -> dict[str, int]:
    """The flags a turn reported, counted.

    The harness names each signal once per occurrence; the weights are applied per signal with
    each repeat counting for less, so what matters here is how many times each one fired.
    """
    counted: dict[str, int] = {}
    for f in flags:
        if f in WEIGHTS:
            counted[f] = counted.get(f, 0) + 1
    return counted


def is_error(tool_result: str) -> bool:
    s = (tool_result or "").lstrip()
    if not s.startswith("{"):
        return False
    try:
        d = json.loads(s)
    except (json.JSONDecodeError, TypeError):
        return False
    if not isinstance(d, dict):
        return False
    return bool(d.get("error")) or (isinstance(d.get("returncode"), int) and d["returncode"] != 0)


def is_timeout(tool_result: str) -> bool:
    return "did not finish within" in (tool_result or "") or "timed out after" in (tool_result or "")


def is_repeat_note(tool_result: str) -> bool:
    return "you already made this exact call" in (tool_result or "")


_WORD = re.compile(r"[a-zA-Z0-9éèàùçâêîôûäöüñ]+")


def similarity(a: str, b: str) -> float:
    """Crude word overlap: enough to notice the person repeating themselves."""
    wa = {w.lower() for w in _WORD.findall(a or "") if len(w) > 2}
    wb = {w.lower() for w in _WORD.findall(b or "") if len(w) > 2}
    if len(wa) < 3 or len(wb) < 3:
        return 0.0
    return len(wa & wb) / len(wa | wb)


def turn_signals(turn_messages: list[dict], flags: list[str], final_text: str) -> dict[str, int]:
    """Count what fired during one finished turn."""
    sig: dict[str, int] = {}
    seen_error = False
    for i, m in enumerate(turn_messages):
        if m["role"] != "tool":
            continue
        content = m.get("content", "")
        if is_timeout(content):
            sig["timeout"] = sig.get("timeout", 0) + 1
            seen_error = True
        elif is_repeat_note(content):
            sig["repeat"] = sig.get("repeat", 0) + 1
        elif is_error(content):
            sig["tool_error"] = sig.get("tool_error", 0) + 1
            seen_error = True
        elif seen_error:
            sig["recovered"] = sig.get("recovered", 0) + 1
            seen_error = False
    if "exhausted" in (flags or []):
        sig["exhausted"] = 1
    if final_text.strip() and not any(k in sig for k in ("tool_error", "timeout", "repeat", "exhausted")):
        sig["completed"] = 1
    return sig


def valence_of(signals: dict[str, int]) -> float:
    """Sum the signals, each one damped as it repeats (the second failure hurts less than the first)."""
    total = 0.0
    for name, n in signals.items():
        w = WEIGHTS.get(name, 0.0)
        for k in range(n):
            total += w / (1 + k)
    return total
