//! One configured check: approvals, the verification through the
//! controller, and the receipt.

use std::collections::BTreeSet;
use std::path::Path;

use crate::baseline::Baseline;
use crate::candidate::{self, Candidate};
use crate::change::ChangeEvidence;
use crate::config::Config;
use crate::oldtests::{self, Changed, Entry};
use crate::outcomes::Outcomes;
use crate::regression::{self, Regression};
use crate::state::{self, Approval};
use crate::{ChangeContext, Error, Outcome, Request, Setting, derived, receipt};

/// What every stage beyond the regression measures: the frozen candidate
/// against the baseline, with the configured command, the pins and the
/// project's roots, the same for all of them, so that each runs the
/// candidate's code.
pub fn run_of<'a>(
    (repo, view): (&'a Path, &'a Path),
    config: &'a Config,
    (baseline, candidate): (&'a str, &'a str),
    cache: Option<emi_driver::ListingCache>,
) -> emi_driver::Run<'a> {
    emi_driver::Run {
        repo,
        view,
        baseline,
        candidate,
        command: &config.tests.command,
        test_globs: &config.tests.globs,
        source: &config.source,
        link: &config.link,
        pins: config.pins.env(),
        roots: &config.python_path,
        cache,
    }
}

/// What is kept per repository from baseline runs (listings, laws,
/// recorded calls), keyed by the environment it was taken in and the
/// driver code that took it. An environment that could not be identified
/// keeps nothing: a result from another one could be stale.
pub fn listing_cache(
    repo_dir: &Path,
    environment: Option<&str>,
    driver: &dyn emi_driver::Driver,
) -> Option<emi_driver::ListingCache> {
    environment
        .map(|e| emi_driver::ListingCache::new(&repo_dir.join("listings"), e, &driver.producer()))
}

