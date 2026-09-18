"""Experience -> weight updates.

    learner    Learner: passive, feedback, memorize, quiz, play, probe, report
    sleep      SleepPolicy: replay, drift check, merge with rollback, when to sleep
    curiosity  Curiosity: idle behaviour, sensing the world and learning from it
    identity   Identity: self-description Groow can rewrite; internalised into weights at night
"""
from .learner import Learner, REHEARSAL_SYSTEM
from .sleep import SleepPolicy
from .curiosity import Curiosity
from .identity import Identity

__all__ = ["Learner", "REHEARSAL_SYSTEM", "SleepPolicy", "Curiosity", "Identity"]
