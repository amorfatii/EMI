//! The measurement: list the tests at both revisions, classify the added
//! ones against the baseline's product code, then see which changed
//! functions the proving tests execute.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use emi_diff::functions::{ChangeKind, ChangedFn, ChangedFunctions, changed_functions};
use emi_diff::{hunks, snapshot};
use emi_profile::sandbox::{Copy, Runner};

use crate::classify::{Verdict, classify, needs_candidate};
use crate::evidence::RedundantTest;
use crate::evidence::{ChangeEvidence, FunctionEvidence, ProvingTest, UnknownTest, status_of};
use crate::evidence::{FailingTest, NOT_COLLECTED, PASSES_AT_BASELINE, RUNS_NO_CHANGED_LINE};
use crate::exec::Session;
use crate::junit::Observed;
use crate::overlay::{TestFiles, Tree, overlay_tests};
use crate::proof::{FnStatus, judge};
use crate::pytest::{self, Command};
use crate::{Candidate, Language, Request};

/// What the opt-in rule would add, and why it is not in the result.
const STRICT_NOT_MEASURED: &str = "the stricter rule (a proving test must kill a mutant on the changed lines that no other test kills) was requested but is not measured; the evidence is the plain rule";

/// Measure the change from the request's baseline to its candidate and
/// return the receipt's `change` object.
///
/// `Err` is for a request that cannot be run at all (an empty command, a
/// ref that does not resolve). A run that cannot be measured is not an
/// error: it is an `unknown` result that says why.
pub fn measure(req: &Request) -> Result<ChangeEvidence, String> {
    // A clean view deliberately has no refs. Resolve names in the live
    // repository once; all later reads use the immutable object identities.
    let mut resolved = req.clone();
    resolved.baseline = snapshot::resolve_ref(&req.repo, &req.baseline)?;
    if let Candidate::Ref(reference) = &req.candidate {
        resolved.candidate = Candidate::Ref(snapshot::resolve_ref(&req.repo, reference)?);
    }
    let view = emi_git::CleanView::of(&req.repo)?;
    measure_in(&resolved, view.path())
}

/// Measure a candidate already frozen in the caller's clean object store.
/// Dependency links and hidden paths still belong to `req.repo`.
pub fn measure_in(req: &Request, objects: &Path) -> Result<ChangeEvidence, String> {
    if req.test_command.is_empty() {
        return Err("the test command is empty".into());
    }
    if req.timeout.is_zero() {
        return Err("the timeout must be longer than zero".into());
    }
    snapshot::normalize_source(&req.source)?;
    // an absolute path: dependency links and the hidden repository are
    // named by it inside the sandbox
    let repo = std::fs::canonicalize(&req.repo)
        .map_err(|e| format!("repository {}: {e}", req.repo.display()))?;
    let scratch = snapshot::TempDir::new("emi-proving-")?;
    let base = snapshot::resolve_ref(objects, &req.baseline)?;
    let cand = match &req.candidate {
        Candidate::Ref(reference) => snapshot::resolve_ref(objects, reference)?,
        Candidate::Worktree => snapshot::commit_worktree(&repo, objects, scratch.path())?,
    };
    let ctx = Context {
        req,
        repo: &repo,
        objects,
        base,
        cand,
        tests: TestFiles {
            globs: &req.test_globs,
        },
    };
    let mut evidence = ctx.run();
    if req.strict {
        evidence.not_measured.push(STRICT_NOT_MEASURED.to_owned());
    }
    Ok(evidence.sorted())
}

struct Context<'a> {
    req: &'a Request,
    /// The live repository, as an absolute path.
    repo: &'a Path,
    /// A clean view that holds both commits.
    objects: &'a Path,
    base: String,
    cand: String,
    tests: TestFiles<'a>,
}

/// What the added tests turned out to be.
#[derive(Default)]
struct Classified {
    proving: Vec<ProvingTest>,
    redundant: Vec<RedundantTest>,
    failing: Vec<FailingTest>,
    companion: Vec<String>,
    skipped: Vec<String>,
    unknown: Vec<UnknownTest>,
    /// What the listing said of files only the candidate cannot collect.
    notes: Vec<String>,
    coverage: Option<(Copy, crate::exec::Coverage)>,
}

