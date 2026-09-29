//! The profile as text for a terminal (the JSON is the full record).

use serde_json::Value;

fn strs(v: &Value) -> Vec<&str> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

fn counts(v: &Value) -> String {
    v.as_object()
        .into_iter()
        .flatten()
        .map(|(k, n)| format!("{k} {n}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn header(p: &Value) -> Vec<String> {
    let s = &p["summary"];
    let head: String = p["head"].as_str().unwrap_or("").chars().take(12).collect();
    let mut out = vec![format!(
        "profile of {} at {head}: {} functions in {} files, {} commits in the window",
        if p["source"] == "" {
            "."
        } else {
            p["source"].as_str().unwrap_or(".")
        },
        s["functions"],
        s["files"],
        s["commits"]
    )];
    let c = &p["coverage"];
    out.push(if c.is_null() {
        "coverage: no report given".to_owned()
    } else if let Some(e) = c["error"].as_str() {
        format!("coverage: the coverage run failed: {e}")
    } else {
        format!(
            "coverage: {} report{}, {} of {} scope files in it; {} entries unmatched, {} ambiguous",
            c["format"].as_str().unwrap_or(""),
            if c["source"] == "run" {
                " measured by emi at the head"
            } else {
                ""
            },
            c["scope_files_in_report"],
            c["scope_files"],
            c["unmatched"],
            c["ambiguous"]
        )
    });
    let m = &p["mutation"];
    out.push(if m.is_null() {
        "mutation: not sampled (no test command given)".to_owned()
    } else if m["baseline"]["passed"] == true {
        format!(
            "mutation: {} mutants over {} functions; unmutated tests passed in {} ms ({})",
            m["mutants"],
            m["functions"],
            m["baseline"]["duration_ms"],
            m["isolation"].as_str().unwrap_or("")
        )
    } else {
        format!(
            "mutation: the unmutated tests did not pass (exit {}), so no mutant result counts",
            m["baseline"]["exit_code"]
        )
    });
    out.push(format!("readiness: {}", counts(&s["readiness"])));
    out
}

fn mutation_cell(f: &Value) -> String {
    let t = &f["facts"]["mutation"];
    if t.is_null() {
        return "-".into();
    }
    let caught = t["killed"].as_u64().unwrap_or(0) + t["timed_out"].as_u64().unwrap_or(0);
    let resolved = caught + t["survived"].as_u64().unwrap_or(0);
    format!("{caught}/{resolved}")
}

fn function_lines(f: &Value) -> Vec<String> {
    let facts = &f["facts"];
    let cov = &facts["coverage"];
    let lines = if cov.is_null() {
        "-".to_owned()
    } else {
        format!("{}/{}", cov["executed"], cov["executable"])
    };
    let mut out = vec![format!(
        "{}:{} {}  [{}]  cognitive {}  revisions {}  lines run {}  mutants caught {}",
        f["file"].as_str().unwrap_or(""),
        f["start"],
        f["function"].as_str().unwrap_or(""),
        f["readiness"].as_str().unwrap_or(""),
        facts["cognitive"],
        facts["revisions"],
        lines,
        mutation_cell(f)
    )];
    for s in strs(&facts["mutation"]["survivors"]).iter().take(3) {
        out.push(format!("    missed: {s}"));
    }
    out.push(format!("    {}", f["advice"].as_str().unwrap_or("")));
    out.extend(
        strs(&f["inferences"])
            .iter()
            .map(|s| format!("    inference: {s}")),
    );
    out.extend(
        strs(&f["unknowns"])
            .iter()
            .map(|s| format!("    unknown: {s}")),
    );
    out
}

/// The profile for a terminal: header, then the most exposed functions.
pub fn render(p: &Value) -> String {
    let mut out = header(p);
    out.push(String::new());
    for f in p["functions"].as_array().into_iter().flatten() {
        out.extend(function_lines(f));
    }
    for s in strs(&p["skipped_files"]) {
        out.push(format!("not profiled: {s}"));
    }
    out.push(String::new());
    out.push(
        "Ranked by revisions × graded erosion. Readiness says whether the tests would notice a \
         wrong change here; it is not a bug prediction."
            .into(),
    );
    out.join("\n") + "\n"
}
