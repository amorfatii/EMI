//! Shared record contracts (port of `emi_contracts.py`): the cross-field
//! semantics JSON Schema cannot express. Every public gate boundary runs
//! these first, so a contradictory change or report fails closed with the
//! oracle's own message instead of passing.

use std::collections::{BTreeSet, HashMap};

use emi_ir::pytext::strip;
use serde_json::{Map, Value};

use crate::pyfmt::repr;

pub const TOLERANCE: f64 = 1e-9;
pub const PROFILE: &str = "emi-0.3";

pub const MATCHED_CLASSIFICATIONS: [&str; 5] = [
    "improvement",
    "regression",
    "unchanged",
    "uncertain",
    "unavailable",
];
pub const BURDEN_SUMMARIES: [&str; 6] = [
    "increased",
    "reduced",
    "unchanged",
    "uncertain",
    "mixed",
    "unavailable",
];
pub const BURDEN_CLASSIFICATIONS: [&str; 5] = [
    "increased",
    "reduced",
    "unchanged",
    "uncertain",
    "unavailable",
];

pub type Result<T> = std::result::Result<T, String>;

/// A finite JSON number (JSON has no booleans-as-numbers or NaN).
pub fn num(v: &Value) -> Option<f64> {
    v.as_f64()
}

/// Python `isinstance(v, int) and not isinstance(v, bool)`.
pub fn is_int(v: &Value) -> bool {
    v.is_i64() || v.is_u64()
}

/// Python `repr(str)`.
pub fn str_repr(s: &str) -> String {
    let quote = if s.contains('\'') && !s.contains('"') {
        '"'
    } else {
        '\''
    };
    let mut out = String::from(quote);
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\x{:02x}", c as u32))
            }
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Python `==` on JSON-decoded values: `1 == 1.0 == True`.
pub fn py_eq(a: &Value, b: &Value) -> bool {
    let scalar = |v: &Value| match v {
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        Value::Number(n) => n.as_f64(),
        _ => None,
    };
    match (a, b) {
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| py_eq(p, q))
        }
        (Value::Object(x), Value::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| py_eq(v, w)))
        }
        _ => match (scalar(a), scalar(b)) {
            (Some(p), Some(q)) => p == q,
            _ => a == b,
        },
    }
}

pub fn keys_are(obj: &Map<String, Value>, keys: &[&str]) -> bool {
    obj.len() == keys.len() && keys.iter().all(|k| obj.contains_key(*k))
}

pub fn direction(
    interval: Option<[f64; 2]>,
    increased: &'static str,
    decreased: &'static str,
) -> &'static str {
    let Some([lo, hi]) = interval else {
        return "unavailable";
    };
    if lo > TOLERANCE {
        increased
    } else if hi < -TOLERANCE {
        decreased
    } else if lo.abs() <= TOLERANCE && hi.abs() <= TOLERANCE {
        "unchanged"
    } else {
        "uncertain"
    }
}

pub fn summarize<'a>(states: impl IntoIterator<Item = &'a str>) -> &'static str {
    let states: BTreeSet<&str> = states.into_iter().collect();
    let has = |s: &str| states.contains(s);
    if has("unavailable") {
        "unavailable"
    } else if has("increased") && has("reduced") {
        "mixed"
    } else if has("uncertain") {
        "uncertain"
    } else if has("increased") {
        "increased"
    } else if has("reduced") {
        "reduced"
    } else {
        "unchanged"
    }
}

pub fn check_interval(value: &Value, label: &str) -> Result<[f64; 2]> {
    let Some([lo, hi]) = value
        .as_array()
        .and_then(|a| <&[Value; 2]>::try_from(a.as_slice()).ok())
    else {
        return Err(format!("{label} must be [low, high]"));
    };
    let (Some(lo), Some(hi)) = (num(lo), num(hi)) else {
        return Err(format!("{label} bounds must be finite numbers"));
    };
    if lo - hi > TOLERANCE {
        return Err(format!("{label} requires low <= high"));
    }
    Ok([lo, hi])
}

pub fn check_nullable_interval(value: Option<&Value>, label: &str) -> Result<Option<[f64; 2]>> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(v) => check_interval(v, label).map(Some),
    }
}

type Bounds = [f64; 2];

