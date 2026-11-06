"""Run pytest under coverage and write the JSON report, in one interpreter.

usage: python -c <this> RCFILE OUTPUT PYTEST_ARGS...

`coverage run -m pytest` and `coverage json` as two processes pay for the
interpreter, coverage and the report's imports twice; this does the same
work once. The report is written only when the tests passed, as the two
steps did: the second ran only after the first exited 0. A changed file
that does not parse is left out of the report (ignore_errors), so it is
unknown rather than failing the report for every file.
"""

import sys

import coverage

rcfile, output, args = sys.argv[1], sys.argv[2], sys.argv[3:]
cov = coverage.Coverage(config_file=rcfile)
cov.start()
import pytest  # noqa: E402  measured from its import on, as `coverage run` does

code = int(pytest.main(args))
cov.stop()
cov.save()
if code != 0:
    sys.exit(code)
cov.json_report(outfile=output, ignore_errors=True)
