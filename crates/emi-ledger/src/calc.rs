//! The EMI calculator (port of `emi.py` `validate_components` .. `review`):
//! density scores with evidence intervals, the per-dimension burden ledger,
//! matched comparisons, weight stress tests and hotspot concentration.
//!
//! Arithmetic follows the oracle operation for operation (`math.fsum`
//! where it sums exactly, left-to-right `+` where it does not), because the
//! gate decides on these bounds to the last unit in the last place.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use indexmap::IndexMap;
use serde_json::{Map, Value, json};

use crate::contracts::{self, DIMS, Result, TOLERANCE, direction, keys_are, num};
use crate::pyfmt::{fsum, square};

pub const WEIGHTS: [f64; 3] = [7.0 / 17.0, 5.0 / 17.0, 5.0 / 17.0];

pub type Bounds = [f64; 2];

#[derive(Clone, Debug, PartialEq)]
pub struct Region {
    pub sloc: f64,
    pub sha: String,
    pub in_cycle: bool,
    /// E and C evidence bounds.
    pub bounds: [Bounds; 2],
}

#[derive(Clone, Debug)]
pub struct Component {
    pub sloc: f64,
    /// Exact mass per dimension, `None` when the evidence has width.
    pub burden: [Option<f64>; 3],
    pub bounds: [Bounds; 3],
    pub observed: [f64; 3],
    pub churn: Option<f64>,
    pub cohesion: Option<f64>,
    pub regions: Option<BTreeMap<String, Region>>,
    /// Dimension indices in the input's `burden` key order, which the
    /// oracle's `unknown` lists follow.
    pub order: [usize; 3],
}

impl Component {
    fn exact(&self) -> bool {
        self.burden.iter().all(Option::is_some)
    }
}

pub type Components = IndexMap<String, Component>;
type Exposures = HashMap<String, f64>;

/// `emi.number`: finite, in range, optionally integral.
fn number(
    value: Option<&Value>,
    label: &str,
    low: f64,
    high: Option<f64>,
    integer: bool,
) -> Result<f64> {
    let Some(v) = value.and_then(num) else {
        return Err(format!("{label} must be numeric"));
    };
    let valid = v >= low && high.is_none_or(|h| v <= h);
    if !valid || (integer && v.fract() != 0.0) {
        return Err(format!("{label} is out of range or has an invalid unit"));
    }
    Ok(v)
}

fn mass_evidence(value: &Value, sloc: f64, label: &str, integer: bool) -> Result<(Bounds, f64)> {
    if value.is_null() {
        return Ok(([0.0, sloc], 0.0));
    }
    if let Some(obj) = value.as_object() {
        if !keys_are(obj, &["known_mass", "unknown_sloc"]) {
            return Err(format!(
                "{label}: partial evidence requires known_mass and unknown_sloc"
            ));
        }
        let unknown = number(
            obj.get("unknown_sloc"),
            &format!("{label}.unknown_sloc"),
            0.0,
            Some(sloc),
            true,
        )?;
        let known = number(
            obj.get("known_mass"),
            &format!("{label}.known_mass"),
            0.0,
            Some(sloc - unknown),
            integer,
        )?;
        return Ok(([known, known + unknown], sloc - unknown));
    }
    let exact = number(Some(value), label, 0.0, Some(sloc), integer)?;
    Ok(([exact, exact], sloc))
}

