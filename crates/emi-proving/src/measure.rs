//! The measurement: list the tests at both revisions, classify the added
//! ones against the baseline's product code, then see which changed
//! functions the proving tests execute.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use emi_diff::functions::{ChangeKind, ChangedFn, ChangedFunctions, changed_functions};
use emi_diff::{hunks, snapshot};
use emi_profile::sandbox::{Copy, Runner};

use crate::classify::{Verdict, classify, failed_at_baseline};
use crate::evidence::{ChangeEvidence, FunctionEvidence, ProvingTest, UnknownTest, status_of};
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
    let view = emi_git::CleanView::of(&repo)?;
    let scratch = snapshot::TempDir::new("emi-proving-")?;
    let base = snapshot::resolve_ref(&repo, &req.baseline)?;
    let cand = match &req.candidate {
        Candidate::Ref(reference) => snapshot::resolve_ref(&repo, reference)?,
        Candidate::Worktree => snapshot::commit_worktree(&repo, view.path(), scratch.path())?,
    };
    let ctx = Context {
        req,
        repo: &repo,
        objects: view.path(),
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
    redundant: Vec<String>,
    unknown: Vec<UnknownTest>,
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
            Ok(classified) => self.judge(&session, &changed, classified),
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
        for f in changed
            .functions
            .iter()
            .filter(|f| !self.tests.matches(&f.file))
        {
            if f.kind == ChangeKind::Removed {
                scope.notes.push(format!(
                    "{}: {} was removed; removed functions are not measured",
                    f.file, f.name
                ));
            } else {
                scope.functions.push(f);
            }
        }
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

    /// What each added test is: the ids the candidate lists and the
    /// baseline does not.
    fn found_tests(&self, s: &Session) -> Result<Classified, String> {
        let tree = s.tree("candidate", &self.cand)?;
        let at_candidate = s.list(&tree, "candidate")?;
        let at_baseline = {
            let pristine = s.tree("baseline-list", &self.base)?;
            s.list(&pristine, "baseline")?
        };
        let added = pytest::added_ids(&at_candidate, &at_baseline, &self.renames()?);
        if added.is_empty() {
            return Ok(Classified::default());
        }
        self.classify_added(s, &tree, &added)
    }

    /// Files git sees as moved by the change: new path to old path.
    fn renames(&self) -> Result<BTreeMap<String, String>, String> {
        let files = hunks::changes(self.objects, &self.base, Some(&self.cand))
            .map_err(|e| format!("the files the change moved could not be read: {e}"))?;
        Ok(files
            .into_iter()
            .filter(|f| f.status == hunks::Status::Renamed)
            .filter_map(|f| f.old_path.map(|old| (f.path, old)))
            .collect())
    }

    /// Run the added tests against the baseline's product code, with the
    /// candidate's test files laid over it, then on the candidate those
    /// that failed there.
    fn classify_added(
        &self,
        s: &Session,
        candidate: &Copy,
        added: &[String],
    ) -> Result<Classified, String> {
        let workspace = s.tree("baseline", &self.base)?;
        let links = overlay_tests(&Tree::of(candidate), &workspace.dir, &self.tests)?;
        let (blocked, runnable): (Vec<&String>, Vec<&String>) = added
            .iter()
            .partition(|id| links.contains(pytest::file_of(id)));
        let runnable: Vec<String> = runnable.into_iter().cloned().collect();
        let at_baseline = s.observe(&workspace, &runnable, "baseline");
        let missing = Observed::Unknown("no result".into());
        let failing: Vec<String> = runnable
            .iter()
            .filter(|id| failed_at_baseline(at_baseline.get(*id).unwrap_or(&missing)))
            .cloned()
            .collect();
        let on_candidate = s.observe(candidate, &failing, "candidate");
        let mut out = Classified::default();
        for id in blocked {
            out.unknown.push(UnknownTest {
                id: id.clone(),
                reason: "its file is a symbolic link, which is not followed, so the test was not run at the baseline".into(),
            });
        }
        for id in &runnable {
            let verdict = classify(
                at_baseline.get(id).unwrap_or(&missing),
                on_candidate.get(id),
            );
            match verdict {
                Verdict::Proving(baseline_result) => out.proving.push(ProvingTest {
                    id: id.clone(),
                    baseline_result,
                }),
                Verdict::Redundant => out.redundant.push(id.clone()),
                Verdict::Unknown(reason) => out.unknown.push(UnknownTest {
                    id: id.clone(),
                    reason,
                }),
            }
        }
        Ok(out)
    }

    /// The evidence once the added tests are classified.
    fn judge(
        &self,
        s: &Session,
        changed: &Result<ChangedFunctions, String>,
        classified: Classified,
    ) -> ChangeEvidence {
        let mut evidence = ChangeEvidence::unknown(Vec::new(), Vec::new());
        let ids: Vec<String> = classified.proving.iter().map(|t| t.id.clone()).collect();
        let unclassified = classified.unknown.len();
        evidence.proving_tests = classified.proving;
        evidence.redundant_tests = classified.redundant;
        evidence.unknown_tests = classified.unknown;
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
        let verdicts = self.verdicts(s, &scope, &ids, unclassified);
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
    /// One verdict per measured function, in order.
    fn verdicts(
        &self,
        s: &Session,
        scope: &Scope,
        proving: &[String],
        unclassified: usize,
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
            self.covered(s, scope, proving)
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
    fn covered(&self, s: &Session, scope: &Scope, proving: &[String]) -> Verdicts {
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
        let tree = match s.tree("coverage", &self.cand) {
            Ok(tree) => tree,
            Err(why) => return all_unknown(why),
        };
        let sources = self.coverage_sources(scope);
        let report = match s.coverage(&tree, proving, &sources) {
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
        // validated by `measure`
        if let Ok(source) = snapshot::normalize_source(&self.req.source)
            && !source.is_empty()
        {
            dirs.insert(source);
        }
        dirs.into_iter().collect()
    }
}
