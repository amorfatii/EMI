//! `emi.review`: a validated change in, the full review report out.

use std::collections::{BTreeSet, HashMap};

use serde_json::{Map, Value, json};

use crate::calc::{
    self, Components, RegionData, WEIGHTS, difference, hotspots, ledger, paired, score,
    sensitivity, summarize_burden,
};
use crate::contracts::{self, PROFILE, Result, TOLERANCE, py_eq, truthy};
use crate::pyfmt::fsum;

const LIMITATIONS: [&str; 9] = [
    "The calculator consumes supplied measurements; source scanning and scope verification belong to its collector.",
    "Cohesion is diagnostic, not included in the core score or core coverage.",
    "Intervals bound missing measurements; they are not statistical confidence intervals.",
    "Hotspot diagnostics depend on component grouping and do not change the structural index.",
    "Hotspot 'risk' labels denote measured burden concentration, not validated risk.",
    "Weight sensitivity tests do not establish predictive accuracy.",
    "Density gains do not prove burden removal or easier future changes.",
    "Construct validity is unproven: long-but-straightforward functions, navigation cost of many small ones, intentional duplication, and stable-interface coupling are known threats the penalties do not model.",
    "Overlapping snapshot intervals decide nothing; review decisions use delta intervals and the burden ledger, and uncertain deltas mean human review.",
];

const PRESERVED: [&str; 10] = [
    "source_sha256_before",
    "source_sha256_after",
    "before_sha",
    "after_sha",
    "before_ref",
    "after_ref",
    "measurement_contract_id",
    "snapshot_id_before",
    "snapshot_id_after",
    "region_diagnostics",
];

struct Scope<'a> {
    raw: &'a Map<String, Value>,
    complete: bool,
    unchanged: BTreeSet<String>,
}

fn check_identities(data: &Map<String, Value>) -> Result<(&str, Option<&Value>, bool)> {
    let old = data
        .get("measurement_profile_before")
        .and_then(Value::as_str);
    let new = data
        .get("measurement_profile_after")
        .and_then(Value::as_str);
    let profile = match (old, new) {
        (Some(o), Some(n)) if !emi_ir::pytext::strip(o).is_empty() && o == n => o,
        _ => {
            return Err(
                "matching nonempty before/after measurement identities are required".into(),
            );
        }
    };
    let get = |k: &str| data.get(k).filter(|v| !v.is_null());
    let (bo, ao) = (
        get("measurement_options_before"),
        get("measurement_options_after"),
    );
    for (label, o) in [("before", bo), ("after", ao)] {
        if o.is_some_and(|o| !o.is_object()) {
            return Err(format!(
                "measurement_options_{label} must be an object when present"
            ));
        }
    }
    match (bo, ao) {
        (Some(_), None) | (None, Some(_)) => {
            return Err(
                "measurement_options_before/after must both be present or both be absent (legacy)"
                    .into(),
            );
        }
        (Some(b), Some(a)) if !py_eq(b, a) => {
            return Err(
                "measurement_options_before/after differ; deltas across scope options are not comparable".into()
            );
        }
        _ => {}
    }
    let identity = match data.get("identity_verified") {
        None => false,
        Some(Value::Bool(b)) => *b,
        Some(_) => return Err("identity_verified must be boolean".into()),
    };
    Ok((profile, bo, identity))
}

fn check_scope<'a>(
    data: &'a Map<String, Value>,
    before: &Components,
    after: &Components,
) -> Result<Scope<'a>> {
    let raw = match data.get("scope").and_then(Value::as_object) {
        Some(s) if s.get("complete").is_some_and(Value::is_boolean) => s,
        _ => {
            return Err(
                "scope requires a complete boolean and expected_components manifest".into(),
            );
        }
    };
    let union: BTreeSet<&str> = before
        .keys()
        .chain(after.keys())
        .map(String::as_str)
        .collect();
    let expected: Option<Vec<&str>> = raw
        .get("expected_components")
        .and_then(Value::as_array)
        .and_then(|l| l.iter().map(Value::as_str).collect());
    let ok = expected.is_some_and(|e| {
        let set: BTreeSet<&str> = e.iter().copied().collect();
        e.iter().all(|k| !emi_ir::pytext::strip(k).is_empty())
            && set.len() == e.len()
            && set == union
    });
    if !ok {
        return Err("scope manifest must match the union of observed component IDs".into());
    }
    let listed: Option<Vec<&str>> = match raw.get("unchanged_components") {
        None => Some(Vec::new()),
        Some(v) => v
            .as_array()
            .and_then(|l| l.iter().map(Value::as_str).collect()),
    };
    let unchanged: BTreeSet<String> = listed.iter().flatten().map(|s| (*s).to_owned()).collect();
    let valid = listed.as_ref().is_some_and(|l| {
        l.len() == unchanged.len()
            && unchanged
                .iter()
                .all(|k| before.contains_key(k) && after.contains_key(k))
    });
    if !valid {
        return Err("scope unchanged_components must list unique IDs present on both sides".into());
    }
    for key in listed.iter().flatten() {
        let (b, a) = (&before[*key], &after[*key]);
        if b.sloc != a.sloc || b.bounds != a.bounds {
            return Err(format!(
                "scope unchanged_components: {key} measurements differ before/after"
            ));
        }
    }
    let complete = raw["complete"] == Value::Bool(true);
    Ok(Scope {
        raw,
        complete,
        unchanged,
    })
}

