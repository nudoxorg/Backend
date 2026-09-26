//! Proven trait identities and every requirement of a generic input.
//! Plain recipe keys deliberately read simply; discovery adds the predicate
//! that proves a supplied concrete value can actually inhabit that key.

use crate::graph::model::{Kind, NodeId, World};
use crate::semantics::recipes::{How, Recipes};
use crate::semantics::types::{TypeExpr, parse, split_top};
use crate::semantics::{bounds, members};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) enum Capability {
    Node(NodeId),
    Path { package: Option<u32>, name: String },
}

#[derive(Clone, Default)]
pub(super) struct Requirement {
    pub(super) variable: Option<String>,
    pub(super) all: Vec<Capability>,
    pub(super) verified: bool,
}

/// Scope Rust's relative path keywords before comparing trait identities.
pub(super) fn absolute_path(world: &World, from: NodeId, path: &[String]) -> Option<Vec<String>> {
    let first = path.first()?;
    let node = world.node(from);
    let package = world.packages[node.pkg as usize].name.replace('-', "_");
    let mut scoped = vec![package];
    let mut at = 1;
    match first.as_str() {
        "crate" => {}
        "self" | "super" => {
            scoped.extend(
                world.modules[node.module as usize]
                    .path
                    .split("::")
                    .filter(|p| !p.is_empty())
                    .map(str::to_owned),
            );
            if first == "super" {
                if scoped.len() == 1 {
                    return None;
                }
                scoped.pop();
                while path.get(at).is_some_and(|part| part == "super") {
                    if scoped.len() == 1 {
                        return None;
                    }
                    scoped.pop();
                    at += 1;
                }
            }
        }
        _ => return Some(path.to_vec()),
    }
    scoped.extend(path.iter().skip(at).cloned());
    Some(scoped)
}

// Known packages use their world identity, including version. A qualified
// external name not present in the extracted closure stays a symbolic path.
fn package_scope(world: &World, from: NodeId, path: &[String]) -> Option<Option<u32>> {
    let first = path.first()?;
    let own = world.node(from).pkg;
    if matches!(first.as_str(), "crate" | "self" | "super") {
        return Some(Some(own));
    }
    if path.len() == 1 {
        return Some(None);
    }
    let candidates: Vec<u32> = world
        .packages
        .iter()
        .enumerate()
        .filter_map(|(a, package)| {
            let short = u32::try_from(a).ok()?;
            let name_matches = package.name.replace('-', "_") == *first
                || world.package_short(short).replace('-', "_") == *first;
            name_matches.then_some(short)
        })
        .collect();
    let visible: Vec<_> = candidates
        .iter()
        .copied()
        .filter(|&package| package == own || world.packages[own as usize].deps.contains(&package))
        .collect();
    match visible.as_slice() {
        [one] => Some(Some(*one)),
        [] => match candidates.as_slice() {
            [] => Some(None),
            [one] => Some(Some(*one)),
            _ => None,
        },
        _ => None,
    }
}

