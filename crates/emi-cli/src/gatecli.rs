//! Gate flags shared by `emi review` and `emi diff`, with the oracle's
//! exit-code contract: 0 pass, 0 warn, 2 block (warn exits 2 with
//! `--fail-on-warn`), and 2 whenever scope is incomplete.

use clap::Args;
use serde_json::Value;

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
}

/// Apply the gate to a review report; returns the JSON to print and the
/// exit code.
pub fn apply(report: Value, args: &GateArgs) -> anyhow::Result<(Value, u8)> {
    if args.max_evidence < 0 {
        anyhow::bail!("--max-evidence must be >= 0; use 0 for margins only");
    }
    if !args.guardrail {
        if args.compact {
            anyhow::bail!(
                "--compact and --log-trajectory need --guardrail; re-run with --guardrail to get a verdict to compact or log"
            );
        }
        return Ok((report, 0));
    }
    let policy = emi_ledger::gate::Policy {
        fail_on_warn: args.fail_on_warn,
        allowance: args.allow_growth,
        headroom: args.allow_headroom,
        max_evidence: args.max_evidence,
        compact: args.compact,
    };
    emi_ledger::gate::apply(report, &policy).map_err(anyhow::Error::msg)
}
