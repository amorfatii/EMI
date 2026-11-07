"""Target classification and replay for `emi drift`.

Run as `python driver.py <mode> <request.json> <result.json>` in a copy of
one commit. The result goes to a file, not stdout, because the code under
test may print.

classify: for each changed function (file, name, first line on the base
side), find its definition in the file's syntax tree and say what it is:
a module-level function, a static or class method of a module-level class
(replayable), an instance method, a nested function, or not found.

replay: import each target the way the test run imported it and call it on
every recorded input, each call on freshly decoded arguments and under its
own timeout. `repeat` calls each input that many times, so the caller can
tell an unstable function (time, randomness, hidden state) from drift.

generate: for each target whose every parameter is annotated, draw inputs
from those types with Hypothesis, derandomized and with no example
database, so the same signature always gives the same inputs. Inputs
without an exact JSON encoding are counted and left out.
"""

import ast
import enum
import importlib
import json
import math
import os
import signal
import sys
import time
import types

TUPLE = "$tuple"
OBJECT = "$object"
ENUM = "$enum"
RESERVED_TAGS = (TUPLE, OBJECT, ENUM)
NON_VALUE_TYPES = (
    type,
    types.ModuleType,
    types.FunctionType,
    types.MethodType,
    types.BuiltinFunctionType,
    types.GeneratorType,
    types.CoroutineType,
)
MAX_TEXT = 10_000
INT_BOUND = 2**63
REPLAYABLE = {"staticmethod": "static", "classmethod": "class"}


class CallTimeout(BaseException):
    """Not an Exception: the function under replay cannot swallow it."""


def _on_alarm(signum, frame):
    raise CallTimeout()


DEFS = (ast.FunctionDef, ast.AsyncFunctionDef)


def kind_of(node, scope):
    """`scope` holds the enclosing function and class definitions."""
    if not scope:
        kind = "function"
    elif len(scope) == 1 and isinstance(scope[0], ast.ClassDef):
        names = {d.id for d in node.decorator_list if isinstance(d, ast.Name)}
        found = [REPLAYABLE[n] for n in names if n in REPLAYABLE]
        kind = found[0] if len(found) == 1 else "instance"
    else:
        return "nested"
    # a coroutine function returns a coroutine, never its result
    return "async" if isinstance(node, ast.AsyncFunctionDef) else kind


def start_line(node):
    return node.decorator_list[0].lineno if node.decorator_list else node.lineno


def find(tree, name, line):
    """(kind, qualname) of the definition of `name` starting on `line` (or whose `def` is on `line`)."""
    stack = [(tree, [])]
    while stack:
        node, scope = stack.pop()
        for child in ast.iter_child_nodes(node):
            if isinstance(child, DEFS) and child.name == name and (
                start_line(child) == line or child.lineno == line
            ):
                return kind_of(child, scope), ".".join([p.name for p in scope] + [name])
            inner = scope + [child] if isinstance(child, DEFS + (ast.ClassDef,)) else scope
            stack.append((child, inner))
    return "not_found", None


def classify(req):
    out = []
    for t in req["targets"]:
        try:
            with open(t["file"], encoding="utf-8") as f:
                tree = ast.parse(f.read())
            kind, qualname = find(tree, t["name"], t["line"])
        except (OSError, SyntaxError, ValueError, UnicodeDecodeError):
            kind, qualname = "not_found", None
        out.append({"kind": kind, "qualname": qualname})
    return {"targets": out}


def _resolve_qualname(module_name, qualname):
    obj = importlib.import_module(module_name)
    for part in qualname.split("."):
        if part:
            obj = getattr(obj, part)
    return obj


def decode(data):
    if isinstance(data, list):
        return [decode(v) for v in data]
    if isinstance(data, dict):
        if list(data) == [TUPLE]:
            return tuple(decode(v) for v in data[TUPLE])
        if list(data) == [ENUM]:
            mod, qn, name = data[ENUM]
            return getattr(_resolve_qualname(mod, qn), name)
        if list(data) == [OBJECT]:
            spec = data[OBJECT]
            cls = _resolve_qualname(spec["module"], spec["qualname"])
            obj = object.__new__(cls)
            for k, v in spec["attrs"].items():
                object.__setattr__(obj, k, decode(v))
            return obj
        return {k: decode(v) for k, v in data.items()}
    return data


class Unencodable(Exception):
    pass


