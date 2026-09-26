//! EMI intermediate representation: the records every language adapter
//! produces, plus the shared rules they are measured under.

pub mod burden;
pub mod clones;
pub mod graph;
pub mod model;
pub mod pyrepr;
pub mod pytext;
pub mod regions;

pub use model::{
    Burden, CloneOccurrence, Component, CouplingRecord, Evidence, FileError, Finding,
    FunctionRecord, InventoryEntry, Measurement, RegionRecord, UnresolvedCall,
};
