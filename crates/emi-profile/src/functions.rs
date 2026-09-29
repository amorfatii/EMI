//! One record per function of the measured scope: its structure (panel
//! metrics and refactoring steps), its file's change history, the lines a
//! coverage report says the tests ran, and its sampled mutants.

use std::collections::BTreeMap;
use std::path::Path;

use emi_panel::Lang;
use emi_panel::explain::{Explained, explain};
use emi_panel::mutate::{Mutant, mutants};

use crate::coverage::{Lines, Resolved};
use crate::history::History;
use crate::mutation::{FunctionKey, Tally};

/// What a coverage report says about one function.
#[derive(Clone, Debug, PartialEq)]
pub enum Coverage {
    NoReport,
    NotInReport,
    /// The report names lines past the end of the file: it is from another
    /// version of it, so its line numbers mean nothing here.
    Stale,
    Lines {
        executable: u32,
        executed: u32,
    },
}

/// Why a function has no mutation result.
#[derive(Clone, Debug, PartialEq)]
pub enum Mutation {
    NotRequested,
    NotSelected,
    NoMutants,
    BaselineFailed,
    Tally(Tally),
}

pub struct Record {
    /// Repository-relative path.
    pub file: String,
    pub e: Explained,
    pub revisions: u32,
    pub authors: u32,
    pub ai_share: f64,
    /// Files that change together with this one: `(file, shared, degree)`.
    pub coupled: Vec<(String, u32, f64)>,
    pub coverage: Coverage,
    pub mutation: Mutation,
}

impl Record {
    pub fn key(&self) -> FunctionKey {
        (
            self.file.clone(),
            self.e.function.name.clone(),
            self.e.function.start,
        )
    }

    /// Revisions × graded erosion: the hotspot product at function level.
    pub fn exposure(&self) -> f64 {
        f64::from(self.revisions) * self.e.mass.total
    }

    pub fn lines(&self) -> Option<(u32, u32)> {
        match self.coverage {
            Coverage::Lines {
                executable,
                executed,
            } => Some((executable, executed)),
            _ => None,
        }
    }
}

/// `scope/id` for a scope-relative component id.
pub fn repo_path(scope: &str, id: &str) -> String {
    if scope.is_empty() {
        id.to_owned()
    } else {
        format!("{scope}/{id}")
    }
}

fn coupled(h: &History, id: &str) -> Vec<(String, u32, f64)> {
    let mut out: Vec<(String, u32, f64)> = h
        .coupling
        .iter()
        .filter_map(|c| match (c.a == id, c.b == id) {
            (true, _) => Some((c.b.clone(), c.shared, c.degree)),
            (_, true) => Some((c.a.clone(), c.shared, c.degree)),
            _ => None,
        })
        .collect();
    out.sort_by(|x, y| y.2.total_cmp(&x.2));
    out.truncate(3);
    out
}

/// Every function of every measured file at the head, read from `tree`
/// (the head's files). Files that cannot be read or parsed are returned
/// with why, never profiled from a partial parse.
pub fn collect(tree: &Path, scope: &str, h: &History) -> (Vec<Record>, Vec<String>) {
    let (mut records, mut skipped) = (Vec::new(), Vec::new());
    let ids = h.measurement["components"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["id"].as_str());
    for id in ids {
        let file = repo_path(scope, id);
        let Some(lang) = Lang::of_path(&file) else {
            continue;
        };
        let src = match std::fs::read(tree.join(&file)) {
            Ok(s) => s,
            Err(e) => {
                skipped.push(format!("{file}: {e}"));
                continue;
            }
        };
        if !emi_panel::parses(lang, &src) {
            skipped.push(format!(
                "{file}: does not parse; its functions are not profiled"
            ));
            continue;
        }
        let spot = h.hotspots.iter().find(|s| s.file == id);
        for e in explain(lang, &src) {
            records.push(Record {
                file: file.clone(),
                e,
                revisions: spot.map_or(0, |s| s.revisions),
                authors: spot.map_or(0, |s| s.authors),
                ai_share: spot.map_or(0.0, |s| s.ai_share),
                coupled: coupled(h, id),
                coverage: Coverage::NoReport,
                mutation: Mutation::NotRequested,
            });
        }
    }
    (records, skipped)
}

