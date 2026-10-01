"""Record each test's execution separately from module imports."""

import coverage
import pytest


@pytest.hookimpl(hookwrapper=True)
def pytest_runtest_protocol(item, nextitem):
    cov = coverage.Coverage.current()
    if cov is None:
        raise RuntimeError("EMI per-test execution requires active coverage")
    cov.switch_context(item.nodeid)
    try:
        yield
    finally:
        cov.switch_context("")