/// The tests whose behaviour at the baseline is to be seen.
struct Targets {
    /// Ids the candidate lists and the baseline does not.
    added: Vec<String>,
    /// Ids both list, in test files the candidate edited.
    edited: BTreeSet<String>,
}

impl Targets {
    fn all(&self) -> Vec<String> {
        let all: BTreeSet<&String> = self.added.iter().chain(&self.edited).collect();
        all.into_iter().cloned().collect()
    }
}

/// Added cases that pass at the baseline, or run no changed line, while
/// another case of the same test function is a proving test are that
/// test's companions: a parametrized test shows the change with some cases
/// and what must stay as it was with the others.
fn companions(mut c: Classified) -> Classified {
    let proving: BTreeSet<&str> = c
        .proving
        .iter()
        .map(|t| pytest::function_of(&t.id))
        .collect();
    let (companion, redundant) = std::mem::take(&mut c.redundant)
        .into_iter()
        .partition(|t| proving.contains(pytest::function_of(&t.id)));
    c.redundant = redundant;
    c.companion = companion.into_iter().map(|t| t.id).collect();
    c
}

/// The changed functions that are measured, and what is left out.
struct Scope<'a> {
    functions: Vec<&'a ChangedFn>,
    /// Changed files in a supported language that did not parse.
    unparsed: usize,
    notes: Vec<String>,
}

