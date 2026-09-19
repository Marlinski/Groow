"""Errors shared by parts that must not import torch."""


class Interrupted(Exception):
    """Generation was preempted by something more urgent, or the turn was stopped."""
