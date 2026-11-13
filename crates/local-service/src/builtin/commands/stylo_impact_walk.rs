//! Candidate-only custom-property reachability over already admitted native
//! declaration and dependency rows. Every returned node is an occurrence;
//! names select candidates and never prove a provider, cascade or DOM match.
use std::collections::{BTreeMap, BTreeSet, VecDeque};

pub(super) type Key = [u8; 32];
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub(super) struct Owner {
    pub host: Key,
    pub rule: Vec<u32>,
    pub ordinal: u32,
}
#[derive(Clone, Debug)]
pub(super) struct Declaration {
    pub id: Key,
    pub owner: Owner,
    pub custom_property: Option<String>,
}
#[derive(Clone, Debug)]
pub(super) struct Dependency {
    pub id: Key,
    pub owner: Owner,
    pub name: String,
}
/// Independent indexing, expansion, depth, result, and witness-memory caps.
#[derive(Clone, Copy, Debug)]
pub(super) struct Limits {
    pub input_rows: usize,
    pub input_bytes: usize,
    pub edges: usize,
    pub nodes: usize,
    pub depth: usize,
    pub rows: usize,
    pub witness_bytes: usize,
    pub temporary_bytes: usize,
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Stop {
    Complete,
    Input,
    Edges,
    Nodes,
    Depth,
    Rows,
    WitnessBytes,
    Cancelled,
    Invalid,
}
/// A candidate native dependency occurrence with its exact native owner, when
/// captured. Witnesses retain seed, candidate occurrence, and exact owner IDs.
#[derive(Debug, Eq, PartialEq)]
pub(super) struct Row {
    pub dependency: Key,
    pub host: Key,
    pub owner: Option<Key>,
    pub witness: Box<[Key]>,
}
#[derive(Debug)]
pub(super) struct Outcome {
    pub rows: Vec<Row>,
    pub stop: Stop,
    pub edges: usize,
    pub nodes: usize,
    pub witness_bytes: usize,
    pub missing_owner: bool,
}
struct Provider<'a> {
    name: &'a str,
    path: Box<[Key]>,
    depth: usize,
}

