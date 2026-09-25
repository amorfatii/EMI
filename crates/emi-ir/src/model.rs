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
}

#[derive(Clone, Debug, Serialize)]
pub struct Burden {
    #[serde(rename = "E")]
    pub erosion: Evidence,
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
}

#[derive(Clone, Debug, Serialize)]
pub struct InventoryEntry {
    pub file: String,
    pub state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// Measurement of one scope by one language adapter.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Measurement {
    pub measurement_profile: String,
    pub components: Vec<Component>,
    pub functions: Vec<FunctionRecord>,
    pub regions: Vec<RegionRecord>,
    pub source_sha256: BTreeMap<String, String>,
    pub errors: Vec<FileError>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallbacks: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inventory: Option<Vec<InventoryEntry>>,
}