fn region_record(
    ident: &str,
    reg: &Value,
    loc: f64,
    seen: &BTreeMap<String, Region>,
) -> Result<(String, Region)> {
    let Some(obj) = reg.as_object() else {
        return Err(format!("{ident}.regions entries must be objects"));
    };
    let rid = match obj.get("id").and_then(Value::as_str) {
        Some(r) if !emi_ir::pytext::strip(r).is_empty() && !seen.contains_key(r) => r.to_owned(),
        _ => {
            return Err(format!(
                "{ident}.regions ids must be unique nonempty strings"
            ));
        }
    };
    let rsloc = number(
        obj.get("sloc"),
        &format!("{ident}.{rid}.sloc"),
        1.0,
        None,
        true,
    )?;
    if rsloc > loc {
        return Err(format!("{ident}.{rid}.sloc exceeds component sloc"));
    }
    let sha = match obj.get("sha").and_then(Value::as_str) {
        Some(s) if !s.is_empty() => s.to_owned(),
        _ => return Err(format!("{ident}.{rid}.sha must be a nonempty string")),
    };
    let Some(in_cycle) = obj.get("in_cycle").and_then(Value::as_bool) else {
        return Err(format!("{ident}.{rid}.in_cycle must be boolean"));
    };
    let mut bounds = [[0.0; 2]; 2];
    for (slot, dim) in bounds.iter_mut().zip(["E", "C"]) {
        let Some(v) = obj.get(dim) else {
            return Err(format!("{ident}.{rid} region requires E and C evidence"));
        };
        *slot = mass_evidence(v, rsloc, &format!("{ident}.{rid}.{dim}"), false)?.0;
    }
    Ok((
        rid,
        Region {
            sloc: rsloc,
            sha,
            in_cycle,
            bounds,
        },
    ))
}

fn check_region_sums(
    ident: &str,
    map: &BTreeMap<String, Region>,
    loc: f64,
    bounds: &[Bounds; 3],
) -> Result<()> {
    let total: f64 = map.values().map(|r| r.sloc).sum();
    if total - loc > TOLERANCE {
        return Err(format!("{ident}.regions sloc sums above component sloc"));
    }
    for (i, (dim, file)) in [("E", bounds[0]), ("C", bounds[2])].into_iter().enumerate() {
        let sum_known = fsum(map.values().map(|r| r.bounds[i][0]));
        let sum_width = fsum(map.values().map(|r| r.bounds[i][1] - r.bounds[i][0]));
        if sum_known - file[0] > TOLERANCE {
            return Err(format!(
                "{ident}.regions {dim} known mass exceeds component evidence"
            ));
        }
        if sum_width - (file[1] - file[0]) > TOLERANCE {
            return Err(format!(
                "{ident}.regions {dim} unknown mass exceeds component evidence"
            ));
        }
    }
    Ok(())
}

fn regions_of(
    ident: &str,
    item: &Map<String, Value>,
    loc: f64,
    bounds: &[Bounds; 3],
) -> Result<Option<BTreeMap<String, Region>>> {
    let list = match item.get("regions") {
        None | Some(Value::Null) => return Ok(None),
        Some(Value::Array(list)) => list,
        Some(_) => return Err(format!("{ident}.regions must be an array")),
    };
    let mut map = BTreeMap::new();
    for reg in list {
        let (rid, region) = region_record(ident, reg, loc, &map)?;
        map.insert(rid, region);
    }
    if !map.is_empty() {
        check_region_sums(ident, &map, loc, bounds)?;
    }
    Ok(Some(map))
}

fn component(item: &Value, revision: &str, result: &Components) -> Result<(String, Component)> {
    let Some(item) = item.as_object() else {
        return Err("component must be an object".into());
    };
    let ident = match item.get("id").and_then(Value::as_str) {
        Some(i) if !emi_ir::pytext::strip(i).is_empty() && !result.contains_key(i) => i.to_owned(),
        _ => return Err(format!("{revision} IDs must be unique nonempty strings")),
    };
    let loc = number(item.get("sloc"), &format!("{ident}.sloc"), 1.0, None, true)?;
    let burden = match item.get("burden").and_then(Value::as_object) {
        Some(b) if keys_are(b, &DIMS) => b,
        _ => {
            return Err(format!(
                "{ident}.burden requires exactly E,V,C masses; use null for unknown"
            ));
        }
    };
    // Evidence is validated in the burden object's own key order, as the
    // oracle iterates `burden.items()`: the first bad key names the error.
    let mut bounds = [[0.0; 2]; 3];
    let mut observed = [0.0; 3];
    let mut order = [0, 1, 2];
    for (i, (key, value)) in burden.iter().enumerate() {
        let d = DIMS.iter().position(|x| x == key).unwrap_or(0);
        order[i] = d;
        (bounds[d], observed[d]) =
            mass_evidence(value, loc, &format!("{ident}.{key}"), key == "V")?;
    }
    let masses = bounds.map(|b| (b[0] == b[1]).then_some(b[0]));
    let regions = regions_of(&ident, item, loc, &bounds)?;
    let optional = |key: &str, high: Option<f64>, integer: bool| -> Result<Option<f64>> {
        match item.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(v) => number(Some(v), &format!("{ident}.{key}"), 0.0, high, integer).map(Some),
        }
    };
    let churn = optional("churn_lines", None, true)?;
    let cohesion = optional("cohesion_fragmentation", Some(1.0), false)?;
    Ok((
        ident,
        Component {
            sloc: loc,
            burden: masses,
            bounds,
            observed,
            churn,
            cohesion,
            regions,
            order,
        },
    ))
}

