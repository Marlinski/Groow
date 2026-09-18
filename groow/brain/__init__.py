"""The neural substrate. `Brain` owns the model, `chatfmt` renders conversations
into token ids with per-role loss weights, `server` batches concurrent generation."""
from .model import Brain, Decision, Interrupted
from .chatfmt import Sample, build_sample, render, trim_messages
from .server import GenServer, ServedBrain

__all__ = ["Brain", "Decision", "Interrupted", "Sample", "build_sample", "render", "trim_messages",
           "GenServer", "ServedBrain"]
