//! Laws: tests a person owns, in their own directory (`.emi/laws`), that
//! say what the code must do. The candidate is held to the laws as the
//! baseline has them: every law file is laid over the candidate in its
//! baseline form, except the changes a person approved. A law the agent
//! wrote or edited without an approval is a proposal; it counts for
//! nothing until approved, and what it would be worth is shown: whether it
//! still holds when a function it exercises is replaced by a trivial
//! stand-in (returns its argument, returns None, raises).
//!
//! What a law shows, from its outcome at the baseline and on the
//! candidate under the same pins:
//!
//! | baseline | candidate | status |
//! |---|---|---|
//! | passed | passed | holds |
//! | passed | failed, once more on a second run | broken |
//! | passed | failed, then passed on a second run | nondeterministic |
//! | failed or absent | passed | now_holds |
//! | failed or absent | failed | unmet |
//! | any | skipped or xfail | pending |
//!
//! Anything else is `unknown`, with the reason.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde_json::{Value, json};

use crate::functions::{ChangeKind, ChangedFn, changed_functions};
use crate::proving::exec::{Coverage, Session};
use crate::proving::junit::Observed;
use crate::proving::overlay::place;
use crate::proving::proof::{FnStatus, judge};
use crate::proving::pytest::Command;
use crate::proving::{Request, measure_resolved};
use crate::sandbox::{Copy, Runner};
use crate::standins::stand_ins;
use crate::syntax;

/// What one law showed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Law {
    pub id: String,
    pub baseline: String,
    pub candidate: String,
    pub status: &'static str,
    /// What the failing law said: the falsifying example, or the failure.
    pub counterexample: Option<String>,
}

/// What a law the agent proposed would be worth.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proposal {
    pub file: String,
    /// Its laws' outcomes on the candidate.
    pub laws: Vec<Law>,
    /// "`f` returns None": stand-ins under which every one of its passing
    /// laws still held. A law that holds for a trivial body does not pin
    /// that function down.
    pub vacuous_for: Vec<String>,
    pub notes: Vec<String>,
}

/// The receipt's `laws` object.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LawEvidence {
    /// The laws the candidate was held to.
    pub laws: Vec<Law>,
    /// Changed functions and the passing laws that execute what the
    /// change wrote in them: those functions are law-checked.
    pub checked: BTreeMap<String, Vec<String>>,
    pub proposals: Vec<Proposal>,
    /// Digest of the law files as they were applied.
    pub digest: Option<String>,
    pub not_measured: Vec<String>,
    /// Why the laws could not be measured at all.
    pub fault: Option<String>,
    pub isolation: Option<String>,
}

impl LawEvidence {
    pub fn count(&self, status: &str) -> usize {
        self.laws.iter().filter(|l| l.status == status).count()
    }

    pub fn to_json(&self) -> Value {
        let law = |l: &Law| {
            json!({"id": l.id, "baseline": l.baseline, "candidate": l.candidate,
                   "status": l.status, "counterexample": l.counterexample})
        };
        json!({
            "laws": self.laws.iter().map(law).collect::<Vec<_>>(),
            "checked_functions": self.checked,
            "proposals": self.proposals.iter().map(|p| json!({
                "file": p.file, "laws": p.laws.iter().map(law).collect::<Vec<_>>(),
                "vacuous_for": p.vacuous_for, "notes": p.notes})).collect::<Vec<_>>(),
            "digest": self.digest,
            "not_measured": self.not_measured,
            "fault": self.fault,
        })
    }
}

/// Which law files count, and how.
pub struct LawFiles<'a> {
    /// The laws directory, repository-relative.
    pub dir: &'a str,
    /// Changed law files a person approved: their candidate version (or
    /// their deletion) counts.
    pub approved: &'a BTreeSet<String>,
    /// Changed law files nobody approved: proposals.
    pub proposed: &'a BTreeSet<String>,
    /// Most stand-in runs for all proposals together.
    pub max_stand_in_runs: usize,
}

impl LawFiles<'_> {
    fn holds(&self, path: &str) -> bool {
        path.strip_prefix(self.dir)
            .is_some_and(|rest| rest.starts_with('/'))
    }
}