/// `emi.validate_components` (after the shared contract check).
pub fn validate_components(items: Option<&Value>, revision: &str) -> Result<Components> {
    contracts::validate_components(items, revision)?;
    let Some(items) = items.and_then(Value::as_array) else {
        return Err(format!("{revision} must be an array"));
    };
    let mut result = Components::new();
    for item in items {
        let (ident, comp) = component(item, revision, &result)?;
        result.insert(ident, comp);
    }
    Ok(result)
}

fn risk(item: &Component, fill: usize) -> f64 {
    let loc = item.sloc;
    fsum((0..3).map(|d| WEIGHTS[d] * square(item.bounds[d][fill] / loc))).sqrt()
}

fn tail_risk(mut pairs: Vec<(f64, f64)>) -> f64 {
    let total = fsum(pairs.iter().map(|p| p.1));
    pairs.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));
    let (mut remaining, mut tail) = (0.20, 0.0);
    for (value, weight) in pairs {
        let take = f64::min(weight / total, remaining);
        tail += value * take;
        remaining -= take;
        if remaining <= 0.0 {
            break;
        }
    }
    tail / 0.20
}

fn fvec(v: [f64; 2]) -> Value {
    json!([v[0], v[1]])
}

fn weights_value(w: [f64; 3]) -> Value {
    json!({"E": w[0], "V": w[1], "C": w[2]})
}

/// `emi.score`: exposure-weighted density with evidence bounds.
pub fn score(components: &Components, exposures: Option<&Exposures>, weights: [f64; 3]) -> Value {
    if components.is_empty() {
        return json!({"score": null, "interval": null, "coverage": null, "reason": "empty scope"});
    }
    let exposure = |key: &str, item: &Component| exposures.map_or(item.sloc, |e| e[key]);
    let total = fsum(components.iter().map(|(k, c)| exposure(k, c)));
    let complete = components.values().all(Component::exact);
    let densities: [[f64; 2]; 3] = std::array::from_fn(|d| {
        std::array::from_fn(|bound| {
            fsum(
                components
                    .iter()
                    .map(|(k, c)| (exposure(k, c) / total) * c.bounds[d][bound] / c.sloc),
            )
        })
    });
    let index = |bound: usize| {
        100.0 * (1.0 - fsum((0..3).map(|d| weights[d] * square(densities[d][bound]))).sqrt())
    };
    let (high, low) = (index(0), index(1));
    let (low, high) = (low.clamp(0.0, 100.0), high.clamp(0.0, 100.0));
    let coverage = fsum(components.iter().map(|(k, c)| {
        (exposure(k, c) / total) * fsum((0..3).map(|d| weights[d] * c.observed[d] / c.sloc))
    }));
    let mut unknown = Map::new();
    for (key, item) in components {
        if !item.exact() {
            let dims: Vec<&str> = item
                .order
                .iter()
                .filter(|d| item.burden[**d].is_none())
                .map(|d| DIMS[*d])
                .collect();
            unknown.insert(key.clone(), json!(dims));
        }
    }
    json!({
        "score": if complete { Some(high) } else { None },
        "interval": [low, high],
        "coverage": coverage,
        "density_intervals": {"E": fvec(densities[0]), "V": fvec(densities[1]), "C": fvec(densities[2])},
        "unknown": unknown,
    })
}