/// What a second look needs beside the regression it looks at again.
struct Second<'a> {
    driver: &'static dyn emi_driver::Driver,
    repo: &'a Path,
    view: &'a Path,
    config: &'a Config,
    baseline: &'a Baseline,
    cand: &'a Candidate,
    store: &'a crate::engine::Store,
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
/// - with nothing restored, the whole suite runs once more under the same
///   pins. A test that fails again depends on what ran before it, which the
///   change altered: still a break. One that passes did not reproduce: its
///   outcome is not decided by the code, so it is neither held nor broken
///   and the check is incomplete. A failure is never cleared by a lucky run.
///
/// Anything that cannot be run a second time leaves the first verdict.
fn confirm(r: Regression, s: &Second) -> Regression {
    if r.status != regression::Status::Broken || r.failed.is_empty() {
        return r;
    }
    let run = run_of(
        (s.repo, s.view),
        s.config,
        (&s.baseline.sha, &s.cand.rev),
        None,
    );
    let alone = match s.driver.passing(&run, &r.failed) {
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
    let again = derived::run(s.store, s.view, s.cand, false)
        .ok()
        .and_then(|v| Outcomes::of(v.check.as_ref()));
    let Some(again) = again else { return r };
    let unreproduced: BTreeSet<String> = alone
        .into_iter()
        .filter(|id| !again.not_passing(id))
        .collect();
    if unreproduced.is_empty() {
        return r;
    }
    regression::not_reproduced(r, &unreproduced, s.decision, s.scope)
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
pub fn warm(
    repo: &Path,
    view: &Path,
    (req, driver): (&Request, &'static dyn emi_driver::Driver),
    config: &Config,
    baseline: &Baseline,
) {
    let environment = environment_of(repo, config, driver).and_then(Result::ok);
    let approved = BTreeSet::new();
    let task = derived_task(
        (repo, view),
        (req, config, baseline),
        &approved,
        (environment.as_deref(), driver),
    );
    if let Ok((store, authority)) = task {
        let _ = derived::baseline_checks_once_in(&store, view, &baseline.sha, &authority);
    }
}

/// The installed environment's identity, when the driver gives one.
fn environment_of(
    repo: &Path,
    config: &Config,
    driver: &dyn emi_driver::Driver,
) -> Option<Result<String, String>> {
    driver.environment(repo, &config.tests.command)
}

/// The derived task for this configuration and baseline, with `approved`
/// files running as the candidate left them, and where its store lives.
fn derived_task(
    (repo, view): (&Path, &Path),
    (req, config, baseline): (&Request, &Config, &Baseline),
    approved: &BTreeSet<String>,
    (environment, driver): (Option<&str>, &'static dyn emi_driver::Driver),
) -> Result<(crate::engine::Store, std::path::PathBuf), String> {
    let trusted = derived::trusted(
        (view, &baseline.sha),
        &derived::trusted_globs(config),
        approved,
        driver,
    )
    .map_err(|e| e.to_string())?;
    // the baseline stands for the candidate: the import check names the
    // project's own code as the baseline has it
    let at_baseline = run_of((repo, view), config, (&baseline.sha, &baseline.sha), None);
    let imports = derived::import_check(&at_baseline, driver).map_err(|e| e.to_string())?;
    let spec = derived::spec(
        repo,
        config,
        &baseline.sha,
        (&trusted, imports.as_deref()),
        (environment, driver),
    );
    let authority = state::authority(&state::repo_dir(req.state_root, repo));
    let store = derived::open((&authority, repo), &baseline.sha, &spec, driver)
        .map_err(|e| e.to_string())?;
    Ok((store, authority))
}

/// The regression evidence from the candidate's run. When that run named
/// its tests one by one, it is compared with the baseline's own runs
/// instead of demanding a suite with no skips: a test the two baseline
/// runs disagree on is quarantined, and the rest are compared with what
/// either baseline run showed.
fn compared(
    (store, authority, view): (&crate::engine::Store, &Path, &Path),
    baseline: &Baseline,
    verified: &derived::Verified,
    (reader, driver): (Option<&dyn emi_driver::Driver>, &dyn emi_driver::Driver),
    scope: &regression::Scope,
) -> Result<Regression, String> {
    let check = verified.check.as_ref();
    let (base, unstable) = if reader.is_some() {
        let runs = derived::baseline_checks_once_in(store, view, &baseline.sha, authority)?;
        let outcomes: Option<Vec<Outcomes>> = runs.iter().map(|r| Outcomes::of(Some(r))).collect();
        match outcomes.as_deref() {
            Some([one, two]) => (Some(one.merged(two)), one.unstable(two)),
            Some([one]) => (Some(one.clone()), BTreeSet::new()),
            _ => (None, BTreeSet::new()),
        }
    } else {
        (None, BTreeSet::new())
    };
    let mut r = match (&base, Outcomes::of(check)) {
        (Some(base), Some(cand)) => {
            regression::from_outcomes(&verified.decision, check, (base, &cand), scope, driver)
        }
        (Some(base), None) => regression::not_started(check, base, driver)
            .unwrap_or_else(|| regression::from_decision(&verified.decision, check, reader, scope)),
        (None, _) => regression::from_decision(&verified.decision, check, reader, scope),
    };
    r.quarantined = unstable.into_iter().collect();
    Ok(r)
}

fn try_measure(
    (repo, view): (&Path, &Path),
    (req, driver): (&Request, &'static dyn emi_driver::Driver),
    (config, baseline, cand): (&Config, &Baseline, &Candidate),
    (entries, environment): (&[Entry], Option<&str>),
) -> Result<Measured, String> {
    let (store, authority) = derived_task(
        (repo, view),
        (req, config, baseline),
        &oldtests::approved_paths(entries),
        (environment, driver),
    )?;
    let verified = derived::run(&store, view, cand, req.setting == Setting::Cooperative)?;
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
    // a command the driver reads test by test; any other is judged by its
    // exit status alone, which the receipt states
    let reader = driver.regression(&config.tests.command).map(|_| driver);
    let scope = oldtests::scope(entries);
    let regression = compared(
        (&store, &authority, view),
        baseline,
        &verified,
        (reader, driver),
        &scope,
    )?;
    let second = Second {
        driver,
        repo,
        view,
        config,
        baseline,
        cand,
        store: &store,
        decision: &verified.decision,
        scope: &scope,
    };
    Ok(Measured {
        regression: if reader.is_some() {
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

/// The evidence beyond the regression and the proposer's tests: the laws,
/// the kept behaviour when asked for, the mutants when configured, and the
/// escape-hatch signals.
struct Beyond {
    laws: emi_driver::LawEvidence,
    preserved: Option<crate::preserve::Preserved>,
    adequacy: Option<crate::adequacy::Adequacy>,
    /// The signals, or why they could not be looked for.
    signals: Result<Vec<serde_json::Value>, String>,
}

fn beyond(
    ctx: &ChangeContext,
    old_tests: &[Entry],
    intent: &crate::preserve::Intent,
    measure_change: impl FnOnce(&ChangeContext) -> ChangeEvidence,
) -> (ChangeEvidence, Beyond) {
    let (view, config, driver) = (ctx.view, ctx.config, ctx.driver);
    let run = run_of(
        (ctx.repo, view),
        config,
        (&ctx.baseline.sha, &ctx.candidate.rev),
        ctx.listing_cache.clone(),
    );
    let not_product = |p: &str| {
        crate::oldtests::in_dir(p, &config.laws.dir)
            || config
                .tests
                .globs
                .iter()
                .any(|g| emi_kernel::glob::fnmatch(p, g))
            || emi_kernel::paths::is_test(p)
    };
    let (change, laws, mut preserved, signals) = std::thread::scope(|scope| {
        let laws_h = scope.spawn(|| crate::laws::measure(&run, config, old_tests, driver));
        let preserved_h = scope.spawn(|| {
            intent
                .preserve
                .then(|| crate::preserve::measure(&run, config, intent, driver))
        });
        let signals_h = scope.spawn(|| {
            crate::signals::find(view, (run.baseline, run.candidate), not_product, driver)
        });
        let change = measure_change(ctx);
        let laws = laws_h
            .join()
            .unwrap_or_else(|_| emi_driver::LawEvidence::fault("laws measurement panicked"));
        // a stage that panicked measured nothing: it is said, never read
        // as "not asked for" or "nothing found"
        let preserved = preserved_h.join().unwrap_or_else(|_| {
            intent
                .preserve
                .then(|| crate::preserve::not_compared("the comparison panicked"))
        });
        let signals = signals_h
            .join()
            .unwrap_or_else(|_| Err("the search panicked".to_owned()));
        (change, laws, preserved, signals)
    });
    if let Some(p) = &mut preserved {
        p.exclude_proven(&change, intent);
    }
    let adequacy = crate::adequacy::measure(&run, config, (&change.to_value(), &laws), driver);
    (
        change,
        Beyond {
            laws,
            preserved,
            adequacy,
            signals,
        },
    )
}

/// What the person declared about the change: the request's, or, in the
/// cooperative setting, the one recorded on this machine.
fn intent(req: &Request, repo_dir: &Path) -> Result<crate::preserve::Intent, Error> {
    match (&req.preserve, req.setting) {
        (Some(paths), _) => Ok(crate::preserve::Intent {
            preserve: true,
            paths: paths.clone(),
        }),
        (None, Setting::Cooperative) => crate::preserve::read(repo_dir),
        (None, Setting::Enforced) => Ok(crate::preserve::Intent::default()),
    }
}

/// What the receipt does not establish: in general, and in this run.
fn limits(
    (repo, req, config, baseline): (&Path, &Request, &Config, &Baseline),
    driver: &dyn emi_driver::Driver,
    change: &ChangeEvidence,
    more: &Beyond,
    (approval_fault, environment): (Option<&String>, Option<&Result<String, String>>),
) -> (Vec<String>, Vec<String>) {
    let limits = receipt::Limits {
        setting: req.setting,
        link: &config.link,
        runner: driver
            .regression(&config.tests.command)
            .is_none()
            .then(|| driver.runner()),
        change,
        config_note: config_note(repo, req, config, baseline),
    };
    let mut not_verified = receipt::not_verified(&limits);
    let mut this_run = receipt::this_run(&limits);
    let mut extra: Vec<String> = approval_fault.cloned().into_iter().collect();
    if let Some(Err(why)) = environment {
        extra.push(why.clone());
    }
    extra.extend(crate::laws::limits(&more.laws, &config.laws.dir));
    extra.extend(
        more.preserved
            .as_ref()
            .map(crate::preserve::limits)
            .unwrap_or_default(),
    );
    extra.extend(
        more.adequacy
            .as_ref()
            .map(crate::adequacy::limits)
            .unwrap_or_default(),
    );
    extra.extend(crate::signals::limits(&more.signals));
    not_verified.extend(extra.iter().cloned());
    this_run.extend(extra);
    (not_verified, this_run)
}

/// Check `cand` against `baseline` under `config`.
pub fn check(
    repo: &Path,
    view: &Path,
    (req, driver): (&Request, &'static dyn emi_driver::Driver),
    (config, baseline, cand): (&Config, &Baseline, &Candidate),
    change: impl FnOnce(&ChangeContext) -> ChangeEvidence,
) -> Result<Outcome, Error> {
    let repo_dir = state::repo_dir(req.state_root, repo);
    let changed = oldtests::find(
        (view, &baseline.sha, &cand.rev),
        (&config.tests.globs, &config.laws.dir),
        driver,
    )?;
    approve(req, &repo_dir, &changed, baseline)?;
    let (approvals, approval_fault) = approvals(req, &repo_dir, &changed);
    let mut old_tests = oldtests::mark(changed, &approvals, &baseline.sha);
    let environment = environment_of(repo, config, driver);
    let env_id = environment.as_ref().and_then(|e| e.as_deref().ok());
    let measured = match try_measure(
        (repo, view),
        (req, driver),
        (config, baseline, cand),
        (&old_tests, env_id),
    ) {
        Ok(m) => m,
        Err(e) => measured_unknown(e),
    };
    oldtests::settle(&mut old_tests, &measured.regression);
    let intent = intent(req, &repo_dir)?;
    let ctx = ChangeContext {
        driver,
        repo,
        config,
        baseline,
        candidate: cand,
        spec: &req.candidate,
        view,
        regression: &measured.regression,
        listing_cache: listing_cache(&repo_dir, env_id, driver),
    };
    let (change, more) = beyond(&ctx, &old_tests, &intent, change);
    let (not_verified, this_run) = limits(
        (repo, req, config, baseline),
        driver,
        &change,
        &more,
        (approval_fault.as_ref(), environment.as_ref()),
    );
    let parts = receipt::Parts {
        repo,
        inputs: receipt::inputs(&receipt::Inputs {
            baseline,
            candidate: cand,
            setting: req.setting,
            config: Some(config),
            environment: environment.as_ref(),
            laws: more.laws.digest.clone(),
            intent: intent.to_value(),
            driver: Some(driver),
        }),
        baseline,
        candidate: cand,
        setting: req.setting,
        isolation: &measured.isolation,
        regression: &measured.regression,
        old_tests: &old_tests,
        change: &change,
        laws: &more.laws,
        preserved: more.preserved.as_ref(),
        adequacy: more.adequacy.as_ref(),
        signals: more.signals.as_deref().ok(),
        on_unproven: config.on_unproven,
        on_redundant: config.on_redundant,
        on_survivors: config.adequacy.on_survivors,
        faults: approval_fault.into_iter().collect(),
        not_verified,
        this_run,
    };
    let (receipt, action) = receipt::assemble(&parts);
    Ok(Outcome { receipt, action })
}
