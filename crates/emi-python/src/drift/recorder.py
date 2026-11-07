"""Input recorder for `emi drift`, loaded as `sitecustomize`.

`emi drift` puts this file's directory first on PYTHONPATH in a copy of the
base commit, so every Python process the test command starts (pytest,
`python -m unittest`, subprocesses) loads it before any test code, and no
test runner plugin or change to the project is needed. A meta path finder
wraps the target functions right after their module executes; each wrapper
records the call's arguments when every one has a stable JSON encoding and
counts the call otherwise. At exit the process writes what it saw to its
own file in the output directory, even when it saw nothing, so the caller
can tell "loaded but never called" from "never loaded".

A `sitecustomize` the environment already had is loaded after this one.
"""

import atexit
import enum
import functools
import importlib.machinery
import importlib.util
import json
import math
import os
import sys
import types

MAX_INPUTS = 50
MAX_TEXT = 10_000
INT_BOUND = 2**63
TUPLE = "$tuple"
OBJECT = "$object"
ENUM = "$enum"
BYTES = "$bytes"
SET = "$set"
FROZENSET = "$frozenset"
NAMEDTUPLE = "$namedtuple"
RESERVED_TAGS = (TUPLE, OBJECT, ENUM, BYTES, SET, FROZENSET, NAMEDTUPLE)
NON_VALUE_TYPES = (
    type,
    types.ModuleType,
    types.FunctionType,
    types.MethodType,
    types.BuiltinFunctionType,
    types.GeneratorType,
    types.CoroutineType,
)

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.realpath(os.environ.get("EMI_DRIFT_ROOT", "."))


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


def canonical(data):
    return json.dumps(data, sort_keys=True, separators=(",", ":"), ensure_ascii=False, allow_nan=False)


def encode(value, depth=0):
    """The value as JSON data, or Unencodable."""
    if depth > 20:
        raise Unencodable()
    kind = type(value)
    if value is None or kind in (bool, str):
        return value
    if kind is int:
        if -INT_BOUND < value < INT_BOUND:
            return value
        raise Unencodable()
    if kind is float:
        if math.isfinite(value):
            return value
        raise Unencodable()
    if kind is bytes:
        return {BYTES: value.hex()}
    if kind is list:
        return [encode(v, depth + 1) for v in value]
    if kind is tuple:
        # a tuple and a list behave differently, so the tag keeps them apart
        return {TUPLE: [encode(v, depth + 1) for v in value]}
    if kind in (set, frozenset):
        items = [encode(v, depth + 1) for v in value]
        items.sort(key=canonical)
        return {SET if kind is set else FROZENSET: items}
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
    if (
        isinstance(value, tuple)
        and kind is not tuple
        and hasattr(kind, "_fields")
        and _valid_type_origin(kind)
    ):
        return {
            NAMEDTUPLE: {
                "module": kind.__module__,
                "qualname": kind.__qualname__,
                "fields": {f: encode(getattr(value, f), depth + 1) for f in kind._fields},
            }
        }
    return _encode_object(value, kind, depth)


class Target:
    def __init__(self, spec):
        self.id = spec["id"]
        self.qualname = spec["qualname"]
        self.kind = spec["kind"]
        self.module = None
        self.root = None
        self.inputs = {}
        self.unrecordable = 0
        self.calls = 0
        self.missing = False

    def record(self, args, kwargs):
        self.calls += 1
        try:
            data = {"args": [encode(a) for a in args], "kwargs": {k: encode(v) for k, v in kwargs.items()}}
            text = canonical(data)
        except (Unencodable, RecursionError, ValueError):
            self.unrecordable += 1
            return
        if len(text) > MAX_TEXT:
            self.unrecordable += 1
            return
        if len(self.inputs) < MAX_INPUTS:
            self.inputs.setdefault(text, data)

    def report(self):
        return {
            "module": self.module,
            "root": self.root,
            "missing": self.missing,
            "inputs": list(self.inputs.values()),
            "unrecordable": self.unrecordable,
            "calls": self.calls,
        }


def load_targets():
    with open(os.environ["EMI_DRIFT_TARGETS"], encoding="utf-8") as f:
        specs = json.load(f)
    by_file = {}
    for spec in specs:
        path = os.path.realpath(os.path.join(ROOT, spec["file"]))
        by_file.setdefault(path, []).append(Target(spec))
    return by_file


def recording(target, fn, skip_first):
    @functools.wraps(fn)
    def wrapper(*args, **kwargs):
        try:
            target.record(args[1:] if skip_first else args, kwargs)
        except Exception:
            target.unrecordable += 1
        return fn(*args, **kwargs)

    return wrapper


def module_root(module, path):
    """The sys.path entry the module was imported from, relative to the copy."""
    parts = module.__name__.split(".")
    up = len(parts) if os.path.basename(path) == "__init__.py" else len(parts) - 1
    root = os.path.dirname(path)
    for _ in range(up):
        root = os.path.dirname(root)
    rel = os.path.relpath(root, ROOT)
    return None if rel.startswith("..") else rel


def install(target, module):
    owner, _, name = target.qualname.rpartition(".")
    holder = module
    if owner:
        for part in owner.split("."):
            holder = getattr(holder, part)
    raw = holder.__dict__.get(name) if owner else getattr(module, name, None)
    if target.kind in ("function", "instance", "async") and callable(raw):
        setattr(holder, name, recording(target, raw, False))
    elif target.kind == "static" and isinstance(raw, staticmethod):
        setattr(holder, name, staticmethod(recording(target, raw.__func__, False)))
    elif target.kind == "class" and isinstance(raw, classmethod):
        setattr(holder, name, classmethod(recording(target, raw.__func__, True)))
    else:
        target.missing = True


class Finder:
    """Finds nothing itself: for a target file it asks the path finder for
    the spec and wraps the targets once the module has executed."""

    def __init__(self, by_file):
        self.by_file = by_file

    def find_spec(self, name, path=None, target=None):
        spec = importlib.machinery.PathFinder.find_spec(name, path)
        origin = spec and spec.origin and os.path.realpath(spec.origin)
        targets = self.by_file.get(origin)
        if not targets or not hasattr(spec.loader, "exec_module"):
            return None
        execute = spec.loader.exec_module

        def exec_module(module):
            execute(module)
            for t in targets:
                t.module, t.root = module.__name__, module_root(module, origin)
                try:
                    install(t, module)
                except Exception:
                    t.missing = True

        spec.loader.exec_module = exec_module
        return spec


def write(by_file):
    out = {
        "executable": sys.executable,
        "sys_path": [os.path.relpath(p, ROOT) for p in map(os.path.realpath, sys.path) if p == ROOT or p.startswith(ROOT + os.sep)],
        "targets": {str(t.id): t.report() for ts in by_file.values() for t in ts},
    }
    path = os.path.join(os.environ["EMI_DRIFT_OUT"], f"records-{os.getpid()}.json")
    with open(path + ".part", "w", encoding="utf-8") as f:
        json.dump(out, f, allow_nan=False)
    os.replace(path + ".part", path)


def chain():
    """Load the sitecustomize this one shadows, if there is one."""
    rest = [p for p in sys.path if os.path.realpath(p or ".") != HERE]
    spec = importlib.machinery.PathFinder.find_spec("sitecustomize", rest)
    if spec is None or spec.loader is None:
        return
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)


def start():
    if "EMI_DRIFT_TARGETS" not in os.environ:
        return
    by_file = load_targets()
    sys.meta_path.insert(0, Finder(by_file))
    atexit.register(write, by_file)


start()
chain()