fn shown(o: Option<&Observed>) -> &'static str {
    match o {
        None => "absent",
        Some(Observed::Passed) => "passed",
        Some(Observed::Failed) => "failed",
        Some(Observed::Errored) => "errored",
        Some(Observed::Skipped) => "skipped",
        Some(Observed::Unknown(_)) => "unknown",
    }
}

/// The status a law's two outcomes give (the table above); `again` is the
/// candidate's second run, asked for only when a held law failed.
pub fn status(base: Option<&Observed>, cand: &Observed, again: Option<&Observed>) -> &'static str {
    use Observed::{Errored, Failed, Passed, Skipped};
    let failing = |o: &Observed| matches!(o, Failed | Errored);
    match (base, cand) {
        (_, Skipped) => "pending",
        (_, Observed::Unknown(_)) | (Some(Observed::Unknown(_)), _) => "unknown",
        (Some(Passed), Passed) => "holds",
        (Some(Passed), c) if failing(c) => match again {
            Some(a) if failing(a) => "broken",
            Some(Passed) => "nondeterministic",
            _ => "unknown",
        },
        (None | Some(Failed | Errored | Skipped), Passed) => "now_holds",
        (None | Some(Failed | Errored | Skipped), c) if failing(c) => "unmet",
        _ => "unknown",
    }
}

/// Everything a laws run shares.
struct Run<'a> {
    session: Session<'a>,
    files: &'a LawFiles<'a>,
    base: String,
    cand: String,
}

impl Run<'_> {
    /// The baseline's law files.
    fn baseline_laws(&self, base: &Copy) -> Vec<String> {
        base.files
            .iter()
            .filter(|p| self.files.holds(p))
            .cloned()
            .collect()
    }

    /// A copy of the candidate holding the laws as they apply: the
    /// baseline's files, then the approved changes, then (for a proposal
    /// check) the proposed files listed in `with`.
    fn laid(&self, tag: &str, base: &Copy, with: &BTreeSet<String>) -> Result<Copy, String> {
        let cand = self.session.tree(tag, &self.cand)?;
        let keep: BTreeMap<String, Vec<u8>> = cand
            .files
            .iter()
            .filter(|p| self.files.holds(p))
            .filter(|p| self.files.approved.contains(*p) || with.contains(*p))
            .map(|p| {
                Ok((
                    p.clone(),
                    std::fs::read(cand.dir.join(p)).map_err(|e| e.to_string())?,
                ))
            })
            .collect::<Result<_, String>>()?;
        let dir = cand.dir.join(self.files.dir);
        if dir.symlink_metadata().is_ok() {
            std::fs::remove_dir_all(&dir).map_err(|e| format!("cannot clear the laws: {e}"))?;
        }
        for path in self.baseline_laws(base) {
            // an approved deletion stays deleted
            if self.files.approved.contains(&path) && !keep.contains_key(&path) {
                continue;
            }
            place(&base.dir, &cand.dir, &path)?;
        }
        for (path, bytes) in keep {
            let to = cand.dir.join(&path);
            emi_tree::snapshot::assert_contained(&cand.dir, to.parent().unwrap_or(&cand.dir))?;
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            std::fs::write(&to, bytes).map_err(|e| format!("cannot place {path}: {e}"))?;
        }
        Ok(cand)
    }

    /// The laws in `copy`: their ids, and the law files that could not be
    /// collected (a law about a function the tree does not have yet does
    /// not import), with what the listing said. None when there is no law
    /// file.
    fn listed(
        &self,
        copy: &Copy,
        what: &str,
    ) -> Result<(Vec<String>, BTreeMap<String, String>), String> {
        if !copy.dir.join(self.files.dir).is_dir() {
            return Ok((Vec::new(), BTreeMap::new()));
        }
        let listing = self.session.list(copy, what)?;
        let said = listing.problem.unwrap_or_default();
        let broken = listing
            .broken
            .into_iter()
            .map(|file| (file, said.clone()))
            .collect();
        Ok((listing.ids.into_iter().collect(), broken))
    }
}