fn validate_mass(
    value: Option<&Value>,
    sloc: f64,
    label: &str,
    integer: bool,
) -> Result<(Bounds, f64)> {
    let value = value.unwrap_or(&Value::Null);
    if value.is_null() {
        return Ok(([0.0, sloc], 0.0));
    }
    if let Some(obj) = value.as_object() {
        if !keys_are(obj, &["known_mass", "unknown_sloc"]) {
            return Err(format!(
                "{label}: partial evidence requires known_mass and unknown_sloc"
            ));
        }
        let unk = &obj["unknown_sloc"];
        let unk = match unk.as_f64() {
            Some(u) if is_int(unk) && u >= 0.0 && u <= sloc => u,
            _ => return Err(format!("{label}.unknown_sloc must be integer in [0, sloc]")),
        };
        let known = match num(&obj["known_mass"]) {
            Some(k) if k >= 0.0 && k - (sloc - unk) <= TOLERANCE => k,
            _ => return Err(format!("{label}.known_mass out of range")),
        };
        if integer && known.fract() != 0.0 {
            return Err(format!("{label}.known_mass must be integer for V"));
        }
        return Ok(([known, known + unk], sloc - unk));
    }
    let Some(v) = num(value) else {
        return Err(format!(
            "{label} must be numeric, null, or partial evidence"
        ));
    };
    if v < 0.0 || v - sloc > TOLERANCE {
        return Err(format!("{label} out of range [0, sloc]"));
    }
    if integer && v.fract() != 0.0 {
        return Err(format!("{label} must be integer for V"));
    }
    Ok(([v, v], sloc))
}

/// Validated shape of one component, as far as later cross-checks need it.
pub struct Checked {
    pub sloc: f64,
    pub bounds: [Bounds; 3],
}

pub const DIMS: [&str; 3] = ["E", "V", "C"];

fn int_at_least_one(v: Option<&Value>) -> Option<f64> {
    let v = v?;
    let n = v.as_f64()?;
    (is_int(v) && n >= 1.0).then_some(n)
}

struct RegionCheck {
    sloc: f64,
    bounds: [Bounds; 2],
}

fn validate_region(
    ident: &str,
    reg: &Value,
    sloc: f64,
    seen: &HashMap<String, RegionCheck>,
) -> Result<(String, RegionCheck)> {
    let Some(reg) = reg.as_object() else {
        return Err(format!("{ident}.regions entries must be objects"));
    };
    let rid = match reg.get("id").and_then(Value::as_str) {
        Some(r) if !strip(r).is_empty() && !seen.contains_key(r) => r.to_owned(),
        _ => {
            return Err(format!(
                "{ident}.regions ids must be unique nonempty strings"
            ));
        }
    };
    let Some(rsloc) = int_at_least_one(reg.get("sloc")) else {
        return Err(format!("{ident}.{rid}.sloc must be integer >= 1"));
    };
    if rsloc > sloc {
        return Err(format!("{ident}.{rid}.sloc exceeds component sloc"));
    }
    if !reg
        .get("sha")
        .and_then(Value::as_str)
        .is_some_and(|s| !s.is_empty())
    {
        return Err(format!("{ident}.{rid}.sha must be nonempty string"));
    }
    if !reg.get("in_cycle").is_some_and(Value::is_boolean) {
        return Err(format!("{ident}.{rid}.in_cycle must be boolean"));
    }
    let mut bounds = [[0.0; 2]; 2];
    for (slot, dim) in bounds.iter_mut().zip(["E", "C"]) {
        if !reg.contains_key(dim) {
            return Err(format!("{ident}.{rid} requires E and C evidence"));
        }
        *slot = validate_mass(reg.get(dim), rsloc, &format!("{ident}.{rid}.{dim}"), false)?.0;
    }
    Ok((
        rid,
        RegionCheck {
            sloc: rsloc,
            bounds,
        },
    ))
}

