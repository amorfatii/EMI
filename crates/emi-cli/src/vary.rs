//! `emi task holdout --vary`: held-out cases the agent never saw, with
//! expected values from a trusted reference. A random split only hides cases
//! from the distribution the agent already met; new inputs catch code that
//! special-cases the visible ones. The Python side (literal parsing, variant
//! generation, reference calls) is one driver run through the single process
//! executor, so a hanging or crashing reference cannot take this process down.

use std::path::Path;
use std::time::Duration;

use emi_ledger::pyfmt::{digest, dumps_line};
use serde_json::{Value, json};

const DRIVER: &str = include_str!("vary_driver.py");
const CASE_TIMEOUT_S: f64 = 2.0;
const TOTAL_TIMEOUT_S: f64 = 60.0;
/// Headroom over the driver's own deadline, so the driver reports what it
/// has before the executor kills it.
const EXEC_SLACK_S: f64 = 30.0;

/// What the driver returned, checked for shape.
#[derive(Debug)]
pub struct Variants {
    pub imports: Vec<String>,
    /// (call name, argument source, expected source)
    pub cases: Vec<(String, String, String)>,
    pub dropped: Value,
    pub stopped_early: bool,
}

/// The seed as an integer: the driver's generator needs one, and the same
/// file and `--seed` must give the same variants.
fn seed_of(text: &str, seed: &str) -> u64 {
    let d = digest("holdout-vary", &json!({"text": text, "seed": seed}));
    u64::from_str_radix(&d[..12], 16).unwrap_or_default()
}

fn text_field<'a>(v: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    v[key]
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("the variant driver's result lacks a string `{key}`"))
}

