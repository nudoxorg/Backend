//! Bounded causal reachability over already admitted facts.
//!
//! This is an internal traversal primitive, not a public impact service. Its
//! caller must select an exact relation root and supply only witnessed edges.
//! This returns a reachability set, with one representative explanation per
//! destination and strength, not every route through a many-to-many graph.
//! An adapter that needs every source occurrence must give occurrences distinct
//! node identities; collapsing them into an owner loses occurrence witnesses.
//! Witness-vector contents are conservatively budgeted; wire output and
//! container allocation overhead still need a policy before public use.
use std::collections::{BTreeMap, BTreeSet, VecDeque};

/// Stable identity supplied by the selected relation adapter, scoped to its root.
pub type Node = [u8; 32];
/// Stable identity of the relation occurrence which witnesses an edge.
pub type Witness = [u8; 32];

/// Weakest proof along a path. Candidate paths never become exact downstream.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Strength {
    Exact,
    Candidate,
}

/// A directional causal edge. Containment must not be supplied as an edge.
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub struct Edge {
    pub from: Node,
    pub to: Node,
    pub strength: Strength,
    pub witness: Witness,
}

/// Independent computational bounds. Zero is a valid bound.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub nodes: usize,
    pub edges: usize,
    pub candidates: usize,
    pub depth: usize,
    pub rows: usize,
    /// Cumulative bytes reserved for both row and queue witness vectors.
    pub witness_bytes: usize,
}

/// Why enumeration stopped. Incomplete results cannot establish absence.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Stop {
    Complete,
    Nodes,
    Edges,
    Candidates,
    Depth,
    Rows,
    Cancelled,
    /// The witness-vector byte budget was exhausted.
    WitnessBytes,
    /// An edge was indexed under a different origin.
    InvalidAdjacency,
}

/// One deterministic causal explanation. Exact and candidate routes to the
/// same destination are separate rows, preserving the proof of each route.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Row {
    pub node: Node,
    pub strength: Strength,
    pub witnesses: Vec<Witness>,
}

/// Coverage gaps are supplied by the owner and retained even for exact-only
/// presentation. `stop != Complete` is an additional enumeration gap.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Outcome {
    pub rows: Vec<Row>,
    pub stop: Stop,
    pub visited: usize,
    pub evaluated_edges: usize,
    pub candidate_expansions: usize,
    /// Conservative cumulative witness-vector bytes reserved.
    pub witness_bytes: usize,
    pub gaps: Vec<String>,
}

