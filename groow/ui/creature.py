"""The creature: pixel art that grows with Groow's age and moves with its mood.

Each sprite is a grid of pixel codes; a pixel is drawn as two block characters
in a colour, so it reads as a square. Four life stages by age since birth:
baby (first day), young (first week), adult (first six months), elder. Moods
change the face and add small overlays; two frames per mood give the motion.
"""
from __future__ import annotations

# colours (night-garden palette)
BODY, BODY_DARK, BODY_LIGHT = "#7ee8c8", "#3f9c82", "#b9f5e3"
EYE, MOUTH = "#0b1016", "#1e4d40"
LEAF, LEAF_DARK = "#9be26f", "#5fa246"
FLOWER, FLOWER_DARK, POLLEN = "#f2c97d", "#d59a3a", "#fff1c4"
SOIL, SOIL_DARK = "#7a5a3a", "#4e3722"
MOSS, BEARD = "#a7b6a0", "#d7dfd2"
SPARK, ZED, BOOK, GEAR, WRENCH, BUBBLE = "#ffe08a", "#8aa0ff", "#e8d9b5", "#9fb7c9", "#ff7b72", "#c9b8ff"

PALETTE = {
    "g": BODY, "G": BODY_DARK, "h": BODY_LIGHT, "e": EYE, "m": MOUTH,
    "l": LEAF, "L": LEAF_DARK, "f": FLOWER, "F": FLOWER_DARK, "y": POLLEN,
    "s": SOIL, "S": SOIL_DARK, "o": MOSS, "w": BEARD,
    "*": SPARK, "z": ZED, "b": BOOK, "t": GEAR, "r": WRENCH, "u": BUBBLE,
}

# ---------------------------------------------------------------- sprites (12 x 12), "." is transparent
BABY = """
............
............
.....l......
....lL......
...gggggg...
..gghhggGg..
..ggeggegG..
..ggggggGG..
..gggmmgGG..
...ggggGG...
..ssssssss..
.SSSSSSSSSS.
"""

YOUNG = """
.....ll.....
....lLl.....
.....L......
...gggggg...
..gghhgggg..
.ggeggggegG.
.gggggggggG.
.ggggmmmgGG.
..gggggGGG..
...GgggGG...
..ssssssss..
.SSSSSSSSSS.
"""

ADULT = """
....yffy....
...yfFFfy...
....fFFf....
.ll..LL..ll.
..lL.LL.Ll..
...gggggg...
..gghhgggg..
.ggeggggegG.
.gggggggggG.
.ggggmmmgGG.
..ggggGGGG..
.SSssssssSS.
"""

ELDER = """
....yffy....
...yfFFfy...
.l..fFFf..l.
..lL.LL.Ll..
...ooggoo...
..gghhgggg..
.ggeggggegG.
.ggggggggGG.
.gwwwwwwwwG.
..wwwwwwww..
...wwwwww...
.SSssssssSS.
"""

STAGES = [("baby", 0, BABY), ("young", 86400, YOUNG), ("adult", 7 * 86400, ADULT), ("elder", 180 * 86400, ELDER)]

CAPTIONS = {
    "idle": "waiting", "listening": "listening", "thinking": "thinking", "tooling": "using a tool",
    "speaking": "speaking", "learning": "learning", "reading": "reading the world", "dreaming": "inner thoughts",
    "sleeping": "sleeping", "repair": "repairing itself",
}


def stage_for(age_seconds: float) -> tuple[str, str]:
    name, sprite = STAGES[0][0], STAGES[0][2]
    for n, t, s in STAGES:
        if age_seconds >= t:
            name, sprite = n, s
    return name, sprite


def _grid(sprite: str) -> list[list[str]]:
    rows = [r for r in sprite.strip("\n").splitlines()]
    return [list(r.ljust(12, ".")[:12]) for r in rows]


def _find(grid, code):
    return [(y, x) for y, row in enumerate(grid) for x, c in enumerate(row) if c == code]


def _apply_mood(grid: list[list[str]], mood: str, tick: int) -> list[list[str]]:
    g = [row[:] for row in grid]
    eyes = _find(g, "e")
    mouth = _find(g, "m")
    blink = tick % 6 == 0
    if mood == "sleeping" or blink and mood not in ("repair",):
        for y, x in eyes:
            g[y][x] = "G"                                  # closed eyes
    if mood == "speaking":
        for y, x in mouth:
            g[y][x] = "e" if tick % 2 else "m"             # mouth opens and closes
    if mood == "thinking":
        for y, x in eyes:
            g[y][x - 1 if tick % 2 else x] = "e"           # eyes glance sideways
            if tick % 2:
                g[y][x] = "g"
    if mood == "learning":
        for i, (y, x) in enumerate([(0, 1), (1, 10), (3, 0), (4, 11)]):
            if (i + tick) % 2 == 0:
                g[y][x] = "*"
    if mood == "repair":
        for y, x in eyes:
            g[y][x] = "r"
    return g


OVERLAYS = {   # small side glyphs drawn to the right of the sprite, per mood and frame
    "thinking": [" .", " . .", " . . ."],
    "tooling": [" ⚙", "  ⚙"],
    "reading": [" ▤", " ▥"],
    "dreaming": ["  ○", " ○ ", "○  "],
    "sleeping": [" z", " z z", " z z z"],
    "repair": [" ✚", "  ✚"],
    "learning": [" ✦", "  ✦"],
    "speaking": [" ▪", " ▪▪", " ▪▪▪"],
}
OVERLAY_COLOR = {"thinking": BODY_LIGHT, "tooling": GEAR, "reading": BOOK, "dreaming": BUBBLE, "sleeping": ZED,
                 "repair": WRENCH, "learning": SPARK, "speaking": BODY_LIGHT}


def render(age_seconds: float, mood: str, tick: int, caption: bool = True):
    """A rich Text of the creature: 12 rows of 24 columns, plus overlay and caption."""
    from rich.text import Text
    stage, sprite = stage_for(age_seconds)
    grid = _apply_mood(_grid(sprite), mood, tick)
    bob = 1 if (tick % 4 in (1, 2) and mood not in ("sleeping",)) else 0     # gentle breathing bob
    t = Text()
    if bob:
        t.append("\n")
    frames = OVERLAYS.get(mood)
    for y, row in enumerate(grid):
        t.append(" ")
        for c in row:
            if c == ".":
                t.append("  ")
            else:
                t.append("██", style=PALETTE.get(c, BODY))
        if frames and y == 2:
            t.append(frames[tick % len(frames)], style=OVERLAY_COLOR.get(mood, BODY_LIGHT))
        t.append("\n")
    if not bob:
        t.append("\n")
    if caption:
        t.append(f"  {CAPTIONS.get(mood, mood)} · {stage}", style="#5c6b7a")
    return t


def frame(mood: str, tick: int) -> str:      # kept for compatibility (plain-text preview)
    stage, sprite = stage_for(0)
    return "\n".join("".join("██" if c != "." else "  " for c in row) for row in _grid(sprite))
