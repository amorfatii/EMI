//! EMI ledger: the calculator and gate that turn two measurements into a
//! review report and a block/warn/pass verdict, ported from the v0.5.1
//! oracle (`emi.py`, `emi_contracts.py`) at output parity.

pub mod calc;
pub mod contracts;
pub mod gate;
pub mod pyfmt;
pub mod review;
pub mod strict;

pub use gate::{accept_rewrite, compact_verdict, guardrail};
pub use review::review;
