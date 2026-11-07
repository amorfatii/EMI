"""EMI's pins, loaded into every test run as a pytest plugin (`-p emi_pins`).

The environment already fixes the hash seed, the time zone and the locale.
This plugin pins what can only be set inside the process:

- `random` (and numpy's global generator, when loaded) is seeded before
  each test from a hash of the test's id, so what a test draws depends on
  the test alone, not on which tests ran before it;
- Hypothesis, when it is installed, runs every collected test (`@given`
  functions and state machines alike) with generation derandomized, no
  example database and no deadline (a deadline turns a slow machine into a
  failing test), over whatever else the project set.

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


_PINNED = {"derandomize": True, "database": None, "deadline": None}


def pytest_collection_finish(session):
    # After collection, so a profile a conftest.py loads is the parent. The
    # profile alone pins nothing already collected: `@given` takes the
    # default settings when its module is imported, and a state machine's
    # TestCase carries its own. Each collected test is pinned where its
    # settings are read when it runs, keeping everything else it set.
    if not _on("EMI_PIN_HYPOTHESIS"):
        return
    try:
        from hypothesis import settings
    except ImportError:
        return
    settings.register_profile("emi-pinned", parent=settings(), **_PINNED)
    settings.load_profile("emi-pinned")
    for item in session.items:
        _pin(settings, item)
    _no_local_constants()


def _no_local_constants():
    # Hypothesis also draws literals found in every project module imported
    # so far, so a test's inputs would depend on which tests ran before it
    # and on the selection run. With none, they depend on the test alone. A
    # Hypothesis without this helper does not draw them at all.
    try:
        from hypothesis.internal.conjecture import providers
    except ImportError:
        return
    if not hasattr(providers, "_get_local_constants"):
        return
    empty = providers.Constants()
    providers._get_local_constants = lambda: empty
    cache = getattr(providers, "CONSTANTS_CACHE", None)
    if cache is not None:
        cache.cache.clear()


def _pin(settings, item):
    test = getattr(item, "obj", None)
    test = getattr(test, "__func__", test)
    if not getattr(test, "is_hypothesis_test", False):
        return
    case = getattr(item, "cls", None)
    if getattr(test, "_hypothesis_state_machine_class", None) is not None and case is not None:
        case.settings = settings(parent=case.settings, **_PINNED)
        return
    own = getattr(test, "_hypothesis_internal_use_settings", None) or settings.default
    test._hypothesis_internal_use_settings = settings(parent=own, **_PINNED)


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