/// Digest of the law files `copy` holds, by path and content.
fn digest(copy: &Copy, dir: &str) -> Result<Option<String>, String> {
    let root = copy.dir.join(dir);
    if !root.is_dir() {
        return Ok(None);
    }
    let mut files = BTreeMap::new();
    let mut stack = vec![root.clone()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).map_err(|e| e.to_string())?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n != "__pycache__") {
                    stack.push(path);
                }
            } else {
                let rel = path.strip_prefix(&copy.dir).map_err(|e| e.to_string())?;
                let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
                files.insert(
                    rel.to_string_lossy().into_owned(),
                    emi_kernel::hash::sha256_hex(&bytes),
                );
            }
        }
    }
    Ok(Some(emi_kernel::json::digest("emi-laws", &json!(files))))
}

/// Measure the laws of `req` (baseline and candidate already resolved to
/// commits in `objects`).
pub fn measure_laws(req: &Request, objects: &Path, files: &LawFiles) -> LawEvidence {
    match measure_resolved(req, objects, |resolved, repo, base, cand| {
        let command =
            Command::parse(&resolved.test_command)?.with_selectors(vec![files.dir.to_owned()]);
        let runner = Runner {
            repo,
            objects: None,
            rev: cand,
            link: &resolved.link,
            env: &resolved.env,
            timeout: resolved.timeout,
            network: resolved.network,
        };
        let run = Run {
            session: Session::new(runner, objects, &command),
            files,
            base: base.to_owned(),
            cand: cand.to_owned(),
        };
        let mut evidence = held_to(&run)?;
        evidence.proposals = proposals(&run);
        evidence.isolation = run.session.isolation();
        Ok(evidence)
    }) {
        Ok(evidence) => evidence,
        Err(why) => LawEvidence {
            fault: Some(format!("the laws could not be measured: {why}")),
            ..LawEvidence::default()
        },
    }
}

/// The laws the candidate is held to, and the functions they check.
fn held_to(run: &Run) -> Result<LawEvidence, String> {
    let base = run.session.tree("laws-baseline", &run.base)?;
    let none = BTreeSet::new();
    let cand = run.laid("laws-candidate", &base, &none)?;
    let mut evidence = LawEvidence {
        digest: digest(&cand, run.files.dir)?,
        ..LawEvidence::default()
    };
    let (base_ids, base_broken) = run.listed(&base, "baseline's laws")?;
    let (cand_ids, cand_broken) = run.listed(&cand, "laws on the candidate")?;
    // a law file the candidate cannot collect is one unmet law, named
    // after the file, whatever its laws are
    for (file, said) in &cand_broken {
        evidence.laws.push(Law {
            id: file.clone(),
            baseline: if base_broken.contains_key(file) {
                "errored"
            } else {
                "unknown"
            }
            .to_owned(),
            candidate: "not collected".to_owned(),
            status: "unmet",
            counterexample: Some(said.clone()),
        });
    }
    if cand_ids.is_empty() {
        return Ok(evidence);
    }
    let (mut at_base, _) = run.session.observe_explained(&base, &base_ids, "laws-base");
    // laws in a file the baseline cannot collect errored there
    for id in &cand_ids {
        if base_broken.contains_key(crate::proving::pytest::file_of(id)) {
            at_base.insert(id.clone(), Observed::Errored);
        }
    }
    let (at_cand, said) = run.session.observe_explained(&cand, &cand_ids, "laws-cand");
    // a held law that fails is run once more, under the same pins
    let suspects: Vec<String> = cand_ids
        .iter()
        .filter(|id| {
            at_base.get(*id) == Some(&Observed::Passed)
                && matches!(at_cand.get(*id), Some(Observed::Failed | Observed::Errored))
        })
        .cloned()
        .collect();
    let again = if suspects.is_empty() {
        BTreeMap::new()
    } else {
        let copy = run.laid("laws-again", &base, &none)?;
        run.session
            .observe_confirming(&copy, &suspects, "laws-again")
    };
    for id in &cand_ids {
        let unknown = Observed::Unknown("the run did not report the law".into());
        let c = at_cand.get(id).unwrap_or(&unknown);
        let s = status(at_base.get(id), c, again.get(id));
        evidence.laws.push(Law {
            id: id.clone(),
            baseline: shown(at_base.get(id)).to_owned(),
            candidate: shown(Some(c)).to_owned(),
            status: s,
            counterexample: matches!(s, "broken" | "unmet" | "nondeterministic")
                .then(|| said.get(id).cloned())
                .flatten(),
        });
        if let (Observed::Unknown(why), "unknown") = (c, s) {
            evidence.not_measured.push(format!("{id}: {why}"));
        }
    }
    // which functions the laws execute is measured apart from the laws'
    // verdicts, which stand when it cannot be
    match checked(run, &base, &evidence.laws) {
        Ok(c) => evidence.checked = c,
        Err(why) => evidence
            .not_measured
            .push(format!("which changed functions the laws execute: {why}")),
    }
    Ok(evidence)
}

