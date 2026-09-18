"""The mind: one conscious thread over a priority queue of signals, and the
inner thoughts it can spawn, index, interrupt and read.

    signals   Signal + Mailbox (InputQueue): what reaches the main thought, in what order; a maildir on disk
    clock     Schedule: alarms and periodic tasks, a file in the home
    lock      Conscious: one conscious turn at a time, a lockfile in the home
    thoughts  Thought + ThoughtManager: inner run-loops with their own trace, budget and state
    mind      Mind: the scheduler; the only thing that talks to the user or the mentor
"""
from .signals import Signal, InputQueue, Mailbox, Priority
from .clock import Schedule
from .lock import Conscious
from .thoughts import Thought, ThoughtManager
from .mind import Mind

__all__ = ["Signal", "InputQueue", "Mailbox", "Priority", "Schedule", "Conscious", "Thought", "ThoughtManager", "Mind"]
