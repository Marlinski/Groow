"""The limbic system: what happened becomes how it felt.

    sensors    free signals read from the transcript: errors, timeouts, repeats, recovery
    judge      a small frozen scorer for the ambiguous part, how a person reacted
    feel       the pass that puts the two together and records a turn's valence
    calibrate  checks the judge against exchanges where we know what a person would say

Nothing in here learns. A judge that could be trained by the thing it judges would drift
toward approving of whatever that thing already does.
"""
from .feel import feel
from .judge import make_judge, GLiClassJudge, NullJudge
from . import sensors

__all__ = ["feel", "make_judge", "GLiClassJudge", "NullJudge", "sensors"]
