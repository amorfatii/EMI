# Behavioral analysis (`emi hotspots`, `emi diff --history`)

Structure says where burden is; history says where it costs. `emi hotspots`
measures the exact tree at `--head` (raw blobs, as `emi diff` does) and joins
it with the history of the scope:

```sh
emi hotspots --repo . --source src --probe js [--days 365] [--format json]
```

- **History** is mined once with `git log -M --no-merges --numstat` inside
  the window (days before HEAD's *commit* time, so reruns are identical).
  Paths follow renames, so a file's history survives moves.
- **Per file**:
  - revisions, lines added and deleted;
  - distinct authors and the main author's share of revisions (knowledge
    concentration);
  - age;
  - AI-attributed and bot revisions.
- **Hotspots** are ranked by `revisions × burden`, where burden is the file's
  E+V+C mass interval `[known, known + unknown]`. They are ranked on the
  interval's midpoint, then its upper end. A file that is burdensome but
  never touched, or touched constantly but clean, is not a hotspot.
- **Change coupling** covers pairs of current files that change together in at
  least `--min-shared` commits (default 5) with degree ≥ `--min-degree`
  percent (shared ÷ mean revisions, default 30). Commits touching more than
  `--max-changeset` scope files (default 30) are ignored as mass edits.
- **AI attribution** matches the author and every `Co-authored-by` trailer
  against `--ai-pattern` (default: Claude/Anthropic, Copilot, Codex/OpenAI,
  Gemini, Cursor, Devin, Aider, and similar agents). `[bot]` accounts are
  counted separately. Per-file `ai_share` is the AI-attributed share of
  revisions.

The default profile for `hotspots` is `graded` (docs/profiles.md: `precise` coupling plus graded erosion): coupling
burden is then mostly known mass instead of unknown width.

## Exposure in reviews: `emi diff --history`

The calculator has always supported an exposure view: components weighted
by `sloc × (1 + min(1, churn / (2·sloc)))` over exactly 90 days of history.
Until now nothing supplied churn. `emi diff --history` attaches `churn_lines`
(lines added + deleted in the 90 days before the *base* commit, following
renames) to every base component and sets `history_days: 90`, so the
report's `exposure.after` and `exposure.matched` are populated. The window is
anchored to the base commit rather than today, so the same diff always
gives the same report. The oracle's `git_churn` helper used "90 days ago".
