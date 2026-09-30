//! The trend summary: totals at both ends of the walk, their change, and
//! the size decomposition of the burden change. The Markdown rendering is
//! a table of the measured commits and one paragraph.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use emi_ledger::pyfmt::g;
use serde_json::{Map, Value, json};

/// How the burden change is split; exact, since the two parts sum to it.
pub const FORMULA: &str =
    "d_burden = d_sloc * first_density + last_sloc * d_density (density = burden / sloc)";

const TOTALS: [&str; 4] = ["sloc", "burden", "files", "clones"];

fn num(row: &Value, key: &str) -> f64 {
    row[key].as_f64().unwrap_or(f64::NAN)
}

/// `(size part, density part) / change`, undefined when nothing changed.
fn share(part: f64, change: f64) -> Value {
    if change == 0.0 {
        Value::Null
    } else {
        json!(part / change)
    }
}

/// Split `last.burden - first.burden` into the growth a size change at
/// the first density explains and the growth the density change at the
/// last size explains. Density needs lines at both ends.
pub fn decompose(first: &Value, last: &Value) -> Result<Value, String> {
    let (s0, b0, s1, b1) = (
        num(first, "sloc"),
        num(first, "burden"),
        num(last, "sloc"),
        num(last, "burden"),
    );
    for (end, s) in [("first", s0), ("last", s1)] {
        if s.is_nan() || s <= 0.0 {
            return Err(format!(
                "the {end} measured commit has no source lines, so its density is undefined"
            ));
        }
    }
    let (d0, d1) = (b0 / s0, b1 / s1);
    let (size, density) = ((s1 - s0) * d0, s1 * (d1 - d0));
    let change = b1 - b0;
    Ok(json!({
        "formula": FORMULA,
        "burden_change": change,
        "first_density": d0,
        "last_density": d1,
        "more_code": {"burden": size, "share": share(size, change)},
        "denser_code": {"burden": density, "share": share(density, change)},
    }))
}

fn ends(row: &Value) -> Value {
    let mut m = Map::new();
    for k in ["sha", "date"].into_iter().chain(TOTALS) {
        m.insert(k.into(), row[k].clone());
    }
    m.insert(
        "unmeasured".into(),
        json!(row["unmeasured"].as_array().map_or(0, Vec::len)),
    );
    Value::Object(m)
}

/// Warnings summed over every guard step, and how many steps the guard
/// could not check.
fn guard_totals(rows: &[Value]) -> (BTreeMap<String, u64>, usize) {
    let mut counts = BTreeMap::new();
    let mut unchecked = 0;
    for row in rows {
        match row["guard_warnings"].as_object() {
            Some(families) => {
                for (family, n) in families {
                    *counts.entry(family.clone()).or_default() += n.as_u64().unwrap_or(0);
                }
            }
            None => unchecked += 1,
        }
    }
    (counts, unchecked)
}

pub fn summary(rows: &[Value], profile: &str) -> Value {
    let measured: Vec<&Value> = rows.iter().filter(|r| r["state"] == "measured").collect();
    let (warnings, unchecked) = guard_totals(rows);
    let mut out = Map::new();
    out.insert("profile".into(), json!(profile));
    out.insert("commits".into(), json!(rows.len()));
    out.insert("measured".into(), json!(measured.len()));
    out.insert("unverified".into(), json!(rows.len() - measured.len()));
    out.insert("guard_warnings".into(), json!(warnings));
    out.insert("guard_steps_unchecked".into(), json!(unchecked));
    let (Some(first), Some(last)) = (measured.first(), measured.last()) else {
        out.insert("decomposition".into(), Value::Null);
        out.insert(
            "decomposition_unavailable".into(),
            json!("no commit could be measured"),
        );
        return Value::Object(out);
    };
    let change: Map<String, Value> = TOTALS
        .iter()
        .map(|k| {
            let d = num(last, k) - num(first, k);
            let v = if *k == "burden" {
                json!(d)
            } else {
                json!(d as i64)
            };
            ((*k).to_owned(), v)
        })
        .collect();
    out.insert("first".into(), ends(first));
    out.insert("last".into(), ends(last));
    out.insert("change".into(), Value::Object(change));
    let split = if measured.len() < 2 {
        Err("fewer than two commits were measured".to_owned())
    } else {
        decompose(first, last)
    };
    match split {
        Ok(d) => {
            out.insert("decomposition".into(), d);
        }
        Err(e) => {
            out.insert("decomposition".into(), Value::Null);
            out.insert("decomposition_unavailable".into(), json!(e));
        }
    }
    Value::Object(out)
}

fn short(v: &Value) -> String {
    v.as_str().unwrap_or("").chars().take(12).collect()
}

fn day(v: &Value) -> String {
    v.as_str().unwrap_or("").chars().take(10).collect()
}

/// Burden in six significant digits; counts as integers.
fn fmt(key: &str, x: f64) -> String {
    if key == "burden" {
        g(x)
    } else {
        format!("{x:.0}")
    }
}

fn signed(key: &str, x: f64) -> String {
    let text = fmt(key, x);
    if x >= 0.0 { format!("+{text}") } else { text }
}

fn warnings(row: &Value) -> String {
    row["guard_warnings"].as_object().map_or("?".into(), |m| {
        m.values()
            .filter_map(Value::as_u64)
            .sum::<u64>()
            .to_string()
    })
}

