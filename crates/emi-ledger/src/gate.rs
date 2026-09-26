//! The block/warn/pass gate (port of `emi.guardrail` and its verdict
//! detail, `compact_verdict`, `accept_rewrite`). It reads a review report
//! as JSON, so reports from any producer are held to the same contract.

use std::cmp::Ordering;

use serde_json::{Map, Value, json};

use crate::contracts::{self, DIMS, PROFILE, Result, TOLERANCE, str_repr, truthy};
use crate::pyfmt::{digest, g};

fn delta_of(burden: &Value, dim: &str) -> Option<[f64; 2]> {
    let d = burden.get(dim)?.get("delta")?.as_array()?;
    Some([d.first()?.as_f64()?, d.get(1)?.as_f64()?])
}

struct Row {
    id: Value,
    dim: &'static str,
    kind: Value,
    delta: [f64; 2],
    class: Value,
}

fn id_key(v: &Value) -> String {
    v.as_str().unwrap_or("").to_owned()
}

fn cmp_f(a: f64, b: f64) -> Ordering {
    a.partial_cmp(&b).unwrap_or(Ordering::Equal)
}

/// `emi._component_growth`: components whose delta may exceed zero.
fn component_growth(report: &Value, dim: &'static str, by_unknown: bool) -> Vec<Row> {
    let mut rows: Vec<Row> = report["components"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|comp| {
            let entry = comp
                .get("burden_change")
                .filter(|v| truthy(v))?
                .get(dim)
                .filter(|v| truthy(v))?;
            let delta = delta_of(comp.get("burden_change")?, dim)?;
            (delta[1] > TOLERANCE).then(|| Row {
                id: comp.get("id").cloned().unwrap_or(Value::Null),
                dim,
                kind: comp.get("kind").cloned().unwrap_or(Value::Null),
                delta,
                class: entry.get("classification").cloned().unwrap_or(Value::Null),
            })
        })
        .collect();
    if by_unknown {
        rows.sort_by(|a, b| {
            cmp_f(a.delta[1] - a.delta[0], b.delta[1] - b.delta[0])
                .then(cmp_f(a.delta[1], b.delta[1]))
                .then(id_key(&a.id).cmp(&id_key(&b.id)))
        });
    } else {
        rows.sort_by(|a, b| proven_order(b, a));
    }
    rows
}

fn proven_order(a: &Row, b: &Row) -> Ordering {
    cmp_f(a.delta[0], b.delta[0])
        .then(cmp_f(a.delta[1], b.delta[1]))
        .then(id_key(&a.id).cmp(&id_key(&b.id)))
}

fn evidence_entry(r: &Row) -> Value {
    json!({"id": r.id, "dimension": r.dim, "delta": r.delta, "classification": r.class, "kind": r.kind})
}

fn block_evidence(
    report: &Value,
    burden: &Value,
    allowance: f64,
    max: usize,
) -> (Vec<Value>, Value) {
    let mut ranked: Vec<Row> = DIMS
        .iter()
        .filter(|d| delta_of(burden, d).is_some_and(|x| x[0] - allowance > TOLERANCE))
        .flat_map(|d| component_growth(report, d, false))
        .collect();
    ranked.sort_by(|a, b| proven_order(b, a));
    ranked.truncate(max);
    let primary = ranked.first().map_or(Value::Null, |top| {
        let total = delta_of(burden, top.dim).map_or(0.0, |d| d[0]);
        let share = if total > TOLERANCE {
            json!(top.delta[0] / total)
        } else {
            Value::Null
        };
        json!({"id": top.id, "dimension": top.dim, "delta": top.delta, "share_of_proven": share})
    });
    (ranked.iter().map(evidence_entry).collect(), primary)
}

fn headroom_evidence(
    report: &Value,
    burden: &Value,
    allowance: f64,
    headroom: f64,
    max: usize,
) -> (Vec<Value>, Value) {
    let mut ranked: Vec<Row> = DIMS
        .iter()
        .filter(|d| {
            delta_of(burden, d)
                .is_some_and(|x| x[0] - allowance <= TOLERANCE && x[1] - headroom > TOLERANCE)
        })
        .flat_map(|d| component_growth(report, d, true))
        .collect();
    ranked.sort_by(|a, b| {
        cmp_f(b.delta[1] - b.delta[0], a.delta[1] - a.delta[0])
            .then(cmp_f(b.delta[1], a.delta[1]))
            .then(id_key(&b.id).cmp(&id_key(&a.id)))
    });
    ranked.truncate(max);
    let primary = ranked.first().map_or(Value::Null, |top| {
        json!({"id": top.id, "dimension": top.dim, "delta": top.delta, "share_of_proven": null})
    });
    (ranked.iter().map(evidence_entry).collect(), primary)
}

