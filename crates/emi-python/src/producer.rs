//! The identity of what EMI itself puts into the Python runs: its version
//! and every script it writes or passes to an interpreter. A result kept
//! from one run for later checks (a listing, the baseline's laws, the
//! baseline's recorded calls) was produced by that code as much as by the
//! repository, so it is kept under this identity and an EMI whose scripts
//! changed measures again instead of reading what the old ones produced.

use serde_json::json;

/// The digest of EMI's version and its embedded Python.
pub fn producer() -> String {
    emi_kernel::json::digest(
        "emi-python-producer",
        &json!({
            "version": env!("CARGO_PKG_VERSION"),
            "pins": crate::pins::SOURCE,
            "report": crate::proving::pytest::REPORT,
            "coverage_run": crate::proving::pytest::COVERAGE_RUN,
            "contexts": crate::proving::exec::CONTEXTS,
            "drift_driver": crate::drift::python::DRIVER,
            "drift_recorder": crate::drift::python::RECORDER,
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_producer_is_stable_and_names_its_scripts() {
        assert_eq!(producer(), producer());
        assert_eq!(producer().len(), 64);
    }
}
