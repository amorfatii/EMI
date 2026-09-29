# Rules (`emi check`)

`emi check DIR` runs declarative rules over every Python and
JavaScript/TypeScript file under `DIR`: the built-in catalog plus your own.
Rule hits are **diagnostics**. They never change burden masses, profiles or
gate verdicts; the gate stays a function of measured E/V/C.

```sh
emi check src                          # text; exit 2 on a new error-level finding
emi check src --format sarif -o emi-rules.sarif
emi check src --rules .emi/rules/      # your rules too (files or directories)
emi check src --write-baseline .emi/baseline.json   # adopt: accept what exists today
emi check src --baseline .emi/baseline.json         # afterwards only new findings count
emi check --list-rules | --test-rules
emi sarif-merge ruff.sarif eslint.sarif emi.sarif -o all.sarif
```

## Writing a rule

```yaml
rules:
  - id: no-console-in-lib
    languages: [js, ts]            # js: .js .jsx .mjs .cjs; ts: .ts .mts .cts .tsx; python: .py
    severity: warning              # note | warning | error
    tests: exclude                 # any (default) | only | exclude
    exclude: ["scripts/*"]         # fnmatch on root-relative paths
    message: "console.{{method}} in library code"
    description: Libraries should not write to the console.
    tags: [hygiene]
    query: |
      (call_expression
        function: (member_expression
          object: (identifier) @obj
          property: (property_identifier) @method)
        (#eq? @obj "console")) @match
    examples:
      match: ["console.log(1);"]
      no_match: ["logger.log(1);"]
```

- **Queries** are [tree-sitter queries] on the pinned grammars. The `@match`
  capture (else the first capture) is the reported location, and
  `{{capture}}` inserts a capture's text into the message.
- **Predicates:** `#eq?`, `#not-eq?`, `#match?` and `#not-match?`.
- **Alternatives:** list them as separate top-level patterns. Predicates
  inside a `[...]` alternation apply to every branch.
- **Examples are tests.** `emi check --test-rules` (and `cargo test` for the
  catalog) requires every `match` snippet to hit and every `no_match`
  snippet not to. An example counts for the languages it parses in, and
  each language needs at least one matching example.

[tree-sitter queries]: https://tree-sitter.github.io/tree-sitter/using-parsers/queries/

## Suppressing a finding

```js
debugger; // emi-ignore[js-debugger] until=2027-01-31: demo harness pauses here
```

- A marker after code covers its own line. A marker on a comment-only line
  covers the next line.
- A reason is mandatory, and `until=` makes the suppression expire. The
  date must be a real `YYYY-MM-DD` (`until=9999` or `until=2026-9-01`
  would never expire); an invalid one is reported and does not suppress.
- A marker without a reason, or past its date, does not suppress. It is
  reported itself (rule `suppression`), so suppressions cannot rot silently.

In SARIF, suppressed findings carry `suppressions: [{kind: inSource,
justification}]`, and baseline findings carry `baselineState: unchanged`.

## Baselines

A baseline is a set of content fingerprints: rule, file, and the matched
text with whitespace normalized, plus an occurrence index. Findings keep
their identity when code moves. `--fail-on` (default `error`) counts only
**new** findings, so a project can adopt EMI without fixing history first.

## Built-in catalog and precision

| Rule | Severity | What |
|---|---|---|
| `todo-marker` | note | TODO/FIXME/XXX/HACK comments |
| `js-debugger` | error | `debugger;` |
| `js-focused-test` | error | `it.only` / `describe.only` / … in tests (fixtures excluded) |
| `js-skipped-test` | warning | `it.skip`, `xit`, `xdescribe` in tests |
| `js-identical-branches` | warning | if/else or ternary with identical branches |
| `js-self-assignment` | warning | `x = x` |
| `js-eval` | warning | `eval(...)`, `new Function(...)` |
| `py-breakpoint` | error | `breakpoint()`, `pdb.set_trace()` |
| `py-mutable-default` | warning | list/dict/set default arguments |
| `py-bare-except` | warning | bare `except:` that neither re-raises nor only passes |
| `py-silent-except` | warning | bare or `Exception`/`BaseException` handler that only passes |
| `py-identical-branches` | warning | if/else (no elif) or conditional expression with identical branches |
| `py-self-assignment` | note | `self.a = self.a` anywhere; `x = x` in function bodies |
| `py-wildcard-import` | note | `from m import *` |

The catalog was measured on the corpus, CPython's stdlib plus
site-packages, and 25k JS/TS files of `node_modules`, and every rule's
sampled hits were read (2026-09-26). The first draft had false-positive
classes, each fixed and pinned with a `no_match` example:
- class-body `codec = codec` idioms;
- `if A: X … elif … else: X` chains;
- `except: cleanup(); raise`;
- narrow `except KeyError: pass`;
- mocha's own `.only` fixtures.

After those fixes every sampled hit showed the construct the rule
describes. The remaining judgment calls are documented in the rule
descriptions, e.g. property-setter self-assignment.