fn dilution_evidence(report: &Value, max: usize) -> (Vec<Value>, Value) {
    let mut newcomers: Vec<(f64, Value)> = Vec::new();
    for comp in report["components"].as_array().into_iter().flatten() {
        if comp.get("kind").and_then(Value::as_str) != Some("new") {
            continue;
        }
        let mut total = 0.0;
        let null = Value::Null;
        let changes = comp.get("burden_change").unwrap_or(&null);
        for dim in DIMS {
            if let Some(d) = delta_of(changes, dim) {
                total += f64::max(0.0, d[1]);
            }
        }
        newcomers.push((total, comp.get("id").cloned().unwrap_or(Value::Null)));
    }
    newcomers.sort_by(|a, b| cmp_f(b.0, a.0).then(id_key(&b.1).cmp(&id_key(&a.1))));
    newcomers.truncate(max);
    let evidence: Vec<Value> = newcomers
        .iter()
        .map(|(_, id)| json!({"id": id, "dimension": "*", "delta": null, "classification": "increased", "kind": "new"}))
        .collect();
    let primary = newcomers.first().map_or(
        Value::Null,
        |(_, id)| json!({"id": id, "dimension": "*", "delta": null, "share_of_proven": null}),
    );
    (evidence, primary)
}

/// `emi._verdict_details`: margins, capped evidence, primary, next step.
fn verdict_details(
    report: &Value,
    allowance: f64,
    headroom: f64,
    kind: &str,
    max: usize,
) -> Map<String, Value> {
    let null = Value::Null;
    let burden = report.get("burden_change").unwrap_or(&null);
    let mut margin = Map::new();
    for dim in DIMS {
        let m = match burden
            .get(dim)
            .and_then(|e| e.get("delta"))
            .filter(|d| !d.is_null())
        {
            None => json!({"proven_excess": null, "possible_excess": null}),
            Some(_) => {
                let d = delta_of(burden, dim).unwrap_or([0.0, 0.0]);
                json!({"proven_excess": d[0] - allowance, "possible_excess": d[1] - headroom})
            }
        };
        margin.insert(dim.to_owned(), m);
    }
    let (evidence, primary) = match kind {
        "block" => block_evidence(report, burden, allowance, max),
        "headroom" => headroom_evidence(report, burden, allowance, headroom, max),
        "dilution" => dilution_evidence(report, max),
        _ => (Vec::new(), Value::Null),
    };
    let next_step = match kind {
        "block"
            if primary
                .get("share_of_proven")
                .and_then(Value::as_f64)
                .unwrap_or(0.0)
                > 0.5 =>
        {
            "split"
        }
        "block" => "shrink",
        "headroom" => "inspect-unknowns",
        "dilution" | "unavailable" => "justify",
        _ => "proceed",
    };
    let mut out = Map::new();
    out.insert("margin".into(), Value::Object(margin));
    out.insert("evidence".into(), Value::Array(evidence));
    out.insert("primary".into(), primary);
    out.insert("next_step".into(), json!(next_step));
    out
}

const NOT_A_REPORT: &str =
    "guardrail needs a review report object (full review JSON, not a change input)";

/// `emi._validate_report_for_gate`: returns (matched, summary, net_sloc).
fn validate_for_gate(report: &Value) -> Result<(String, String, f64)> {
    let matched = report["matched"]["classification"]
        .as_str()
        .unwrap_or("")
        .to_owned();
    let summary = report["burden_summary"].as_str().unwrap_or("").to_owned();
    let Some(net) = report["growth"].get("net_sloc").and_then(Value::as_f64) else {
        return Err("guardrail needs growth.net_sloc as a finite number".into());
    };
    Ok((matched, summary, net))
}

fn structural_gain(report: &Value) -> bool {
    let interval = |side: &str| -> Option<[f64; 2]> {
        let i = report
            .get("structural")?
            .get(side)?
            .get("interval")?
            .as_array()?;
        Some([i.first()?.as_f64()?, i.get(1)?.as_f64()?])
    };
    match (interval("before"), interval("after")) {
        (Some(b), Some(a)) => a[0] - b[1] > TOLERANCE,
        _ => false,
    }
}

pub fn gate_policy_id(allowance: f64, headroom: f64) -> String {
    digest(
        "gate-policy",
        &json!({"profile": PROFILE, "weights": {"E": 7, "V": 5, "C": 5}, "allowance": allowance,
                "headroom": headroom, "uncertainty": "conservative", "version": "emi-gate-0.1"}),
    )
}

