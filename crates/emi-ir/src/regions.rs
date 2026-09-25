//! Region assignment shared by every adapter: functions claim eligible lines
//! innermost-first, the remainder forms one module region, and complexity
//! events are attributed to the region owning their start line.

use std::collections::{BTreeSet, HashMap};

/// Name reported for the leftover region, as in the v0.5 probes.
pub const MODULE_REGION: &str = "<module/class blocks>";

/// A function-like node offered for region ownership.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub name: String,
    pub qname: String,
    /// Line reported in function records.
    pub line: u32,
    /// First line owned (includes decorators).
    pub start: u32,
    /// Last line owned.
    pub end: u32,
    /// Sort key: smaller claims first; adapters encode their oracle's order.
    pub key: (u32, u64, u64),
}

#[derive(Clone, Debug)]
pub struct Region {
    pub name: String,
    pub qname: String,
    pub line: u32,
    pub lines: BTreeSet<u32>,
    pub is_module: bool,
}

/// Assign eligible lines to regions. Candidates that end up owning no line
/// (for example overload stubs) produce no region.
pub fn assign(
    mut candidates: Vec<Candidate>,
    eligible: &BTreeSet<u32>,
    module_qname: &str,
) -> Vec<Region> {
    candidates.sort_by_key(|c| c.key);
    let mut owned = BTreeSet::new();
    let mut regions = Vec::new();
    for c in candidates {
        let lines: BTreeSet<u32> = eligible
            .range(c.start..=c.end)
            .filter(|ln| !owned.contains(*ln))
            .copied()
            .collect();
        if lines.is_empty() {
            continue;
        }
        owned.extend(lines.iter().copied());
        regions.push(Region {
            name: c.name,
            qname: c.qname,
            line: c.line,
            lines,
            is_module: false,
        });
    }
    let rest: BTreeSet<u32> = eligible.difference(&owned).copied().collect();
    if !rest.is_empty() {
        regions.push(Region {
            name: MODULE_REGION.to_owned(),
            qname: module_qname.to_owned(),
            line: 1,
            lines: rest,
            is_module: true,
        });
    }
    regions
}

/// Cyclomatic complexity per region: 1 plus the increments of every branch
/// event whose start line the region owns.
pub fn complexity(regions: &[Region], events: &[(u32, u32)]) -> Vec<u32> {
    let mut by_line: HashMap<u32, usize> = HashMap::new();
    for (idx, r) in regions.iter().enumerate() {
        for &ln in &r.lines {
            by_line.insert(ln, idx);
        }
    }
    let mut counts = vec![1u32; regions.len()];
    for &(line, inc) in events {
        if let Some(&idx) = by_line.get(&line) {
            counts[idx] += inc;
        }
    }
    counts
}

/// SHA-256 of the region's source lines joined by `\n`, as the probes'
/// region inventory computes it. `lines_of_source` is the adapter's own
/// line split (Python probe: `str.splitlines`; JS probe: `split("\n")`).
pub fn content_sha(lines_of_source: &[&str], lines: &BTreeSet<u32>) -> String {
    let mut buf = String::new();
    for (i, &ln) in lines.iter().enumerate() {
        if i > 0 {
            buf.push('\n');
        }
        if let Some(text) = lines_of_source.get(ln as usize - 1) {
            buf.push_str(text);
        }
    }
    crate::pytext::sha256_hex(buf.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(name: &str, start: u32, end: u32) -> Candidate {
        Candidate {
            name: name.into(),
            qname: name.into(),
            line: start,
            start,
            end,
            key: (end - start, u64::from(start), 0),
        }
    }

    #[test]
    fn inner_functions_claim_first_and_rest_is_module() {
        let eligible: BTreeSet<u32> = [1, 2, 3, 4, 5, 7].into_iter().collect();
        let regions = assign(
            vec![cand("outer", 2, 5), cand("inner", 3, 4)],
            &eligible,
            "<module>",
        );
        let got: Vec<(&str, Vec<u32>)> = regions
            .iter()
            .map(|r| (r.name.as_str(), r.lines.iter().copied().collect()))
            .collect();
        assert_eq!(
            got,
            vec![
                ("inner", vec![3, 4]),
                ("outer", vec![2, 5]),
                (MODULE_REGION, vec![1, 7])
            ]
        );
        let cc = complexity(&regions, &[(3, 1), (5, 2), (7, 1), (9, 5)]);
        assert_eq!(cc, vec![2, 3, 2]);
    }
}