fn check_region_totals(
    ident: &str,
    regions: &HashMap<String, RegionCheck>,
    sloc: f64,
    bounds: &[Bounds; 3],
) -> Result<()> {
    let total: f64 = regions.values().map(|r| r.sloc).sum();
    if total - sloc > TOLERANCE {
        return Err(format!("{ident}.regions sloc sums above component sloc"));
    }
    for (i, (dim, file)) in [("E", bounds[0]), ("C", bounds[2])].into_iter().enumerate() {
        let sum_known = crate::pyfmt::fsum(regions.values().map(|r| r.bounds[i][0]));
        let sum_width =
            crate::pyfmt::fsum(regions.values().map(|r| r.bounds[i][1] - r.bounds[i][0]));
        let (f_known, f_width) = (file[0], file[1] - file[0]);
        if sum_known - f_known > TOLERANCE {
            return Err(format!(
                "{ident}.regions {dim} known mass exceeds component"
            ));
        }
        if sum_width - f_width > TOLERANCE {
            return Err(format!("{ident}.regions {dim} unknown exceeds component"));
        }
        let rem_sloc = sloc - total;
        let (rem_known, rem_width) = (f_known - sum_known, f_width - sum_width);
        if rem_known - rem_sloc > TOLERANCE {
            return Err(format!(
                "{ident}.regions {dim} residual known exceeds remaining sloc"
            ));
        }
        if rem_width - rem_sloc > TOLERANCE {
            return Err(format!(
                "{ident}.regions {dim} residual unknown exceeds remaining sloc"
            ));
        }
        if total == sloc && (rem_known.abs() > TOLERANCE || rem_width.abs() > TOLERANCE) {
            return Err(format!(
                "{ident}.regions {dim} full coverage totals mismatch"
            ));
        }
    }
    Ok(())
}

fn validate_component(
    item: &Value,
    revision: &str,
    result: &HashMap<String, Checked>,
) -> Result<(String, Checked)> {
    let Some(item) = item.as_object() else {
        return Err("component must be an object".into());
    };
    let ident = match item.get("id").and_then(Value::as_str) {
        Some(i) if !strip(i).is_empty() && !result.contains_key(i) => i.to_owned(),
        _ => return Err(format!("{revision} IDs must be unique nonempty strings")),
    };
    let Some(sloc) = int_at_least_one(item.get("sloc")) else {
        return Err(format!("{ident}.sloc must be integer >= 1"));
    };
    if sloc > 9_007_199_254_740_992.0 {
        return Err(format!("{ident}.sloc exceeds safe integer range"));
    }
    let burden = match item.get("burden").and_then(Value::as_object) {
        Some(b) if keys_are(b, &DIMS) => b,
        _ => return Err(format!("{ident}.burden requires exactly E,V,C")),
    };
    let mut bounds = [[0.0; 2]; 3];
    for (slot, dim) in bounds.iter_mut().zip(DIMS) {
        *slot = validate_mass(burden.get(dim), sloc, &format!("{ident}.{dim}"), dim == "V")?.0;
    }
    match item.get("regions") {
        None | Some(Value::Null) => {}
        Some(Value::Array(regions)) => {
            let mut seen = HashMap::new();
            for reg in regions {
                let (rid, check) = validate_region(&ident, reg, sloc, &seen)?;
                seen.insert(rid, check);
            }
            if !seen.is_empty() {
                check_region_totals(&ident, &seen, sloc, &bounds)?;
            }
        }
        Some(_) => return Err(format!("{ident}.regions must be an array")),
    }
    Ok((ident, Checked { sloc, bounds }))
}

/// `emi_contracts.validate_components`.
pub fn validate_components(
    items: Option<&Value>,
    revision: &str,
) -> Result<HashMap<String, Checked>> {
    let Some(items) = items.and_then(Value::as_array) else {
        return Err(format!("{revision} must be an array"));
    };
    let mut result = HashMap::new();
    for item in items {
        let (ident, checked) = validate_component(item, revision, &result)?;
        result.insert(ident, checked);
    }
    Ok(result)
}

pub fn validate_collection_errors(value: &Value) -> Result<()> {
    let obj = match value.as_object() {
        Some(o) if keys_are(o, &["before", "after"]) => o,
        _ => return Err("collection_errors requires before/after arrays".into()),
    };
    for side in ["before", "after"] {
        let Some(items) = obj[side].as_array() else {
            return Err(format!("collection_errors.{side} must be an array"));
        };
        if items
            .iter()
            .any(|e| !e.get("file").is_some_and(Value::is_string))
        {
            return Err(format!("collection_errors.{side} entries need file string"));
        }
    }
    Ok(())
}

fn nonempty_list(value: &Value) -> bool {
    value.as_array().is_some_and(|a| !a.is_empty())
}