fn interval_of(v: &Value) -> Option<Bounds> {
    let a = v.get("interval")?.as_array()?;
    Some([a.first()?.as_f64()?, a.get(1)?.as_f64()?])
}

fn unavailable() -> Value {
    json!({"delta": null, "interval": null, "classification": "unavailable", "possible_regression": false})
}

/// `emi.difference`: after-minus-before score interval.
pub fn difference(before: &Value, after: &Value, allowed: bool) -> Value {
    let (Some(b), Some(a), true) = (interval_of(before), interval_of(after), allowed) else {
        return unavailable();
    };
    let interval = [a[0] - b[1], a[1] - b[0]];
    let delta = match (before["score"].as_f64(), after["score"].as_f64()) {
        (Some(x), Some(y)) => json!(y - x),
        _ => Value::Null,
    };
    json!({"delta": delta, "interval": interval,
           "classification": direction(Some(interval), "improvement", "regression"),
           "possible_regression": interval[0] < -TOLERANCE})
}

fn burden_bounds(components: &Components, d: usize) -> Bounds {
    [
        fsum(components.values().map(|c| c.bounds[d][0])),
        fsum(components.values().map(|c| c.bounds[d][1])),
    ]
}

/// Components that carry nonempty region maps, per side.
#[derive(Default)]
pub struct RegionData<'a> {
    before: HashMap<&'a str, &'a BTreeMap<String, Region>>,
    after: HashMap<&'a str, &'a BTreeMap<String, Region>>,
}

impl<'a> RegionData<'a> {
    pub fn collect(before: &'a Components, after: &'a Components) -> Option<Self> {
        let side = |comps: &'a Components| -> HashMap<&'a str, &'a BTreeMap<String, Region>> {
            comps
                .iter()
                .filter_map(|(k, c)| {
                    c.regions
                        .as_ref()
                        .filter(|r| !r.is_empty())
                        .map(|r| (k.as_str(), r))
                })
                .collect()
        };
        let data = Self {
            before: side(before),
            after: side(after),
        };
        (!data.before.is_empty() || !data.after.is_empty()).then_some(data)
    }
}

fn side_bounds(comps: &Components, ident: &str, d: usize) -> Bounds {
    comps.get(ident).map_or([0.0, 0.0], |c| c.bounds[d])
}

/// `emi._region_delta`: file delta with exact, hash-equal regions removed.
fn region_delta(
    ident: &str,
    before: &Components,
    after: &Components,
    d: usize,
    regions: Option<&RegionData>,
) -> Option<Bounds> {
    let regions = regions?;
    let (rb, ra) = (regions.before.get(ident)?, regions.after.get(ident)?);
    let r = usize::from(d == 2);
    let (mut zlo, mut zhi) = (Vec::new(), Vec::new());
    for (rid, left) in rb.iter() {
        let Some(right) = ra.get(rid) else { continue };
        if left.sha == right.sha
            && left.sloc == right.sloc
            && left.in_cycle == right.in_cycle
            && left.bounds[r] == right.bounds[r]
            && left.bounds[r][0] == left.bounds[r][1]
        {
            zlo.push(left.bounds[r][0]);
            zhi.push(left.bounds[r][1]);
        }
    }
    if zlo.is_empty() {
        return None;
    }
    let ([blo, bhi], [alo, ahi]) = (side_bounds(before, ident, d), side_bounds(after, ident, d));
    let (zlo, zhi) = (fsum(zlo), fsum(zhi));
    Some([(alo - zlo) - (bhi - zhi), (ahi - zhi) - (blo - zlo)])
}

fn exact_unchanged(ident: &str, before: &Components, after: &Components, d: usize) -> bool {
    match (before.get(ident), after.get(ident)) {
        (Some(b), Some(a)) => {
            b.bounds[d] == a.bounds[d]
                && b.bounds[d][0] == b.bounds[d][1]
                && b.burden[d].is_some()
                && a.burden[d].is_some()
        }
        _ => false,
    }
}