fn table_row(row: &Value) -> String {
    let head = format!("| {} | `{}` |", day(&row["date"]), short(&row["sha"]));
    if row["state"] != "measured" {
        let error: String = row["error"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(120)
            .collect();
        return format!(
            "{head} unverified: {} | | {} | |",
            error.replace('|', "\\|").replace('\n', " "),
            warnings(row)
        );
    }
    format!(
        "{head} {} | {} | {} | {} |",
        row["sloc"],
        g(num(row, "burden")),
        warnings(row),
        row["clones"]
    )
}

fn totals_sentence(s: &Value) -> String {
    let (first, last) = (&s["first"], &s["last"]);
    let parts: Vec<String> = [
        ("SLOC", "sloc"),
        ("burden", "burden"),
        ("files", "files"),
        ("clones", "clones"),
    ]
    .iter()
    .map(|(label, k)| {
        format!(
            "{label} {} → {} ({})",
            fmt(k, num(first, k)),
            fmt(k, num(last, k)),
            signed(k, num(&s["change"], k))
        )
    })
    .collect();
    format!(
        "From `{}` ({}) to `{}` ({}): {}.",
        short(&first["sha"]),
        day(&first["date"]),
        short(&last["sha"]),
        day(&last["date"]),
        parts.join(", ")
    )
}

fn percent(v: &Value) -> String {
    v.as_f64()
        .map_or("share undefined".into(), |s| format!("{:.0}%", 100.0 * s))
}

fn decomposition_sentence(s: &Value) -> String {
    let d = &s["decomposition"];
    if d.is_null() {
        return format!(
            "The burden change cannot be split by size: {}.",
            s["decomposition_unavailable"]
                .as_str()
                .unwrap_or("unavailable")
        );
    }
    format!(
        "Split by size (Δburden = ΔSLOC × first density + last SLOC × Δdensity): \
         growth explained by more code {} ({}), by denser code {} ({}); \
         density went from {} to {} burden per line.",
        signed("burden", num(&d["more_code"], "burden")),
        percent(&d["more_code"]["share"]),
        signed("burden", num(&d["denser_code"], "burden")),
        percent(&d["denser_code"]["share"]),
        g(num(d, "first_density")),
        g(num(d, "last_density"))
    )
}

fn guard_sentence(s: &Value) -> String {
    let counts = s["guard_warnings"].as_object().cloned().unwrap_or_default();
    let total: u64 = counts.values().filter_map(Value::as_u64).sum();
    let mut out = format!("Guard warnings along the way: {total}");
    if total > 0 {
        let by: Vec<String> = counts.iter().map(|(k, v)| format!("{k} {v}")).collect();
        let _ = write!(out, " ({})", by.join(", "));
    }
    let unchecked = s["guard_steps_unchecked"].as_u64().unwrap_or(0);
    if unchecked > 0 {
        let _ = write!(out, "; {unchecked} step(s) could not be checked");
    }
    out + "."
}

pub fn markdown(rows: &[Value], s: &Value, source: &str) -> String {
    let scope = if source.is_empty() { "." } else { source };
    let mut out = format!(
        "## Codebase trend: `{scope}`\n\n{} commits, {} measured, {} unverified (profile `{}`). \
         Burden counts measured mass only; each row's unresolved evidence is listed in the JSONL output.\n\n\
         | Date | Commit | SLOC | Burden | Guard warnings | Clones |\n|---|---|--:|--:|--:|--:|\n",
        s["commits"],
        s["measured"],
        s["unverified"],
        s["profile"].as_str().unwrap_or("")
    );
    for row in rows {
        out += &table_row(row);
        out.push('\n');
    }
    out.push('\n');
    if s["first"].is_object() {
        out += &totals_sentence(s);
        out.push(' ');
    }
    out += &decomposition_sentence(s);
    out.push(' ');
    out += &guard_sentence(s);
    out += "\n\nA description of the history, not a quality verdict.\n";
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(sloc: u64, burden: f64) -> Value {
        json!({"sha": "a", "date": "2026-01-01", "state": "measured", "sloc": sloc,
               "burden": burden, "files": 1, "clones": 0, "guard_warnings": {}, "unmeasured": []})
    }

    #[test]
    fn parts_sum_to_the_change() {
        let d = decompose(&row(100, 20.0), &row(150, 45.0)).unwrap_or_default();
        let (size, density) = (
            num(&d["more_code"], "burden"),
            num(&d["denser_code"], "burden"),
        );
        assert!((size + density - 25.0).abs() < 1e-9);
        assert!((size - 10.0).abs() < 1e-9, "50 lines at 0.2");
        assert!((density - 15.0).abs() < 1e-9, "150 lines, 0.2 -> 0.3");
    }

    #[test]
    fn empty_ends_have_no_density() {
        assert!(decompose(&row(0, 0.0), &row(10, 1.0)).is_err());
        let s = summary(&[row(0, 0.0), row(10, 1.0)], "parity");
        assert!(s["decomposition"].is_null());
        assert_eq!(s["change"]["sloc"], json!(10));
    }

    #[test]
    fn no_change_has_undefined_shares() {
        let d = decompose(&row(10, 2.0), &row(10, 2.0)).unwrap_or_default();
        assert!(d["more_code"]["share"].is_null());
    }

    #[test]
    fn unchecked_guard_steps_are_counted() {
        let mut r = row(10, 1.0);
        r["guard_warnings"] = Value::Null;
        let mut w = row(10, 1.0);
        w["guard_warnings"] = json!({"tests": 2});
        let s = summary(&[r, w], "parity");
        assert_eq!(s["guard_steps_unchecked"], json!(1));
        assert_eq!(s["guard_warnings"], json!({"tests": 2}));
    }
}
