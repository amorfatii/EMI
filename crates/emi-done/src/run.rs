//! One configured check: approvals, the verification through the
//! controller, and the receipt.

use std::collections::BTreeSet;
use std::path::Path;

use emi_controller::Candidate as Subject;

use crate::baseline::Baseline;
use crate::candidate::{self, Candidate};
use crate::change::ChangeEvidence;
use crate::config::Config;
use crate::oldtests::{self, Changed, Entry};
use crate::outcomes::Outcomes;
use crate::regression::{self, Regression};
use crate::state::{self, Approval};
use crate::{ChangeContext, Error, Outcome, Request, Setting, derived, receipt};

/// The request the proving measurement and the second look share: the
/// frozen candidate, the configured command, and the same import path as
/// the regression run, so that all of them run the candidate's code.
pub fn proving_request(
    repo: &Path,
    config: &Config,
    baseline: &Baseline,
    cand: &Candidate,
) -> emi_proving::Request {
    let mut req = emi_proving::Request::new(repo, &baseline.sha, config.tests.command.clone());
    req.candidate = emi_proving::Candidate::Ref(cand.rev.clone());
    req.language = emi_proving::Language::from_name(&config.language);
    req.test_globs.clone_from(&config.tests.globs);
    req.source.clone_from(&config.source);
    req.link.clone_from(&config.link);
    if !config.python_path.is_empty() {
        req.env
            .push(("PYTHONPATH".to_owned(), config.python_path.join(":")));
    }
    req
}

/// What a second look needs beside the regression it looks at again.
struct Second<'a> {
    repo: &'a Path,
    view: &'a Path,
    config: &'a Config,
    baseline: &'a Baseline,
    cand: &'a Candidate,
    store: &'a emi_controller::Store,
    subject: &'a Subject,
    decision: &'a serde_json::Value,
    scope: &'a regression::Scope,
}

fn first_of(ids: &BTreeSet<String>) -> &str {
    ids.iter().next().map_or("?", String::as_str)
}

/// A failure is a break when it shows again. The failed tests are run by
/// themselves on the candidate as it is; most real breaks fail there too,
/// and that is all it costs. Those that pass were not broken by the
/// product code alone:
///
/// - with old test files restored, they fail only beside the baseline
///   versions of files the change edited (a fixture, a helper, expected
///   output): the person's question, like a restored test that fails;
/// - with nothing restored, the whole suite runs once more, and a test
///   that passes in it was flaky. One that fails again depends on what
///   ran before it, which the change altered: still a break.
///
/// Anything that cannot be run a second time leaves the first verdict.
fn confirm(r: Regression, s: &Second) -> Regression {
    if r.status != regression::Status::Broken || r.failed.is_empty() {
        return r;
    }
    let req = proving_request(s.repo, s.config, s.baseline, s.cand);
    let alone = match emi_proving::passing(&req, s.view, &r.failed) {
        Ok(alone) if !alone.is_empty() => alone,
        _ => return r,
    };
    if s.scope.any_restored() {
        let note = format!(
            "{} old test(s) fail only beside the baseline versions of the test files the change \
             edited (first: {}); whether those edits are acceptable is the person's question",
            alone.len(),
            first_of(&alone)
        );
        return regression::cleared(r, &alone, note, s.decision, s.scope);
    }
    let again = derived::run(s.store, s.subject, false)
        .ok()
        .and_then(|v| Outcomes::of(v.check.as_ref()));
    let Some(again) = again else { return r };
    let flaky: BTreeSet<String> = alone
        .into_iter()
        .filter(|id| !again.not_passing(id))
        .collect();
    if flaky.is_empty() {
        return r;
    }
    let note = format!(
        "{} test(s) failed once and passed when run again, alone and in the whole suite \
         (first: {}): flaky, not a break",
        flaky.len(),
        first_of(&flaky)
    );
    regression::cleared(r, &flaky, note, s.decision, s.scope)
}

/// What the verification found, or why it found nothing.
struct Measured {
    regression: Regression,
    /// `sandboxed`, what ran instead, or `unknown` when nothing ran.
    isolation: String,
}

fn measured_unknown(why: String) -> Measured {
    Measured {
        regression: Regression::unknown(vec![why]),
        isolation: "unknown".to_owned(),
    }
}

