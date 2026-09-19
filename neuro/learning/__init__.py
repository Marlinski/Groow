"""Turning samples into weight changes.

    trainingset  the files that say what is to be learned, and how far they have been read
    trainer      the one place gradients come from: supervised steps with rehearsal, and
                 policy steps with advantages normalised inside a group
    learner      rehearsal, probes and the report
    prompts      the wording used when replaying old exchanges

Imported lazily, so a process that only wants to read a training set does not pull in torch.
"""
import importlib
from typing import TYPE_CHECKING

if TYPE_CHECKING:  # pragma: no cover
    from .learner import Learner
    from .trainer import Trainer
    from .trainingset import TrainingSets

_LAZY = {"Learner": ".learner", "Trainer": ".trainer", "TrainingSets": ".trainingset"}


def __getattr__(name: str):
    if name in _LAZY:
        return getattr(importlib.import_module(_LAZY[name], __name__), name)
    raise AttributeError(f"module {__name__!r} has no attribute {name!r}")


__all__ = list(_LAZY)
