//! Pins: what every test run is held to so that its outcome depends on the
//! code and not on the run. They are applied identically at the baseline
//! and on the candidate, and are part of a decision's inputs.

use serde_json::{Value, json};

/// The environment variables and pytest options a pinned run uses.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pins {
    /// `PYTHONHASHSEED`: the order of sets and string-keyed hashes.
    pub hash_seed: u32,
    /// `TZ`.
    pub tz: String,
    /// `LC_ALL`.
    pub locale: String,
    /// Seed `random` (and numpy, when loaded) before each test from the
    /// test's id, so a test's draws do not depend on which ran before it.
    pub seed_random: bool,
    /// Load a Hypothesis profile that derandomizes generation, keeps no
    /// example database and has no deadline.
    pub derandomize_hypothesis: bool,
}

impl Default for Pins {
    fn default() -> Self {
        Pins {
            hash_seed: 0,
            tz: "UTC".to_owned(),
            locale: "C.UTF-8".to_owned(),
            seed_random: true,
            derandomize_hypothesis: true,
        }
    }
}

/// The pytest plugin module that applies the in-process pins.
pub const PLUGIN: &str = "emi_pins";
/// Where, relative to a run's working directory, the plugin is written.
pub const PLUGIN_DIR: &str = ".emi-pins";
/// Set to `1` on a run that only confirms a failure: the plugin then lets
/// Hypothesis stop at its first failure instead of shrinking it.
pub const CONFIRM_ENV: &str = "EMI_PIN_CONFIRM";

fn flag(on: bool) -> String {
    if on { "1" } else { "0" }.to_owned()
}

impl Pins {
    /// The environment a pinned run adds to its own.
    pub fn env(&self) -> Vec<(String, String)> {
        vec![
            ("PYTHONHASHSEED".to_owned(), self.hash_seed.to_string()),
            ("TZ".to_owned(), self.tz.clone()),
            ("LC_ALL".to_owned(), self.locale.clone()),
            ("PYTHONDONTWRITEBYTECODE".to_owned(), "1".to_owned()),
            ("EMI_PIN_RANDOM".to_owned(), flag(self.seed_random)),
            (
                "EMI_PIN_HYPOTHESIS".to_owned(),
                flag(self.derandomize_hypothesis),
            ),
        ]
    }

    /// The pytest options a pinned run adds: the pins plugin, and no
    /// plugin that reorders tests or remembers earlier runs.
    pub fn pytest_args() -> Vec<String> {
        ["-p", PLUGIN, "-p", "no:randomly", "-p", "no:cacheprovider"]
            .map(str::to_owned)
            .to_vec()
    }

    /// `PYTHONPATH` for a run: the project's own import roots first, then
    /// the plugin's directory.
    pub fn python_path(roots: &[String]) -> String {
        let mut parts: Vec<&str> = roots.iter().map(String::as_str).collect();
        parts.push(PLUGIN_DIR);
        parts.join(":")
    }

    pub fn to_value(&self) -> Value {
        json!({
            "hash_seed": self.hash_seed,
            "tz": self.tz,
            "locale": self.locale,
            "seed_random": self.seed_random,
            "derandomize_hypothesis": self.derandomize_hypothesis,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_pins_and_their_environment() {
        let env = Pins::default().env();
        assert!(env.contains(&("PYTHONHASHSEED".into(), "0".into())));
        assert!(env.contains(&("TZ".into(), "UTC".into())));
        assert!(env.contains(&("EMI_PIN_HYPOTHESIS".into(), "1".into())));
        assert_eq!(
            Pins::python_path(&["src".into()]),
            format!("src:{PLUGIN_DIR}")
        );
        assert_eq!(Pins::python_path(&[]), PLUGIN_DIR);
    }
}