impl Context<'_> {
    fn run(&self) -> ChangeEvidence {
        let changed = changed_functions(self.objects, &self.base, &self.cand);
        let command = match &self.req.language {
            Language::Python => Command::parse(&self.req.test_command),
            Language::Other(name) => Err(format!(
                "language `{name}` is not supported: only Python with pytest is measured"
            )),
        };
        let command = match command {
            Ok(command) => command,
            Err(why) => return self.unmeasured(&changed, vec![why]),
        };
        let env = self.env();
        let runner = Runner {
            repo: self.repo,
            rev: &self.cand,
            link: &self.req.link,
            env: &env,
            timeout: self.req.timeout,
            network: self.req.network,
        };
        let session = Session::new(runner, self.objects, &command);
        let mut evidence = match self.found_tests(&session) {
            Ok(classified) => {
                let classified = companions(self.refine(&session, &changed, classified));
                self.judge(&changed, classified)
            }
            Err(why) => self.unmeasured(&changed, vec![why]),
        };
        evidence.isolation = session.isolation();
        evidence
    }

    /// The test environment: the request's, with bytecode caches off so a
    /// stale one cannot stand in for a test file laid over the baseline.
    fn env(&self) -> Vec<(String, String)> {
        let mut env = vec![("PYTHONDONTWRITEBYTECODE".to_owned(), "1".to_owned())];
        env.extend(self.req.env.iter().cloned());
        env
    }

    /// Nothing about the tests could be measured: `unknown`, with the
    /// functions the change touched listed as unknown too.
    fn unmeasured(
        &self,
        changed: &Result<ChangedFunctions, String>,
        mut reasons: Vec<String>,
    ) -> ChangeEvidence {
        let mut functions = Vec::new();
        match changed {
            Ok(changed) => {
                let scope = self.scope(changed);
                reasons.extend(scope.notes.clone());
                functions = scope
                    .functions
                    .iter()
                    .map(|f| function_evidence(f, FnStatus::Unknown))
                    .collect();
            }
            Err(why) => reasons.push(format!("the changed functions could not be read: {why}")),
        }
        ChangeEvidence::unknown(functions, reasons)
    }

    /// Split the changed functions into those measured and the notes on
    /// those that are not.
    fn scope<'c>(&self, changed: &'c ChangedFunctions) -> Scope<'c> {
        let mut scope = Scope {
            functions: Vec::new(),
            unparsed: 0,
            notes: Vec::new(),
        };
        let mut other_languages = BTreeSet::new();
        for f in changed
            .functions
            .iter()
            .filter(|f| !self.tests.matches(&f.file))
        {
            if self.req.language == Language::Python && !f.file.ends_with(".py") {
                other_languages.insert(f.file.clone());
                continue;
            }
            if f.kind == ChangeKind::Removed {
                scope.notes.push(format!(
                    "{}: {} was removed; removed functions are not measured",
                    f.file, f.name
                ));
            } else {
                scope.functions.push(f);
            }
        }
        scope.notes.extend(
            other_languages
                .into_iter()
                .map(|file| format!("{file}: not Python; its changed functions are not measured")),
        );
        for file in changed.unparsed.iter().filter(|f| !self.tests.matches(f)) {
            scope.unparsed += 1;
            scope.notes.push(format!(
                "{file}: could not be parsed at one revision; its changed functions are unknown"
            ));
        }
        for file in changed
            .unsupported
            .iter()
            .filter(|f| !self.tests.matches(f))
        {
            scope.notes.push(format!(
                "{file}: not in a supported language; its changed functions are not measured"
            ));
        }
        scope
    }

    /// What each added test is (the ids the candidate lists and the
    /// baseline does not), and which edited old tests show the change.
    fn found_tests(&self, s: &Session) -> Result<Classified, String> {
        let tree = s.tree("candidate", &self.cand)?;
        let at_candidate = s.list(&tree, "candidate")?;
        let at_baseline = {
            let pristine = s.tree("baseline-list", &self.base)?;
            s.list(&pristine, "baseline")?
        };
        let mut out = Classified::default();
        // a file the baseline already could not collect is no news; one
        // only the candidate cannot collect is the candidate's to repair:
        // its tests ran before and do not run now
        for file in at_candidate.broken.difference(&at_baseline.broken) {
            out.failing.push(FailingTest {
                id: file.clone(),
                candidate_result: NOT_COLLECTED,
            });
        }
        if !out.failing.is_empty() {
            out.notes.extend(at_candidate.problem.clone());
        }
        let files = hunks::changes(self.objects, &self.base, Some(&self.cand))
            .map_err(|e| format!("the files the change touched could not be read: {e}"))?;
        let added = pytest::added_ids(&at_candidate.ids, &at_baseline.ids, &renames(&files));
        let edited_files: BTreeSet<&str> = files
            .iter()
            .filter(|f| f.status == hunks::Status::Modified && self.tests.matches(&f.path))
            .map(|f| f.path.as_str())
            .collect();
        let edited = at_candidate
            .ids
            .iter()
            .filter(|id| at_baseline.ids.contains(*id))
            .filter(|id| edited_files.contains(pytest::file_of(id)))
            .cloned()
            .collect();
        let targets = Targets { added, edited };
        if targets.added.is_empty() && targets.edited.is_empty() {
            return Ok(out);
        }
        self.classify_tests(s, &tree, &targets, out)
    }

    /// Run the tests against the baseline's product code, with the
    /// candidate's test files laid over it, then on the candidate those
    /// that did not pass there.
    fn classify_tests(
        &self,
        s: &Session,
        candidate: &Copy,
        targets: &Targets,
        mut out: Classified,
    ) -> Result<Classified, String> {
        let workspace = s.tree("baseline", &self.base)?;
        let links = overlay_tests(&Tree::of(candidate), &workspace.dir, &self.tests)?;
        let (blocked, runnable): (Vec<String>, Vec<String>) = targets
            .all()
            .into_iter()
            .partition(|id| links.contains(pytest::file_of(id)));
        let at_baseline = s.observe(&workspace, &runnable, "baseline");
        let missing = Observed::Unknown("no result".into());
        let again: Vec<String> = runnable
            .iter()
            .filter(|id| needs_candidate(at_baseline.get(*id).unwrap_or(&missing)))
            .cloned()
            .collect();
        let on_candidate = s.observe(candidate, &again, "candidate");
        for id in blocked
            .into_iter()
            .filter(|id| !targets.edited.contains(id))
        {
            out.unknown.push(UnknownTest {
                id,
                reason: "its file is a symbolic link, which is not followed, so the test was not run at the baseline".into(),
            });
        }
        for id in runnable {
            let verdict = classify(
                at_baseline.get(&id).unwrap_or(&missing),
                on_candidate.get(&id),
            );
            if targets.edited.contains(&id) {
                out.edited(id, &verdict);
            } else {
                out.added(id, verdict);
            }
        }
        Ok(out)
    }

    /// The evidence once the added tests are classified.
    fn judge(
        &self,
        changed: &Result<ChangedFunctions, String>,
        classified: Classified,
    ) -> ChangeEvidence {
        let mut evidence = ChangeEvidence::unknown(Vec::new(), Vec::new());
        let ids: Vec<String> = classified.proving.iter().map(|t| t.id.clone()).collect();
        // a failing added test may yet prove the change once it passes,
        // so it keeps a function from being called unproven, like one
        // that could not be classified
        let unclassified = classified.unknown.len() + classified.failing.len();
        evidence.proving_tests = classified.proving;
        evidence.redundant_tests = classified.redundant;
        evidence.failing_tests = classified.failing;
        evidence.companion_tests = classified.companion;
        evidence.skipped_tests = classified.skipped;
        evidence.unknown_tests = classified.unknown;
        evidence.not_measured.extend(classified.notes);
        let changed = match changed {
            Ok(changed) => changed,
            Err(why) => {
                evidence
                    .not_measured
                    .push(format!("the changed functions could not be read: {why}"));
                return evidence;
            }
        };
        let scope = self.scope(changed);
        let verdicts = self.verdicts(&scope, &ids, unclassified, classified.coverage.as_ref());
        let mut statuses = Vec::new();
        for (f, (status, why)) in scope.functions.iter().zip(verdicts) {
            statuses.push(status);
            evidence.functions.push(function_evidence(f, status));
            evidence.not_measured.extend(why);
        }
        evidence.not_measured.extend(scope.notes);
        evidence.status = status_of(&statuses, scope.unparsed);
        evidence
    }
}