fn check_options(data: &Map<String, Value>) -> Result<()> {
    let get = |k: &str| data.get(k).filter(|v| !v.is_null());
    let (before, after) = (
        get("measurement_options_before"),
        get("measurement_options_after"),
    );
    for (label, opts) in [("before", before), ("after", after)] {
        if opts.is_some_and(|o| !o.is_object()) {
            return Err(format!(
                "measurement_options_{label} must be object when present"
            ));
        }
    }
    match (before, after) {
        (Some(b), Some(a)) if !py_eq(b, a) => {
            Err("measurement_options differ; deltas incomparable".into())
        }
        (Some(_), None) | (None, Some(_)) => {
            Err("measurement_options must both be present or both absent (legacy)".into())
        }
        _ => Ok(()),
    }
}

fn check_scope(
    scope: &Map<String, Value>,
    before: &HashMap<String, Checked>,
    after: &HashMap<String, Checked>,
) -> Result<()> {
    let union: BTreeSet<&str> = before
        .keys()
        .chain(after.keys())
        .map(String::as_str)
        .collect();
    let expected_ok = scope
        .get("expected_components")
        .and_then(Value::as_array)
        .is_some_and(|list| {
            let names: Option<Vec<&str>> = list.iter().map(Value::as_str).collect();
            names.is_some_and(|n| {
                let set: BTreeSet<&str> = n.iter().copied().collect();
                set.len() == n.len() && set == union
            })
        });
    if !expected_ok {
        return Err("scope manifest must match union of observed IDs".into());
    }
    let empty = Value::Array(Vec::new());
    let unchanged = scope.get("unchanged_components").unwrap_or(&empty);
    let names: Option<Vec<&str>> = unchanged
        .as_array()
        .and_then(|l| l.iter().map(Value::as_str).collect());
    let valid = names.as_ref().is_some_and(|n| {
        let set: BTreeSet<&str> = n.iter().copied().collect();
        set.len() == n.len()
            && set
                .iter()
                .all(|k| before.contains_key(*k) && after.contains_key(*k))
    });
    let (true, Some(names)) = (valid, names) else {
        return Err("unchanged_components must list unique IDs present on both sides".into());
    };
    for key in names {
        let (b, a) = (&before[key], &after[key]);
        if b.sloc != a.sloc || b.bounds != a.bounds {
            return Err(format!("unchanged {key} measurements differ"));
        }
    }
    Ok(())
}

/// `emi_contracts.validate_change`.
pub fn validate_change(data: &Value) -> Result<()> {
    let data = match data.as_object() {
        Some(d) if d.get("profile").and_then(Value::as_str) == Some(PROFILE) => d,
        _ => return Err(format!("profile must be {PROFILE}")),
    };
    for key in ["measurement_profile_before", "measurement_profile_after"] {
        if !data
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|s| !strip(s).is_empty())
        {
            return Err(format!("{key} must be nonempty string"));
        }
    }
    if data["measurement_profile_before"] != data["measurement_profile_after"] {
        return Err("measurement profiles differ; deltas incomparable".into());
    }
    check_options(data)?;
    if !data.get("identity_verified").is_some_and(Value::is_boolean) {
        return Err("identity_verified must be boolean".into());
    }
    let before = validate_components(data.get("before"), "before")?;
    let after = validate_components(data.get("after"), "after")?;
    if before.is_empty() && after.is_empty() {
        return Err("empty review".into());
    }
    let scope = match data.get("scope").and_then(Value::as_object) {
        Some(s) if s.get("complete").is_some_and(Value::is_boolean) => s,
        _ => return Err("scope requires complete boolean and expected_components".into()),
    };
    check_scope(scope, &before, &after)?;
    if let Some(ce) = data.get("collection_errors").filter(|v| !v.is_null()) {
        validate_collection_errors(ce)?;
        if scope["complete"] == Value::Bool(true)
            && (nonempty_list(&ce["before"]) || nonempty_list(&ce["after"]))
        {
            return Err("scope.complete cannot be true with nonempty collection_errors".into());
        }
    }
    Ok(())
}

