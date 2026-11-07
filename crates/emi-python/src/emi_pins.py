"""EMI's pins, loaded into every test run as a pytest plugin (`-p emi_pins`).

The environment already fixes the hash seed, the time zone and the locale.
This plugin pins what can only be set inside the process:

- `random` (and numpy's global generator, when loaded) is seeded before
  each test from a hash of the test's id, so what a test draws depends on
  the test alone, not on which tests ran before it;
- Hypothesis, when it is installed, runs every collected test (`@given`
  functions and state machines alike) with generation derandomized, no
  example database and no deadline (a deadline turns a slow machine into a
  failing test), over whatever else the project set. It reports the first
  failure it finds (looking on for others ends on a ten-second clock), it
  does not explain a failure (the explain phase reruns it under a tracer:
  slow, and its text names paths on this machine), and it shrinks a failure
  for a fixed number of test calls rather than until a clock runs out, so
  the counterexample does not depend on the machine's speed.

A run that only confirms a failure (EMI_PIN_CONFIRM=1) does not shrink:
generation is derandomized, so it reaches the same first failure, and
whether it fails is all it is asked.

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


_PINNED = {"derandomize": True, "database": None, "deadline": None, "report_multiple_bugs": False}

#: Test calls Hypothesis may make shrinking after its first failure.
SHRINK_CALLS = 1000


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
    settings.register_profile("emi-pinned", parent=_pinned(settings, settings()))
    settings.load_profile("emi-pinned")
    for item in session.items:
        _pin(settings, item)
    _no_local_constants()
    _bounded_shrinking()


def _pinned(settings, parent):
    from hypothesis import Phase

    dropped = {Phase.explain}
    if os.environ.get("EMI_PIN_CONFIRM") == "1":
        dropped.add(Phase.shrink)
    phases = tuple(p for p in parent.phases if p not in dropped)
    return settings(parent=parent, phases=phases, **_PINNED)


def _bounded_shrinking():
    # Hypothesis stops shrinking after five minutes by the clock, so a slow
    # shrink ends wherever the machine had got to. A count of calls ends it
    # at the same place on every machine, and the clock is moved out of
    # reach. A Hypothesis without these internals is left as it is.
    try:
        from hypothesis.internal.conjecture import engine
    except ImportError:
        return
    runner = getattr(engine, "ConjectureRunner", None)
    reason = getattr(getattr(engine, "ExitReason", None), "max_shrinks", None)
    if runner is None or reason is None or not hasattr(engine, "MAX_SHRINKING_SECONDS"):
        return
    if getattr(runner.test_function, "_emi_bounded", False):
        return
    calls = runner.test_function

    def test_function(self, data):
        first = getattr(self, "first_bug_found_at", None)
        if first is not None and self.call_count - first >= SHRINK_CALLS:
            self.exit_with(reason)
        return calls(self, data)

    test_function._emi_bounded = True
    runner.test_function = test_function
    engine.MAX_SHRINKING_SECONDS = 10**9


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
        case.settings = _pinned(settings, case.settings)
        return
    own = getattr(test, "_hypothesis_internal_use_settings", None) or settings.default
    test._hypothesis_internal_use_settings = _pinned(settings, own)


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