/// A result is always candidate-only, even after an exact ownership step. It
/// retains one representative path per dependency occurrence, not all paths.
/// Coverage and producer/source gaps must be carried separately by the owner.
pub(super) fn walk<'a, F: FnMut() -> bool>(
    seed: Key,
    seed_name: &'a str,
    declarations: &'a [Declaration],
    dependencies: &[Dependency],
    limits: Limits,
    mut cancelled: F,
) -> Outcome {
    let mut out = Outcome {
        rows: Vec::new(),
        stop: Stop::Complete,
        edges: 0,
        nodes: 0,
        witness_bytes: 0,
        missing_owner: false,
    };
    if cancelled() {
        out.stop = Stop::Cancelled;
        return out;
    }
    let Some(rows) = declarations.len().checked_add(dependencies.len()) else {
        out.stop = Stop::Input;
        return out;
    };
    if rows > limits.input_rows || seed_name.len() > limits.input_bytes {
        out.stop = Stop::Input;
        return out;
    }
    let mut input_bytes = seed_name.len();
    for (owner, name) in declarations
        .iter()
        .map(|d| (&d.owner, d.custom_property.as_deref().unwrap_or("")))
        .chain(dependencies.iter().map(|d| (&d.owner, d.name.as_str())))
    {
        if cancelled() {
            out.stop = Stop::Cancelled;
            return out;
        }
        let Some(next) = owner
            .rule
            .len()
            .checked_mul(4)
            .and_then(|n| n.checked_add(name.len()))
            .and_then(|n| input_bytes.checked_add(n))
        else {
            out.stop = Stop::Input;
            return out;
        };
        input_bytes = next;
        if input_bytes > limits.input_bytes {
            out.stop = Stop::Input;
            return out;
        }
    }
    let mut owners = BTreeMap::new();
    let mut ids = BTreeSet::from([seed]);
    for declaration in declarations {
        if cancelled() {
            out.stop = Stop::Cancelled;
            return out;
        }
        if !ids.insert(declaration.id) || owners.insert(&declaration.owner, declaration).is_some() {
            out.stop = Stop::Invalid;
            return out;
        }
    }
    let mut by_name: BTreeMap<&str, Vec<&Dependency>> = BTreeMap::new();
    for dependency in dependencies {
        if cancelled() {
            out.stop = Stop::Cancelled;
            return out;
        }
        if !ids.insert(dependency.id) {
            out.stop = Stop::Invalid;
            return out;
        }
        by_name
            .entry(&dependency.name)
            .or_default()
            .push(dependency);
    }
    for candidates in by_name.values_mut() {
        if cancelled() {
            out.stop = Stop::Cancelled;
            return out;
        }
        candidates.sort_unstable_by_key(|d| d.id);
    }
    if cancelled() {
        out.stop = Stop::Cancelled;
        return out;
    }
    if limits.nodes == 0 {
        out.stop = Stop::Nodes;
        return out;
    }
    let Some(name) = seed_name.strip_prefix("--").filter(|name| !name.is_empty()) else {
        out.stop = Stop::Invalid;
        return out;
    };
    let mut providers = BTreeSet::from([seed]);
    let mut seen = BTreeSet::new();
    let mut queue = VecDeque::from([Provider {
        name,
        path: vec![seed].into_boxed_slice(),
        depth: 0,
    }]);
    out.nodes = 1;
    // Seed path is bounded as well as each copied result and queued explanation.
    out.witness_bytes = 32;
    if out.witness_bytes > limits.witness_bytes {
        out.stop = Stop::WitnessBytes;
        return out;
    }
    while let Some(provider) = queue.pop_front() {
        if cancelled() {
            out.stop = Stop::Cancelled;
            return out;
        }
        let Some(candidates) = by_name.get(provider.name) else {
            continue;
        };
        for dep in candidates {
            if cancelled() {
                out.stop = Stop::Cancelled;
                return out;
            }
            if out.edges == limits.edges {
                out.stop = Stop::Edges;
                return out;
            }
            out.edges += 1;
            if seen.contains(&dep.id) {
                continue;
            }
            if provider.depth == limits.depth {
                out.stop = Stop::Depth;
                return out;
            }
            if out.nodes == limits.nodes {
                out.stop = Stop::Nodes;
                return out;
            }
            if out.rows.len() == limits.rows {
                out.stop = Stop::Rows;
                return out;
            }
            let owner = owners.get(&dep.owner).copied();
            let count = provider
                .path
                .len()
                .saturating_add(1)
                .saturating_add(usize::from(owner.is_some()));
            // Both the output row and potential queued provider retain a copy;
            // this cumulative upper bound remains conservative after popping.
            let Some(reserved) = count
                .checked_mul(32)
                .and_then(|n| n.checked_mul(2))
                .and_then(|n| out.witness_bytes.checked_add(n))
            else {
                out.stop = Stop::WitnessBytes;
                return out;
            };
            if reserved > limits.witness_bytes {
                out.stop = Stop::WitnessBytes;
                return out;
            }
            out.witness_bytes = reserved;
            seen.insert(dep.id);
            out.nodes += 1;
            let mut witness = Vec::with_capacity(count);
            witness.extend_from_slice(&provider.path);
            witness.push(dep.id);
            if let Some(owner) = owner {
                witness.push(owner.id);
            } else {
                out.missing_owner = true;
            }
            let witness = witness.into_boxed_slice();
            out.rows.push(Row {
                dependency: dep.id,
                host: dep.owner.host,
                owner: owner.map(|d| d.id),
                witness: witness.clone(),
            });
            if let Some(owner) = owner {
                if let Some(next) = owner
                    .custom_property
                    .as_deref()
                    .and_then(|name| name.strip_prefix("--"))
                    .filter(|name| !name.is_empty() && !name.contains('\\'))
                {
                    if providers.insert(owner.id) {
                        if out.nodes == limits.nodes {
                            out.stop = Stop::Nodes;
                            return out;
                        }
                        out.nodes += 1;
                        queue.push_back(Provider {
                            name: next,
                            path: witness,
                            depth: provider.depth + 1,
                        });
                    }
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn id(x: u8) -> Key {
        [x; 32]
    }
    fn owner(x: u8) -> Owner {
        Owner {
            host: id(1),
            rule: vec![x as u32],
            ordinal: 0,
        }
    }
    fn limits() -> Limits {
        Limits {
            input_rows: 20,
            input_bytes: 1000,
            edges: 30,
            nodes: 30,
            depth: 10,
            rows: 20,
            witness_bytes: 4096,
            temporary_bytes: 1024 * 1024,
        }
    }
    #[test]
    fn transitive_candidates_follow_only_the_exact_native_owner() {
        let declarations = vec![
            Declaration {
                id: id(10),
                owner: owner(1),
                custom_property: Some("--paint".into()),
            },
            Declaration {
                id: id(11),
                owner: owner(2),
                custom_property: Some("--unrelated".into()),
            },
            Declaration {
                id: id(12),
                owner: owner(3),
                custom_property: None,
            },
        ];
        let dependencies = vec![
            Dependency {
                id: id(20),
                owner: owner(1),
                name: "tone".into(),
            },
            Dependency {
                id: id(21),
                owner: owner(3),
                name: "paint".into(),
            },
            Dependency {
                id: id(22),
                owner: owner(2),
                name: "other".into(),
            },
        ];
        let result = walk(
            id(1),
            "--tone",
            &declarations,
            &dependencies,
            limits(),
            || false,
        );
        assert_eq!(result.stop, Stop::Complete);
        assert_eq!(result.rows.len(), 2);
        assert_eq!(result.rows[0].witness.as_ref(), &[id(1), id(20), id(10)]);
        assert_eq!(
            result.rows[1].witness.as_ref(),
            &[id(1), id(20), id(10), id(21), id(12)]
        );
        assert!(!result.rows.iter().any(|row| row.dependency == id(22)));
    }
    #[test]
    fn cycles_terminate_and_depth_reports_unexpanded_candidates() {
        let declarations = vec![
            Declaration {
                id: id(10),
                owner: owner(1),
                custom_property: Some("--paint".into()),
            },
            Declaration {
                id: id(11),
                owner: owner(2),
                custom_property: Some("--tone".into()),
            },
        ];
        let dependencies = vec![
            Dependency {
                id: id(20),
                owner: owner(1),
                name: "tone".into(),
            },
            Dependency {
                id: id(21),
                owner: owner(2),
                name: "paint".into(),
            },
        ];
        let result = walk(
            id(1),
            "--tone",
            &declarations,
            &dependencies,
            limits(),
            || false,
        );
        assert_eq!(result.stop, Stop::Complete);
        assert_eq!(result.rows.len(), 2);
        let mut bounded = limits();
        bounded.depth = 1;
        assert_eq!(
            walk(
                id(1),
                "--tone",
                &declarations,
                &dependencies,
                bounded,
                || false
            )
            .stop,
            Stop::Depth
        );
    }
    #[test]
    fn missing_owner_and_budget_are_explicit() {
        let dependencies = vec![Dependency {
            id: id(20),
            owner: owner(1),
            name: "tone".into(),
        }];
        let result = walk(id(1), "--tone", &[], &dependencies, limits(), || false);
        assert!(result.missing_owner);
        assert_eq!(result.rows[0].owner, None);
        let mut cap = limits();
        cap.edges = 0;
        assert_eq!(
            walk(id(1), "--tone", &[], &dependencies, cap, || false).stop,
            Stop::Edges
        );
        assert_eq!(
            walk(id(1), "--tone", &[], &dependencies, limits(), || true).stop,
            Stop::Cancelled
        );
    }
}
