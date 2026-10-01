"""Run pytest and end with one line per test: `EMI-OUTCOME <outcome> <id>`.

What pytest prints while it runs cannot be read back test by test: a test
that writes to the terminal pushes its outcome word onto another line, and
a parallel runner prints no per-test line at all. The reports pytest hands
its plugins are exact, so this collects them and prints them at the end,
closed by a count. The arguments are pytest's own.
"""
import sys

import pytest

_RANK = {"passed": 0, "xpassed": 1, "skipped": 2, "xfailed": 2, "failed": 3, "error": 4}


def _outcome(report):
    if report.failed:
        return "failed" if report.when == "call" else "error"
    if report.skipped:
        return "xfailed" if hasattr(report, "wasxfail") else "skipped"
    if report.when == "call":
        return "xpassed" if hasattr(report, "wasxfail") else "passed"
    return None


class _Outcomes:
    def __init__(self):
        self.rows = {}

    def pytest_collectreport(self, report):
        # a file that cannot be collected: none of its tests ran
        if report.failed:
            self.rows[report.nodeid] = "error"

    def pytest_runtest_logreport(self, report):
        new = _outcome(report)
        # the worst phase is the test's outcome (a teardown error after a
        # passing call is an error)
        if new is not None and _RANK[new] >= _RANK.get(self.rows.get(report.nodeid), -1):
            self.rows[report.nodeid] = new

    def pytest_unconfigure(self, config):
        out = sys.__stdout__
        out.write("\n")
        for nodeid in sorted(self.rows):
            out.write("EMI-OUTCOME %s %s\n" % (self.rows[nodeid], nodeid.replace("\n", " ")))
        out.write("EMI-OUTCOMES %d\n" % len(self.rows))
        out.flush()


sys.exit(int(pytest.main(sys.argv[1:], plugins=[_Outcomes()])))