fn history_days(data: &Map<String, Value>) -> Result<Option<f64>> {
    match data.get("history_days") {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let Some(d) = v.as_f64() else {
                return Err("history_days must be numeric".into());
            };
            if !(0.0..=90.0).contains(&d) || d.fract() != 0.0 {
                return Err("history_days is out of range or has an invalid unit".into());
            }
            Ok(Some(d))
        }
    }
}

fn sloc_weights(
    comps: &Components,
    multipliers: Option<&HashMap<String, f64>>,
) -> HashMap<String, f64> {
    comps
        .iter()
        .map(|(k, c)| {
            (
                k.clone(),
                c.sloc * multipliers.and_then(|m| m.get(k)).copied().unwrap_or(1.0),
            )
        })
        .collect()
}

fn exposure(ctx: &Ctx, days: Option<f64>) -> Value {
    let history_ok = days == Some(90.0) && ctx.before.values().all(|c| c.churn.is_some());
    if !history_ok {
        return json!({"available": false, "reason": "requires complete 90-day history for established components",
                      "after": null, "matched": null});
    }
    let multipliers: HashMap<String, f64> = ctx
        .before
        .iter()
        .map(|(k, c)| {
            (
                k.clone(),
                1.0 + f64::min(1.0, c.churn.unwrap_or(0.0) / (2.0 * c.sloc)),
            )
        })
        .collect();
    let current = sloc_weights(ctx.after, Some(&multipliers));
    let frozen = sloc_weights(ctx.before, Some(&multipliers));
    json!({"available": true, "reason": null,
           "after": score(ctx.after, Some(&current), WEIGHTS),
           "matched": paired(ctx.before, ctx.after, &frozen, ctx.allowed, ctx.reason, &ctx.unchanged)})
}

struct Ctx<'a> {
    before: &'a Components,
    after: &'a Components,
    unchanged: BTreeSet<String>,
    regions: Option<RegionData<'a>>,
    allowed: bool,
    reason: Option<&'static str>,
}

fn one(comps: &Components, key: &str) -> Components {
    comps
        .get(key)
        .map(|c| (key.to_owned(), c.clone()))
        .into_iter()
        .collect()
}

fn component_result(ctx: &Ctx, key: &str) -> Value {
    let (old_items, new_items) = (one(ctx.before, key), one(ctx.after, key));
    let (old, new) = (
        score(&old_items, None, WEIGHTS),
        score(&new_items, None, WEIGHTS),
    );
    let is_unchanged = ctx.unchanged.contains(key);
    let only: BTreeSet<String> = if is_unchanged {
        [key.to_owned()].into()
    } else {
        BTreeSet::new()
    };
    let changes = ledger(&old_items, &new_items, &only, ctx.regions.as_ref());
    let density = match (old["score"].as_f64(), new["score"].as_f64()) {
        (Some(x), Some(y)) if is_unchanged && ctx.allowed => {
            json!({"delta": y - x, "interval": [0.0, 0.0],
            "classification": "unchanged", "possible_regression": false})
        }
        _ => difference(&old, &new, ctx.allowed),
    };
    let kind = match (ctx.allowed, old_items.is_empty(), new_items.is_empty()) {
        (false, _, _) => "unverified",
        (true, false, false) => "surviving",
        (true, _, false) => "new",
        _ => "removed",
    };
    let cohesion = |c: &Components| c.get(key).and_then(|c| c.cohesion);
    let summary = if ctx.allowed {
        summarize_burden(&changes)
    } else {
        "unavailable"
    };
    json!({"id": key, "kind": kind, "before": old, "after": new, "density_change": density,
           "burden_change": if ctx.allowed { Value::Object(changes) } else { Value::Null },
           "burden_summary": summary,
           "cohesion_diagnostic": {"before": cohesion(ctx.before), "after": cohesion(ctx.after)}})
}

fn ids_where(components: &[Value], pred: impl Fn(&Value) -> bool) -> Vec<Value> {
    components
        .iter()
        .filter(|c| pred(&c["density_change"]))
        .map(|c| c["id"].clone())
        .collect()
}

fn nonempty(v: &Value) -> bool {
    v.as_array().is_some_and(|a| !a.is_empty())
}

fn collection_errors(data: &Map<String, Value>, complete: bool) -> Result<Value> {
    let errors = data
        .get("collection_errors")
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| json!({"before": [], "after": []}));
    contracts::validate_collection_errors(&errors)?;
    if complete && (nonempty(&errors["before"]) || nonempty(&errors["after"])) {
        return Err("scope.complete cannot be true with nonempty collection_errors".into());
    }
    Ok(errors)
}