/// The directories coverage reports on: the repository and where the
/// changed files are.
fn sources(changed: &[ChangedFn]) -> Vec<String> {
    let mut dirs: BTreeSet<String> = changed
        .iter()
        .map(|f| f.file.rsplit_once('/').map_or(".", |(d, _)| d).to_owned())
        .collect();
    dirs.insert(".".to_owned());
    dirs.into_iter().collect()
}

/// Which passing laws execute what the change wrote in each changed
/// function.
fn checked(run: &Run, base: &Copy, laws: &[Law]) -> Result<BTreeMap<String, Vec<String>>, String> {
    let passing: Vec<String> = laws
        .iter()
        .filter(|l| matches!(l.status, "holds" | "now_holds"))
        .map(|l| l.id.clone())
        .collect();
    let changed = changed_functions(run.session.objects, &run.base, &run.cand)?;
    let functions: Vec<&ChangedFn> = changed
        .functions
        .iter()
        .filter(|f| f.kind != ChangeKind::Removed && !run.files.holds(&f.file))
        .collect();
    let mut out = BTreeMap::new();
    if passing.is_empty() || functions.is_empty() {
        return Ok(out);
    }
    let copy = run.laid("laws-coverage", base, &BTreeSet::new())?;
    let owned: Vec<ChangedFn> = functions.iter().map(|f| (*f).clone()).collect();
    let coverage = run.session.coverage(&copy, &passing, &sources(&owned))?;
    for f in functions {
        let text = std::fs::read_to_string(copy.dir.join(&f.file)).map_err(|e| e.to_string())?;
        let mut by: Vec<String> = Vec::new();
        for id in &passing {
            let one = coverage.for_tests(std::slice::from_ref(id))?;
            if judge(f, &text, one.files.get(&f.file)).0 == FnStatus::Exercised {
                by.push(id.clone());
            }
        }
        if !by.is_empty() {
            let entry = out.entry(format!("{}::{}", f.file, f.name)).or_default();
            entry.extend(by);
            entry.sort();
            entry.dedup();
        }
    }
    Ok(out)
}

/// Product functions whose bodies `law` executes, in `copy`.
fn executed_functions(
    coverage: &Coverage,
    copy: &Copy,
    law: &str,
    dir: &str,
) -> Vec<(String, syntax::Function)> {
    let Ok(one) = coverage.for_tests(&[law.to_owned()]) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (file, lines) in &one.files {
        if file.starts_with(dir) || emi_kernel::paths::is_test(file) || lines.executed.is_empty() {
            continue;
        }
        let Ok(src) = std::fs::read(copy.dir.join(file)) else {
            continue;
        };
        for f in syntax::functions(&src).unwrap_or_default() {
            // the header runs on import; a one-line definition is all body
            let body = if f.body_start > f.start {
                f.body_start..=f.end
            } else {
                f.start..=f.end
            };
            if lines.executed.iter().any(|l| body.contains(l)) {
                out.push((file.clone(), f));
            }
        }
    }
    out
}

/// What each proposed law file would be worth.
fn proposals(run: &Run) -> Vec<Proposal> {
    let mut budget = run.files.max_stand_in_runs;
    let mut out = Vec::new();
    for file in run.files.proposed {
        let mut p = Proposal {
            file: file.clone(),
            laws: Vec::new(),
            vacuous_for: Vec::new(),
            notes: Vec::new(),
        };
        if let Err(why) = propose(run, file, &mut p, &mut budget) {
            p.notes.push(why);
        }
        out.push(p);
    }
    out
}

