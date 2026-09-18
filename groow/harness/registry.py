"""Tool registry: plain Python functions become tools the model can call.

    registry = ToolRegistry()

    @registry.tool
    def calculator(expression: str) -> dict:
        \"\"\"Evaluate an arithmetic expression.

        Args:
            expression: e.g. "17 * 23 + 4"
        \"\"\"
        ...

The JSON schema the model sees is derived from the signature (type hints,
defaults) and the docstring (summary + an `Args:` section). Calls are
dispatched by name with argument coercion and every error is returned to the
model as data instead of raising, so a bad call is something it can read and
recover from.
"""
from __future__ import annotations

import inspect
import json
import re
import time
import typing
from dataclasses import dataclass, field
from typing import Any, Callable

_TYPE_MAP = {str: "string", int: "integer", float: "number", bool: "boolean", list: "array", dict: "object"}


@dataclass
class ToolSpec:
    name: str
    description: str
    parameters: dict
    fn: Callable[..., Any]
    group: str = "general"
    executor: str = "io"      # "gpu": the tool trains/measures weights and must run on the GPU executor
    calls: int = 0
    total_seconds: float = 0.0

    def schema(self) -> dict:
        return {"type": "function", "function": {
            "name": self.name, "description": self.description, "parameters": self.parameters}}


@dataclass
class ToolRegistry:
    tools: dict[str, ToolSpec] = field(default_factory=dict)
    on_progress: Callable[[str], None] | None = None   # tools can report interim progress through this

    # ------------------------------------------------------------------ registration
    def tool(self, fn: Callable | None = None, *, name: str | None = None, description: str | None = None,
             group: str = "general", executor: str = "io"):
        def wrap(f: Callable) -> Callable:
            spec = _spec_from_function(f, name=name, description=description, group=group)
            spec.executor = executor
            self.tools[spec.name] = spec
            return f
        return wrap(fn) if fn is not None else wrap

    def include(self, other: "ToolRegistry") -> None:
        self.tools.update(other.tools)

    def remove(self, name: str) -> None:
        self.tools.pop(name, None)

    # ------------------------------------------------------------------ what the model sees
    def schemas(self, groups: list[str] | None = None) -> list[dict]:
        return [t.schema() for t in self.tools.values() if groups is None or t.group in groups]

    def names(self) -> list[str]:
        return list(self.tools)

    def progress(self, msg: str) -> None:
        if self.on_progress:
            self.on_progress(msg)

    # ------------------------------------------------------------------ dispatch
    def call(self, name: str, args: dict | None) -> str:
        """Execute a tool and return a JSON string for the model."""
        spec = self.tools.get(name)
        if spec is None:
            return json.dumps({"error": f"unknown tool {name!r}", "available": self.names()})
        args = dict(args or {})
        try:
            kwargs = _coerce_args(spec, args)
        except ValueError as e:
            return json.dumps({"error": str(e), "expected": spec.parameters})
        t = time.time()
        try:
            result = spec.fn(**kwargs)
        except Exception as e:  # tools must never crash the loop; the model reads the error
            result = {"error": f"{type(e).__name__}: {e}"}
        finally:
            spec.calls += 1
            spec.total_seconds += time.time() - t
        if not isinstance(result, (dict, list)):
            result = {"result": result}
        return json.dumps(result, ensure_ascii=False, default=str)

    def stats(self) -> dict:
        return {n: {"calls": t.calls, "seconds": round(t.total_seconds, 1)} for n, t in self.tools.items() if t.calls}


# ---------------------------------------------------------------------- schema derivation
def _spec_from_function(fn: Callable, name: str | None, description: str | None, group: str) -> ToolSpec:
    sig = inspect.signature(fn)
    hints = typing.get_type_hints(fn)
    summary, arg_docs = _parse_docstring(fn.__doc__ or "")
    props, required = {}, []
    for pname, p in sig.parameters.items():
        if p.kind in (p.VAR_POSITIONAL, p.VAR_KEYWORD):
            continue
        prop = _json_type(hints.get(pname, str))
        if pname in arg_docs:
            prop["description"] = arg_docs[pname]
        if p.default is inspect.Parameter.empty:
            required.append(pname)
        else:
            prop["default"] = p.default
        props[pname] = prop
    return ToolSpec(name=name or fn.__name__, description=description or summary or fn.__name__,
                    parameters={"type": "object", "properties": props, "required": required}, fn=fn, group=group)


def _json_type(tp) -> dict:
    origin = typing.get_origin(tp)
    if origin is typing.Union or str(origin) == "<class 'types.UnionType'>":
        args = [a for a in typing.get_args(tp) if a is not type(None)]
        return _json_type(args[0]) if args else {"type": "string"}
    if origin in (list, tuple):
        inner = typing.get_args(tp)
        return {"type": "array", "items": _json_type(inner[0]) if inner else {"type": "string"}}
    if origin is dict:
        return {"type": "object"}
    return {"type": _TYPE_MAP.get(tp, "string")}


def _parse_docstring(doc: str) -> tuple[str, dict[str, str]]:
    doc = inspect.cleandoc(doc)
    parts = re.split(r"\n\s*Args?:\s*\n", doc, maxsplit=1)
    summary = parts[0].strip()
    args: dict[str, str] = {}
    if len(parts) > 1:
        current = None
        for line in parts[1].splitlines():
            m = re.match(r"\s*(\w+)\s*(?:\([^)]*\))?\s*:\s*(.*)", line)
            if m and not line.startswith("        "):
                current = m.group(1)
                args[current] = m.group(2).strip()
            elif current and line.strip():
                args[current] += " " + line.strip()
    return summary, args


def _coerce_args(spec: ToolSpec, args: dict) -> dict:
    props = spec.parameters["properties"]
    out = {}
    for k, v in args.items():
        if k not in props:
            continue   # ignore hallucinated extras rather than failing
        want = props[k].get("type")
        try:
            if want == "integer" and not isinstance(v, bool):
                v = int(v)
            elif want == "number":
                v = float(v)
            elif want == "boolean" and isinstance(v, str):
                v = v.strip().lower() in ("1", "true", "yes", "on")
            elif want == "string" and not isinstance(v, str):
                v = json.dumps(v) if isinstance(v, (dict, list)) else str(v)
        except (TypeError, ValueError):
            raise ValueError(f"argument {k!r} should be {want}, got {v!r}")
        out[k] = v
    missing = [r for r in spec.parameters["required"] if r not in out]
    if missing:
        raise ValueError(f"missing required argument(s): {missing}")
    return out
