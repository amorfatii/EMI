//! Replays the oracle's recorded answers (corpus/golden/ledger.jsonl, from
//! `tools/ledger_diff.py --record`): every printed report must hash the
//! same, and every refused change must be refused with the same message.

use emi_ledger::gate::{Policy, apply};
use emi_ledger::pyfmt::dumps_pretty;
use serde_json::Value;
use sha2::{Digest, Sha256};

fn policy(argv: &[Value]) -> (bool, Policy) {
    let args: Vec<&str> = argv.iter().filter_map(Value::as_str).collect();
    let value = |flag: &str| {
        args.iter()
            .position(|a| *a == flag)
            .and_then(|i| args.get(i + 1))
            .and_then(|v| v.parse::<f64>().ok())
    };
    #[allow(clippy::cast_possible_truncation)]
    let max_evidence = value("--max-evidence").map_or(5, |m| m as i64);
    let p = Policy {
        fail_on_warn: args.contains(&"--fail-on-warn"),
        allowance: value("--allow-growth").unwrap_or(10.0),
        headroom: value("--allow-headroom"),
        max_evidence,
        compact: args.contains(&"--compact"),
    };
    (args.contains(&"--guardrail"), p)
}

#[test]
fn reproduces_oracle_reports_and_refusals() {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../corpus/golden/ledger.jsonl"
    );
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut checked = 0;
    for (n, line) in text.lines().enumerate() {
        let row: Value = serde_json::from_str(line).unwrap_or_default();
        let input =
            emi_ledger::strict::loads(row["input"].as_str().unwrap_or("")).unwrap_or_default();
        let report = emi_ledger::review(&input);
        for run in row["runs"].as_array().into_iter().flatten() {
            checked += 1;
            if let Some(error) = run.get("error").and_then(Value::as_str) {
                assert_eq!(
                    report.as_ref().err().map(String::as_str),
                    Some(error),
                    "case {n}"
                );
                continue;
            }
            let Ok(report) = report.clone() else {
                panic!("case {n}: refused {report:?}")
            };
            let (gated, policy) = policy(run["argv"].as_array().map_or(&[], Vec::as_slice));
            let (out, code) = if gated {
                apply(report, &policy).unwrap_or_default()
            } else {
                (report, 0)
            };
            let digest = hex::encode(Sha256::digest(dumps_pretty(&out).as_bytes()));
            assert_eq!(
                Some(digest.as_str()),
                run["sha256"].as_str(),
                "case {n} {}",
                run["argv"]
            );
            assert_eq!(
                Some(u64::from(code)),
                run["code"].as_u64(),
                "case {n} {}",
                run["argv"]
            );
        }
    }
    assert!(checked > 900, "only {checked} runs replayed");
}
