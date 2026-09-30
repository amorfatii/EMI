# Places to look (`emi places`)

A change can pass every test of its repository and still break something:
a caller in another repository, a client of an API, a file that always
changes with the one that did. `emi places` lists where to look for that.
It reads two deterministic signals and scores nothing: it is neither a
score nor a gate, and it exits 0 whenever it could read the change.

```sh
emi places --base origin/main --head HEAD --format text
emi places --repo . --base HEAD~1 --head HEAD --source src --limit 50   # JSON
```

The JSON document is `{"surface": {...}, "impact": {...}}`, written by the
canonical encoder.

## Public surface and schema boundaries

`surface` compares the exported names of every changed file between the
base and the head:

- `changes`: `{file, name, lang, kind, old_signature, new_signature}` where
  `kind` is `added`, `removed` or `signature_changed`. A signature is the
  declared parameter list with its return type, whitespace normalised so
  that reformatting is not a change (`(a, b=1) -> int`).
- `boundaries`: `{file, label}` for changed paths that are schema
  boundaries: `migration` (`migrations/`, `alembic/versions/`), `protobuf`
  (`*.proto`), `sql`, `openapi` (`openapi*`/`swagger*` `.yaml`/`.yml`/`.json`),
  `json_schema` (`*.schema.json`), `serializer` (`serializers.py`), `model`
  (`models.py`), `graphql`, `avro` (`*.avsc`) and `thrift`. One path can
  carry several labels (`migrations/0001.sql`).
- `unparsed`: changed source files whose names could not be read on some
  side. Their surface is unknown, not unchanged.
- `unsupported`: changed files in no language below.

What counts as exported, per language:

| Language | Exported | Signature |
|---|---|---|
| Python | a literal `__all__` list or tuple, exactly; otherwise module-level `def`, `class` and assigned names not starting with `_` | functions: parameters and return annotation |
| Go | capitalised top-level functions, types, constants and variables, and capitalised methods of capitalised types (`Type.Method`) | functions and methods: parameters and results |
| Rust | `pub` items at the top level and in `pub` inline modules (`m.item`), and `pub` methods of inherent impls (`Type.method`) | functions: parameters and return type |
| JavaScript / TypeScript | `export` declarations, `export { a as b }`, `export * as ns`, `export * from 'x'` (named `* from 'x'`), and `default` | functions: parameters (and TypeScript return type) |

Limits: names bound inside Python `if`/`try` blocks at module level are
not read, and an `__all__` that is computed or modified in place
(`__all__ += ...`, `__all__.extend(...)`) makes the file `unparsed`.
Rust `pub(crate)` and other restricted visibility is not public, and
`pub use` re-exports are not listed. CommonJS `module.exports` is not read.
Types, classes and constants have no signature, so only their addition or
removal is reported. A renamed file is compared under its new path; the
move itself is not reported as a surface change.

## Missing co-changes and importers

`impact` answers "what else usually changes with this, and who uses it":

- `co_change`: files that historically change together with a touched file
  (change coupling, as `emi hotspots` computes it, see
  [history.md](history.md)) but were not touched by this change:
  `{touched, partner, shared_commits, degree}`, highest degree first. The
  history is read **up to the base commit only**, so the change is never
  its own evidence. The thresholds are the `emi hotspots` defaults and
  can be set the same way: `--days` 365 before the base commit's time,
  `--min-shared` 5 shared commits, `--min-degree` 30 percent of the pair's
  mean revisions, `--max-changeset` 30 files per commit. `thresholds`
  echoes the values used. `too_few_revisions` lists touched files with
  fewer revisions than `--min-shared` in the window: none of their
  partners could reach the threshold, which is not the same as having
  none.
- `dependants`: untouched files that import a touched file, from the
  import graph the scanner builds for coupling, read at the base commit
  within `--source` (default: the whole repository): `{file, imports}`.
  Python test files are not product modules and are not listed. Import
  resolution is the scanner's (see [languages.md](languages.md)); an
  import it cannot resolve has no edge.

Both lists show at most `--limit` rows (default 20); `total` counts all.

When a signal cannot be read, it says so instead of reporting an empty
list:

- `co_change.status` is `unverified` with a `reason` in a shallow clone
  (the history before the base is truncated) or when the window holds
  fewer commits than `--min-shared`.
- `dependants.status` is `verified` when every needed language's import
  graph was read in full, `partial` when some language failed
  (`unresolved`, with the reason) or some files did not parse
  (`unparsed`), and `unverified` when none could be read.