fn parse(out: &str) -> anyhow::Result<Variants> {
    let v: Value = serde_json::from_str(out)
        .map_err(|e| anyhow::anyhow!("the variant driver's result is not JSON: {e}"))?;
    let list = |key: &str| -> anyhow::Result<&Vec<Value>> {
        v[key]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("the variant driver's result lacks `{key}`"))
    };
    let imports = list("imports")?
        .iter()
        .map(|i| i.as_str().map(str::to_owned))
        .collect::<Option<Vec<_>>>()
        .ok_or_else(|| anyhow::anyhow!("an import of the driver's result is not text"))?;
    let cases = list("cases")?
        .iter()
        .map(|c| {
            Ok((
                text_field(c, "call")?.to_owned(),
                text_field(c, "args")?.to_owned(),
                text_field(c, "expected")?.to_owned(),
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(Variants {
        imports,
        cases,
        dropped: v["dropped"].clone(),
        stopped_early: v["stopped_early"]
            .as_bool()
            .ok_or_else(|| anyhow::anyhow!("the variant driver's result lacks `stopped_early`"))?,
    })
}

/// Derive variants of `visible`'s cases and ask the reference for their
/// expected values.
pub fn variants(
    visible: &str,
    reference: &Path,
    seed: &str,
    count: usize,
    case_timeout_s: f64,
) -> anyhow::Result<Variants> {
    let request = dumps_line(&json!({
        "source": visible,
        "reference": reference,
        "seed": seed_of(visible, seed),
        "count": count,
        "case_timeout": case_timeout_s,
        "total_timeout": TOTAL_TIMEOUT_S,
    }));
    let cwd = reference
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut spec = emi_exec::Spec::new(&["python3", "-c", DRIVER, &request], cwd);
    // sets and dicts of strings must print in the same order on every run
    spec.env.push(("PYTHONHASHSEED".into(), "0".into()));
    spec.timeout = Duration::from_secs_f64(TOTAL_TIMEOUT_S + EXEC_SLACK_S);
    let o = emi_exec::run(&spec);
    if o.status != emi_exec::Status::Exited || o.exit_code != Some(0) {
        anyhow::bail!(
            "no variants written: {}{}{}",
            o.stderr.trim(),
            o.launch_error
                .as_deref()
                .map_or(String::new(), |e| format!("python3 could not run: {e}")),
            if o.status == emi_exec::Status::TimedOut {
                "the reference did not finish in time"
            } else {
                ""
            }
        );
    }
    parse(&o.stdout)
}

/// The held-out file: the visible file's imports, then one test per case.
pub fn render(v: &Variants) -> String {
    let mut out = String::new();
    for i in &v.imports {
        out += &format!("{i}\n");
    }
    for (k, (call, args, expected)) in v.cases.iter().enumerate() {
        out += &format!(
            "\n\ndef test_emi_variant_{}():\n    assert {call}({args}) == {expected}\n",
            k + 1
        );
    }
    out
}

pub fn run(
    file: &Path,
    reference: &Path,
    out: &Path,
    seed: &str,
    count: usize,
) -> anyhow::Result<u8> {
    if count == 0 {
        anyhow::bail!("--count must be at least 1");
    }
    let visible = std::fs::read_to_string(file)?;
    let v = variants(&visible, reference, seed, count, CASE_TIMEOUT_S)?;
    if v.cases.is_empty() {
        anyhow::bail!(
            "no variant case could be written, nothing written (dropped: {}){}",
            dumps_line(&v.dropped),
            if v.stopped_early {
                "; the reference ran out of time"
            } else {
                ""
            }
        );
    }
    std::fs::write(out, render(&v))?;
    println!(
        "{}",
        json!({"variants_written": v.cases.len(), "dropped": v.dropped,
               "stopped_early": v.stopped_early,
               "visible_file": file, "held_out_file": out,
               "draft_flag": crate::holdout::draft_flag(file, out),
               "next": "the visible file is unchanged; pass the --held-out flag to task draft"})
    );
    Ok(0)
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{Variants, render, variants};

    struct Dir(PathBuf);

    impl Drop for Dir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn scratch(name: &str) -> Dir {
        let d = std::env::temp_dir().join(format!("emi-vary-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap_or_default();
        Dir(d)
    }

    fn python3() -> bool {
        let spec = emi_exec::Spec::new(&["python3", "-c", "pass"], ".");
        let ok = emi_exec::run(&spec).exit_code == Some(0);
        if !ok {
            println!("python3 is not available: variant test skipped");
        }
        ok
    }

    const VISIBLE: &str = "from solution import add\n\n\ndef check(candidate):\n    assert candidate(1, 2) == 3\n    assert candidate(2, 2) == 4\n    assert candidate(0, 0) == 0\n    assert candidate(5, 5) == 10\n    assert helper_is_not_a_case(1) is None\n\n\ndef test_add():\n    check(add)\n";
    const REFERENCE: &str = "def add(a, b):\n    return a + b\n";

    fn make(dir: &Dir, reference: &str, count: usize) -> Option<Variants> {
        let path = dir.0.join("reference.py");
        std::fs::write(&path, reference).unwrap_or_default();
        variants(VISIBLE, &path, "1", count, 2.0).ok()
    }

    /// Whether the held-out file `text` passes against a `solution.py`.
    fn passes_against(dir: &Dir, text: &str, solution: &str) -> bool {
        std::fs::write(dir.0.join("solution.py"), solution).unwrap_or_default();
        std::fs::write(dir.0.join("held.py"), text).unwrap_or_default();
        let run = "import runpy\ng = runpy.run_path('held.py')\nfor n in sorted(g):\n    if n.startswith('test_emi_variant_'):\n        g[n]()\n";
        let spec = emi_exec::Spec::new(&["python3", "-c", run], &dir.0);
        emi_exec::run(&spec).exit_code == Some(0)
    }

    #[test]
    fn variants_pass_the_reference_and_catch_a_table_of_the_visible_cases() {
        if !python3() {
            return;
        }
        let dir = scratch("catch");
        let v = make(&dir, REFERENCE, 20);
        let Some(v) = v else { panic!("no variants") };
        assert!(!v.cases.is_empty() && v.cases.len() <= 20, "{:?}", v.cases);
        assert_eq!(v.dropped["unparsed_visible"], 1, "{}", v.dropped);
        let text = render(&v);
        assert!(text.starts_with("from solution import add\n"), "{text}");
        assert!(
            text.contains("def test_emi_variant_1():\n    assert add("),
            "{text}"
        );
        assert!(passes_against(&dir, &text, REFERENCE));
        let table =
            "def add(a, b):\n    return {(1, 2): 3, (2, 2): 4, (0, 0): 0, (5, 5): 10}[(a, b)]\n";
        assert!(!passes_against(&dir, &text, table));
    }

    #[test]
    fn a_reference_that_raises_or_hangs_is_counted_not_hidden() {
        if !python3() {
            return;
        }
        let dir = scratch("drops");
        let reference = "def add(a, b):\n    if a < 0:\n        raise ValueError(a)\n    if a == 3:\n        while True:\n            pass\n    return a + b\n";
        let path = dir.0.join("reference.py");
        std::fs::write(&path, reference).unwrap_or_default();
        let v = variants(VISIBLE, &path, "2", 500, 0.3).unwrap_or_else(|e| panic!("{e}"));
        let n = |k: &str| v.dropped[k].as_u64().unwrap_or_default();
        assert!(n("reference_error") > 0, "{}", v.dropped);
        assert!(n("timeout") > 0, "{}", v.dropped);
        assert!(!v.cases.is_empty());
        assert!(
            v.cases.iter().all(|(_, args, _)| !args.starts_with('-')),
            "{:?}",
            v.cases
        );
    }

    #[test]
    fn the_same_seed_gives_the_same_variants() {
        if !python3() {
            return;
        }
        let dir = scratch("same");
        let a = make(&dir, REFERENCE, 10).map(|v| render(&v));
        let b = make(&dir, REFERENCE, 10).map(|v| render(&v));
        assert!(a.is_some() && a == b);
        let path: &Path = &dir.0.join("reference.py");
        let other = variants(VISIBLE, path, "another", 10, 2.0)
            .ok()
            .map(|v| render(&v));
        assert_ne!(a, other);
    }

    #[test]
    fn a_reference_that_cannot_be_loaded_is_an_error() {
        if !python3() {
            return;
        }
        let dir = scratch("broken");
        let path = dir.0.join("reference.py");
        std::fs::write(&path, "def add(:\n").unwrap_or_default();
        let err = variants(VISIBLE, &path, "0", 5, 2.0)
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(err.contains("could not be loaded"), "{err}");
    }
}
