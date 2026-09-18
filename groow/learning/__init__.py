"""Experience -> weight updates (the daemon's side; a turn process never imports these).

    trainingset  TrainingSets: files in the home that say what to learn
    trainer      Trainer: the one place gradients come from
    hippocampus  Hippocampus: logs (and how they felt) -> training samples
    learner      Learner: rehearsal, quiz, probe, report; learn for the mentor
    sleep        SleepPolicy: consume, replay, internalise identity, drift check, merge with rollback
    curiosity    Curiosity: idle behaviour, sensing the world
    identity     Identity: self-description; internalised into weights at night

Imported lazily so `groow.learning.identity` can be used by a process with no torch.
"""
import importlib
from typing import TYPE_CHECKING

_WHERE = {"Learner": "learner", "REHEARSAL_SYSTEM": "learner", "TrainingSets": "trainingset",
          "Trainer": "trainer", "Hippocampus": "hippocampus", "SleepPolicy": "sleep",
          "Curiosity": "curiosity", "Identity": "identity"}

if TYPE_CHECKING:  # pragma: no cover
    from .identity import Identity
    from .learner import Learner


def __getattr__(name):
    if name in _WHERE:
        return getattr(importlib.import_module(f".{_WHERE[name]}", __name__), name)
    raise AttributeError(name)


__all__ = list(_WHERE)
