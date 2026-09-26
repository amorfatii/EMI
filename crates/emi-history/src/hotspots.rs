//! Hotspots: measured burden × change frequency. A file that is both
//! burdensome and frequently changed is where maintenance cost accrues;
//! either alone is not a hotspot.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use crate::FileHistory;

#[derive(Clone, Debug, Serialize)]
pub struct Hotspot {
    pub file: String,
    pub sloc: u64,
    /// Total E+V+C burden mass as `[known, known + unknown]`.
    pub burden: [f64; 2],
    pub revisions: u32,
    /// `revisions × burden`, the ranking interval.
    pub score: [f64; 2],
    pub authors: u32,
    pub main_author_share: f64,
    pub ai_share: f64,
    pub age_days: f64,
}

fn bounds(v: &Value, sloc: f64) -> [f64; 2] {
    match v {
        Value::Null => [0.0, sloc],
        Value::Object(o) => {
            let known = o.get("known_mass").and_then(Value::as_f64).unwrap_or(0.0);
            let unknown = o.get("unknown_sloc").and_then(Value::as_f64).unwrap_or(0.0);
            [known, known + unknown]
        }
        other => {
            let x = other.as_f64().unwrap_or(0.0);
            [x, x]
        }
    }
}

/// Rank the measurement's components by `revisions × burden` (midpoint of
/// the interval, then its upper end, then path).
pub fn rank(
    measurement: &Value,
    histories: &BTreeMap<String, FileHistory>,
    now: i64,
) -> Vec<Hotspot> {
    let mut out: Vec<Hotspot> = measurement["components"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let file = c["id"].as_str()?.to_owned();
            let h = histories.get(&file)?;
            let sloc = c["sloc"].as_f64().unwrap_or(0.0);
            let burden = ["E", "V", "C"].iter().fold([0.0, 0.0], |acc, d| {
                let b = bounds(&c["burden"][*d], sloc);
                [acc[0] + b[0], acc[1] + b[1]]
            });
            let revs = f64::from(h.revisions);
            Some(Hotspot {
                sloc: sloc as u64,
                burden,
                revisions: h.revisions,
                score: [revs * burden[0], revs * burden[1]],
                authors: h.authors,
                main_author_share: h.main_author_share,
                ai_share: f64::from(h.ai_revisions) / revs.max(1.0),
                age_days: (now - h.first_change) as f64 / 86_400.0,
                file,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        let mid = |h: &Hotspot| h.score[0] + h.score[1];
        mid(b)
            .total_cmp(&mid(a))
            .then(b.score[1].total_cmp(&a.score[1]))
            .then(a.file.cmp(&b.file))
    });
    out
}