impl Classified {
    /// File a test the candidate added under what it turned out to be.
    fn added(&mut self, id: String, verdict: Verdict) {
        match verdict {
            Verdict::Proving(baseline_result) => self.proving.push(ProvingTest {
                id,
                baseline_result,
                edited: false,
            }),
            Verdict::Redundant => self.redundant.push(RedundantTest {
                id,
                reason: PASSES_AT_BASELINE,
            }),
            Verdict::Failing(candidate_result) => self.failing.push(FailingTest {
                id,
                candidate_result,
            }),
            Verdict::Skipped => self.skipped.push(id),
            Verdict::Unknown(reason) => self.unknown.push(UnknownTest { id, reason }),
        }
    }

    /// An old test in a file the candidate edited counts only when it
    /// shows the change. One that passes at the baseline is an old test
    /// doing its work, not a redundant one, and one that fails at both
    /// revisions is the regression evidence's to judge.
    fn edited(&mut self, id: String, verdict: &Verdict) {
        if let Verdict::Proving(baseline_result) = verdict {
            self.proving.push(ProvingTest {
                id,
                baseline_result: *baseline_result,
                edited: true,
            });
        }
    }
}

/// Files git sees as moved by the change: new path to old path.
fn renames(files: &[hunks::FileChange]) -> BTreeMap<String, String> {
    files
        .iter()
        .filter(|f| f.status == hunks::Status::Renamed)
        .filter_map(|f| f.old_path.clone().map(|old| (f.path.clone(), old)))
        .collect()
}

fn function_evidence(f: &ChangedFn, status: FnStatus) -> FunctionEvidence {
    FunctionEvidence {
        file: f.file.clone(),
        function: f.name.clone(),
        line: f.new.map_or(0, |(start, _)| start),
        status,
    }
}

type Verdicts = Vec<(FnStatus, Option<String>)>;

