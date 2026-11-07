//! Rust behind EMI's language boundary (ADR 0009), regression first: the
//! project's tests run with `cargo test`, read test by test from its own
//! output. Every other stage says it is not measured here; the receipt
//! then states what a Rust check is worth.

use std::collections::BTreeSet;
use std::path::Path;
use std::time::Duration;

use emi_driver::{Driver, RegressionPlan, Summary};
use emi_exec::{Isolation, Spec, Status};

mod output;

/// The Rust driver.
pub struct Rust;

/// Files that decide how the tests are built and run: the manifest
/// (features, a target's `harness`), the build script, cargo's own
/// configuration and the toolchain file.
const RUNNER_CONFIG: &[&str] = &[
    "Cargo.toml",
    "build.rs",
    ".cargo/config.toml",
    ".cargo/config",
    "rust-toolchain",
    "rust-toolchain.toml",
];

/// Whether `command` runs `cargo test`.
fn is_cargo_test(command: &[String]) -> bool {
    matches!(command, [cargo, test, ..]
        if Path::new(cargo).file_name().is_some_and(|n| n == "cargo") && test == "test")
}

/// `command` with `extra` before its `--`, if it has one, and cargo's
/// messages on the same stream as the tests' (the `Running` lines that
/// name each test binary's source go to stderr).
fn merged(command: &[String], extra: &[&str], after: &[&str]) -> Vec<String> {
    let split = command.iter().position(|a| a == "--");
    let (cargo, rest) = command.split_at(split.unwrap_or(command.len()));
    let mut argv = vec![
        "sh".to_owned(),
        "-c".to_owned(),
        "exec \"$0\" \"$@\" 2>&1".to_owned(),
    ];
    argv.extend(cargo.iter().cloned());
    argv.extend(extra.iter().map(|a| (*a).to_owned()));
    if rest.is_empty() && !after.is_empty() {
        argv.push("--".to_owned());
    }
    argv.extend(rest.iter().cloned());
    argv.extend(after.iter().map(|a| (*a).to_owned()));
    argv
}

impl Driver for Rust {
    fn language(&self) -> &'static str {
        "rust"
    }

    fn runner(&self) -> &'static str {
        "cargo test"
    }

    fn producer(&self) -> String {
        emi_kernel::json::digest(
            "emi-rust-producer",
            &serde_json::json!({"version": env!("CARGO_PKG_VERSION")}),
        )
    }

    /// Without network (the sandbox has none), every test binary run
    /// whatever another one showed.
    fn regression(&self, command: &[String]) -> Option<RegressionPlan> {
        if !is_cargo_test(command) {
            return None;
        }
        Some(RegressionPlan {
            run: merged(command, &["--offline", "--no-fail-fast"], &[]),
            list: merged(command, &["--offline"], &["--list"]),
            format: "cargo-test",
        })
    }

    fn run_env(&self, _roots: &[String]) -> Vec<(String, String)> {
        // colour codes would split the words the output is read by
        vec![("CARGO_TERM_COLOR".to_owned(), "never".to_owned())]
    }

    /// Nothing runs inside the test process: the pins are the environment.
    fn install_pins(&self, _dir: &Path) -> Result<(), String> {
        Ok(())
    }

    fn is_runner_config(&self, path: &str) -> bool {
        RUNNER_CONFIG
            .iter()
            .any(|n| path == *n || path.ends_with(&format!("/{n}")))
    }

    fn runner_config_globs(&self) -> Vec<String> {
        RUNNER_CONFIG
            .iter()
            .flat_map(|n| [(*n).to_owned(), format!("*/{n}")])
            .collect()
    }

    fn summary(&self, output: &str) -> Option<Summary> {
        output::summary(output)
    }

    fn failures(&self, output: &str) -> Vec<String> {
        output::outcomes(output)
            .unwrap_or_default()
            .into_iter()
            .filter(|(_, outcome)| *outcome == "failed")
            .map(|(id, _)| id)
            .collect()
    }

    fn outcome_lines(&self, output: &str) -> Option<Vec<(String, &'static str)>> {
        output::outcomes(output)
    }

    fn listed(&self, output: &str) -> Option<Result<BTreeSet<String>, String>> {
        Some(output::listed(output))
    }

    /// The toolchain: two checks with the same `rustc -vV` built the tests
    /// with the same compiler; the dependencies are the lock file's, in
    /// the tree.
    fn environment(&self, repo: &Path, command: &[String]) -> Option<Result<String, String>> {
        if !is_cargo_test(command) {
            return None;
        }
        let mut spec = Spec::new(&["rustc", "-vV"], repo);
        spec.timeout = Duration::from_secs(60);
        spec.isolation = Isolation::Best { network: false };
        let o = emi_exec::run(&spec);
        Some(match (o.status, o.exit_code) {
            (Status::Exited, Some(0)) => Ok(emi_kernel::json::digest(
                "rust-environment",
                &serde_json::json!(o.stdout),
            )),
            _ => Err(format!(
                "the toolchain could not be identified: {}",
                o.launch_error.unwrap_or_else(|| o
                    .stderr
                    .lines()
                    .last()
                    .unwrap_or("no output")
                    .to_owned())
            )),
        })
    }
}

#[cfg(test)]
// a test that cannot read its own fixture must fail loudly
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn cargo_test_is_run_whole_and_listed_with_its_messages() {
        let plan = Rust
            .regression(&argv("cargo test --workspace"))
            .expect("a plan");
        assert_eq!(&plan.run[..3], ["sh", "-c", "exec \"$0\" \"$@\" 2>&1"]);
        assert_eq!(
            &plan.run[3..],
            argv("cargo test --workspace --offline --no-fail-fast")
        );
        assert_eq!(
            &plan.list[3..],
            argv("cargo test --workspace --offline -- --list")
        );
        let with_args = Rust
            .regression(&argv("cargo test -- --test-threads 1"))
            .expect("a plan");
        assert_eq!(
            &with_args.list[3..],
            argv("cargo test --offline -- --test-threads 1 --list")
        );
        assert!(Rust.regression(&argv("make test")).is_none());
    }

    #[test]
    fn the_manifest_and_build_script_configure_the_runner() {
        assert!(Rust.is_runner_config("Cargo.toml"));
        assert!(Rust.is_runner_config("crates/a/build.rs"));
        assert!(Rust.is_runner_config(".cargo/config.toml"));
        assert!(!Rust.is_runner_config("src/lib.rs"));
        assert!(!Rust.is_runner_config("tests/config.toml"));
    }
}
