//! EMI's Python adapter: everything that knows Python or pytest.
//!
//! - Syntax, on one parser (tree-sitter-python at the workspace pin):
//!   where functions and their bodies are ([`syntax`]), the functions a
//!   change touched ([`functions`]), single-token mutants ([`mutate`]).
//! - Runs that gather evidence, each sandboxed through `emi-exec`: which
//!   added tests prove the change ([`proving`]), coverage ([`coverage`],
//!   [`covrun`]), mutants of the changed lines ([`diffmut`], [`mutation`]),
//!   and the same inputs replayed against the old and new code ([`drift`]).

pub mod coverage;
pub mod covrun;
pub mod diffmut;
pub mod drift;
pub mod functions;
pub mod mutate;
pub mod mutation;
pub mod pins;
pub mod proving;
pub mod report;
pub mod sandbox;
pub mod standins;
pub mod syntax;

pub type Result<T> = std::result::Result<T, String>;