def _valid_type_origin(kind):
    module = getattr(kind, "__module__", None)
    qualname = getattr(kind, "__qualname__", None)
    return (
        isinstance(module, str)
        and module not in ("builtins", "__main__")
        and isinstance(qualname, str)
        and "<locals>" not in qualname
    )


def _encode_object(value, kind, depth):
    if isinstance(value, NON_VALUE_TYPES) or getattr(kind, "__new__", None) is not object.__new__:
        raise Unencodable()
    if not _valid_type_origin(kind):
        raise Unencodable()
    attrs = {}
    has_state = False
    if hasattr(value, "__dict__"):
        has_state = True
        d = value.__dict__
        if not all(type(k) is str for k in d):
            raise Unencodable()
        for k, v in d.items():
            attrs[k] = encode(v, depth + 1)
    for cls in kind.__mro__:
        if "__slots__" in cls.__dict__:
            has_state = True
            slots = cls.__dict__["__slots__"]
            if isinstance(slots, str):
                slots = (slots,)
            for s in slots:
                if s in ("__dict__", "__weakref__"):
                    continue
                if hasattr(value, s):
                    attrs[s] = encode(getattr(value, s), depth + 1)
    if not has_state:
        raise Unencodable()
    return {
        OBJECT: {
            "module": kind.__module__,
            "qualname": kind.__qualname__,
            "attrs": attrs,
        }
    }


def encode(value, depth=0):
    """The recorder's encoding, applied to a return value or instance state."""
    if depth > 20:
        raise Unencodable()
    kind = type(value)
    if value is None or kind in (bool, str):
        return value
    if kind is int and -INT_BOUND < value < INT_BOUND:
        return value
    if kind is float and math.isfinite(value):
        return value
    if kind is list:
        return [encode(v, depth + 1) for v in value]
    if kind is tuple:
        return {TUPLE: [encode(v, depth + 1) for v in value]}
    if kind is dict:
        if (len(value) == 1 and next(iter(value)) in RESERVED_TAGS) or not all(
            type(k) is str for k in value
        ):
            raise Unencodable()
        return {k: encode(v, depth + 1) for k, v in value.items()}
    if isinstance(value, enum.Enum):
        if _valid_type_origin(kind):
            return {ENUM: [kind.__module__, kind.__qualname__, value.name]}
        raise Unencodable()
    return _encode_object(value, kind, depth)


def type_name(cls):
    module = getattr(cls, "__module__", "")
    return cls.__qualname__ if module == "builtins" else f"{module}.{cls.__qualname__}"


def returned(value):
    try:
        data = encode(value)
        text = json.dumps(data, allow_nan=False)
    except (Unencodable, RecursionError, ValueError):
        return {"unencodable_result": type_name(type(value))}
    if len(text) > MAX_TEXT:
        return {"unencodable_result": type_name(type(value))}
    return {"returned": data}


def call(fn, data, seconds, kind="function"):
    signal.setitimer(signal.ITIMER_REAL, seconds)
    try:
        args = decode(data["args"])
        kwargs = decode(data["kwargs"])
        value = fn(*args, **kwargs)
    except CallTimeout:
        return {"timeout": seconds}
    except BaseException as e:
        return {"raised": type_name(type(e))}
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
    out = returned(value)
    if "returned" in out and kind == "instance" and args:
        try:
            after = encode(args[0])
            if after != data["args"][0]:
                out["self"] = after
                if len(json.dumps(out, allow_nan=False)) > MAX_TEXT:
                    return {"unencodable_result": type_name(type(args[0]))}
        except (Unencodable, RecursionError, ValueError):
            return {"unencodable_result": type_name(type(args[0]))}
    return out


def resolve_with_holder(t):
    module = importlib.import_module(t["module"])
    owner, _, name = t["qualname"].rpartition(".")
    holder = _resolve_qualname(t["module"], owner) if owner else module
    fn = getattr(holder, name)
    if not callable(fn):
        raise TypeError(f"{t['qualname']} is not callable")
    return fn, (holder if owner else None)


def resolve(t):
    return resolve_with_holder(t)[0]


def replay_one(t, req, deadline):
    try:
        fn = resolve(t)
    except BaseException as e:
        return {"import_error": f"{type_name(type(e))}: {e}"[:500]}
    kind = t.get("kind", "function")
    outcomes = []
    for data in t["inputs"]:
        if time.monotonic() > deadline:
            break
        outcomes.append(
            [call(fn, data, req["call_timeout"], kind) for _ in range(req["repeat"])]
        )
    return {"outcomes": outcomes}


