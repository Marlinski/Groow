"""The limbic system: what happened becomes how it felt.

    sensors  free signals from the transcript (errors, timeouts, repeats, recovery, restatement)
    judge    a small frozen scorer for the ambiguous part (how the person reacted); pluggable
    limbic   Limbic: per-turn valence, per-decision credit, pain/pleasure accumulators, drives
"""
from .limbic import Limbic
from .judge import make_judge, GLiClassJudge, NullJudge
from . import sensors

__all__ = ["Limbic", "make_judge", "GLiClassJudge", "NullJudge", "sensors"]