fn check_knob(value: f64, label: &str) -> Result<f64> {
    if value.is_finite() && value >= 0.0 {
        Ok(value)
    } else {
        Err(format!("{label} is out of range or has an invalid unit"))
    }
}

fn verdict(decision: &str, reason: String, policy: &str, details: Map<String, Value>) -> Value {
    let mut out = Map::new();
    out.insert("decision".into(), json!(decision));
    out.insert("reasons".into(), json!([reason]));
    out.insert("gate_policy_id".into(), json!(policy));
    out.extend(details);
    Value::Object(out)
}

/// `emi.guardrail`: deterministic block/warn/pass for a review report.
pub fn guardrail(
    report: &Value,
    allowance: f64,
    headroom: Option<f64>,
    max_evidence: i64,
) -> Result<Value> {
    let allowance = check_knob(allowance, "allowance")?;
    let headroom = match headroom {
        None => allowance,
        Some(h) => check_knob(h, "headroom_allowance")?,
    };
    let max = usize::try_from(max_evidence).map_err(|_| "max_evidence must be >= 0".to_owned())?;
    if !report.is_object() {
        return Err(NOT_A_REPORT.into());
    }
    contracts::validate_report_semantic(report)?;
    let (matched, summary, net) = validate_for_gate(report)?;
    let policy = gate_policy_id(allowance, headroom);
    let burden = &report["burden_change"];
    let (mut over, mut maybe) = (Vec::new(), Vec::new());
    for dim in DIMS {
        let Some(d) = delta_of(burden, dim) else {
            continue;
        };
        if d[0] - allowance > TOLERANCE {
            over.push(format!("{dim} +{} (allowance {})", g(d[0]), g(allowance)));
        } else if d[1] - headroom > TOLERANCE {
            maybe.push(format!(
                "{dim} up to +{} (headroom allowance {})",
                g(d[1]),
                g(headroom)
            ));
        }
    }
    let details = |kind: &str| verdict_details(report, allowance, headroom, kind, max);
    if !over.is_empty() {
        let reason = format!("absolute burden grew beyond allowance: {}", over.join("; "));
        return Ok(verdict("block", reason, &policy, details("block")));
    }
    if matched == "unavailable" || summary == "unavailable" {
        let reason = "comparison unavailable (identity unverified or scope incomplete); cannot prove no worsening — human review required";
        return Ok(verdict(
            "warn",
            reason.into(),
            &policy,
            details("unavailable"),
        ));
    }
    if structural_gain(report)
        && summary == "unchanged"
        && matched != "improvement"
        && net > TOLERANCE
    {
        let reason = format!(
            "overall score rose with no measured burden removed while source grew (+{} SLOC): added code diluted density without fixing anything — confirm the new code is necessary",
            g(net)
        );
        return Ok(verdict("warn", reason, &policy, details("dilution")));
    }
    if report.get("density_only_improvement").is_some_and(truthy) && net > TOLERANCE {
        let reason = format!(
            "matched density improved with no measured burden removed while source grew (+{} SLOC): added code diluted density without fixing anything — confirm the new code is necessary",
            g(net)
        );
        return Ok(verdict("warn", reason, &policy, details("dilution")));
    }
    if !maybe.is_empty() {
        let reason = format!(
            "burden could exceed allowance behind missing evidence: {}",
            maybe.join("; ")
        );
        return Ok(verdict("warn", reason, &policy, details("headroom")));
    }
    let reason = format!(
        "burden increases (if any) fit inside the allowance, no dilution shape, no hidden headroom above {}",
        g(headroom)
    );
    Ok(verdict("pass", reason, &policy, details("pass")))
}

/// `emi.compact_verdict`: the token-cheap payload for model consumers.
pub fn compact_verdict(
    report: &Value,
    gate: &Value,
    allowance: f64,
    headroom: f64,
) -> Result<Value> {
    if !report.is_object() {
        return Err("compact_verdict needs a review report object".into());
    }
    contracts::validate_report_semantic(report)?;
    if !matches!(
        gate.get("decision").and_then(Value::as_str),
        Some("block" | "warn" | "pass")
    ) {
        return Err("compact_verdict needs a guardrail verdict with decision".into());
    }
    let get = |v: &Value, k: &str| v.get(k).cloned().unwrap_or(Value::Null);
    let list = |k: &str| report.get(k).cloned().unwrap_or_else(|| json!([]));
    Ok(
        json!({"decision": gate["decision"], "reasons": gate["reasons"],
              "next_step": get(gate, "next_step"), "margin": get(gate, "margin"),
              "evidence": gate.get("evidence").cloned().unwrap_or_else(|| json!([])),
              "primary": get(gate, "primary"), "allowance": allowance, "headroom": headroom,
              "measurement_profile": get(report, "measurement_profile"),
              "burden_summary": get(report, "burden_summary"),
              "regressions": list("regressions"), "possible_regressions": list("possible_regressions")}),
    )
}

