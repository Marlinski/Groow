"""One conscious thought at a time.

The mind pops a signal, fires a turn process and waits for it before popping the next,
so turns are serialised by construction. This lock makes that a fact rather than a habit:
a second turn, started by hand or by another daemon, is refused while one is running.
Inner thoughts are not covered: they are meant to run in parallel, and they cannot speak.

    state/conscious.lock   the pid of the process currently holding the conscious thread
"""
from __future__ import annotations

import errno
import os
from pathlib import Path


class Conscious:
    def __init__(self, state_dir: Path | str):
        self.path = Path(state_dir) / "conscious.lock"
        self.held = False

    def holder(self) -> int | None:
        """The pid holding it, or None. A stale lock (dead process) is cleared."""
        try:
            pid = int(self.path.read_text().strip())
        except (OSError, ValueError):
            return None
        try:
            os.kill(pid, 0)
            return pid
        except PermissionError:
            return pid
        except OSError:
            self.path.unlink(missing_ok=True)
            return None

    def acquire(self) -> bool:
        holder = self.holder()
        if holder is not None:
            return holder == os.getpid()
        try:
            fd = os.open(self.path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o644)
        except OSError as e:
            if e.errno == errno.EEXIST:
                return False
            raise
        with os.fdopen(fd, "w") as f:
            f.write(str(os.getpid()))
        self.held = True
        return True

    def release(self) -> None:
        if self.held and self.holder() == os.getpid():
            self.path.unlink(missing_ok=True)
        self.held = False

    def __enter__(self):
        if not self.acquire():
            raise RuntimeError(f"another conscious turn is running (pid {self.holder()})")
        return self

    def __exit__(self, *exc):
        self.release()
