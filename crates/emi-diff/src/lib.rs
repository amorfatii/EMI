//! `emi diff`: measure a change between two git revisions and gate it.

pub mod backtest;
pub mod change;
pub mod functions;
pub mod hunks;
pub mod snapshot;
pub mod surface;
pub mod trajectory;

pub use change::{Request, build_change};