fn check_burden_axis(dim: &str, entry: &Value, summary: &str) -> Result<()> {
    let Some(entry) = entry.as_object().filter(|e| e.contains_key("delta")) else {
        return Err(format!("burden_change.{dim} requires delta"));
    };
    let cls = entry
        .get("classification")
        .and_then(Value::as_str)
        .unwrap_or("");
    if !BURDEN_CLASSIFICATIONS.contains(&cls)
        || !entry.get("classification").is_some_and(Value::is_string)
    {
        return Err(format!("burden_change.{dim}.classification invalid"));
    }
    let delta = &entry["delta"];
    if delta.is_null() {
        if cls != "unavailable" || summary != "unavailable" {
            return Err(format!(
                "burden_change.{dim} null delta requires unavailable classifications"
            ));
        }
        return Ok(());
    }
    let [lo, hi] = check_interval(delta, &format!("burden_change.{dim}.delta"))?;
    for bound in ["before", "after"] {
        if let Some(v) = entry.get(bound).filter(|v| !v.is_null()) {
            check_interval(v, &format!("burden_change.{dim}.{bound}"))?;
        }
    }
    let expect = direction(Some([lo, hi]), "increased", "reduced");
    if cls != expect {
        return Err(format!(
            "burden_change.{dim} classification {} contradicts interval [{}, {}] (expect {})",
            str_repr(cls),
            repr(lo),
            repr(hi),
            str_repr(expect)
        ));
    }
    Ok(())
}

fn class_in<'a>(v: Option<&'a Value>, allowed: &[&str]) -> Option<&'a str> {
    v.and_then(Value::as_str).filter(|s| allowed.contains(s))
}

fn check_growth(growth: &Map<String, Value>) -> Result<()> {
    let mut vals = [0.0; 3];
    for (slot, key) in vals
        .iter_mut()
        .zip(["before_sloc", "after_sloc", "net_sloc"])
    {
        *slot = growth
            .get(key)
            .and_then(num)
            .ok_or_else(|| format!("growth.{key} must be finite number"))?;
    }
    if ((vals[1] - vals[0]) - vals[2]).abs() > 1e-6 {
        return Err("growth.net_sloc contradicts before/after totals".into());
    }
    Ok(())
}

fn check_matched(matched: &Map<String, Value>) -> Result<&str> {
    let interval = check_nullable_interval(matched.get("interval"), "matched.interval")?;
    let mcls = matched
        .get("classification")
        .and_then(Value::as_str)
        .unwrap_or("");
    match interval {
        None if mcls != "unavailable" => {
            return Err("matched null interval requires unavailable classification".into());
        }
        Some(i) => {
            let expect = direction(Some(i), "improvement", "regression");
            if mcls != expect {
                return Err(format!(
                    "matched classification {} contradicts interval (expect {})",
                    str_repr(mcls),
                    str_repr(expect)
                ));
            }
        }
        None => {}
    }
    if !matched
        .get("possible_regression")
        .is_some_and(Value::is_boolean)
    {
        return Err("matched.possible_regression must be boolean".into());
    }
    let Some(available) = matched.get("comparison_available").and_then(Value::as_bool) else {
        return Err("matched.comparison_available must be boolean".into());
    };
    if available && interval.is_none() {
        return Err("matched comparison_available true requires interval".into());
    }
    if !available && mcls != "unavailable" {
        return Err("matched unavailable comparison requires unavailable classification".into());
    }
    Ok(mcls)
}

fn check_components(report: &Map<String, Value>) -> Result<()> {
    let Some(components) = report.get("components").and_then(Value::as_array) else {
        return Err("components must be an array".into());
    };
    let mut by_id: HashMap<&str, &Value> = HashMap::new();
    for comp in components {
        let Some(id) = comp
            .get("id")
            .and_then(Value::as_str)
            .filter(|_| comp.is_object())
        else {
            return Err("component entries require id".into());
        };
        by_id.insert(id, comp);
        let null = Value::Null;
        let dc = comp
            .get("density_change")
            .filter(|v| truthy(v))
            .unwrap_or(&null);
        let cls = class_in(dc.get("classification"), &MATCHED_CLASSIFICATIONS)
            .ok_or_else(|| format!("component {id} density classification invalid"))?;
        match check_nullable_interval(dc.get("interval"), &format!("component {id} interval"))? {
            None if cls != "unavailable" => {
                return Err(format!("component {id} null interval requires unavailable"));
            }
            Some(i) if cls != direction(Some(i), "improvement", "regression") => {
                return Err(format!(
                    "component {id} density classification contradicts interval"
                ));
            }
            _ => {}
        }
    }
    let ids = |key: &str| -> Vec<String> {
        report
            .get(key)
            .and_then(Value::as_array)
            .map_or_else(Vec::new, |l| {
                l.iter()
                    .map(|v| v.as_str().map_or_else(|| v.to_string(), str::to_owned))
                    .collect()
            })
    };
    for rid in ids("regressions") {
        let comp = by_id
            .get(rid.as_str())
            .ok_or_else(|| format!("regressions references unknown {}", str_repr(&rid)))?;
        if comp
            .pointer("/density_change/classification")
            .and_then(Value::as_str)
            != Some("regression")
        {
            return Err(format!(
                "regressions {} is not a density regression",
                str_repr(&rid)
            ));
        }
    }
    for rid in ids("possible_regressions") {
        let comp = by_id
            .get(rid.as_str())
            .ok_or_else(|| format!("possible_regressions references unknown {}", str_repr(&rid)))?;
        if !comp
            .pointer("/density_change/possible_regression")
            .is_some_and(truthy)
        {
            return Err(format!(
                "possible_regressions {} lacks possible_regression",
                str_repr(&rid)
            ));
        }
    }
    Ok(())
}

