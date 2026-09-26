//! Gate flags shared by `emi review` and `emi diff`, with the oracle's
//! exit-code contract: 0 pass, 0 warn, 2 block (warn exits 2 with
//! `--fail-on-warn`), and 2 whenever scope is incomplete.

use std::path::PathBuf;

use clap::Args;
use serde_json::{Map, Value};

#[derive(Args, Clone)]
pub struct GateArgs {
    /// Add the block/warn/pass gate to the report and set the exit code.
    #[arg(long)]
    pub guardrail: bool,
    /// Exit 2 on warn as well as block (with --guardrail).
    #[arg(long)]
    pub fail_on_warn: bool,
    /// Burden units of guaranteed growth tolerated before blocking.
    #[arg(long, default_value_t = 10.0, value_name = "UNITS")]
    pub allow_growth: f64,
    /// Possible growth behind missing evidence tolerated before a warn
    /// (default: follows --allow-growth).
    #[arg(long, value_name = "UNITS")]
    pub allow_headroom: Option<f64>,
    /// Cap verdict evidence entries (0 gives margins only).
    #[arg(
        long,
        default_value_t = 5,
        value_name = "N",
        allow_negative_numbers = true
    )]
    pub max_evidence: i64,
    /// With --guardrail, emit only the token-cheap verdict payload.
    #[arg(long)]
    pub compact: bool,
    /// Append one JSONL row per gate run to DIR/emi-trajectory.jsonl.
    #[arg(long, value_name = "DIR")]
    pub log_trajectory: Option<PathBuf>,
    /// Fail (exit 2) instead of warning when the trajectory append fails.
    #[arg(long)]
    pub trajectory_strict: bool,
}

impl GateArgs {
    /// Flag combinations the oracle refuses before doing any work.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.max_evidence < 0 {
            anyhow::bail!("--max-evidence must be >= 0; use 0 for margins only");
        }
        if self.trajectory_strict && self.log_trajectory.is_none() {
            anyhow::bail!("--trajectory-strict needs --log-trajectory DIR");
        }
        if !self.guardrail && (self.compact || self.log_trajectory.is_some()) {
            anyhow::bail!(
                "--compact and --log-trajectory need --guardrail; re-run with --guardrail to get a verdict to compact or log"
            );
        }
        Ok(())
    }

    pub fn headroom(&self) -> f64 {
        self.allow_headroom.unwrap_or(self.allow_growth)
    }
}

/// Gate a review report (when `--guardrail`) and log the run. `record`
/// holds the trajectory row's leading fields; `allowance` and `headroom`
/// are appended to it. Returns the JSON to print and the exit code.
pub fn apply(
    report: Value,
    args: &GateArgs,
    mut record: Map<String, Value>,
) -> anyhow::Result<(Value, u8)> {
    if !args.guardrail {
        return Ok((report, 0));
    }
    let policy = emi_ledger::gate::Policy {
        fail_on_warn: args.fail_on_warn,
        allowance: args.allow_growth,
        headroom: args.allow_headroom,
        max_evidence: args.max_evidence,
        compact: args.compact,
    };
    let gated = emi_ledger::gate::apply(report, &policy).map_err(anyhow::Error::msg)?;
    if let Some(dir) = &args.log_trajectory {
        record.insert("allowance".into(), Value::from(args.allow_growth));
        record.insert("headroom".into(), Value::from(args.headroom()));
        let row = emi_diff::trajectory::row(record, &gated.report, &gated.gate);
        emi_diff::trajectory::append(dir, &row, args.trajectory_strict)
            .map_err(|e| anyhow::anyhow!("trajectory log failed in strict mode: {e}"))?;
    }
    Ok((gated.output, gated.code))
}
