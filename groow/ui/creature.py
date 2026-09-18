"""The creature: a small sprout that lives in the corner of the UI and moves with
Groow's mood. Groow grows, so it is a plant. Frames are plain text, one list
per mood; the widget cycles them."""
from __future__ import annotations

FRAMES: dict[str, list[str]] = {
    "idle": [
        r"""
      .-.
     ( o )
   \  \|/  /
    \__|__/
     ~~~~~
""", r"""
       .-.
      ( o )
    \  \|/  /
     \__|__/
      ~~~~~
""", r"""
      .-.
     ( o )
   \  \|/  /
    \__|__/
     ~~~~~
""", r"""
     .-.
    ( o )
  \  \|/  /
   \__|__/
    ~~~~~
"""],
    "listening": [
        r"""
      .-.
     ( o )   .
   \  \|/  /
    \__|__/
     ~~~~~
""", r"""
      .-.
     ( o )   
   \  \|/  /
    \__|__/
     ~~~~~
"""],
    "thinking": [
        r"""
      .-.   .
     ( - )
   \  \|/  /
    \__|__/
     ~~~~~
""", r"""
      .-.   . .
     ( - )
   \  \|/  /
    \__|__/
     ~~~~~
""", r"""
      .-.   . . .
     ( - )
   \  \|/  /
    \__|__/
     ~~~~~
"""],
    "tooling": [
        r"""
      .-.   ⚙
     ( o )
   \  \|/  /
    \__|__/
     ~~~~~
""", r"""
      .-.    ⚙
     ( o )
   \  \|/  /
    \__|__/
     ~~~~~
""", r"""
      .-.   ⚙
     ( o )
   \  \|/  /
    \__|__/
     ~~~~~
"""],
    "speaking": [
        r"""
      .-.   ( )
     ( o )
   \  \|/  /
    \__|__/
     ~~~~~
""", r"""
      .-.   (  )
     ( o )
   \  \|/  /
    \__|__/
     ~~~~~
""", r"""
      .-.   (   )
     ( o )
   \  \|/  /
    \__|__/
     ~~~~~
"""],
    "learning": [
        r"""
    ✦ .-.
     ( o ) ✦
   \  \|/  /
    \__|__/
     ~~~~~
""", r"""
      .-. ✦
   ✦ ( o )
   \  \|/  /
    \__|__/
     ~~~~~
""", r"""
      .-.
     ( o )
   \✦ \|/ ✦/
    \__|__/
     ~~~~~
"""],
    "reading": [
        r"""
      .-.
     ( o )  ▤
   \  \|/  /
    \__|__/
     ~~~~~
""", r"""
      .-.
     ( o )  ▥
   \  \|/  /
    \__|__/
     ~~~~~
"""],
    "dreaming": [
        r"""
      .-.    o
     ( o )  o
   \  \|/  /
    \__|__/
     ~~~~~
""", r"""
      .-.   o
     ( o )   o
   \  \|/  /
    \__|__/
     ~~~~~
""", r"""
      .-.     o
     ( o )  o
   \  \|/  /
    \__|__/
     ~~~~~
"""],
    "sleeping": [
        r"""
      .-.   z
     ( - )
    \ \|/ /
     \_|_/
     ~~~~~
""", r"""
      .-.   z z
     ( - )
    \ \|/ /
     \_|_/
     ~~~~~
""", r"""
      .-.   z z z
     ( - )
    \ \|/ /
     \_|_/
     ~~~~~
"""],
    "repair": [
        r"""
      .-.   🔧
     ( x )
   \  \|/  /
    \__|__/
     ~~~~~
""", r"""
      .-.  🔧
     ( x )
   \  \|/  /
    \__|__/
     ~~~~~
"""],
}

CAPTIONS = {
    "idle": "waiting", "listening": "listening", "thinking": "thinking", "tooling": "using a tool",
    "speaking": "speaking", "learning": "learning", "reading": "reading the world", "dreaming": "inner thoughts",
    "sleeping": "sleeping", "repair": "repairing itself",
}


def frame(mood: str, tick: int) -> str:
    frames = FRAMES.get(mood) or FRAMES["idle"]
    return frames[tick % len(frames)].strip("\n")
