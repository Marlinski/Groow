"""The terminal UI: a Textual app that speaks the gateway protocol.

    app       GroowUI: conversation, inner thoughts, identity card, status bar, the creature
    creature  ASCII sprout animation frames per mood
"""
from .app import GroowUI, run_ui

__all__ = ["GroowUI", "run_ui"]