fn propose(run: &Run, file: &str, p: &mut Proposal, budget: &mut usize) -> Result<(), String> {
    let base = run.session.tree("laws-proposal-base", &run.base)?;
    let only: BTreeSet<String> = [file.to_owned()].into();
    let copy = run.laid("laws-proposal", &base, &only)?;
    if !copy.dir.join(file).is_file() {
        p.notes
            .push("the proposal deletes this law file".to_owned());
        return Ok(());
    }
    let (listed, broken) = run.listed(&copy, "proposed laws")?;
    if let Some(said) = broken.get(file) {
        p.notes
            .push(format!("the proposed law file cannot be collected: {said}"));
        return Ok(());
    }
    let ids: Vec<String> = listed
        .into_iter()
        .filter(|id| crate::proving::pytest::file_of(id) == file)
        .collect();
    let (seen, said) = run.session.observe_explained(&copy, &ids, "proposal");
    let mut passing = Vec::new();
    for id in &ids {
        let o = seen
            .get(id)
            .cloned()
            .unwrap_or(Observed::Unknown("not reported".into()));
        if o == Observed::Passed {
            passing.push(id.clone());
        }
        p.laws.push(Law {
            id: id.clone(),
            baseline: "absent".to_owned(),
            candidate: shown(Some(&o)).to_owned(),
            status: if o == Observed::Passed {
                "holds"
            } else {
                "unmet"
            },
            counterexample: said.get(id).cloned(),
        });
    }
    if passing.is_empty() {
        return Ok(());
    }
    let cov_copy = run.laid("laws-proposal-cov", &base, &only)?;
    let coverage = run
        .session
        .coverage(&cov_copy, &passing, &[".".to_owned()])?;
    let mut targets: BTreeMap<(String, u32), (String, syntax::Function)> = BTreeMap::new();
    for id in &passing {
        for (f_file, f) in executed_functions(&coverage, &cov_copy, id, run.files.dir) {
            targets.insert((f_file.clone(), f.start), (f_file, f));
        }
    }
    for (f_file, f) in targets.into_values() {
        let src = std::fs::read(copy.dir.join(&f_file)).map_err(|e| e.to_string())?;
        for stand_in in stand_ins(&src, &f) {
            if *budget == 0 {
                p.notes
                    .push("the stand-in budget ran out before every function was tried".into());
                return Ok(());
            }
            *budget -= 1;
            let trial = run.laid("laws-standin", &base, &only)?;
            std::fs::write(trial.dir.join(&f_file), &stand_in.source).map_err(|e| e.to_string())?;
            let seen = run.session.observe(&trial, &passing, "standin");
            if passing
                .iter()
                .all(|id| seen.get(id) == Some(&Observed::Passed))
            {
                p.vacuous_for
                    .push(format!("{f_file}::{} {}", f.name, stand_in.label));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_status_table() {
        use Observed::*;
        let u = Unknown("x".into());
        let rows: &[(Option<&Observed>, &Observed, Option<&Observed>, &str)] = &[
            (Some(&Passed), &Passed, None, "holds"),
            (Some(&Passed), &Failed, Some(&Failed), "broken"),
            (Some(&Passed), &Errored, Some(&Failed), "broken"),
            (Some(&Passed), &Failed, Some(&Passed), "nondeterministic"),
            (Some(&Passed), &Failed, None, "unknown"),
            (Some(&Failed), &Passed, None, "now_holds"),
            (None, &Passed, None, "now_holds"),
            (Some(&Failed), &Failed, None, "unmet"),
            (None, &Errored, None, "unmet"),
            (Some(&Passed), &Skipped, None, "pending"),
            (Some(&u), &Passed, None, "unknown"),
            (Some(&Passed), &u, None, "unknown"),
        ];
        for (base, cand, again, want) in rows {
            assert_eq!(
                status(*base, cand, *again),
                *want,
                "{base:?} {cand:?} {again:?}"
            );
        }
    }
}
