//! Which Python environment the tests run in, as one digest: the
//! interpreter's version and implementation, the machine's architecture,
//! and every installed distribution with its version. Two checks with the
//! same digest ran their tests against the same installed code; a check
//! after `pip install` has another one, and so does not reuse the
//! baseline's recorded runs.

use std::path::Path;
use std::time::Duration;

use emi_exec::{Isolation, Spec, Status};

/// What the interpreter prints about itself. Nothing in it names a path
/// or a time, so the same environment prints the same text anywhere.
const PROBE: &str = r#"
import platform, sys
try:
    import importlib.metadata as metadata
    dists = sorted({"%s==%s" % ((d.metadata["Name"] or "").lower(), d.version)
                    for d in metadata.distributions()})
except Exception as e:
    print("distributions unreadable: %s" % type(e).__name__)
    sys.exit(3)
print(sys.implementation.name, ".".join(map(str, sys.version_info[:3])), platform.machine())
for d in dists:
    print(d)
"#;

/// The environment digest, or why there is none. `interpreter` is run in
/// `repo` with no network, as the tests' own Python.
pub fn id(repo: &Path, interpreter: &str) -> Result<String, String> {
    let mut spec = Spec::new(&[interpreter, "-I", "-c", PROBE], repo);
    spec.timeout = Duration::from_secs(60);
    spec.isolation = Isolation::Best { network: false };
    spec.output_limit = 4 << 20;
    let o = emi_exec::run(&spec);
    match (o.status, o.exit_code) {
        (Status::Exited, Some(0)) if o.stdout_bytes <= spec.output_limit as u64 => Ok(
            emi_kernel::json::digest("python-environment", &serde_json::json!(o.stdout)),
        ),
        _ => Err(format!(
            "the test environment could not be identified ({}): {}",
            interpreter,
            o.launch_error.unwrap_or_else(|| o
                .stderr
                .lines()
                .last()
                .unwrap_or("no output")
                .to_owned())
        )),
    }
}
