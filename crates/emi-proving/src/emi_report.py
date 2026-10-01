"""Run pytest and end with exact lines a reader can rely on.

What pytest prints while it works cannot be read back test by test: a test
that writes to the terminal pushes its outcome word onto another line, a
parallel runner prints no per-test line, and a project's own options change
how a listing looks. The reports pytest hands its plugins are exact, so
this collects them and prints them at the end, closed by a count:

- a run: `EMI-OUTCOME <outcome> <id>` per test, then `EMI-OUTCOMES <n>`;
- a listing (`--collect-only`): `EMI-ITEM <id>` per test and
  `EMI-BROKEN <file>` per file that could not be collected, then
  `EMI-ITEMS <n>`.

The arguments are pytest's own.
"""
import sys

import pytest

_RANK = {"passed": 0, "xpassed": 1, "skipped": 2, "xfailed": 2, "failed": 3, "error": 4}
_LISTING = ("--collect-only", "--collectonly", "--co")


def _outcome(report):
    if report.failed:
        return "failed" if report.when == "call" else "error"
    if report.skipped:
        return "xfailed" if hasattr(report, "wasxfail") else "skipped"
    if report.when == "call":
        return "xpassed" if hasattr(report, "wasxfail") else "passed"
    return None


def _line(text):
    return str(text).replace("\n", " ")


class _Reporter:
    def __init__(self, listing):
        self.listing = listing
        self.rows = {}
        self.items = []

    def pytest_collectreport(self, report):
        # a file that cannot be collected: none of its tests ran
        if report.failed:
            self.rows[report.nodeid] = "error"

    def pytest_collection_finish(self, session):
        self.items = [item.nodeid for item in session.items]

    def pytest_runtest_logreport(self, report):
        new = _outcome(report)
        # the worst phase is the test's outcome (a teardown error after a
        # passing call is an error)
        if new is not None and _RANK[new] >= _RANK.get(self.rows.get(report.nodeid), -1):
            self.rows[report.nodeid] = new

    def pytest_unconfigure(self, config):
        out = sys.__stdout__
        out.write("\n")
        if self.listing:
            for nodeid in self.items:
                out.write("EMI-ITEM %s\n" % _line(nodeid))
            for nodeid in sorted(self.rows):
                out.write("EMI-BROKEN %s\n" % _line(nodeid))
            out.write("EMI-ITEMS %d\n" % len(self.items))
        else:
            for nodeid in sorted(self.rows):
                out.write("EMI-OUTCOME %s %s\n" % (self.rows[nodeid], _line(nodeid)))
            out.write("EMI-OUTCOMES %d\n" % len(self.rows))
        out.flush()


_ARGS = sys.argv[1:]
sys.exit(int(pytest.main(_ARGS, plugins=[_Reporter(any(a in _LISTING for a in _ARGS))])))
