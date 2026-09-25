//! Grouping of clone candidates across a scope, in the probes' order:
//! groups appear in order of their first occurrence (file order, then region
//! order), and only signatures seen more than once form a group.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::CloneOccurrence;

/// Collects `(signature, occurrence)` pairs and yields the clone groups.
#[derive(Default)]
pub struct Grouper {
    order: Vec<[u8; 32]>,
    by_signature: HashMap<[u8; 32], Vec<CloneOccurrence>>,
}

impl Grouper {
    pub fn add(&mut self, signature: [u8; 32], file: &str, lines: &BTreeSet<u32>) {
        let entry = self.by_signature.entry(signature).or_insert_with(|| {
            self.order.push(signature);
            Vec::new()
        });
        entry.push(CloneOccurrence {
            file: file.to_owned(),
            lines: lines.iter().copied().collect(),
        });
    }

    /// Groups with more than one occurrence, plus the lines each file loses to them.
    pub fn finish(
        mut self,
        clone_lines: &mut BTreeMap<String, BTreeSet<u32>>,
    ) -> Vec<Vec<CloneOccurrence>> {
        let mut groups = Vec::new();
        for sig in self.order {
            let Some(occurrences) = self.by_signature.remove(&sig) else {
                continue;
            };
            if occurrences.len() < 2 {
                continue;
            }
            for occ in &occurrences {
                clone_lines
                    .entry(occ.file.clone())
                    .or_default()
                    .extend(occ.lines.iter().copied());
            }
            groups.push(occurrences);
        }
        groups
    }
}
