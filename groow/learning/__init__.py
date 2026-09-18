"""Experience -> weight updates.

    trainingset  TrainingSets: files in the home that say what to learn (anyone appends, the trainer consumes)
    trainer      Trainer: the one place gradients come from (SFT and policy-gradient steps from samples)
    hippocampus  Hippocampus: prepares training samples from the day's journal (facts stated, things read)
    learner      Learner: rehearsal, quiz, probe, report (and the legacy direct learn for the mentor)
    sleep        SleepPolicy: consume, replay, internalise identity, drift check, merge with rollback
    curiosity    Curiosity: idle behaviour, sensing the world
    identity     Identity: self-description; internalised into weights at night
"""
from .learner import Learner, REHEARSAL_SYSTEM
from .trainingset import TrainingSets
from .trainer import Trainer
from .hippocampus import Hippocampus
from .sleep import SleepPolicy
from .curiosity import Curiosity
from .identity import Identity

__all__ = ["Learner", "REHEARSAL_SYSTEM", "TrainingSets", "Trainer", "Hippocampus", "SleepPolicy", "Curiosity", "Identity"]
