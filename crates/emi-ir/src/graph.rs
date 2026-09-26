//! Import-graph utilities shared by the adapters.

use std::collections::{BTreeSet, HashMap, HashSet};

/// Tarjan strongly connected components, iterative, in the oracle's order:
/// roots and children visited sorted, groups sorted, singletons omitted.
pub fn strongly_connected(
    graph: &std::collections::BTreeMap<String, BTreeSet<String>>,
) -> Vec<Vec<String>> {
    let mut index_of: HashMap<&str, usize> = HashMap::new();
    let mut lowlink: HashMap<&str, usize> = HashMap::new();
    let mut on_stack: HashSet<&str> = HashSet::new();
    let (mut stack, mut groups, mut counter) = (Vec::new(), Vec::new(), 0usize);
    let empty = BTreeSet::new();
    for root in graph.keys() {
        if index_of.contains_key(root.as_str()) {
            continue;
        }
        index_of.insert(root, counter);
        lowlink.insert(root, counter);
        counter += 1;
        stack.push(root.as_str());
        on_stack.insert(root);
        let mut work: Vec<(&str, std::collections::btree_set::Iter<String>)> =
            vec![(root.as_str(), graph.get(root).unwrap_or(&empty).iter())];
        while let Some((node, children)) = work.last_mut() {
            let node = *node;
            let mut descended = None;
            for child in children.by_ref() {
                if !index_of.contains_key(child.as_str()) {
                    index_of.insert(child, counter);
                    lowlink.insert(child, counter);
                    counter += 1;
                    stack.push(child);
                    on_stack.insert(child);
                    descended = Some(child.as_str());
                    break;
                } else if on_stack.contains(child.as_str())
                    && index_of[child.as_str()] < lowlink[node]
                {
                    lowlink.insert(node, index_of[child.as_str()]);
                }
            }
            if let Some(child) = descended {
                work.push((child, graph.get(child).unwrap_or(&empty).iter()));
                continue;
            }
            work.pop();
            if let Some((parent, _)) = work.last()
                && lowlink[node] < lowlink[*parent]
            {
                lowlink.insert(parent, lowlink[node]);
            }
            if lowlink[node] == index_of[node] {
                let mut group = Vec::new();
                while let Some(member) = stack.pop() {
                    on_stack.remove(member);
                    group.push(member.to_owned());
                    if member == node {
                        break;
                    }
                }
                if group.len() > 1 {
                    group.sort();
                    groups.push(group);
                }
            }
        }
    }
    groups.sort();
    groups
}
