"""The birth certificate: who Groow is in the eyes of the world, written once
at init and never by Groow itself. Everything here is fact, not self-image:
identity.md is what Groow thinks it is; birth.json is what it is."""
from __future__ import annotations

import json
import platform
import time
import uuid
from dataclasses import dataclass, asdict
from pathlib import Path


@dataclass
class Birth:
    id: str
    name: str
    born: float                 # unix time
    lineage: str                # base model id
    hardware: str
    mentor: str
    version: str

    @property
    def age_seconds(self) -> float:
        return time.time() - self.born

    def age_text(self) -> str:
        s = int(self.age_seconds)
        d, s = divmod(s, 86400)
        h, s = divmod(s, 3600)
        m, _ = divmod(s, 60)
        if d:
            return f"{d}d {h}h"
        if h:
            return f"{h}h {m}m"
        return f"{m}m"

    def born_text(self) -> str:
        return time.strftime("%Y-%m-%d %H:%M UTC", time.gmtime(self.born))

    def card(self) -> dict:
        return {**asdict(self), "born_text": self.born_text(), "age": self.age_text()}

    def line(self) -> str:
        return f"{self.name} · id {self.id} · born {self.born_text()} · age {self.age_text()} · lineage {self.lineage}"


def _hardware() -> str:
    try:
        import torch
        if torch.cuda.is_available():
            p = torch.cuda.get_device_properties(0)
            return f"{p.name} {p.total_memory // 2**30} GB"
    except Exception:
        pass
    return platform.machine()


def load_or_create(state_dir: Path, lineage: str, mentor: str = "Marlinski", version: str = "0.3.0") -> Birth:
    p = Path(state_dir) / "birth.json"
    if p.exists():
        return Birth(**json.loads(p.read_text()))
    b = Birth(id=uuid.uuid4().hex[:8], name="Groow", born=time.time(), lineage=lineage, hardware=_hardware(),
              mentor=mentor, version=version)
    p.write_text(json.dumps(asdict(b), indent=2))
    try:
        p.chmod(0o444)     # read-only on disk as well; Groow has no tool that writes it
    except OSError:
        pass
    return b