impl Context<'_> {
    /// A file-level import failure at the baseline does not prove every
    /// test in that file. Attribute executed bodies to each test separately.
    fn refine(
        &self,
        s: &Session,
        changed: &Result<ChangedFunctions, String>,
        mut c: Classified,
    ) -> Classified {
        if c.proving.is_empty() {
            return c;
        }
        let measured = changed.as_ref().map_err(Clone::clone).and_then(|changed| {
            let scope = self.scope(changed);
            let tree = s.tree("coverage", &self.cand)?;
            let ids = c.proving.iter().map(|t| t.id.clone()).collect::<Vec<_>>();
            let report = s.coverage(&tree, &ids, &self.coverage_sources(&scope))?;
            Ok((tree, report))
        });
        let tests = std::mem::take(&mut c.proving);
        match measured {
            Err(why) => c.unknown.extend(tests.into_iter().map(|t| UnknownTest {
                id: t.id,
                reason: format!("per-test execution could not be measured: {why}"),
            })),
            Ok((tree, report)) => {
                for test in tests {
                    match self.reaches(changed, &tree, &report, &test.id) {
                        Ok(true) => c.proving.push(test),
                        // an old test the candidate edited that does not
                        // reach the change is an old test, nothing more
                        Ok(false) if test.edited => {}
                        Ok(false) => c.redundant.push(RedundantTest {
                            id: test.id,
                            reason: RUNS_NO_CHANGED_LINE,
                        }),
                        Err(reason) => c.unknown.push(UnknownTest {
                            id: test.id,
                            reason,
                        }),
                    }
                }
                c.coverage = Some((tree, report));
            }
        }
        c
    }

    fn reaches(
        &self,
        changed: &Result<ChangedFunctions, String>,
        tree: &Copy,
        report: &crate::exec::Coverage,
        id: &str,
    ) -> Result<bool, String> {
        let changed = changed.as_ref().map_err(Clone::clone)?;
        let scope = self.scope(changed);
        let one = report.for_tests(&[id.to_owned()])?;
        let mut unknown = Vec::new();
        for f in scope.functions {
            let text =
                std::fs::read_to_string(tree.dir.join(&f.file)).map_err(|e| e.to_string())?;
            match judge(f, &text, one.files.get(&f.file)) {
                (FnStatus::Proven, _) => return Ok(true),
                (FnStatus::Unknown, why) => unknown.extend(why),
                _ => {}
            }
        }
        if scope.unparsed != 0 || !unknown.is_empty() {
            return Err(format!(
                "changed code could not be attributed to {id}: {}",
                unknown.join("; ")
            ));
        }
        Ok(false)
    }

    /// One verdict per measured function, in order.
    fn verdicts(
        &self,
        scope: &Scope,
        proving: &[String],
        unclassified: usize,
        coverage: Option<&(Copy, crate::exec::Coverage)>,
    ) -> Verdicts {
        let judged = if scope.functions.is_empty() {
            Vec::new()
        } else if proving.is_empty() {
            // no test executes anything: no run is needed to say so
            scope
                .functions
                .iter()
                .map(|_| (FnStatus::Unproven, None))
                .collect()
        } else {
            self.covered(scope, proving, coverage)
        };
        if unclassified == 0 {
            return judged;
        }
        // an added test that could not be classified may be a proving test
        scope
            .functions
            .iter()
            .zip(judged)
            .map(|(f, verdict)| match verdict {
                (FnStatus::Unproven, _) => (
                    FnStatus::Unknown,
                    Some(format!(
                        "{}: {}: no known proving test executes it, but {unclassified} added test(s) could not be classified",
                        f.file, f.name
                    )),
                ),
                other => other,
            })
            .collect()
    }

    /// Verdicts from a coverage run of the proving tests on the candidate.
    fn covered(
        &self,
        scope: &Scope,
        proving: &[String],
        cached: Option<&(Copy, crate::exec::Coverage)>,
    ) -> Verdicts {
        let all_unknown = |why: String| -> Verdicts {
            scope
                .functions
                .iter()
                .map(|_| {
                    (
                        FnStatus::Unknown,
                        Some(format!("coverage could not be measured: {why}")),
                    )
                })
                .collect()
        };
        let Some((tree, coverage)) = cached else {
            return all_unknown("no per-test execution was recorded".into());
        };
        let report = match coverage.for_tests(proving) {
            Ok(report) => report,
            Err(why) => return all_unknown(why),
        };
        scope
            .functions
            .iter()
            .map(|f| match std::fs::read_to_string(tree.dir.join(&f.file)) {
                Ok(text) => {
                    let (status, why) = judge(f, &text, report.files.get(&f.file));
                    (status, why.map(|w| format!("{}: {w}", f.file)))
                }
                Err(e) => (
                    FnStatus::Unknown,
                    Some(format!(
                        "{}: {}: the file cannot be read: {e}",
                        f.file, f.name
                    )),
                ),
            })
            .collect()
    }

    /// Directories coverage must report on even when no test imports
    /// them: the product directory and where the changed files are. A file
    /// no test loads is then unproven (listed, nothing executed), not
    /// missing from the report.
    fn coverage_sources(&self, scope: &Scope) -> Vec<String> {
        let mut dirs: BTreeSet<String> = scope
            .functions
            .iter()
            .map(|f| match f.file.rsplit_once('/') {
                Some((dir, _)) => dir.to_owned(),
                None => ".".to_owned(),
            })
            .collect();
        dirs.insert(".".to_owned());
        // validated by `measure`
        if let Ok(source) = snapshot::normalize_source(&self.req.source)
            && !source.is_empty()
        {
            dirs.insert(source);
        }
        dirs.into_iter().collect()
    }
}
