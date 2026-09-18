"""The mind: one conscious thread over a priority queue of signals, and the
inner thoughts it can spawn, index, interrupt and read.

    signals   Signal + InputQueue: what reaches the main thought, in what order
    thoughts  Thought + ThoughtManager: inner run-loops with their own trace, budget and state
    mind      Mind: the scheduler; the only thing that talks to the user or the mentor
"""
from .signals import Signal, InputQueue, Priority
from .thoughts import Thought, ThoughtManager
from .mind import Mind

__all__ = ["Signal", "InputQueue", "Priority", "Thought", "ThoughtManager", "Mind"]