fn ledger_delta(
    before: &Components,
    after: &Components,
    unchanged: &BTreeSet<String>,
    regions: Option<&RegionData>,
    d: usize,
) -> Bounds {
    let idents: BTreeSet<&str> = before
        .keys()
        .chain(after.keys())
        .map(String::as_str)
        .collect();
    let (mut lo, mut hi) = (Vec::new(), Vec::new());
    for ident in idents {
        if unchanged.contains(ident) && exact_unchanged(ident, before, after, d) {
            continue;
        }
        let region = if d == 1 {
            None
        } else {
            region_delta(ident, before, after, d, regions)
        };
        if let Some([l, h]) = region {
            lo.push(l);
            hi.push(h);
        } else {
            let ([blo, bhi], [alo, ahi]) =
                (side_bounds(before, ident, d), side_bounds(after, ident, d));
            lo.push(alo - bhi);
            hi.push(ahi - blo);
        }
    }
    [fsum(lo), fsum(hi)]
}

/// `emi.ledger`: per-dimension total burden bounds with an exact delta.
pub fn ledger(
    before: &Components,
    after: &Components,
    unchanged: &BTreeSet<String>,
    regions: Option<&RegionData>,
) -> Map<String, Value> {
    let mut result = Map::new();
    for (d, key) in DIMS.iter().enumerate() {
        let (old, new) = (burden_bounds(before, d), burden_bounds(after, d));
        let delta = if unchanged.is_empty() && regions.is_none() {
            [new[0] - old[1], new[1] - old[0]]
        } else {
            ledger_delta(before, after, unchanged, regions, d)
        };
        result.insert(
            (*key).to_owned(),
            json!({"before": old, "after": new, "delta": delta,
                   "classification": direction(Some(delta), "increased", "reduced")}),
        );
    }
    result
}

pub fn summarize_burden(changes: &Map<String, Value>) -> &'static str {
    contracts::summarize(
        changes
            .values()
            .filter_map(|e| e["classification"].as_str()),
    )
}

fn subset(comps: &Components, keys: &[&String]) -> Components {
    keys.iter()
        .map(|k| ((*k).clone(), comps[k.as_str()].clone()))
        .collect()
}

fn common_keys<'a>(before: &'a Components, after: &Components) -> Vec<&'a String> {
    let mut keys: Vec<&String> = before
        .keys()
        .filter(|k| after.contains_key(k.as_str()))
        .collect();
    keys.sort();
    keys
}

fn all_exact(before: &Components, after: &Components, common: &[&String]) -> bool {
    common
        .iter()
        .all(|k| before[k.as_str()].exact() && after[k.as_str()].exact())
}

/// `emi.paired`: matched comparison on survivors with frozen parent weights.
pub fn paired(
    before: &Components,
    after: &Components,
    exposures: &Exposures,
    allowed: bool,
    reason: Option<&str>,
    unchanged: &BTreeSet<String>,
) -> Value {
    let common = common_keys(before, after);
    let old = score(&subset(before, &common), Some(exposures), WEIGHTS);
    let new = score(&subset(after, &common), Some(exposures), WEIGHTS);
    let moved = common.iter().any(|k| !unchanged.contains(k.as_str()));
    let comparison = if allowed && !common.is_empty() && !moved && all_exact(before, after, &common)
    {
        let delta = match (old["score"].as_f64(), new["score"].as_f64()) {
            (Some(x), Some(y)) => json!(y - x),
            _ => Value::Null,
        };
        json!({"delta": delta, "interval": [0.0, 0.0], "classification": "unchanged", "possible_regression": false})
    } else {
        difference(&old, &new, allowed)
    };
    let available = allowed && !common.is_empty();
    let reason = if available {
        Value::Null
    } else {
        json!(reason.unwrap_or(if allowed {
            "no surviving components"
        } else {
            "identity not verified"
        }))
    };
    let mut out = Map::new();
    out.insert("before".into(), old);
    out.insert("after".into(), new);
    if let Value::Object(c) = comparison {
        out.extend(c);
    }
    out.insert("comparison_available".into(), json!(available));
    out.insert("reason".into(), reason);
    Value::Object(out)
}