/// Run the test command on the baseline now, so that the first check of a
/// candidate finds the result waiting. The task is the one a check of an
/// unchanged tree derives (nothing approved, nothing restored); a check
/// that later derives another runs its own baseline. Nothing is returned:
/// the result is in the task's store, and a failure here is found again,
/// and reported, by the check itself.
pub fn warm(repo: &Path, view: &Path, req: &Request, config: &Config, baseline: &Baseline) {
    if !derived::is_pytest(&config.tests.command) {
        return;
    }
    let Ok(trusted) = derived::trusted(view, &baseline.sha, &config.tests.globs, &BTreeSet::new())
    else {
        return;
    };
    let Ok(own) = derived::own_names(view, &baseline.sha, config) else {
        return;
    };
    let spec = derived::spec(repo, config, &baseline.sha, &trusted, &own);
    let authority = state::authority(&state::repo_dir(req.state_root, repo));
    if let Ok(store) = derived::open(&authority, repo, &baseline.sha, &spec) {
        let _ = derived::baseline_check_once(&store, &baseline.sha, &authority);
    }
}

/// The regression evidence from the candidate's run. When that run named
/// its tests one by one, it is compared with the baseline's own run
/// instead of demanding a suite with no skips.
fn compared(
    (store, authority): (&emi_controller::Store, &Path),
    baseline: &Baseline,
    verified: &derived::Verified,
    pytest: bool,
    scope: &regression::Scope,
) -> Result<Regression, String> {
    let check = verified.check.as_ref();
    let base = if pytest {
        let base = derived::baseline_check_once(store, &baseline.sha, authority)?;
        Outcomes::of(base.as_ref())
    } else {
        None
    };
    Ok(match (&base, Outcomes::of(check)) {
        (Some(base), Some(cand)) => {
            regression::from_outcomes(&verified.decision, check, (base, &cand), scope)
        }
        (Some(base), None) => regression::not_started(check, base)
            .unwrap_or_else(|| regression::from_decision(&verified.decision, check, pytest, scope)),
        (None, _) => regression::from_decision(&verified.decision, check, pytest, scope),
    })
}

fn try_measure(
    repo: &Path,
    view: &Path,
    req: &Request,
    (config, baseline, cand): (&Config, &Baseline, &Candidate),
    entries: &[Entry],
) -> Result<Measured, String> {
    let trusted = derived::trusted(
        view,
        &baseline.sha,
        &config.tests.globs,
        &oldtests::approved_paths(entries),
    )
    .map_err(|e| e.to_string())?;
    let own = derived::own_names(view, &baseline.sha, config).map_err(|e| e.to_string())?;
    let spec = derived::spec(repo, config, &baseline.sha, &trusted, &own);
    let authority = state::authority(&state::repo_dir(req.state_root, repo));
    let store = derived::open(&authority, repo, &baseline.sha, &spec).map_err(|e| e.to_string())?;
    let subject = match req.candidate {
        candidate::Spec::Worktree => Subject::Worktree,
        candidate::Spec::Ref(_) => Subject::Ref(cand.id.clone()),
    };
    let verified = derived::run(&store, &subject, req.setting == Setting::Cooperative)?;
    let isolation = verified.decision["assurance"]["isolation"]
        .as_str()
        .unwrap_or("unknown")
        .to_owned();
    // a working tree edited while its tests ran is not what was measured
    let now = candidate::content_id_now(repo, view, &req.candidate).map_err(|e| e.to_string())?;
    if now != cand.content_id {
        return Ok(Measured {
            regression: Regression::unknown(vec![
                "the working tree changed while the tests ran".to_owned(),
            ]),
            isolation,
        });
    }
    // tests that import an installed copy say nothing about the candidate
    if let Some(why) = verified.foreign_imports {
        return Ok(Measured {
            regression: Regression::unknown(vec![why]),
            isolation,
        });
    }
    let pytest = derived::is_pytest(&config.tests.command);
    let scope = oldtests::scope(entries);
    let regression = compared((&store, &authority), baseline, &verified, pytest, &scope)?;
    let second = Second {
        repo,
        view,
        config,
        baseline,
        cand,
        store: &store,
        subject: &subject,
        decision: &verified.decision,
        scope: &scope,
    };
    Ok(Measured {
        regression: if pytest {
            confirm(regression, &second)
        } else {
            regression
        },
        isolation,
    })
}

