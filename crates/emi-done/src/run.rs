//! One configured check: approvals, the verification through the
//! controller, and the receipt.

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
    let spec = derived::spec(repo, config, &baseline.sha, &trusted);
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
    let pytest = derived::is_pytest(&config.tests.command);
    let scope = oldtests::scope(entries);
    let check = verified.check.as_ref();
    // when the candidate's run named its tests one by one, compare it with
    // the baseline's own run instead of demanding a suite with no skips
    let compared = match Outcomes::of(check) {
        Some(cand) => {
            let base = derived::baseline_check(&store, &baseline.sha)?;
            Outcomes::of(base.as_ref()).map(|base| (base, cand))
        }
        None => None,
    };
    let regression = match &compared {
        Some((base, cand)) => {
            regression::from_outcomes(&verified.decision, check, (base, cand), &scope)
        }
        None => regression::from_decision(&verified.decision, check, pytest, &scope),
    };
    Ok(Measured {
        regression,
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
