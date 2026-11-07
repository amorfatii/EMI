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
import importlib
import json
import math
import os
import signal
import sys
import time

TUPLE = "$tuple"
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


def decode(data):
    if isinstance(data, list):
        return [decode(v) for v in data]
    if isinstance(data, dict):
        if list(data) == [TUPLE]:
            return tuple(decode(v) for v in data[TUPLE])
        return {k: decode(v) for k, v in data.items()}
    return data


class Unencodable(Exception):
    pass


def encode(value, depth=0):
    """The recorder's encoding, applied to a return value."""
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
    if kind is dict and list(value) != [TUPLE] and all(type(k) is str for k in value):
        return {k: encode(v, depth + 1) for k, v in value.items()}
    raise Unencodable()


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


def call(fn, data, seconds):
    signal.setitimer(signal.ITIMER_REAL, seconds)
    try:
        value = fn(*decode(data["args"]), **decode(data["kwargs"]))
    except CallTimeout:
        return {"timeout": seconds}
    except BaseException as e:
        return {"raised": type_name(type(e))}
    finally:
        signal.setitimer(signal.ITIMER_REAL, 0)
    return returned(value)


def resolve(t):
    module = importlib.import_module(t["module"])
    owner, _, name = t["qualname"].rpartition(".")
    holder = getattr(module, owner) if owner else module
    fn = getattr(holder, name)
    if not callable(fn):
        raise TypeError(f"{t['qualname']} is not callable")
    return fn


def replay_one(t, req, deadline):
    try:
        fn = resolve(t)
    except BaseException as e:
        return {"import_error": f"{type_name(type(e))}: {e}"[:500]}
    outcomes = []
    for data in t["inputs"]:
        if time.monotonic() > deadline:
            break
        outcomes.append([call(fn, data, req["call_timeout"]) for _ in range(req["repeat"])])
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


def _strategy(fn):
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
    for name, p in inspect.signature(fn).parameters.items():
        if p.kind in (p.VAR_POSITIONAL, p.VAR_KEYWORD):
            continue
        if name not in hints:
            return f"parameter {name} has no annotation"
        fields[name] = st.from_type(hints[name])
        (keyword if p.kind == p.KEYWORD_ONLY else positional).append(name)
    return positional, keyword, st.fixed_dictionaries(fields)


def generate_one(t, req):
    try:
        from hypothesis import HealthCheck, Phase, given, settings
    except ImportError:
        return {"error": "Hypothesis is not installed in the test environment"}
    try:
        fn = resolve(t)
        made = _strategy(fn)
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
