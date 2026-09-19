"""The weights.

`Brain` owns them: the frozen base, the overlay that moves, generation, the supervised and
policy steps that move it, and the merge that makes what it learned permanent. `chatfmt`
renders a conversation into tokens with a weight per role, so a turn can be learned from
unevenly: what it said matters more than what it was told.
"""
from .model import Brain, Decision, Interrupted
from .chatfmt import Sample, build_sample, render, trim_messages

__all__ = ["Brain", "Decision", "Interrupted", "Sample", "build_sample", "render", "trim_messages"]
