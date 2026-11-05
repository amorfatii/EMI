//! A second look at tests a full run said fail: run exactly those, once,
//! on the candidate as it is. A failure that does not show again was not
//! caused by the candidate's product code alone (a flaky test, a
//! dependence on what ran before, a test file run in another version).

use std::collections::BTreeSet;
use std::path::Path;

use crate::sandbox::Runner;

use crate::proving::exec::Session;
use crate::proving::junit::Observed;
use crate::proving::pytest::Command;
use crate::proving::{Candidate, Request};

/// The `ids` that pass when run by themselves on the request's candidate,
/// which must be a commit in `objects`. `Err` when nothing could be run.
/// A test that gave no result is not among the passing ones.
pub fn passing(req: &Request, objects: &Path, ids: &[String]) -> Result<BTreeSet<String>, String> {
    let Candidate::Ref(rev) = &req.candidate else {
        return Err("a second run needs the candidate as a commit".into());
    };
    let command = Command::parse(&req.test_command)?;
    let repo = std::fs::canonicalize(&req.repo)
        .map_err(|e| format!("repository {}: {e}", req.repo.display()))?;
    let mut env = vec![("PYTHONDONTWRITEBYTECODE".to_owned(), "1".to_owned())];
    env.extend(req.env.iter().cloned());
    let runner = Runner {
        repo: &repo,
        rev,
        link: &req.link,
        env: &env,
        timeout: req.timeout,
        network: req.network,
    };
    let session = Session::new(runner, objects, &command);
    let tree = session.tree("rerun", rev)?;
    let mut sorted = ids.to_vec();
    sorted.sort();
    sorted.dedup();
    Ok(session
        .observe(&tree, &sorted, "rerun")
        .into_iter()
        .filter(|(_, seen)| *seen == Observed::Passed)
        .map(|(id, _)| id)
        .collect())
}