fn burden_increases(burden: &Map<String, Value>) -> Map<String, Value> {
    burden
        .iter()
        .filter(|(_, e)| {
            e["delta"]
                .get(1)
                .and_then(Value::as_f64)
                .is_some_and(|hi| hi > TOLERANCE)
        })
        .map(|(k, e)| (k.clone(), e["classification"].clone()))
        .collect()
}

fn gate_reason(complete: bool, identity: bool) -> Option<&'static str> {
    if !complete {
        Some("scope incomplete")
    } else if !identity {
        Some("identity not verified")
    } else {
        None
    }
}

/// The report's identity, scope and provenance fields, in oracle order.
fn header(
    data: &Map<String, Value>,
    profile: &str,
    options: Option<&Value>,
    identity: bool,
    scope: &Map<String, Value>,
    errors: Value,
) -> Map<String, Value> {
    let mut out = Map::new();
    let mut put = |k: &str, v: Value| {
        out.insert(k.to_owned(), v);
    };
    put("profile", json!(PROFILE));
    put("measurement_profile", json!(profile));
    put(
        "measurement_options",
        json!({"before": options, "after": data.get("measurement_options_after").filter(|v| !v.is_null())}),
    );
    put(
        "measurement_compatibility",
        json!(if options.is_some() {
            "current"
        } else {
            "legacy-no-options"
        }),
    );
    put(
        "status",
        json!("experimental; predictive accuracy unvalidated"),
    );
    put("identity_verified", json!(identity));
    put("scope", Value::Object(scope.clone()));
    put("collection_errors", errors);
    for key in PRESERVED {
        if let Some(v) = data.get(key) {
            put(key, v.clone());
        }
    }
    out
}

/// `emi.review`.
pub fn review(input: &Value) -> Result<Value> {
    let Some(data) = input.as_object() else {
        return Err("review needs a change object".into());
    };
    contracts::validate_change(input)?;
    let (profile, options, identity) = check_identities(data)?;
    let before = calc::validate_components(data.get("before"), "before")?;
    let after = calc::validate_components(data.get("after"), "after")?;
    if before.is_empty() && after.is_empty() {
        return Err("empty review".into());
    }
    let scope = check_scope(data, &before, &after)?;
    let allowed = identity && scope.complete;
    let reason = gate_reason(scope.complete, identity);
    let ctx = Ctx {
        before: &before,
        after: &after,
        unchanged: scope.unchanged.clone(),
        regions: RegionData::collect(&before, &after),
        allowed,
        reason,
    };
    let parent: HashMap<String, f64> = sloc_weights(&before, None);
    let matched = paired(&before, &after, &parent, allowed, reason, &ctx.unchanged);
    let days = history_days(data)?;
    let exposure = exposure(&ctx, days);
    let keys: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    let components: Vec<Value> = keys
        .into_iter()
        .map(|k| component_result(&ctx, k))
        .collect();
    let mut burden = ledger(&before, &after, &ctx.unchanged, ctx.regions.as_ref());
    if !scope.complete {
        for entry in burden.values_mut() {
            entry["delta"] = Value::Null;
            entry["classification"] = json!("unavailable");
        }
    }
    let summary = summarize_burden(&burden);
    let density_only = matched["classification"] == "improvement" && summary == "unchanged";
    let total_before = fsum(before.values().map(|c| c.sloc));
    let total_after = fsum(after.values().map(|c| c.sloc));
    let errors = collection_errors(data, scope.complete)?;
    let increases = burden_increases(&burden);
    let mut out = header(data, profile, options, identity, scope.raw, errors);
    let mut put = |k: &str, v: Value| {
        out.insert(k.to_owned(), v);
    };
    put(
        "structural",
        json!({"before": score(&before, None, WEIGHTS), "after": score(&after, None, WEIGHTS)}),
    );
    put("matched", matched);
    put(
        "weight_sensitivity",
        sensitivity(&before, &after, &parent, allowed, &ctx.unchanged),
    );
    put(
        "hotspots",
        json!({"before": hotspots(&before), "after": hotspots(&after)}),
    );
    put("burden_change", Value::Object(burden));
    put("burden_summary", json!(summary));
    put("density_only_improvement", json!(density_only));
    let regressions = ids_where(&components, |d| d["classification"] == "regression");
    let possible = ids_where(&components, |d| truthy(&d["possible_regression"]));
    put("components", Value::Array(components));
    put("regressions", Value::Array(regressions));
    put("possible_regressions", Value::Array(possible));
    put("burden_increases", Value::Object(increases));
    put("exposure", exposure);
    put("history_days", json!(days));
    put(
        "growth",
        json!({"before_sloc": total_before, "after_sloc": total_after, "net_sloc": total_after - total_before}),
    );
    put("limitations", json!(LIMITATIONS));
    Ok(Value::Object(out))
}
