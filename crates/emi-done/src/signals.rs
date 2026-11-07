//! Escape-hatch signals: lines the change added to product code that are a
//! known way to make checks pass without the code doing what they check.
//! They are patterns, not proof: code written another way passes them,
//! and a line that matches may be fine. So they are reported for a person
//! and never change the action, and the receipt labels them `pattern`.

use std::path::Path;

use serde_json::{Value, json};

fn signal(s: &emi_driver::Signal, file: &str, text: &str) -> Value {
    json!({"kind": s.kind, "file": file, "line": s.line, "text": text.trim(), "means": s.means,
           "assurance": "pattern"})
}

/// The signals in the lines the change from `baseline` to `rev` added to
/// product files (not tests, not laws), or why they could not be looked
/// for.
pub fn find(
    view: &Path,
    (baseline, rev): (&str, &str),
    is_test: impl Fn(&str) -> bool,
    driver: &dyn emi_driver::Driver,
) -> Result<Vec<Value>, String> {
    let files = emi_tree::hunks::changes(view, baseline, Some(rev))
        .map_err(|e| format!("the change's files could not be read: {e}"))?;
    let mut out = Vec::new();
    for f in files
        .iter()
        .filter(|f| driver.is_product_file(&f.path) && !is_test(&f.path))
    {
        let found = driver
            .signals(&f.path, &f.added)
            .map_err(|why| why.reason().to_owned())?;
        for s in &found {
            let text = f
                .added
                .iter()
                .find(|(line, _)| *line == s.line)
                .map_or("", |(_, text)| text.as_str());
            out.push(signal(s, &f.path, text));
        }
    }
    Ok(out)
}

/// The receipt's limits line for the signals, when there are any or they
/// could not be looked for.
pub fn limits(signals: &Result<Vec<Value>, String>) -> Vec<String> {
    let signals = match signals {
        Ok(found) if found.is_empty() => return Vec::new(),
        Ok(found) => found,
        Err(why) => {
            return vec![format!("Escape-hatch signals were not looked for: {why}.")];
        }
    };
    let first = &signals[0];
    vec![format!(
        "{} escape-hatch signal(s) in the change's product code, found by pattern and not acted \
         on (first: {}:{} {}).",
        signals.len(),
        first["file"].as_str().unwrap_or("?"),
        first["line"],
        first["kind"].as_str().unwrap_or("?")
    )]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signals_that_could_not_be_looked_for_are_said_not_found() {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("{e}"));
        let found = find(
            dir.path(),
            ("base", "rev"),
            |_| false,
            &emi_python::driver::Python,
        );
        assert!(found.is_err(), "{found:?}");
        let lines = limits(&found);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].starts_with("Escape-hatch signals were not looked for"));
        assert!(limits(&Ok(Vec::new())).is_empty());
    }
}
