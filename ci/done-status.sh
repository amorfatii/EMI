#!/usr/bin/env bash
# One measurement; publish the receipt before making its action the job status.
set -euo pipefail
: "${BASELINE:?Set BASELINE to the trusted task baseline}"
: "${CANDIDATE:=HEAD}"
: "${GITHUB_OUTPUT:?GITHUB_OUTPUT is required}"
: "${GITHUB_STEP_SUMMARY:?GITHUB_STEP_SUMMARY is required}"
set +e
emi done --baseline "$BASELINE" --candidate "$CANDIDATE" --setting enforced --format json > emi-done.json
code=$?
set -e
EMI_EXIT_CODE="$code" python3 - <<'PY'
import json
import os
from pathlib import Path

code = int(os.environ['EMI_EXIT_CODE'])
try:
    receipt = json.loads(Path('emi-done.json').read_text())
    action = receipt['action']
    expected = {'stop': 0, 'repair': 3, 'ask_person': 4, 'incomplete': 5}[action]
    if expected != code:
        raise ValueError('receipt action disagrees with the exit code')
    regression = receipt['regression']['status']
    change = receipt['change']['status']
    if action == 'stop' and regression != 'held':
        raise ValueError('stop without held regression evidence')
    comment = (f'## EMI: {action}\n\nRegression: **{regression}**. '
               f'Change evidence: **{change}**.\n\n'
               'Stop is not a claim of task correctness. EMI never merges.\n\n'
               '<details><summary>Receipt and limits</summary>\n\n```json\n'
               + json.dumps(receipt, indent=2, ensure_ascii=True) + '\n```\n</details>\n')
except (OSError, ValueError, KeyError, TypeError) as error:
    action, regression, change = 'incomplete', 'unknown', 'unknown'
    comment = f'## EMI: incomplete\n\nNo readable completion receipt: {error}.\n'
    code = code or 5
Path('emi-comment.md').write_text(comment)
with open(os.environ['GITHUB_OUTPUT'], 'a') as output:
    output.write(f'code={code}\naction={action}\nregression={regression}\nchange={change}\n')
with open(os.environ['GITHUB_STEP_SUMMARY'], 'a') as summary:
    summary.write(comment)
PY
