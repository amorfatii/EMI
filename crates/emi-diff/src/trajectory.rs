//! `_log_trajectory`: one JSONL row per gate run, the features a later
//! outcome join (merged, reverted, time-to-merge) needs.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use serde_json::{Map, Value, json};

/// `datetime.now(timezone.utc).isoformat()`.
pub fn utc_now_iso() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = i64::try_from(now.as_secs()).unwrap_or(0);
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil-from-days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    let micros = now.subsec_micros();
    let time = format!("{:02}:{:02}:{:02}", rem / 3600, rem % 3600 / 60, rem % 60);
    let frac = if micros == 0 {
        String::new()
    } else {
        format!(".{micros:06}")
    };
    format!("{year:04}-{month:02}-{day:02}T{time}{frac}+00:00")
}

/// Build the row: `record` fields first, then the verdict and identities.
pub fn row(record: Map<String, Value>, report: &Value, gate: &Value) -> Value {
    let mut row = Map::new();
    row.insert("timestamp".into(), json!(utc_now_iso()));
    row.extend(record);
    let burden = report.get("burden_change").cloned().unwrap_or(Value::Null);
    let side = |which: &str| -> Value {
        let mut m = Map::new();
        for dim in ["E", "V", "C"] {
            m.insert(
                dim.into(),
                burden
                    .get(dim)
                    .and_then(|e| e.get(which))
                    .cloned()
                    .unwrap_or(Value::Null),
            );
        }
        Value::Object(m)
    };
    let get = |v: &Value, k: &str| v.get(k).cloned().unwrap_or(Value::Null);
    let mut put = |k: &str, v: Value| {
        row.insert(k.to_owned(), v);
    };
    put("decision", get(gate, "decision"));
    put("next_step", get(gate, "next_step"));
    put("burden_summary", get(report, "burden_summary"));
    put("burden_before", side("before"));
    put("burden_after", side("after"));
    put(
        "net_sloc",
        report
            .get("growth")
            .map_or(Value::Null, |g| get(g, "net_sloc")),
    );
    for key in [
        "measurement_profile",
        "measurement_contract_id",
        "snapshot_id_before",
        "snapshot_id_after",
    ] {
        put(key, get(report, key));
    }
    put("gate_policy_id", get(gate, "gate_policy_id"));
    put("source_sha256_before", get(report, "source_sha256_before"));
    put("source_sha256_after", get(report, "source_sha256_after"));
    put("margin", get(gate, "margin"));
    put("primary", get(gate, "primary"));
    put(
        "evidence",
        gate.get("evidence").cloned().unwrap_or_else(|| json!([])),
    );
    Value::Object(row)
}

/// Append a row to `dir/emi-trajectory.jsonl`. Failures warn and return
/// false (logging never blocks a gate), unless `strict`.
pub fn append(dir: &Path, row: &Value, strict: bool) -> Result<bool, String> {
    let write = || -> std::io::Result<()> {
        std::fs::create_dir_all(dir)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join("emi-trajectory.jsonl"))?;
        file.write_all((emi_ledger::pyfmt::dumps_line(row) + "\n").as_bytes())
    };
    match write() {
        Ok(()) => Ok(true),
        Err(e) if strict => Err(format!("trajectory log failed in strict mode ({e})")),
        Err(e) => {
            eprintln!("warning: trajectory log failed ({e}); gate result stands");
            Ok(false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_is_python_isoformat() {
        let t = utc_now_iso();
        let (date, rest) = t.split_once('T').unwrap_or_default();
        assert_eq!(date.len(), 10);
        assert!(rest.ends_with("+00:00"));
        assert!(matches!(rest.len(), 14 | 21), "{t}");
    }

    #[test]
    fn row_keeps_the_oracle_field_order() {
        let mut record = Map::new();
        record.insert("repo".into(), json!("."));
        record.insert("measurement_profile".into(), json!("first"));
        let report =
            json!({"measurement_profile": "p", "burden_change": {"E": {"before": [1.0, 2.0]}}});
        let gate = json!({"decision": "pass", "gate_policy_id": "g"});
        let row = row(record, &report, &gate);
        let keys: Vec<&String> = row
            .as_object()
            .map(|m| m.keys().collect())
            .unwrap_or_default();
        assert_eq!(keys[..3], ["timestamp", "repo", "measurement_profile"]);
        assert_eq!(
            row["measurement_profile"], "p",
            "later value, first position"
        );
        assert_eq!(row["burden_before"]["E"], json!([1.0, 2.0]));
        assert_eq!(row["evidence"], json!([]));
    }
}
