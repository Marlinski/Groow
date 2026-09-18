"""Skills: tools Groow writes for itself.

A skill is one Python file in state/skills/<name>.py: a docstring, plain
functions with docstrings (each becomes a tool; type hints give the schema),
optionally CLI = {"command": "function"} (each becomes an executable in
~/.local/bin, on Groow's PATH; the function takes argv and returns a dict),
and TESTS. A register(reg) function is accepted too for full control.

    \"\"\"Word tools.\"\"\"

    def word_count(text: str) -> dict:
        \"\"\"Count words in a text.

        Args:
            text: the text to count
        \"\"\"
        return {"words": len(text.split())}

    TESTS = [("word_count", {"text": "a b c"}, {"words": 3})]

Lifecycle:  draft (state/skills/_drafts) -> checked in a subprocess -> installed
            (hot-loaded) -> disabled / rolled back / quarantined after a crash.
Everything is logged; previous versions are kept; the core package is never
touched by this path (core changes go through patch proposals).
"""
from __future__ import annotations

import hashlib
import importlib.util
import json
import re
import shutil
import subprocess
import sys
import time
import traceback
from pathlib import Path

from .registry import ToolRegistry

NAME_RE = re.compile(r"^[a-z][a-z0-9_]{2,30}$")


class SkillManager:
    def __init__(self, state_dir: Path, protected: set[str], memory=None, check_timeout: int = 60, home: Path | None = None):
        self.home = Path(home or Path.home())
        self.bin = self.home / ".local" / "bin"
        self.dir = Path(state_dir) / "skills"
        self.drafts = self.dir / "_drafts"
        self.quarantine_dir = self.dir / "_quarantine"
        self.versions = self.dir / "_versions"
        for d in (self.dir, self.drafts, self.quarantine_dir, self.versions):
            d.mkdir(parents=True, exist_ok=True)
        self.manifest_path = self.dir / "manifest.json"
        self.incidents_path = Path(state_dir) / "incidents.jsonl"
        self.patches_dir = Path(state_dir) / "patches"
        self.patches_dir.mkdir(exist_ok=True)
        self.protected = set(protected)
        self.memory = memory
        self.check_timeout = check_timeout
        self.registry = ToolRegistry()          # live registry of installed skill tools
        self.on_change = None                   # callable() -> None; the app rebuilds its tool set
        self.manifest: dict = json.loads(self.manifest_path.read_text()) if self.manifest_path.exists() else {}

    # ------------------------------------------------------------------ helpers
    def _save_manifest(self) -> None:
        self.manifest_path.write_text(json.dumps(self.manifest, indent=2))

    def _log(self, kind: str, **f) -> None:
        if self.memory is not None:
            self.memory.log(kind, **f)

    @staticmethod
    def _describe(source: str) -> str:
        m = re.match(r'\s*(?:"""|\'\'\')(.*?)(?:"""|\'\'\')', source, re.DOTALL)
        return (m.group(1).strip().splitlines()[0] if m else "")[:140]

    def installed(self) -> list[str]:
        return sorted(p.stem for p in self.dir.glob("*.py"))

    # ------------------------------------------------------------------ checking (subprocess)
    def check(self, path: Path) -> dict:
        try:
            checker = Path(__file__).with_name("skillcheck.py")
            r = subprocess.run([sys.executable, str(checker), str(path), ",".join(sorted(self.protected))],
                               capture_output=True, text=True, timeout=self.check_timeout, cwd=str(Path.cwd()))
        except subprocess.TimeoutExpired:
            return {"ok": False, "errors": [f"check timed out after {self.check_timeout}s (infinite loop?)"]}
        try:
            return json.loads(r.stdout.strip().splitlines()[-1])
        except Exception:
            return {"ok": False, "errors": ["checker crashed", r.stderr[-2000:]]}

    # ------------------------------------------------------------------ draft / install / disable / rollback
    def draft(self, name: str, source: str) -> dict:
        if not NAME_RE.match(name):
            return {"ok": False, "errors": ["name must be lowercase letters, digits, underscores, 3-31 chars"]}
        if name.startswith("_"):
            return {"ok": False, "errors": ["names starting with _ are reserved"]}
        p = self.drafts / f"{name}.py"
        p.write_text(source)
        shutil.rmtree(self.drafts / "__pycache__", ignore_errors=True)
        report = self.check(p)
        report["draft"] = str(p)
        report["next"] = "install_skill(name) to make these tools available" if report.get("ok") else "fix the errors and draft again"
        self._log("skill_draft", name=name, ok=report.get("ok"), errors=len(report.get("errors", [])))
        return report

    def install(self, name: str) -> dict:
        src = self.drafts / f"{name}.py"
        if not src.exists():
            return {"ok": False, "error": f"no draft named {name}; draft_skill first"}
        report = self.check(src)
        if not report.get("ok"):
            return {"ok": False, "error": "draft does not pass its checks", "report": report}
        dest = self.dir / f"{name}.py"
        version = self.manifest.get(name, {}).get("version", 0) + 1
        if dest.exists():
            shutil.copy(dest, self.versions / f"{name}.v{version - 1}.py")
        shutil.move(src, dest)
        loaded = self._load_file(dest)
        if not loaded.get("ok"):
            # the subprocess liked it but the live load failed: keep the old version
            prev = self.versions / f"{name}.v{version - 1}.py"
            if prev.exists():
                shutil.copy(prev, dest)
                self._load_file(dest)
            else:
                dest.unlink(missing_ok=True)
            return {"ok": False, "error": "live load failed, previous version kept", "detail": loaded}
        self.manifest[name] = {"version": version, "installed": time.time(), "description": self._describe(dest.read_text()),
                               "sha": hashlib.sha1(dest.read_bytes()).hexdigest()[:10], "tools": loaded["tools"],
                               "commands": loaded.get("commands", []), "crashes": 0, "enabled": True}
        self._save_manifest()
        self._log("skill_install", name=name, version=version, tools=loaded["tools"])
        if self.on_change:
            self.on_change()
        return {"ok": True, "name": name, "version": version, "tools": loaded["tools"], "commands": loaded.get("commands", [])}

    def disable(self, name: str, reason: str = "") -> dict:
        p = self.dir / f"{name}.py"
        if not p.exists():
            return {"ok": False, "error": f"no installed skill {name}"}
        self._unload(name)
        self._remove_commands(name)
        shutil.move(p, self.quarantine_dir / f"{name}.py")
        self.manifest.setdefault(name, {})["enabled"] = False
        self._save_manifest()
        self._log("skill_disable", name=name, reason=reason)
        if self.on_change:
            self.on_change()
        return {"ok": True, "name": name, "status": "disabled (in _quarantine)"}

    def rollback(self, name: str) -> dict:
        versions = sorted(self.versions.glob(f"{name}.v*.py"), key=lambda p: int(p.stem.split(".v")[-1]))
        if not versions:
            return {"ok": False, "error": f"no previous version of {name}"}
        prev = versions[-1]
        shutil.copy(prev, self.drafts / f"{name}.py")
        prev.unlink()
        r = self.install(name)
        self._log("skill_rollback", name=name, ok=r.get("ok"))
        return r

    def read(self, name: str) -> dict:
        for d in (self.dir, self.drafts, self.quarantine_dir):
            p = d / f"{name}.py"
            if p.exists():
                return {"name": name, "where": d.name if d != self.dir else "installed", "source": p.read_text()[:12000],
                        "manifest": self.manifest.get(name)}
        return {"error": f"no skill {name}"}

    def listing(self) -> dict:
        return {"installed": {n: {k: v for k, v in self.manifest.get(n, {}).items() if k in ("version", "description", "tools", "commands", "crashes")}
                              for n in self.installed() if not n.startswith("_")},
                "drafts": sorted(p.stem for p in self.drafts.glob("*.py")),
                "quarantined": sorted(p.stem for p in self.quarantine_dir.glob("*.py"))}

    # ------------------------------------------------------------------ live loading
    def _unload(self, name: str) -> None:
        for tool in list(self.registry.tools.values()):
            if tool.group == f"skill:{name}":
                self.registry.remove(tool.name)

    def _load_file(self, path: Path) -> dict:
        name = path.stem
        self._unload(name)
        shutil.rmtree(path.parent / "__pycache__", ignore_errors=True)
        try:
            spec = importlib.util.spec_from_file_location(f"groow_skill_{name}", path)
            mod = importlib.util.module_from_spec(spec)
            sys.modules[spec.name] = mod
            mod.__cached__ = None
            code = compile(path.read_text(), str(path), "exec")     # bypass bytecode caching entirely
            exec(code, mod.__dict__)
            tmp = ToolRegistry()
            from .skillcheck import auto_register
            auto_register(mod, tmp, f"skill:{name}")
            for tname, tspec in tmp.tools.items():
                if tname in self.protected:
                    raise ValueError(f"tool {tname} collides with a core tool")
                tspec.group = f"skill:{name}"
                self.registry.tools[tname] = tspec
            cli = getattr(mod, "CLI", None) or {}
            self._write_commands(name, path, cli)
            return {"ok": True, "tools": list(tmp.tools), "commands": sorted(cli)}
        except Exception:
            tb = traceback.format_exc(limit=6)
            self.record_incident("skill_load", tb, skill=name)
            return {"ok": False, "traceback": tb}

    def _write_commands(self, name: str, path: Path, cli: dict) -> None:
        """Each CLI entry becomes an executable in ~/.local/bin (on Groow's PATH) that runs the skill's function."""
        self.bin.mkdir(parents=True, exist_ok=True)
        for cmd, fname in cli.items():
            wrapper = self.bin / cmd
            wrapper.write_text(f"""#!{sys.executable}
# command '{cmd}' of the groow skill '{name}' (state/skills/{name}.py). Regenerated on install.
import importlib.util, json, sys
spec = importlib.util.spec_from_file_location("groow_skill_{name}", {str(path.resolve())!r})
mod = importlib.util.module_from_spec(spec); sys.modules[spec.name] = mod; spec.loader.exec_module(mod)
out = getattr(mod, {fname!r})(sys.argv[1:])
if isinstance(out, dict) and "text" in out and len(out) == 1:
    print(out["text"])
else:
    print(json.dumps(out, ensure_ascii=False, indent=1, default=str))
sys.exit(1 if isinstance(out, dict) and out.get("error") else 0)
""")
            wrapper.chmod(0o755)

    def _remove_commands(self, name: str) -> None:
        for wrapper in self.bin.glob("*"):
            try:
                if f"groow skill '{name}'" in wrapper.read_text()[:300]:
                    wrapper.unlink()
            except (OSError, UnicodeDecodeError):
                pass

    def load_all(self) -> dict:
        """At startup: load every installed skill; a skill that fails to load is quarantined."""
        loaded, quarantined = [], []
        for p in sorted(self.dir.glob("*.py")):
            r = self._load_file(p)
            if r.get("ok"):
                loaded.append(p.stem)
            else:
                self.disable(p.stem, reason="failed to load at startup")
                quarantined.append(p.stem)
        return {"loaded": loaded, "quarantined": quarantined}

    # ------------------------------------------------------------------ incidents & blame
    def record_incident(self, kind: str, tb: str, **meta) -> dict:
        rec = {"ts": time.time(), "kind": kind, "traceback": tb[-4000:], **meta}
        with self.incidents_path.open("a") as f:
            f.write(json.dumps(rec, ensure_ascii=False) + "\n")
        return rec

    def incidents(self, last: int = 5) -> list[dict]:
        if not self.incidents_path.exists():
            return []
        lines = self.incidents_path.read_text().splitlines()[-last:]
        return [json.loads(l) for l in lines if l.strip()]

    def blame(self, tb: str) -> str | None:
        """Which installed skill does this traceback point into, if any."""
        m = re.findall(r'File "([^"]*[/\\]skills[/\\]([a-z0-9_]+)\.py)"', tb)
        return m[-1][1] if m else None

    def quarantine_after_crash(self, name: str, tb: str) -> dict:
        self.manifest.setdefault(name, {})
        self.manifest[name]["crashes"] = self.manifest[name].get("crashes", 0) + 1
        self._save_manifest()
        self.record_incident("skill_crash", tb, skill=name)
        return self.disable(name, reason="crashed the process")

    # ------------------------------------------------------------------ core patches (review gate)
    def propose_patch(self, path: str, description: str, patch: str) -> dict:
        if not re.match(r"^groow/[A-Za-z0-9_/]+\.py$", path):
            return {"ok": False, "error": "path must be a file inside the groow/ package, e.g. groow/harness/loop.py"}
        pid = hashlib.sha1(f"{time.time()}{path}".encode()).hexdigest()[:8]
        (self.patches_dir / f"{pid}.json").write_text(json.dumps(
            {"id": pid, "ts": time.time(), "path": path, "description": description, "patch": patch, "status": "proposed"},
            ensure_ascii=False, indent=1))
        self._log("patch_proposed", id=pid, path=path)
        return {"ok": True, "id": pid, "status": "proposed; the mentor reviews it (state/patches/). Tell him with ask if it matters."}
