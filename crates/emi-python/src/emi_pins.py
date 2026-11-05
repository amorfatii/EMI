"""EMI's pins, loaded into every test run as a pytest plugin (`-p emi_pins`).

The environment already fixes the hash seed, the time zone and the locale.
This plugin pins what can only be set inside the process:

- `random` (and numpy's global generator, when loaded) is seeded before
  each test from a hash of the test's id, so what a test draws depends on
  the test alone, not on which tests ran before it;
- Hypothesis, when it is installed, runs under a profile derived from the
  project's own with generation derandomized, no example database and no
  deadline (a deadline turns a slow machine into a failing test).

Each part can be switched off through the environment (EMI_PIN_RANDOM,
EMI_PIN_HYPOTHESIS) as the configuration says.
"""

import hashlib
import os
import random
import sys


def _on(name):
    return os.environ.get(name, "1") == "1"


def _seed(nodeid):
    return int.from_bytes(hashlib.sha256(nodeid.encode("utf-8")).digest()[:8], "big")


def pytest_collection_finish(session):
    # after every conftest.py: a profile the project loads there is the parent
    if not _on("EMI_PIN_HYPOTHESIS"):
        return
    try:
        from hypothesis import settings
    except ImportError:
        return
    settings.register_profile(
        "emi-pinned", parent=settings(), derandomize=True, database=None, deadline=None
    )
    settings.load_profile("emi-pinned")


def pytest_runtest_setup(item):
    if not _on("EMI_PIN_RANDOM"):
        return
    seed = _seed(item.nodeid)
    random.seed(seed)
    numpy = sys.modules.get("numpy")
    if numpy is not None:
        try:
            numpy.random.seed(seed % (2**32))
        except Exception:
            pass
