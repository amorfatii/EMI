//! EMI's kernel: everything a decision depends on, with no I/O.
//!
//! The rest of EMI gathers evidence by running things; this crate only
//! encodes, hashes and decides. Keeping it free of process, file and
//! network access is what makes a decision a pure function of its inputs:
//! the same evidence always yields the same decision and the same bytes.

pub mod glob;
pub mod hash;
pub mod json;
pub mod strict;
pub mod time;
