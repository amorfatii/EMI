//! Per-test execution, excluding module collection and unrelated tests.

use std::collections::{BTreeMap, BTreeSet};

use emi_profile::coverage::Resolved;
use serde_json::Value;

type Files = BTreeMap<String, BTreeSet<u32>>;

pub struct Coverage {
    pub resolved: Resolved,
    contexts: BTreeMap<String, Files>,
}

impl Coverage {
    pub fn read(resolved: Resolved, text: &str) -> Result<Self, String> {
        let value: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        let files = value["files"]
            .as_object()
            .ok_or("coverage has no files object")?;
        let mut contexts = BTreeMap::<String, Files>::new();
        for (path, file) in files {
            let lines = file["contexts"]
                .as_object()
                .ok_or("coverage has no per-test contexts")?;
            for (line, names) in lines {
                let line: u32 = line.parse().map_err(|_| "invalid coverage line")?;
                for name in names.as_array().ok_or("invalid coverage contexts")? {
                    let name = name.as_str().ok_or("invalid coverage context name")?;
                    if !name.is_empty() {
                        contexts
                            .entry(name.into())
                            .or_default()
                            .entry(path.trim_start_matches("./").into())
                            .or_default()
                            .insert(line);
                    }
                }
            }
        }
        Ok(Self { resolved, contexts })
    }

    pub fn for_tests(&self, ids: &[String]) -> Result<Resolved, String> {
        let mut report = self.resolved.clone();
        for lines in report.files.values_mut() {
            lines.executed.clear();
        }
        for id in ids {
            let files = self
                .contexts
                .get(id)
                .ok_or_else(|| format!("no per-test execution recorded for {id}"))?;
            for (path, executed) in files {
                if let Some(lines) = report.files.get_mut(path) {
                    lines.executed.extend(executed);
                }
            }
        }
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use emi_profile::coverage::Lines;

    #[test]
    fn collection_and_other_tests_cannot_inflate_execution() {
        let mut resolved = Resolved::default();
        resolved.files.insert(
            "a.py".into(),
            Lines {
                executable: [1, 2, 3].into(),
                executed: [1, 2, 3].into(),
            },
        );
        let c = Coverage::read(
            resolved,
            r#"{"files":{"a.py":{"contexts":{"1":[""],"2":["test_a"],"3":["test_b"]}}}}"#,
        )
        .unwrap_or_else(|e| panic!("{e}"));
        let one = c
            .for_tests(&["test_a".into()])
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(one.files["a.py"].executed, [2].into());
        assert!(c.for_tests(&["missing".into()]).is_err());
    }
}
