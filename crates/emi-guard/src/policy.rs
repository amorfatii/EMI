//! What a change may do: the scope envelope, permitted path classes,
//! per-guardrail tiers, and growth budgets. Parsed from JSON (a task
//! contract's `guardrails` block, or `emi guard --policy FILE`).

use std::collections::BTreeMap;

use emi_scan::classify::PathClass;
use serde_json::Value;

/// How a violation of a guardrail is enforced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Tier {
    /// Informational only: reported, never affects the decision.
    Off,
    /// Needs a human look; the change is not accepted automatically.
    Review,
    /// Unambiguous violation; the change is rejected.
    Block,
}

impl Tier {
    pub fn as_str(self) -> &'static str {
        match self {
            Tier::Off => "off",
            Tier::Review => "review",
            Tier::Block => "block",
        }
    }

    fn parse(s: &str) -> Result<Tier, String> {
        match s {
            "off" => Ok(Tier::Off),
            "review" => Ok(Tier::Review),
            "block" => Ok(Tier::Block),
            other => Err(format!(
                "unknown guardrail tier {other:?} (block, review or off)"
            )),
        }
    }
}

/// Default tier by guardrail family: unambiguous violations block,
/// judgment calls need review.
pub fn default_tier(id: &str) -> Tier {
    let family = id.split('.').next().unwrap_or(id);
    match family {
        "scope" | "tests" | "prohibited" => Tier::Block,
        _ => Tier::Review,
    }
}

#[derive(Clone, Debug)]
pub struct Policy {
    /// Globs (posix fnmatch, `*` spans `/`) a change may touch.
    pub permitted: Vec<String>,
    pub excluded: Vec<String>,
    /// Paths only a controller revision may change (checks, runtime config).
    pub protected: Vec<String>,
    /// Allowed change kinds: add, modify, delete, rename.
    pub change_kinds: Vec<String>,
    /// Path classes a change may touch without a `prohibited.*` violation.
    pub allow_classes: Vec<PathClass>,
    /// Per-guardrail tier overrides.
    pub tiers: BTreeMap<String, Tier>,
    pub max_net_sloc: Option<i64>,
    pub max_files: Option<usize>,
}

impl Default for Policy {
    fn default() -> Self {
        Policy {
            permitted: vec!["*".to_owned()],
            excluded: Vec::new(),
            protected: Vec::new(),
            change_kinds: ["add", "modify", "delete", "rename"]
                .map(str::to_owned)
                .to_vec(),
            allow_classes: Vec::new(),
            tiers: BTreeMap::new(),
            max_net_sloc: None,
            max_files: None,
        }
    }
}

fn strings(v: &Value, key: &str) -> Result<Option<Vec<String>>, String> {
    match v.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Array(items)) => items
            .iter()
            .map(|i| {
                i.as_str()
                    .map(str::to_owned)
                    .ok_or_else(|| format!("{key} must be a list of strings"))
            })
            .collect::<Result<Vec<_>, _>>()
            .map(Some),
        Some(_) => Err(format!("{key} must be a list of strings")),
    }
}

fn class_named(name: &str) -> Result<PathClass, String> {
    use PathClass::*;
    [
        Vendored, SecretLike, Lockfile, Ci, TestConfig, Test, Generated, Docs, Product, Other,
    ]
    .into_iter()
    .find(|c| c.as_str() == name)
    .ok_or_else(|| format!("unknown path class {name:?}"))
}

impl Policy {
    /// Parse a policy object; missing keys keep their defaults.
    pub fn from_json(v: &Value) -> Result<Policy, String> {
        let mut p = Policy::default();
        if !v.is_object() {
            return Err("guardrail policy must be a JSON object".to_owned());
        }
        if let Some(x) = strings(v, "permitted_paths")? {
            p.permitted = x;
        }
        if let Some(x) = strings(v, "excluded_paths")? {
            p.excluded = x;
        }
        if let Some(x) = strings(v, "protected_paths")? {
            p.protected = x;
        }
        if let Some(x) = strings(v, "change_kinds")? {
            p.change_kinds = x;
        }
        if let Some(x) = strings(v, "allow_classes")? {
            p.allow_classes = x.iter().map(|c| class_named(c)).collect::<Result<_, _>>()?;
        }
        if let Some(obj) = v.get("tiers").and_then(Value::as_object) {
            for (id, tier) in obj {
                let t = tier
                    .as_str()
                    .ok_or_else(|| format!("tier of {id} must be a string"))?;
                p.tiers.insert(id.clone(), Tier::parse(t)?);
            }
        }
        p.max_net_sloc = v.get("max_net_sloc").and_then(Value::as_i64);
        p.max_files = v
            .get("max_files")
            .and_then(Value::as_u64)
            .map(|n| n as usize);
        Ok(p)
    }

    /// Tier of guardrail `id`: an exact override, then a family override
    /// (`"tests": "review"`), then the default.
    pub fn tier(&self, id: &str) -> Tier {
        let family = id.split('.').next().unwrap_or(id);
        self.tiers
            .get(id)
            .or_else(|| self.tiers.get(family))
            .copied()
            .unwrap_or_else(|| default_tier(id))
    }
}
