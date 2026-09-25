#!/usr/bin/env python3
"""Print the v0.5.1 oracle's (function, line, sloc, cc) table for a snippet.

Usage: oracle_snippet.py python|js FILE   (EMI_ORACLE points at the emi-v0.4 checkout)
"""
import json
import os
import shutil
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, os.environ.get("EMI_ORACLE", str(Path.home() / "Projects/emi-v0.4")))

lang, path = sys.argv[1], Path(sys.argv[2])
with tempfile.TemporaryDirectory() as tmp:
    shutil.copy(path, Path(tmp) / path.name)
    if lang == "python":
        import python_probe as probe
    else:
        import js_probe as probe
    out = probe.collect(tmp, on_syntax_error="record")
print(json.dumps([[f["function"], f["line"], f["sloc"], f["cc"]] for f in out["functions"]]))
print(json.dumps([[r["id"], r["sloc"]] for r in out["regions"]]))