/// Record the approvals the caller asked for. Each must name a changed old
/// test of this candidate, bound to the version it holds now.
fn approve(
    req: &Request,
    repo_dir: &Path,
    changed: &[Changed],
    baseline: &Baseline,
) -> Result<(), Error> {
    if req.approve.is_empty() {
        return Ok(());
    }
    if req.setting == Setting::Enforced {
        return Err(Error::Usage(
            "approvals are recorded on a person's machine; enforced verification does not read \
             them, so --approve is refused with --setting enforced"
                .to_owned(),
        ));
    }
    for item in req.approve {
        let Some(c) = changed.iter().find(|c| &c.item() == item) else {
            let known: Vec<String> = changed.iter().map(Changed::item).collect();
            return Err(Error::Usage(format!(
                "{item} is not a changed old test or test-runner configuration file of this candidate \
                 (changed: {})",
                if known.is_empty() {
                    "none".to_owned()
                } else {
                    known.join(", ")
                }
            )));
        };
        state::record(repo_dir, &c.approval(&baseline.sha))?;
    }
    Ok(())
}

/// The approvals that apply, and a fault when they cannot be read. They
/// matter only when an old test changed, and only in the cooperative
/// setting.
fn approvals(
    req: &Request,
    repo_dir: &Path,
    changed: &[Changed],
) -> (Vec<Approval>, Option<String>) {
    if changed.is_empty() || req.setting == Setting::Enforced {
        return (Vec::new(), None);
    }
    match state::load(repo_dir) {
        Ok(all) => (all, None),
        Err(e) => (
            Vec::new(),
            Some(format!("approvals could not be read: {e}")),
        ),
    }
}

/// How the configuration in use relates to the baseline's, when it is
/// the candidate's own copy (cooperative verification).
fn config_note(repo: &Path, req: &Request, config: &Config, baseline: &Baseline) -> Option<String> {
    if req.setting == Setting::Enforced {
        return None;
    }
    match crate::config::read_at(repo, &baseline.sha) {
        Ok(Some(base)) if &base == config => None,
        Ok(Some(_)) => Some(format!(
            "The {} in use is the candidate's and differs from the baseline's.",
            crate::config::PATH
        )),
        Ok(None) => Some(format!(
            "The baseline has no {}; the candidate's copy was used.",
            crate::config::PATH
        )),
        Err(e) => Some(format!(
            "The baseline's configuration could not be read ({e}); the candidate's was used."
        )),
    }
}

/// Check `cand` against `baseline` under `config`.
pub fn check(
    repo: &Path,
    view: &Path,
    req: &Request,
    (config, baseline, cand): (&Config, &Baseline, &Candidate),
    change: impl FnOnce(&ChangeContext) -> ChangeEvidence,
) -> Result<Outcome, Error> {
    let repo_dir = state::repo_dir(req.state_root, repo);
    let changed = oldtests::find(view, &baseline.sha, &cand.rev, &config.tests.globs)?;
    approve(req, &repo_dir, &changed, baseline)?;
    let (approvals, approval_fault) = approvals(req, &repo_dir, &changed);
    let mut old_tests = oldtests::mark(changed, &approvals, &baseline.sha);
    let measured = match try_measure(repo, view, req, (config, baseline, cand), &old_tests) {
        Ok(m) => m,
        Err(e) => measured_unknown(e),
    };
    oldtests::settle(&mut old_tests, &measured.regression);
    let change = change(&ChangeContext {
        repo,
        config,
        baseline,
        candidate: cand,
        spec: &req.candidate,
        view,
        regression: &measured.regression,
    });
    let limits = receipt::Limits {
        setting: req.setting,
        link: &config.link,
        pytest: derived::is_pytest(&config.tests.command),
        change: &change,
        config_note: config_note(repo, req, config, baseline),
    };
    let mut not_verified = receipt::not_verified(&limits);
    not_verified.extend(approval_fault.clone());
    let mut this_run = receipt::this_run(&limits);
    this_run.extend(approval_fault.clone());
    let parts = receipt::Parts {
        repo,
        baseline,
        candidate: cand,
        setting: req.setting,
        isolation: &measured.isolation,
        regression: &measured.regression,
        old_tests: &old_tests,
        change: &change,
        on_unproven: config.on_unproven,
        faults: approval_fault.into_iter().collect(),
        not_verified,
        this_run,
    };
    let (receipt, action) = receipt::assemble(&parts);
    Ok(Outcome { receipt, action })
}
