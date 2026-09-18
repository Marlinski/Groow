"""Senses: how Groow perceives the world outside the conversation.

    news    RSS/Atom feeds -> dated, sourced items (the first sense)

A sense produces *grounded* text with a timestamp and a source. It never
trains anything itself; the learner decides what to do with what was sensed.
"""
from .news import NewsSense, NewsItem

__all__ = ["NewsSense", "NewsItem"]
