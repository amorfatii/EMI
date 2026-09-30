//! A receipt as plain Markdown: one heading per section, tables where the
//! content is tabular, no HTML. Anything not measured says so and why;
//! no cell is left blank.

use emi_ledger::pyfmt::dumps_line;
use serde_json::Value;

/// Headings, in the order every receipt has them.
pub const SECTIONS: [&str; 8] = [
    "Decision",
    "What was verified",
    "Evidence kind",
    "Escalations",
    "Scope",
    "How much the tests pin the change",
    "Places to look",
    "Not verified",
];

/// `receipt` (from [`super::receipt`]) as Markdown.
pub fn markdown(r: &Value) -> String {
    let mut out = vec![format!(
        "# Verification receipt: {}",
        r["task_id"].as_str().unwrap_or("?")
    )];
    let render: [fn(&Value, &mut Vec<String>); 8] = [
        decision,
        verified,
        evidence,
        escalations,
        scope,
        pinning,
        places,
        not_verified,
    ];
    for (heading, part) in SECTIONS.iter().zip(render) {
        out.push(String::new());
        out.push(format!("## {heading}"));
        out.push(String::new());
        part(r, &mut out);
    }
    out.join("\n") + "\n"
}

/// `v` for a table cell or a list item: text as it is, anything else as
/// one line of JSON; pipes escaped and lines joined so a cell stays one.
fn text(v: &Value) -> String {
    let s = match v {
        Value::String(s) => s.clone(),
        Value::Null => "not measured".into(),
        other => dumps_line(other),
    };
    s.replace('|', "\\|").replace('\n', " ")
}

fn code(v: &Value) -> String {
    format!("`{}`", text(v).replace('`', "'"))
}

/// Whether `v` is a "not measured" marker.
fn unmeasured(v: &Value) -> bool {
    v["state"] == "not_measured"
}

fn why_not(v: &Value) -> String {
    format!(
        "not measured ({})",
        v["reason"].as_str().unwrap_or("no reason recorded")
    )
}

/// A list of paths as code, `none` when empty, or why it was not measured.
fn paths(v: &Value) -> String {
    if unmeasured(v) {
        return why_not(v);
    }
    let items: Vec<String> = v.as_array().into_iter().flatten().map(code).collect();
    if items.is_empty() {
        "none".into()
    } else {
        items.join(", ")
    }
}

fn row(cells: &[String]) -> String {
    format!("| {} |", cells.join(" | "))
}

fn decision(r: &Value, out: &mut Vec<String>) {
    let d = &r["decision"];
    let content = if unmeasured(&d["content_id"]) {
        why_not(&d["content_id"])
    } else {
        code(&d["content_id"])
    };
    out.push("| | |".into());
    out.push("|---|---|".into());
    out.push(row(&[
        "Decision".into(),
        format!("**{}**", text(&d["decision"])),
    ]));
    out.push(row(&["Candidate".into(), code(&d["candidate_id"])]));
    out.push(row(&["Content verified".into(), content]));
    out.push(row(&[
        "Contract".into(),
        format!(
            "{} (revision {})",
            code(&d["contract_id"]),
            text(&d["revision"])
        ),
    ]));
    out.push(row(&["Recorded at".into(), text(&d["at"])]));
    out.push(String::new());
    out.push("Reasons:".into());
    out.push(String::new());
    out.extend(
        d["reasons"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|x| format!("- {}", text(x))),
    );
}

fn requirements(v: &Value, out: &mut Vec<String>) {
    if unmeasured(v) {
        out.push(format!("Requirements: {}", why_not(v)));
        return;
    }
    out.push("| Requirement | Acceptance | State | Check | Check status |".into());
    out.push("|---|---|---|---|---|".into());
    for req in v.as_array().into_iter().flatten() {
        for c in req["checks"].as_array().into_iter().flatten() {
            out.push(row(&[
                text(&req["id"]),
                text(&req["acceptance"]),
                text(&req["state"]),
                code(&c["check"]),
                text(&c["status"]),
            ]));
        }
    }
}

fn inventory(v: &Value) -> String {
    match v["state"].as_str() {
        Some("not_measured") => why_not(v),
        Some("unverified") => format!("unverified ({})", text(&v["detail"])),
        _ => format!(
            "{} (baseline {} tests, candidate {} tests, missing: {})",
            text(&v["state"]),
            text(&v["baseline"]),
            text(&v["candidate"]),
            paths(&v["missing"])
        ),
    }
}

fn shaken(v: &Value) -> String {
    if unmeasured(v) {
        return why_not(v);
    }
    let changed: Vec<String> = v["changed"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            format!(
                "{} round {}: {}",
                code(&c["check"]),
                text(&c["round"]),
                text(&c["status"])
            )
        })
        .collect();
    format!(
        "{} ({} round(s), {} test(s) renamed; changed: {})",
        text(&v["state"]),
        text(&v["rounds"]),
        text(&v["tests_renamed"]),
        if changed.is_empty() {
            "none".into()
        } else {
            changed.join(", ")
        }
    )
}