def replay(req):
    here = os.getcwd()
    roots = [t["root"] for t in req["targets"] if t.get("root")] + req["sys_path"]
    for p in reversed(roots):
        sys.path.insert(0, os.path.join(here, p))
    signal.signal(signal.SIGALRM, _on_alarm)
    deadline = time.monotonic() + req["total_timeout"]
    loud = sys.stdout
    sys.stdout = open(os.devnull, "w")
    try:
        results = {str(t["id"]): replay_one(t, req, deadline) for t in req["targets"]}
    finally:
        sys.stdout = loud
    return {"results": results}


def _holder_strategy(holder, st, typing, inspect):
    if (
        holder is None
        or not isinstance(holder, type)
        or getattr(holder, "__new__", None) is not object.__new__
    ):
        return None
    init = getattr(holder, "__init__", None)
    if init is object.__init__:
        return st.builds(holder)
    try:
        hints = typing.get_type_hints(init)
        sig = inspect.signature(init)
    except Exception:
        return None
    for idx, (pname, p) in enumerate(sig.parameters.items()):
        if idx == 0 or p.kind in (p.VAR_POSITIONAL, p.VAR_KEYWORD):
            continue
        if pname not in hints and p.default is p.empty:
            return None
    return st.builds(holder)


def _strategy(fn, holder=None, kind="function"):
    """`(positional names, keyword-only names, strategy of a dict)`, or why
    the function's parameters cannot be generated."""
    import inspect
    import typing

    from hypothesis import strategies as st

    try:
        hints = typing.get_type_hints(fn)
    except Exception as e:
        return f"its annotations cannot be read ({type_name(type(e))})"
    positional, keyword, fields = [], [], {}
    for idx, (name, p) in enumerate(inspect.signature(fn).parameters.items()):
        if p.kind in (p.VAR_POSITIONAL, p.VAR_KEYWORD):
            continue
        if name in hints:
            fields[name] = st.from_type(hints[name])
        elif idx == 0 and kind == "instance":
            self_st = _holder_strategy(holder, st, typing, inspect)
            if self_st is None:
                return f"parameter {name} has no annotation"
            fields[name] = self_st
        else:
            return f"parameter {name} has no annotation"
        (keyword if p.kind == p.KEYWORD_ONLY else positional).append(name)
    return positional, keyword, st.fixed_dictionaries(fields)


def generate_one(t, req):
    try:
        from hypothesis import HealthCheck, Phase, given, settings
    except ImportError:
        return {"error": "Hypothesis is not installed in the test environment"}
    try:
        fn, holder = resolve_with_holder(t)
        made = _strategy(fn, holder, t.get("kind", "function"))
    except BaseException as e:
        return {"error": f"{type_name(type(e))}: {e}"[:300]}
    if isinstance(made, str):
        return {"error": made}
    positional, keyword, strategy = made
    seen, unencodable = {}, [0]

    @settings(max_examples=req["examples"], derandomize=True, database=None, deadline=None,
              phases=[Phase.generate], suppress_health_check=list(HealthCheck))
    @given(strategy)
    def collect(values):
        try:
            data = {"args": [encode(values[n]) for n in positional],
                    "kwargs": {n: encode(values[n]) for n in keyword}}
            text = json.dumps(data, sort_keys=True, separators=(",", ":"), allow_nan=False)
        except (Unencodable, RecursionError, ValueError):
            unencodable[0] += 1
            return
        if len(text) <= MAX_TEXT:
            seen.setdefault(text, data)

    try:
        collect()
    except BaseException as e:
        return {"error": f"generation failed: {type_name(type(e))}"}
    return {"inputs": [seen[k] for k in sorted(seen)], "unencodable": unencodable[0]}


def generate(req):
    here = os.getcwd()
    for p in reversed([t["root"] for t in req["targets"] if t.get("root")] + req["sys_path"]):
        sys.path.insert(0, os.path.join(here, p))
    loud = sys.stdout
    sys.stdout = open(os.devnull, "w")
    try:
        results = {str(t["id"]): generate_one(t, req) for t in req["targets"]}
    finally:
        sys.stdout = loud
    return {"results": results}


def main():
    mode, request, result = sys.argv[1:4]
    with open(request, encoding="utf-8") as f:
        req = json.load(f)
    modes = {"classify": classify, "replay": replay, "generate": generate}
    out = modes[mode](req)
    with open(result + ".part", "w", encoding="utf-8") as f:
        json.dump(out, f, allow_nan=False)
    os.replace(result + ".part", result)


main()