/// Traverse a pre-indexed relation. Each adjacency list must be in deterministic
/// order. Callers must bind `adjacency` and seeds to one immutable exact root;
/// no cross-root or spelling-based identity joins are performed here.
pub fn traverse<F: FnMut() -> bool>(
    adjacency: &BTreeMap<Node, Vec<Edge>>,
    seeds: &[Node],
    limits: Limits,
    gaps: Vec<String>,
    mut cancelled: F,
) -> Outcome {
    let mut out = Outcome {
        rows: Vec::new(),
        stop: Stop::Complete,
        visited: 0,
        evaluated_edges: 0,
        candidate_expansions: 0,
        witness_bytes: 0,
        gaps,
    };
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::new();
    // Bound copying and sorting before touching a caller-supplied seed list.
    // Duplicate-heavy over-limit inputs are conservatively refused too.
    if seeds.len() > limits.nodes {
        out.stop = Stop::Nodes;
        return out;
    }
    let mut ordered_seeds = seeds.to_vec();
    ordered_seeds.sort_unstable();
    ordered_seeds.dedup();
    for seed in ordered_seeds {
        if seen.len() == limits.nodes {
            out.stop = Stop::Nodes;
            return out;
        }
        seen.insert((seed, Strength::Exact));
        queue.push_back((seed, Strength::Exact, Vec::new(), 0usize));
    }
    out.visited = seen.len();
    while let Some((node, strength, path, depth)) = queue.pop_front() {
        if cancelled() {
            out.stop = Stop::Cancelled;
            break;
        }
        let Some(edges) = adjacency.get(&node) else {
            continue;
        };
        for edge in edges {
            if cancelled() {
                out.stop = Stop::Cancelled;
                return out;
            }
            if out.evaluated_edges == limits.edges {
                out.stop = Stop::Edges;
                return out;
            }
            out.evaluated_edges += 1;
            if edge.from != node {
                out.stop = Stop::InvalidAdjacency;
                return out;
            }
            let next_strength = strength.max(edge.strength);
            if seen.contains(&(edge.to, next_strength)) {
                continue;
            }
            if depth == limits.depth {
                out.stop = Stop::Depth;
                return out;
            }
            if next_strength == Strength::Candidate {
                if out.candidate_expansions == limits.candidates {
                    out.stop = Stop::Candidates;
                    return out;
                }
            }
            if out.visited == limits.nodes {
                out.stop = Stop::Nodes;
                return out;
            }
            if out.rows.len() == limits.rows {
                out.stop = Stop::Rows;
                return out;
            }
            let Some(witness_bytes) = path
                .len()
                .checked_add(1)
                .and_then(|n| n.checked_mul(core::mem::size_of::<Witness>()))
                .and_then(|n| n.checked_mul(2))
                .and_then(|n| out.witness_bytes.checked_add(n))
            else {
                out.stop = Stop::WitnessBytes;
                return out;
            };
            if witness_bytes > limits.witness_bytes {
                out.stop = Stop::WitnessBytes;
                return out;
            }
            out.witness_bytes = witness_bytes;
            if next_strength == Strength::Candidate {
                out.candidate_expansions += 1;
            }
            let mut next_path = path.clone();
            next_path.push(edge.witness);
            seen.insert((edge.to, next_strength));
            out.visited += 1;
            out.rows.push(Row {
                node: edge.to,
                strength: next_strength,
                witnesses: next_path.clone(),
            });
            queue.push_back((edge.to, next_strength, next_path, depth + 1));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id(n: u8) -> Node {
        [n; 32]
    }
    fn edge(a: u8, b: u8, strength: Strength, w: u8) -> Edge {
        Edge {
            from: id(a),
            to: id(b),
            strength,
            witness: id(w),
        }
    }
    fn limits() -> Limits {
        Limits {
            nodes: 30,
            edges: 30,
            candidates: 30,
            depth: 30,
            rows: 30,
            witness_bytes: 4096,
        }
    }
    #[test]
    fn candidate_does_not_upgrade_and_cycle_terminates() {
        let graph = BTreeMap::from([
            (
                id(1),
                vec![
                    edge(1, 2, Strength::Candidate, 10),
                    edge(1, 3, Strength::Exact, 11),
                ],
            ),
            (id(2), vec![edge(2, 4, Strength::Exact, 12)]),
            (id(3), vec![edge(3, 4, Strength::Exact, 13)]),
            (id(4), vec![edge(4, 2, Strength::Exact, 14)]),
        ]);
        let o = traverse(
            &graph,
            &[id(1)],
            limits(),
            vec!["unknown provider".into()],
            || false,
        );
        assert_eq!(o.stop, Stop::Complete);
        assert!(o.rows.iter().any(|r| r.node == id(4)
            && r.strength == Strength::Candidate
            && r.witnesses == vec![id(10), id(12)]));
        assert!(o.rows.iter().any(|r| r.node == id(4)
            && r.strength == Strength::Exact
            && r.witnesses == vec![id(11), id(13)]));
        assert_eq!(o.gaps, ["unknown provider"]);
    }
    #[test]
    fn malformed_adjacency_and_oversized_seed_input_are_refused() {
        let graph = BTreeMap::from([(id(1), vec![edge(2, 3, Strength::Exact, 10)])]);
        assert_eq!(
            traverse(&graph, &[id(1)], limits(), vec![], || false).stop,
            Stop::InvalidAdjacency
        );
        let mut l = limits();
        l.nodes = 1;
        assert_eq!(
            traverse(&graph, &[id(1), id(1)], l, vec![], || false).stop,
            Stop::Nodes
        );
    }
    #[test]
    fn visited_cycle_at_depth_limit_is_complete() {
        let graph = BTreeMap::from([
            (id(1), vec![edge(1, 2, Strength::Exact, 10)]),
            (id(2), vec![edge(2, 1, Strength::Exact, 11)]),
        ]);
        let mut l = limits();
        l.depth = 1;
        let result = traverse(&graph, &[id(1)], l, vec![], || false);
        assert_eq!(result.stop, Stop::Complete);
        assert_eq!(result.evaluated_edges, 2);
    }
    #[test]
    fn duplicate_candidate_does_not_consume_expansion_budget() {
        let graph = BTreeMap::from([(
            id(1),
            vec![
                edge(1, 2, Strength::Candidate, 10),
                edge(1, 2, Strength::Candidate, 11),
            ],
        )]);
        let mut l = limits();
        l.candidates = 1;
        let result = traverse(&graph, &[id(1)], l, vec![], || false);
        assert_eq!(result.stop, Stop::Complete);
        assert_eq!(result.candidate_expansions, 1);
        assert_eq!(result.rows[0].witnesses, vec![id(10)]);
    }
    #[test]
    fn bounds_and_cancellation_are_not_complete() {
        let graph = BTreeMap::from([(id(1), vec![edge(1, 2, Strength::Candidate, 10)])]);
        let mut l = limits();
        l.edges = 0;
        assert_eq!(
            traverse(&graph, &[id(1)], l, vec![], || false).stop,
            Stop::Edges
        );
        l = limits();
        l.witness_bytes = 63;
        assert_eq!(
            traverse(&graph, &[id(1)], l, vec![], || false).stop,
            Stop::WitnessBytes
        );
        l = limits();
        l.candidates = 0;
        assert_eq!(
            traverse(&graph, &[id(1)], l, vec![], || false).stop,
            Stop::Candidates
        );
        assert_eq!(
            traverse(&graph, &[id(1)], limits(), vec![], || true).stop,
            Stop::Cancelled
        );
    }
}