fn verified(r: &Value, out: &mut Vec<String>) {
    let v = &r["verified"];
    requirements(&v["requirements"], out);
    out.push(String::new());
    out.push(format!(
        "- Trusted files restored: {}",
        paths(&v["trusted_restored"])
    ));
    out.push(format!(
        "- Held-out files injected: {}",
        paths(&v["held_out_injected"])
    ));
    out.push(format!("- Test inventory: {}", inventory(&v["inventory"])));
    out.push(format!("- Shaken reruns: {}", shaken(&v["shaken_reruns"])));
}

fn evidence(r: &Value, out: &mut Vec<String>) {
    let e = &r["evidence_kind"];
    if unmeasured(e) {
        out.push(why_not(e));
        return;
    }
    out.push(format!("- Catch kind: {}", text(&e["catch_kind"])));
    out.push(format!("- Structural signals: {}", paths(&e["structural"])));
    out.push(format!("- Pattern signals: {}", paths(&e["pattern"])));
    if let Some(u) = e.get("unclassified") {
        out.push(format!("- Unclassified signals: {}", paths(u)));
    }
    out.push(format!("- Isolation: {}", text(&e["isolation"])));
    if let Some(caveat) = e["caveat"].as_str() {
        out.push(String::new());
        out.push(format!("Note: {caveat}."));
    }
}

fn escalations(r: &Value, out: &mut Vec<String>) {
    let e = &r["escalations"];
    match e["state"].as_str() {
        Some("not_measured") => out.push(why_not(e)),
        Some("none") => out.push("none".into()),
        Some("open") => out.extend(
            e["open"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|x| format!("- Escalation {}: {}", text(&x["index"]), text(&x["reason"]))),
        ),
        _ => out.push(format!("{} ({})", text(&e["state"]), text(&e["detail"]))),
    }
}

fn scope(r: &Value, out: &mut Vec<String>) {
    let s = &r["scope"];
    let violations = &s["envelope_violations"];
    match violations.as_array().filter(|v| !v.is_empty()) {
        _ if unmeasured(violations) => {
            out.push(format!("Envelope violations: {}", why_not(violations)))
        }
        None => out.push("Envelope violations: none".into()),
        Some(list) => {
            out.push("Envelope violations:".into());
            out.push(String::new());
            out.push("| Path | Change | Reason |".into());
            out.push("|---|---|---|".into());
            out.extend(list.iter().map(|v| {
                row(&[
                    code(&v["path"]),
                    text(&v["change_kind"]),
                    text(&v["reason"]),
                ])
            }));
        }
    }
    out.push(String::new());
    let policy = if unmeasured(&s["runner_config_policy"]) {
        why_not(&s["runner_config_policy"])
    } else {
        text(&s["runner_config_policy"])
    };
    out.push(format!(
        "- Test-runner configuration added: {} (policy: {policy})",
        paths(&s["runner_config_added"])
    ));
    out.push(format!(
        "- Test files outside the trusted set changed: {}",
        paths(&s["untrusted_test_changes"])
    ));
}

/// A count, or "not measured" when it has none.
fn count(v: &Value) -> String {
    if v.is_number() {
        text(v)
    } else {
        "not measured".into()
    }
}

