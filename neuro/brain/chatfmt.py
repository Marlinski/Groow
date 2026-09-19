"""Rendering conversations with the model's own chat template and deriving
per-token loss weights from message roles.

We render the *whole* conversation with the tokenizer's chat template (so the
training distribution matches generation exactly), then locate each
`<|im_start|>role ... <|im_end|>` span in the text and map character offsets to
tokens. Tool responses are rendered by the Qwen template inside a `user` turn
wrapped in <tool_response>, so we detect them and give them the `tool` weight.
"""
from __future__ import annotations

import re
from dataclasses import dataclass

IM_START = "<|im_start|>"
IM_END = "<|im_end|>"
SPAN_RE = re.compile(r"<\|im_start\|>(\w+)\n(.*?)<\|im_end\|>", re.DOTALL)


@dataclass
class Sample:
    input_ids: list[int]
    weights: list[float]      # per-token weight applied to the loss of *predicting* that token
    text: str = ""

    def __len__(self) -> int:
        return len(self.input_ids)

    @property
    def learnable_tokens(self) -> int:
        return sum(1 for w in self.weights if w != 0)


def render(tok, messages, tools=None, add_generation_prompt=False, enable_thinking=False) -> str:
    return tok.apply_chat_template(
        messages,
        tools=tools or None,
        add_generation_prompt=add_generation_prompt,
        enable_thinking=enable_thinking,
        tokenize=False,
    )


def build_sample(tok, messages, role_weights: dict[str, float], tools=None,
                 max_len: int = 2048, enable_thinking: bool = False) -> Sample:
    """Tokenise a conversation and weight tokens by the role that produced them.

    For assistant turns the weight covers everything after the role header up to
    and including <|im_end|> (so the model also learns when to stop). For other
    roles it covers the body only.
    """
    text = render(tok, messages, tools=tools, enable_thinking=enable_thinking)
    enc = tok(text, return_offsets_mapping=True, add_special_tokens=False)
    ids = enc["input_ids"]
    offsets = enc["offset_mapping"]

    # character-level weights, then project onto tokens
    char_w = [0.0] * (len(text) + 1)
    for m in SPAN_RE.finditer(text):
        role, body = m.group(1), m.group(2)
        if role == "user" and body.lstrip().startswith("<tool_response>"):
            role = "tool"
        w = float(role_weights.get(role, 0.0))
        if role == "assistant" and "<tool_call>" in body and "tool_call_only" in role_weights:
            visible = re.sub(r"<tool_call>.*?</tool_call>", "", body, flags=re.DOTALL).replace("<think>", "").replace("</think>", "").strip()
            if not visible:
                w = float(role_weights["tool_call_only"])   # a turn that is nothing but a tool call: habit, not knowledge
        if w == 0.0:
            continue
        if role == "assistant":
            start = m.start(2)          # right after "<|im_start|>assistant\n"
            end = m.end()               # includes <|im_end|>
        else:
            start, end = m.start(2), m.end(2)
        for c in range(start, end):
            char_w[c] = w

    weights = []
    for (s, e) in offsets:
        if e <= s:
            weights.append(0.0)
            continue
        # a token inherits the weight of its span (use the max over its chars; spans do not overlap)
        weights.append(max(char_w[s:e]) if e > s else 0.0)

    if len(ids) > max_len:              # keep the tail: the newest turns matter most
        ids, weights = ids[-max_len:], weights[-max_len:]
    return Sample(input_ids=ids, weights=weights, text=text)


def trim_messages(messages: list[dict], keep_last: int) -> list[dict]:
    """Keep the system prompt plus the last `keep_last` messages, never splitting a
    tool-call from its tool responses."""
    if len(messages) <= keep_last + 1:
        return list(messages)
    sys_msgs = [m for m in messages[:1] if m["role"] == "system"]
    rest = messages[len(sys_msgs):]
    tail = rest[-keep_last:]
    # do not start on a tool message or right after a tool call
    while tail and (tail[0]["role"] == "tool" or (tail[0]["role"] == "assistant" and tail[0].get("tool_calls"))):
        tail = tail[1:]
    return sys_msgs + tail