fn py_value(v: &Value) -> String {
    match v {
        Value::Null => "None".into(),
        Value::Bool(b) => {
            if *b {
                "True".into()
            } else {
                "False".into()
            }
        }
        Value::String(s) => str_repr(s),
        Value::Number(n) => n
            .as_f64()
            .filter(|_| n.is_f64())
            .map_or_else(|| n.to_string(), crate::pyfmt::repr),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(py_value).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(_) => v.to_string(),
    }
}

fn decision(kind: &str, reason: String) -> Value {
    json!({"decision": kind, "reasons": [reason]})
}

/// `emi.accept_rewrite`: anti-gaming acceptance for EMI-guided rewrites.
pub fn accept_rewrite(report: &Value) -> Result<Value> {
    if !report.is_object() {
        return Err("accept_rewrite needs a review report object".into());
    }
    contracts::validate_report_semantic(report)?;
    let field = |path: &str| report.pointer(path).cloned().unwrap_or(Value::Null);
    let matched = field("/matched/classification");
    if matched != "improvement" {
        return Ok(decision(
            "reject",
            format!(
                "matched comparison is {}, need 'improvement'",
                py_fstr(&matched)
            ),
        ));
    }
    let summary = field("/burden_summary");
    if summary != "reduced" {
        if report.get("density_only_improvement").is_some_and(truthy) {
            return Ok(decision(
                "needs-review",
                "density improved but no measured burden was removed (dilution, e.g. added clean helpers); not a rewrite win".into(),
            ));
        }
        return Ok(decision(
            "reject",
            format!("burden_summary is {}, need 'reduced'", py_fstr(&summary)),
        ));
    }
    if report.get("regressions").is_some_and(truthy) {
        return Ok(decision(
            "reject",
            format!(
                "component regressions: {}",
                py_value(&report["regressions"])
            ),
        ));
    }
    if report.get("possible_regressions").is_some_and(truthy) {
        let listed = py_value(&report["possible_regressions"]);
        return Ok(decision(
            "reject",
            format!("possible regressions under uncertainty: {listed}"),
        ));
    }
    let sens = field("/weight_sensitivity/classification");
    if sens != "improvement" {
        return Ok(decision(
            "needs-review",
            format!(
                "weight sensitivity is {}, need 'improvement' across stress weights",
                py_fstr(&sens)
            ),
        ));
    }
    Ok(decision(
        "accept",
        "matched improvement with reduced burden, no regressions, robust to weights".into(),
    ))
}

/// `'{x}'` as the oracle's f-strings render a JSON-decoded value in quotes.
fn py_fstr(v: &Value) -> String {
    match v {
        Value::String(s) => format!("'{s}'"),
        other => format!("'{}'", py_value(other)),
    }
}

/// A gate run as the CLI performs it.
#[derive(Clone, Debug)]
pub struct Policy {
    pub fail_on_warn: bool,
    pub allowance: f64,
    pub headroom: Option<f64>,
    pub max_evidence: i64,
    pub compact: bool,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            fail_on_warn: false,
            allowance: 10.0,
            headroom: None,
            max_evidence: 5,
            compact: false,
        }
    }
}

/// Gate a review report: the JSON to print and the exit code (0 pass,
/// 0 warn, 2 block; warn exits 2 with `fail_on_warn`; 2 whenever scope is
/// incomplete).
pub fn apply(mut report: Value, policy: &Policy) -> Result<(Value, u8)> {
    let gate = guardrail(
        &report,
        policy.allowance,
        policy.headroom,
        policy.max_evidence,
    )?;
    let decision = gate["decision"].as_str().unwrap_or("");
    let fails = decision == "block" || (decision == "warn" && policy.fail_on_warn);
    let incomplete =
        report.get("scope").and_then(|s| s.get("complete")) == Some(&Value::Bool(false));
    let code = if fails || incomplete { 2 } else { 0 };
    if policy.compact {
        let headroom = policy.headroom.unwrap_or(policy.allowance);
        return Ok((
            compact_verdict(&report, &gate, policy.allowance, headroom)?,
            code,
        ));
    }
    report["guardrail"] = gate;
    Ok((report, code))
}