pub(super) fn index(
    world: &World,
    recipes: &Recipes,
) -> (
    HashMap<String, HashSet<Capability>>,
    HashMap<(usize, usize), Requirement>,
) {
    let mut trait_nodes: HashMap<String, Vec<NodeId>> = HashMap::new();
    let mut external: HashMap<String, HashSet<(Option<u32>, String)>> = HashMap::new();
    let paths = |from: NodeId, source: &str| match parse(source) {
        TypeExpr::Named { path, .. } => package_scope(world, from, &path)
            .and_then(|package| absolute_path(world, from, &path).map(|path| (package, path))),
        _ => None,
    };
    for (a, node) in world.nodes.iter().enumerate() {
        let Ok(from) = NodeId::try_from(a) else {
            continue;
        };
        if node.kind == Kind::Trait
            && let Ok(i) = NodeId::try_from(a)
        {
            trait_nodes
                .entry(node.name.to_string())
                .or_default()
                .push(i);
        }
        for source in node.impls_ext.iter().chain(&node.derives) {
            if let Some((package, path)) = paths(from, source)
                && path.len() > 1
            {
                external
                    .entry(path.last().cloned().unwrap_or_default())
                    .or_default()
                    .insert((package, path.join("::")));
            }
        }
        let parent = node.parent.map(|p| world.node(p));
        let lists: Vec<_> = [
            node.generics.as_deref(),
            parent.and_then(|p| p.generics.as_deref()),
        ]
        .into_iter()
        .flatten()
        .collect();
        for generic in bounds::generics(&lists, node.where_.as_deref().unwrap_or("")) {
            for bound in generic.bounds {
                for source in split_top(&bound, '+') {
                    if let Some((package, path)) = paths(from, &source)
                        && path.len() > 1
                    {
                        external
                            .entry(path.last().cloned().unwrap_or_default())
                            .or_default()
                            .insert((package, path.join("::")));
                    }
                }
            }
        }
    }
    let resolve = |from: NodeId, source: &str| -> Option<Capability> {
        let TypeExpr::Named { path, args } = parse(source) else {
            return None;
        };
        // Applied trait identities cannot be proven from an impl's bare trait ID.
        if !args.is_empty() {
            return None;
        }
        let package = package_scope(world, from, &path)?;
        let path = absolute_path(world, from, &path)?;
        let last = path.last()?;
        let written = path.join("::");
        let mut candidates: Vec<_> = trait_nodes
            .get(last)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&i| {
                if package.is_some_and(|package| world.node(i).pkg != package) {
                    return false;
                }
                if path.len() == 1 {
                    true
                } else {
                    let full =
                        format!("{}::{}", world.qual(i), world.node(i).name).replace('-', "_");
                    let written = written.replace('-', "_");
                    let package = world.packages[world.node(i).pkg as usize]
                        .name
                        .replace('-', "_");
                    let package_short = world.package_short(world.node(i).pkg).replace('-', "_");
                    full == written
                        || full.ends_with(&format!("::{written}"))
                        || (path.len() == 2 && (path[0] == package || path[0] == package_short))
                }
            })
            .collect();
        if path.len() == 1
            && candidates
                .iter()
                .any(|&i| world.node(i).module == world.node(from).module)
        {
            candidates.retain(|&i| world.node(i).module == world.node(from).module);
        }
        match candidates.as_slice() {
            [one] => Some(Capability::Node(*one)),
            [] => {
                if path.len() > 1 {
                    return Some(Capability::Path {
                        package,
                        name: written,
                    });
                }
                let own = world.node(from).pkg;
                let visible: Vec<_> = external
                    .get(last)
                    .into_iter()
                    .flatten()
                    .filter(|(package, _)| {
                        package.is_none_or(|package| {
                            package == own || world.packages[own as usize].deps.contains(&package)
                        })
                    })
                    .collect();
                match visible.as_slice() {
                    [] => absolute_path(world, from, &["self".to_owned(), written]).map(|path| {
                        Capability::Path {
                            package: Some(own),
                            name: path.join("::"),
                        }
                    }),
                    [one] => Some(Capability::Path {
                        package: one.0,
                        name: one.1.clone(),
                    }),
                    _ => None,
                }
            }
            _ => None,
        }
    };
    let standard_mapping = |from: NodeId, path: &[String]| {
        let standard = |name: &str| matches!(name, "std" | "core" | "alloc");
        if path.len() > 1 {
            return path.first().is_some_and(|name| standard(name));
        }
        let Some(last) = path.last() else {
            return false;
        };
        let local: Vec<_> = trait_nodes
            .get(last)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&i| world.node(i).module == world.node(from).module)
            .collect();
        if !local.is_empty() {
            return local
                .iter()
                .all(|&i| standard(&world.packages[world.node(i).pkg as usize].name));
        }
        if let Some(paths) = external.get(last) {
            return paths
                .iter()
                .all(|(_, path)| path.split("::").next().is_some_and(standard));
        }
        trait_nodes.get(last).is_none_or(|nodes| {
            nodes
                .iter()
                .all(|&i| standard(&world.packages[world.node(i).pkg as usize].name))
        })
    };
    let mut capabilities = HashMap::new();
    for (a, node) in world.nodes.iter().enumerate() {
        let Ok(i) = NodeId::try_from(a) else { continue };
        if node.parent.is_some() || !node.kind.is_type_like() {
            continue;
        }
        // A generic declaration is not evidence for a particular instance.
        // Derives and impls may require conditions on its erased arguments.
        if !bounds::generics(
            &[node.generics.as_deref().unwrap_or("")],
            node.where_.as_deref().unwrap_or(""),
        )
        .is_empty()
        {
            capabilities.insert(format!("#{i}"), HashSet::new());
            continue;
        }
        let mut caps: HashSet<_> = node
            .impls
            .iter()
            .filter(|imp| !imp.generic)
            .filter_map(|imp| imp.trait_)
            .map(Capability::Node)
            .collect();
        caps.extend(
            node.derives
                .iter()
                .chain(&node.impls_ext)
                .filter_map(|source| resolve(i, source)),
        );
        capabilities.insert(format!("#{i}"), caps);
    }
    let mut requirements = HashMap::new();
    for (q, e) in recipes.table().iter().enumerate() {
        if !matches!(e.how, How::Call | How::Method) {
            continue;
        }
        let node = world.node(e.node);
        let parent = node.parent.map(|p| world.node(p));
        let lists: Vec<_> = [
            node.generics.as_deref(),
            parent.and_then(|p| p.generics.as_deref()),
        ]
        .into_iter()
        .flatten()
        .collect();
        let generic: HashMap<_, _> = bounds::generics(&lists, node.where_.as_deref().unwrap_or(""))
            .into_iter()
            .map(|g| (g.name, g.bounds))
            .collect();
        let offset = usize::from(e.how == How::Method);
        for (a, param) in members::params(&node.params).iter().enumerate() {
            let j = a + offset;
            let mut expr = parse(&param.ty);
            while let TypeExpr::Ref { inner, .. } | TypeExpr::Ptr { inner, .. } = expr {
                expr = *inner;
            }
            let variable = match &expr {
                TypeExpr::Named { path, .. }
                    if path.len() == 1 && generic.contains_key(&path[0]) =>
                {
                    Some(path[0].clone())
                }
                _ => None,
            };
            let bound_sources = match expr {
                TypeExpr::Named { path, .. }
                    if path.len() == 1 && generic.contains_key(&path[0]) =>
                {
                    generic.get(&path[0]).cloned().unwrap_or_default()
                }
                TypeExpr::Any(bound) => bound.iter().map(ToString::to_string).collect(),
                _ => continue,
            };
            let mut required = Requirement {
                variable,
                all: Vec::new(),
                verified: true,
            };
            for source in bound_sources {
                for part in split_top(&source, '+') {
                    let part = part.trim();
                    if part.starts_with('\'') || part.starts_with('?') || part == "Sized" {
                        continue;
                    }
                    let mapped = match parse(part) {
                        TypeExpr::Named { path, args } => {
                            let name = path.last().map(String::as_str).unwrap_or("");
                            match name {
                                "Display" | "ToString"
                                    if args.is_empty() && standard_mapping(e.node, &path) =>
                                {
                                    e.ins.get(j).is_some_and(|key| key == "text")
                                }
                                "AsRef" | "Into" | "Borrow" if standard_mapping(e.node, &path) => {
                                    args.first().and_then(TypeExpr::last).is_some_and(|inner| {
                                        let key = match inner {
                                            "str" | "String" | "OsStr" | "OsString" => "text",
                                            "Path" | "PathBuf" => "path",
                                            _ => "",
                                        };
                                        e.ins.get(j).is_some_and(|input| input == key)
                                    })
                                }
                                _ => false,
                            }
                        }
                        _ => false,
                    };
                    if mapped {
                        continue;
                    }
                    match resolve(e.node, part) {
                        Some(cap) => required.all.push(cap),
                        None => required.verified = false,
                    }
                }
            }
            requirements.insert((q, j), required);
        }
    }
    (capabilities, requirements)
}
