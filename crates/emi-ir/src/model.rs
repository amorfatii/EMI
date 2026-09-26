//! Measurement records. Field names and JSON shapes follow the v0.5 probe
//! output so the golden corpus can be compared field by field.

use std::collections::BTreeMap;

use serde::Serialize;
use serde::ser::{SerializeMap, Serializer};

/// Mass evidence for one dimension: exactly known, or a known part plus
/// source lines whose contribution is unknown (bounds `[known, known + unknown]`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Evidence {
    Exact(f64),
    Partial { known_mass: f64, unknown_sloc: u32 },
}

impl Evidence {
    /// `[low, high]` mass bounds.
    pub fn bounds(self) -> (f64, f64) {
        match self {
            Evidence::Exact(v) => (v, v),
            Evidence::Partial {
                known_mass,
                unknown_sloc,
            } => (known_mass, known_mass + f64::from(unknown_sloc)),
        }
    }
}

impl Serialize for Evidence {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match *self {
            Evidence::Exact(v) => s.serialize_f64(v),
            Evidence::Partial {
                known_mass,
                unknown_sloc,
            } => {
                let mut map = s.serialize_map(Some(2))?;
                map.serialize_entry("known_mass", &known_mass)?;
                map.serialize_entry("unknown_sloc", &unknown_sloc)?;
                map.end()
            }
        }
    }
}

/// One measured region (a function, or the leftover module/class lines).
#[derive(Clone, Debug, Serialize)]
pub struct FunctionRecord {
    pub file: String,
    pub function: String,
    pub line: u32,
    pub sloc: u32,
    /// None when the region touches unparseable lines.
    pub cc: Option<u32>,
    pub erosion_mass: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub erosion_exact: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lines: Option<Vec<u32>>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub coupling: Option<CouplingRecord>,
}

/// A call whose target could not be resolved statically.
#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct UnresolvedCall {
    pub line: Option<u32>,
    pub column: Option<u32>,
    pub reason: String,
}

/// Coupling evidence of one region.
#[derive(Clone, Debug, Default, Serialize)]
pub struct CouplingRecord {
    pub resolved_fanout: u32,
    pub resolved_dependencies: Vec<String>,
    pub local_callees: Vec<String>,
    /// Unresolved calls outside a static cycle make coupling unknown.
    pub coupling_unknown: bool,
    pub unresolved_calls: Vec<UnresolvedCall>,
    pub in_static_cycle: bool,
    /// Regions of the same file whose local callees name this region.
    pub local_callers: Vec<String>,
}

/// Region inventory entry: qualified id plus a content hash of its lines.
#[derive(Clone, Debug, Serialize)]
pub struct RegionRecord {
    pub file: String,
    pub id: String,
    pub sloc: u32,
    pub sha: String,
    #[serde(rename = "E")]
    pub erosion: Evidence,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_cycle: Option<bool>,
    #[serde(rename = "C", skip_serializing_if = "Option::is_none")]
    pub coupling: Option<Evidence>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Burden {
    #[serde(rename = "E")]
    pub erosion: Evidence,
    #[serde(rename = "V", skip_serializing_if = "Option::is_none")]
    pub redundancy: Option<u32>,
    #[serde(rename = "C", skip_serializing_if = "Option::is_none")]
    pub coupling: Option<Evidence>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Component {
    pub id: String,
    pub sloc: u32,
    pub burden: Burden,
}

/// A file that could not be measured, or was measured only in part.
#[derive(Clone, Debug, Serialize)]
pub struct FileError {
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stage: Option<String>,
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_lines: Option<Vec<u32>>,
    /// Raw-byte hash of a file that was read but failed later (Python).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct InventoryEntry {
    pub file: String,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// A locatable rule finding (waste, masking, verbosity or test-value rule).
#[derive(Clone, Debug, Default, Serialize, PartialEq)]
pub struct Finding {
    pub file: String,
    pub rule: String,
    pub lines: Vec<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
    /// Comment-bloat accounting.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment_lines: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub header_free: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counted: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allowed: Option<u32>,
}

impl Finding {
    pub fn new(file: &str, rule: &str, lines: Vec<u32>) -> Self {
        Finding {
            file: file.to_owned(),
            rule: rule.to_owned(),
            lines,
            ..Finding::default()
        }
    }
}

/// One occurrence of a clone: the body lines it covers in a file.
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct CloneOccurrence {
    pub file: String,
    pub lines: Vec<u32>,
}

/// Measurement of one scope by one language adapter.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Measurement {
    pub measurement_profile: String,
    pub components: Vec<Component>,
    pub functions: Vec<FunctionRecord>,
    pub regions: Vec<RegionRecord>,
    pub clone_groups: Vec<Vec<CloneOccurrence>>,
    pub clone_groups_normalized: Vec<Vec<CloneOccurrence>>,
    pub verbosity_findings: Vec<Finding>,
    pub static_dependency_cycles: Vec<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub test_value_findings: Option<Vec<Finding>>,
    pub source_sha256: BTreeMap<String, String>,
    pub errors: Vec<FileError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallbacks: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inventory: Option<Vec<InventoryEntry>>,
    /// Count of inventory entries per state, in first-seen order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inventory_summary: Option<serde_json::Map<String, serde_json::Value>>,
    /// The extraction contract: two measurements are comparable only when
    /// their options are equal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measurement_options: Option<serde_json::Map<String, serde_json::Value>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub measurement_contract_id: Option<String>,
    /// `ok`, `no-eligible`, `all-failed` (Python also reports `test-only`
    /// and `empty` through `ScanError::NoSources`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope_complete: Option<bool>,
}