/// The state line of a review signal, with why it measured nothing.
fn signal_state(g: &Value) -> String {
    let why = ["reason", "detail"]
        .iter()
        .find_map(|k| g[*k].as_str())
        .map(|w| format!(" ({w})"))
        .or_else(|| {
            let reasons: Vec<&str> = g["reasons"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect();
            (!reasons.is_empty()).then(|| format!(" ({})", reasons.join("; ")))
        })
        .unwrap_or_default();
    format!("- State: {}{why}", text(&g["state"]))
}

fn mutation(m: &Value, out: &mut Vec<String>) {
    out.push("### Mutation".into());
    out.push(String::new());
    if unmeasured(m) {
        out.push(why_not(m));
        return;
    }
    out.push(signal_state(m));
    // no verdicts (not run, nothing to mutate): the state line says why
    if m["killed"].is_number() {
        out.push(format!(
            "- Mutants run: {} of {} candidates; killed {}, survived {}, timed out {}, errors {}",
            count(&m["run"]),
            count(&m["candidates"]),
            count(&m["killed"]),
            count(&m["survived"]),
            count(&m["timed_out"]),
            count(&m["errors"])
        ));
    }
    match m["survivors"].as_array() {
        None => out.push("- Survivors: not measured".into()),
        Some(s) if s.is_empty() => out.push("- Survivors: none".into()),
        Some(s) => {
            out.push(String::new());
            out.push("| File | Line | Function | Original | Mutant |".into());
            out.push("|---|---|---|---|---|".into());
            out.extend(s.iter().map(|x| {
                row(&[
                    code(&x["file"]),
                    text(&x["line"]),
                    code(&x["function"]),
                    code(&x["original"]),
                    code(&x["replacement"]),
                ])
            }));
        }
    }
    out.push(String::new());
    out.push(format!("Note: {}.", text(&m["note"])));
}

fn drift(g: &Value, out: &mut Vec<String>) {
    out.push("### Drift".into());
    out.push(String::new());
    if unmeasured(g) {
        out.push(why_not(g));
        return;
    }
    out.push(signal_state(g));
    let c = &g["counts"];
    out.push(format!(
        "- Functions: {} modified, {} compared on the tests' inputs, {} with drift",
        count(&c["targets"]),
        count(&c["compared"]),
        count(&c["with_drift"])
    ));
    match g["drifts"].as_array().filter(|d| !d.is_empty()) {
        None => out.push("- Drifts: none".into()),
        Some(list) => {
            out.push(String::new());
            out.push("| Function | Input | Old → new |".into());
            out.push("|---|---|---|".into());
            out.extend(list.iter().map(|x| {
                row(&[
                    format!("{} {}", code(&x["file"]), code(&x["function"])),
                    code(&x["input"]),
                    format!("{} → {}", code(&x["old"]), code(&x["new"])),
                ])
            }));
        }
    }
    out.push(String::new());
    out.push(format!("Note: {}.", text(&g["note"])));
}

fn pinning(r: &Value, out: &mut Vec<String>) {
    mutation(&r["pinning"]["mutation"], out);
    out.push(String::new());
    drift(&r["pinning"]["drift"], out);
}

/// Items of `v[key]` rendered by `one`, `none` when empty.
fn items(v: &Value, key: &str, one: fn(&Value) -> String) -> String {
    let all: Vec<String> = v[key].as_array().into_iter().flatten().map(one).collect();
    if all.is_empty() {
        "none".into()
    } else {
        all.join("; ")
    }
}

/// Why a places signal could not be read, if it could not.
fn unread(v: &Value) -> Option<String> {
    if unmeasured(v) {
        return Some(why_not(v));
    }
    (v["state"] == "unverified" || v["status"] == "unverified").then(|| {
        format!(
            "could not be read ({})",
            v["reason"].as_str().unwrap_or("no reason recorded")
        )
    })
}

fn surfaces(v: &Value) -> Vec<String> {
    if let Some(why) = unread(v) {
        return vec![format!("- Public surfaces: {why}")];
    }
    vec![
        format!(
            "- Public surfaces: {}",
            items(v, "changes", |c| format!(
                "`{}` in `{}` ({})",
                text(&c["name"]),
                text(&c["file"]),
                text(&c["kind"])
            ))
        ),
        format!(
            "- Schema boundaries: {}",
            items(v, "boundaries", |b| format!(
                "`{}` ({})",
                text(&b["file"]),
                text(&b["label"])
            ))
        ),
    ]
}

fn co_changes(v: &Value) -> String {
    let shown = unread(v).unwrap_or_else(|| {
        items(v, "missing", |m| {
            format!(
                "`{}` (changed with `{}` in {} commits)",
                text(&m["partner"]),
                text(&m["touched"]),
                text(&m["shared_commits"])
            )
        })
    });
    format!("- Usually changed together, not touched: {shown}")
}

fn importers(v: &Value) -> String {
    let shown = unread(v).unwrap_or_else(|| {
        let listed = items(v, "dependants", |d| {
            format!("`{}` imports `{}`", text(&d["file"]), text(&d["imports"]))
        });
        if v["status"] == "partial" {
            format!("{listed} (some import graphs could not be read)")
        } else {
            listed
        }
    });
    format!("- Importers of what changed: {shown}")
}

fn places(r: &Value, out: &mut Vec<String>) {
    let p = &r["places_to_look"];
    out.extend(surfaces(&p["public_surfaces"]));
    out.push(co_changes(&p["co_change_partners"]));
    out.push(importers(&p["importers"]));
}

fn not_verified(r: &Value, out: &mut Vec<String>) {
    out.push("| What | State | What it means |".into());
    out.push("|---|---|---|".into());
    for x in r["not_verified"].as_array().into_iter().flatten() {
        let meaning = match x["detail"].as_str() {
            Some(d) => format!("{} ({})", text(&x["meaning"]), text(&Value::from(d))),
            None => text(&x["meaning"]),
        };
        out.push(row(&[text(&x["what"]), text(&x["state"]), meaning]));
    }
}