/// Python truthiness of a JSON value.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `emi_contracts.validate_report_semantic`.
pub fn validate_report_semantic(report: &Value) -> Result<()> {
    let Some(report) = report.as_object() else {
        return Err(
            "guardrail needs a review report object (full review JSON, not a change input)".into(),
        );
    };
    if report.get("profile").and_then(Value::as_str) != Some(PROFILE) {
        return Err(format!(
            "guardrail needs a review report object with profile {PROFILE} (full review JSON, not a change input)"
        ));
    }
    let matched = report.get("matched").and_then(Value::as_object);
    let burden = report.get("burden_change").and_then(Value::as_object);
    let summary = class_in(report.get("burden_summary"), &BURDEN_SUMMARIES);
    let growth = report.get("growth").and_then(Value::as_object);
    let (Some(matched), Some(burden), Some(summary), Some(growth)) =
        (matched, burden, summary, growth)
    else {
        return Err("report requires matched/burden_change[E,V,C]/burden_summary/growth".into());
    };
    if class_in(matched.get("classification"), &MATCHED_CLASSIFICATIONS).is_none()
        || !keys_are(burden, &DIMS)
    {
        return Err("report requires matched/burden_change[E,V,C]/burden_summary/growth".into());
    }
    let empty = Value::Object(Map::new());
    for dim in DIMS {
        let entry = Some(&burden[dim]).filter(|v| truthy(v)).unwrap_or(&empty);
        check_burden_axis(dim, entry, summary)?;
    }
    let states: BTreeSet<&str> = DIMS
        .iter()
        .filter_map(|d| burden[*d]["classification"].as_str())
        .collect();
    if summarize(states.iter().copied()) != summary {
        let listed: Vec<String> = states.iter().map(|s| str_repr(s)).collect();
        return Err(format!(
            "burden_summary {} contradicts axes [{}]",
            str_repr(summary),
            listed.join(", ")
        ));
    }
    check_growth(growth)?;
    let mcls = check_matched(matched)?;
    let Some(doi) = report
        .get("density_only_improvement")
        .and_then(Value::as_bool)
    else {
        return Err("density_only_improvement must be boolean".into());
    };
    if doi && !(mcls == "improvement" && summary == "unchanged") {
        return Err(
            "density_only_improvement true requires matched improvement + unchanged burden".into(),
        );
    }
    check_components(report)?;
    if let Some(ce) = report.get("collection_errors").filter(|v| !v.is_null()) {
        validate_collection_errors(ce)?;
        let complete =
            report.get("scope").and_then(|s| s.get("complete")) == Some(&Value::Bool(true));
        if complete && (nonempty_list(&ce["before"]) || nonempty_list(&ce["after"])) {
            return Err("scope.complete true contradicts nonempty collection_errors".into());
        }
    }
    let ws = report.get("weight_sensitivity");
    if let Some(avail) = ws.and_then(|w| w.get("available")) {
        if !avail.is_boolean() {
            return Err("weight_sensitivity.available must be boolean".into());
        }
        if *avail == Value::Bool(false)
            && ws
                .and_then(|w| w.get("classification"))
                .and_then(Value::as_str)
                != Some("unavailable")
        {
            return Err(
                "weight_sensitivity unavailable requires unavailable classification".into(),
            );
        }
    }
    Ok(())
}