/// `emi.hotspots`: tail burden concentration (grouping-sensitive).
pub fn hotspots(components: &Components) -> Value {
    if components.is_empty() {
        return json!({"tail_risk_interval": null, "worst_known": [], "grouping_sensitive": true});
    }
    let tail: Vec<f64> = (0..2)
        .map(|b| tail_risk(components.values().map(|c| (risk(c, b), c.sloc)).collect()))
        .collect();
    let mut known: Vec<(&String, f64)> = components
        .iter()
        .filter(|(_, c)| c.exact())
        .map(|(k, c)| (k, risk(c, 0)))
        .collect();
    known.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let worst: Vec<Value> = known
        .into_iter()
        .take(10)
        .map(|(k, r)| json!({"id": k, "risk": r}))
        .collect();
    json!({"tail_risk_interval": tail, "worst_known": worst, "grouping_sensitive": true, "tail_weight_fraction": 0.2})
}

fn stress_weights() -> Vec<[f64; 3]> {
    let mut out = vec![WEIGHTS];
    for e in 2..7 {
        for v in 2..7 {
            let c = 10 - e - v;
            if (2..=6).contains(&c) {
                out.push([
                    f64::from(e) / 10.0,
                    f64::from(v) / 10.0,
                    f64::from(c) / 10.0,
                ]);
            }
        }
    }
    out
}

const STRESS_MEANING: &str =
    "finite weight stress test, not empirical calibration or a confidence interval";

/// `emi.sensitivity`: the matched delta across a finite weight grid.
pub fn sensitivity(
    before: &Components,
    after: &Components,
    exposures: &Exposures,
    allowed: bool,
    unchanged: &BTreeSet<String>,
) -> Value {
    let common = common_keys(before, after);
    if !allowed || common.is_empty() {
        return json!({"available": false, "configurations": 0, "delta_range": null, "classification": "unavailable"});
    }
    let configurations = stress_weights();
    if common.iter().all(|k| unchanged.contains(k.as_str())) && all_exact(before, after, &common) {
        let outcomes: Vec<Value> = configurations
            .iter()
            .map(|w| {
                json!({"weights": weights_value(*w), "delta": 0.0, "interval": [0.0, 0.0],
                            "classification": "unchanged", "possible_regression": false})
            })
            .collect();
        return json!({"available": true, "configurations": outcomes.len(), "delta_range": [0.0, 0.0],
                      "classification": "unchanged", "outcomes": outcomes, "meaning": STRESS_MEANING});
    }
    let (left, right) = (subset(before, &common), subset(after, &common));
    let mut outcomes = Vec::new();
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    let mut signs = BTreeSet::new();
    for w in &configurations {
        let diff = difference(
            &score(&left, Some(exposures), *w),
            &score(&right, Some(exposures), *w),
            true,
        );
        let interval = interval_of(&diff).unwrap_or([0.0, 0.0]);
        lo = lo.min(interval[0]);
        hi = hi.max(interval[1]);
        signs.insert(diff["classification"].as_str().unwrap_or("").to_owned());
        let mut row = Map::new();
        row.insert("weights".into(), weights_value(*w));
        if let Value::Object(d) = diff {
            row.extend(d);
        }
        outcomes.push(Value::Object(row));
    }
    let classification = if signs.contains("improvement") && signs.contains("regression") {
        "weight_sensitive".to_owned()
    } else if signs.len() == 1 {
        signs.pop_first().unwrap_or_default()
    } else {
        "inconclusive".to_owned()
    };
    json!({"available": true, "configurations": outcomes.len(), "delta_range": [lo, hi],
           "classification": classification, "outcomes": outcomes, "meaning": STRESS_MEANING})
}
