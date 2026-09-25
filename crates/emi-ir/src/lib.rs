//! EMI intermediate representation: the records every language adapter
//! produces, plus the shared rules they are measured under.

pub mod burden;
pub mod model;
pub mod pytext;
pub mod regions;

pub use model::{
    Burden, Component, Evidence, FileError, FunctionRecord, InventoryEntry, Measurement,
    RegionRecord,
};