/// Highest exposure first, then graded erosion, then position.
pub fn rank(records: &mut [Record]) {
    records.sort_by(|a, b| {
        b.exposure()
            .total_cmp(&a.exposure())
            .then(b.e.mass.total.total_cmp(&a.e.mass.total))
            .then(a.file.cmp(&b.file))
            .then(a.e.function.start.cmp(&b.e.function.start))
    });
}

fn line_count(tree: &Path, file: &str) -> u32 {
    std::fs::read(tree.join(file)).map_or(0, |s| {
        let n = s.iter().filter(|b| **b == b'\n').count();
        u32::try_from(n + usize::from(!s.ends_with(b"\n"))).unwrap_or(u32::MAX)
    })
}

fn function_lines(lines: &Lines, start: u32, end: u32) -> Coverage {
    let within = |set: &std::collections::BTreeSet<u32>| {
        u32::try_from(set.range(start..=end).count()).unwrap_or(u32::MAX)
    };
    Coverage::Lines {
        executable: within(&lines.executable),
        executed: within(&lines.executed),
    }
}

/// Join a resolved report onto the records.
pub fn attach_coverage(records: &mut [Record], report: &Resolved, tree: &Path) {
    let mut stale: BTreeMap<String, bool> = BTreeMap::new();
    for r in records {
        let Some(lines) = report.files.get(&r.file) else {
            r.coverage = Coverage::NotInReport;
            continue;
        };
        let is_stale = *stale.entry(r.file.clone()).or_insert_with(|| {
            let last = lines.executable.last().copied().unwrap_or(0);
            last > line_count(tree, &r.file)
        });
        r.coverage = if is_stale {
            Coverage::Stale
        } else {
            function_lines(lines, r.e.function.start, r.e.function.end)
        };
    }
}

/// Up to `k` of `all`, spread evenly over them.
fn spread(all: Vec<Mutant>, k: usize) -> Vec<Mutant> {
    if all.len() <= k {
        return all;
    }
    let n = all.len();
    (0..k).filter_map(|i| all.get(i * n / k).cloned()).collect()
}

/// The mutants to run for one record: inside the function, and on lines
/// the tests execute when a report says which (a mutant on a line no test
/// runs survives by construction; coverage already says so).
pub fn choose(
    r: &Record,
    file_mutants: &[Mutant],
    report: Option<&Resolved>,
    k: usize,
) -> Vec<Mutant> {
    let f = &r.e.function;
    let executed = report
        .and_then(|c| c.files.get(&r.file))
        .map(|l| &l.executed);
    let inside: Vec<Mutant> = file_mutants
        .iter()
        .filter(|m| m.line >= f.start && m.line <= f.end)
        .filter(|m| executed.is_none_or(|e| e.contains(&m.line)))
        .cloned()
        .collect();
    spread(inside, k)
}

/// Mutants of every file in `files`, read from `tree`.
pub fn file_mutants(tree: &Path, files: &[&str]) -> BTreeMap<String, Vec<Mutant>> {
    files
        .iter()
        .filter_map(|f| {
            let lang = Lang::of_path(f)?;
            let src = std::fs::read(tree.join(f)).ok()?;
            Some(((*f).to_owned(), mutants(lang, &src)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(line: u32) -> Mutant {
        Mutant {
            line,
            start: 0,
            end: 0,
            original: "<".into(),
            replacement: ">=".into(),
        }
    }

    #[test]
    fn spread_keeps_order_and_reaches_the_end_region() {
        let picked: Vec<u32> = spread((1..=10).map(m).collect(), 3)
            .iter()
            .map(|x| x.line)
            .collect();
        assert_eq!(picked, vec![1, 4, 7]);
        assert_eq!(spread(vec![m(1)], 3).len(), 1);
    }

    #[test]
    fn function_lines_count_only_the_span() {
        let lines = Lines {
            executable: [1, 2, 3, 10].into(),
            executed: [2, 10].into(),
        };
        assert_eq!(
            function_lines(&lines, 2, 5),
            Coverage::Lines {
                executable: 2,
                executed: 1
            }
        );
    }
}
