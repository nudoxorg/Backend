//! Your code and the symbol, read from a [`World`]: which packages of yours
//! refer to it (or to one of its members), how often, through which member,
//! on which real lines, and which of yours name its siblings instead.
//!
//! Pure over the world and a source reader (the index, or the fixture's
//! `repo/` and `registry/` folders): nothing here draws. A "place" is one
//! referring declaration (and, for a type, one member of it): the counts are
//! the world's edges, not a path scan, and the lines are mined from each
//! caller's body (`semantics::usage`). What the world has not walked is
//! unread, and the caller says so with [`Basis::Unknown`].

use super::plan::{Effect, Lang, words};
use super::reach::{Basis, CrateUse, Instead, Line, Reach, Reached, Scope};
use crate::graph::{Kind, NodeId, Package, Rel, World};
use crate::semantics::usage::{Needle, mine};
use std::sync::Arc;

/// Lines kept per crate.
pub const KEPT: usize = 8;
/// Lines kept per member per crate.
const PER_MEMBER: usize = 3;
/// Crates of other packages shown with lines.
const OTHERS: usize = 4;

/// Every relation that makes a declaration a use of the symbol.
const REFS: Rel = Rel(Rel::CALLS.0
    | Rel::TAKES.0
    | Rel::GIVES.0
    | Rel::USES.0
    | Rel::TYPE.0
    | Rel::HAS.0
    | Rel::IMPL.0
    | Rel::DERIVES.0);

/// One place: a declaration that refers to the symbol, through a member of
/// it or directly.
struct Hit {
    caller: NodeId,
    member: Option<NodeId>,
}

/// Who refers to `i`, and to each of its members: one hit per referring
/// declaration and member. A declaration inside the symbol's own item is no
/// use of it.
fn hits(world: &World, i: NodeId) -> Vec<Hit> {
    let mut out: Vec<Hit> = Vec::new();
    let mut push = |caller: NodeId, member: Option<NodeId>, out: &mut Vec<Hit>| {
        if world.top(caller) == world.top(i) || world.node(caller).orphan {
            return;
        }
        if !out
            .iter()
            .any(|hit| hit.caller == caller && hit.member == member)
        {
            out.push(Hit { caller, member });
        }
    };
    for (j, rel) in world.in_edges(i) {
        if rel.0 & REFS.0 != 0 {
            push(j, None, &mut out);
        }
    }
    for &m in world.kids(i) {
        // Fields and variants say what it holds, not what is done with it.
        if !matches!(world.node(m).kind, Kind::Method) {
            for (j, rel) in world.in_edges(m) {
                if rel.0 & (Rel::USES.0 | Rel::CALLS.0) != 0 {
                    push(j, Some(m), &mut out);
                }
            }
            continue;
        }
        for (j, rel) in world.in_edges(m) {
            if rel.0 & (Rel::CALLS.0 | Rel::USES.0) != 0 {
                push(j, Some(m), &mut out);
            }
        }
    }
    out
}

/// The reach of `i`: yours first, then the other packages that name it.
#[must_use]
pub fn reach_of(
    world: &World,
    i: NodeId,
    read: &mut dyn FnMut(&Package, &str) -> Option<Arc<str>>,
) -> Reach {
    let node = world.node(i);
    let hits = hits(world, i);
    // Group by the referring declaration's package.
    let mut packages: Vec<u32> = Vec::new();
    for hit in &hits {
        let pkg = world.node(world.top(hit.caller)).pkg;
        if !packages.contains(&pkg) {
            packages.push(pkg);
        }
    }
    let mut yours: Vec<CrateUse> = Vec::new();
    let mut others: Vec<CrateUse> = Vec::new();
    let mut yours_pkgs: Vec<u32> = Vec::new();
    for pkg in packages {
        let mine_hits: Vec<&Hit> = hits
            .iter()
            .filter(|hit| world.node(world.top(hit.caller)).pkg == pkg)
            .collect();
        let used = crate_use(world, i, pkg, &mine_hits, read);
        if world.packages[pkg as usize].yours {
            yours_pkgs.push(pkg);
            yours.push(used);
        } else if pkg != node.pkg {
            // A package's own uses of what it writes are not another package's.
            others.push(used);
        }
    }
    yours.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.name.cmp(&b.name)));
    others.sort_by(|a, b| {
        b.lines
            .len()
            .cmp(&a.lines.len())
            .then_with(|| b.count.cmp(&a.count))
            .then_with(|| a.name.cmp(&b.name))
    });
    others.truncate(OTHERS + 8);

    // The members of it your code reaches.
    let mut reached: Vec<Reached> = Vec::new();
    for used in &yours {
        for (name, _) in &used.members {
            if reached.iter().any(|known| known.name == *name) {
                continue;
            }
            if let Some(&member) = world
                .kids(i)
                .iter()
                .find(|&&m| world.node(m).name.as_ref() == name.as_str())
            {
                let member = world.node(member);
                reached.push(Reached {
                    name: name.clone(),
                    effect: match member.recv.as_deref() {
                        Some("reads") => Effect::Reads,
                        Some("changes") => Effect::Changes,
                        Some("consumes") => Effect::UsesUp,
                        _ if member.recv.is_none() && member.kind == Kind::Method => Effect::Makes,
                        _ => Effect::None,
                    },
                    gives: member
                        .ret
                        .as_deref()
                        .map(|ret| words(ret, Lang::Rust).plain())
                        .filter(|words| !words.is_empty()),
                });
            }
        }
    }

    // Yours that never name it but name its siblings.
    let mut instead: Vec<Instead> = Vec::new();
    for (pkg, package) in world.packages.iter().enumerate() {
        let pkg = u32::try_from(pkg).unwrap_or(u32::MAX);
        if !package.yours || yours_pkgs.contains(&pkg) {
            continue;
        }
        let mut here: Vec<String> = Vec::new();
        let mut elsewhere: Vec<String> = Vec::new();
        for &sibling in &world.items {
            let other = world.node(sibling);
            if sibling == i || other.pkg != node.pkg || other.name == node.name {
                continue;
            }
            let named = world
                .in_edges(sibling)
                .any(|(j, rel)| rel.0 & REFS.0 != 0 && world.node(world.top(j)).pkg == pkg);
            if !named {
                continue;
            }
            let list = if other.module == node.module {
                &mut here
            } else {
                &mut elsewhere
            };
            if !list.contains(&other.name.to_string()) {
                list.push(other.name.to_string());
            }
        }
        let (uses, scope) = if here.is_empty() {
            (elsewhere, Scope::Package)
        } else {
            (here, Scope::Module)
        };
        if !uses.is_empty() {
            instead.push(Instead {
                name: world.package_short(pkg).to_owned(),
                uses: uses.into_iter().take(3).collect(),
                scope,
            });
        }
    }
    instead.sort_by(|a, b| a.name.cmp(&b.name));

    let basis = if hits.is_empty() && yours.is_empty() && others.is_empty() && instead.is_empty() {
        Basis::Unknown
    } else {
        Basis::Resolved
    };
    // A symbol nobody in the world refers to is not "unused": the world may
    // simply not have walked its users. Say what was read.
    Reach {
        yours,
        instead,
        others,
        reached,
        basis: if basis == Basis::Unknown && world.in_degree[i as usize] == 0 {
            Basis::Resolved
        } else {
            basis
        },
        note: Some(
            "Counted from the declarations that refer to it; lines are mined from their bodies."
                .to_owned(),
        ),
    }
}

/// One package's use: its places, members and lines.
fn crate_use(
    world: &World,
    i: NodeId,
    pkg: u32,
    hits: &[&Hit],
    read: &mut dyn FnMut(&Package, &str) -> Option<Arc<str>>,
) -> CrateUse {
    let node = world.node(i);
    let mut members: Vec<(String, u32)> = Vec::new();
    for hit in hits {
        if let Some(m) = hit.member {
            let name = world.node(m).name.to_string();
            match members.iter_mut().find(|(known, _)| *known == name) {
                Some((_, n)) => *n += 1,
                None => members.push((name, 1)),
            }
        }
    }
    members.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));

    // Lines: a few for each member (most-reached first), then the direct
    // ones, in file order within the crate.
    let mut order: Vec<&&Hit> = Vec::new();
    for (name, _) in &members {
        order.extend(
            hits.iter()
                .filter(|hit| {
                    hit.member
                        .is_some_and(|m| world.node(m).name.as_ref() == name.as_str())
                })
                .take(PER_MEMBER),
        );
    }
    order.extend(
        hits.iter()
            .filter(|hit| hit.member.is_none())
            .take(PER_MEMBER),
    );
    let mut lines: Vec<Line> = Vec::new();
    for hit in order {
        if lines.len() >= KEPT {
            break;
        }
        let top = world.node(world.top(hit.caller));
        let Some(file) = top.file.clone() else {
            continue;
        };
        let package = &world.packages[pkg as usize];
        let Some(text) = read(package, &file) else {
            continue;
        };
        let caller = world.node(hit.caller);
        let start = if caller.line > 0 {
            caller.line
        } else {
            top.line
        };
        let end = caller.end.or(top.end).unwrap_or(top.line);
        let name = hit
            .member
            .map_or_else(|| node.name.to_string(), |m| world.node(m).name.to_string());
        let needle = Needle {
            name,
            member: hit.member.is_some() || (node.kind == Kind::Method && node.parent.is_some()),
        };
        let Some(excerpt) = mine(&text, start, end, &needle) else {
            continue;
        };
        let Some(code) = excerpt.lines.get(excerpt.hit) else {
            continue;
        };
        let key = (file.to_string(), excerpt.line);
        if lines
            .iter()
            .any(|known| (known.file.as_str(), known.line) == (key.0.as_str(), key.1))
        {
            continue;
        }
        lines.push(Line {
            file: file.to_string(),
            line: excerpt.line,
            text: code.clone(),
            mark: u32::try_from(excerpt.mark.start)
                .ok()
                .zip(u32::try_from(excerpt.mark.end).ok()),
            member: hit.member.map(|m| world.node(m).name.to_string()),
            caller: Some(world.name_of(hit.caller).to_string()),
            link: None,
        });
    }
    // File order, so a crate's lines read like its source.
    lines.sort_by(|a, b| a.file.cmp(&b.file).then(a.line.cmp(&b.line)));
    CrateUse {
        name: world.package_short(pkg).to_owned(),
        count: u32::try_from(hits.len()).unwrap_or(u32::MAX),
        members,
        lines,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{Edge, Module, Node};

    fn package(name: &str, yours: bool) -> Package {
        Package {
            name: name.into(),
            version: "0.1.0".into(),
            yours,
            external: !yours,
            deps: Vec::new(),
        }
    }

    /// `lib` writes `Value` (a type with `as_str` and `as_table`) and
    /// `Table`; `app` and `tool` are yours: `app` calls both methods and
    /// names `Value` directly; `tool` names only `Table`.
    fn toy() -> (World, NodeId) {
        let packages = vec![
            package("lib", false),
            package("app", true),
            package("tool", true),
        ];
        let modules = vec![
            Module {
                pkg: 0,
                path: "value".into(),
                file: "value.rs".into(),
            },
            Module {
                pkg: 1,
                path: String::new().into(),
                file: "main.rs".into(),
            },
            Module {
                pkg: 2,
                path: String::new().into(),
                file: "main.rs".into(),
            },
        ];
        let mut value = Node::new(Kind::Enum, "Value", 0, 0);
        value.file = Some("value.rs".into());
        let mut table = Node::new(Kind::Type, "Table", 0, 0);
        table.file = Some("value.rs".into());
        let mut as_str = Node::new(Kind::Method, "as_str", 0, 0).member_of(0);
        as_str.recv = Some("reads".into());
        as_str.ret = Some("Option<&str>".into());
        let mut as_table = Node::new(Kind::Method, "as_table", 0, 0).member_of(0);
        as_table.recv = Some("reads".into());
        let mut load = Node::new(Kind::Function, "load", 1, 1);
        load.file = Some("main.rs".into());
        load.line = 1;
        load.end = Some(4);
        let mut show = Node::new(Kind::Function, "show", 2, 2);
        show.file = Some("main.rs".into());
        show.line = 1;
        show.end = Some(3);
        let nodes = vec![value, table, as_str, as_table, load, show];
        let edges = vec![
            Edge {
                from: 4,
                to: 0,
                rel: Rel::USES,
            },
            Edge {
                from: 4,
                to: 2,
                rel: Rel::CALLS,
            },
            Edge {
                from: 4,
                to: 3,
                rel: Rel::CALLS,
            },
            Edge {
                from: 5,
                to: 1,
                rel: Rel::USES,
            },
        ];
        (
            World::new(packages, modules, nodes, edges).expect("a toy world"),
            0,
        )
    }

    fn reader(package: &Package, file: &str) -> Option<Arc<str>> {
        let text = match (package.name.as_ref(), file) {
            ("app", "main.rs") => {
                "fn load() {\n    let v: lib::Value = read();\n    v.as_str();\n    v.as_table();\n}\n"
            }
            ("tool", "main.rs") => "fn show() {\n    let t: lib::Table = read();\n}\n",
            _ => return None,
        };
        Some(Arc::from(text))
    }

    #[test]
    fn a_crate_of_yours_that_reaches_a_type_through_its_members_says_which() {
        let (world, value) = toy();
        let reach = reach_of(&world, value, &mut reader);
        let [app] = &reach.yours[..] else {
            panic!("one crate of yours names Value: {:?}", reach.yours)
        };
        assert_eq!(app.name, "app");
        assert_eq!(app.count, 3, "the type itself, as_str, as_table");
        assert_eq!(
            app.members,
            vec![("as_str".to_owned(), 1), ("as_table".to_owned(), 1)]
        );
        let said: Vec<_> = app.lines.iter().map(|line| line.text.trim()).collect();
        assert!(
            said.iter().any(|text| text.contains("as_str")),
            "its real line for as_str is kept: {said:?}"
        );
        assert!(
            said.iter().any(|text| text.contains("let v: lib::Value")),
            "and the line that names it: {said:?}"
        );
    }

    #[test]
    fn a_crate_that_names_a_sibling_but_not_the_type_is_said_to_use_the_sibling_instead() {
        let (world, value) = toy();
        let reach = reach_of(&world, value, &mut reader);
        let [tool] = &reach.instead[..] else {
            panic!("one crate names its sibling: {:?}", reach.instead)
        };
        assert_eq!(
            (tool.name.as_str(), tool.uses.as_slice(), tool.scope),
            ("tool", &["Table".to_owned()][..], Scope::Module)
        );
    }

    #[test]
    fn the_package_that_writes_a_symbol_is_not_another_package_that_uses_it() {
        let (world, value) = toy();
        // `lib` (not yours) refers to its own `Value` through a method of its own.
        let mut edges = world.edges.clone();
        let mut nodes = world.nodes.clone();
        let mut inside = Node::new(Kind::Function, "helper", 0, 0);
        inside.file = Some("value.rs".into());
        inside.line = 1;
        inside.end = Some(3);
        nodes.push(inside);
        edges.push(Edge {
            from: u32::try_from(nodes.len() - 1).expect("id"),
            to: value,
            rel: Rel::USES,
        });
        let world = World::new(world.packages.clone(), world.modules.clone(), nodes, edges)
            .expect("a toy world");
        let reach = reach_of(&world, value, &mut reader);
        assert!(
            reach.others.is_empty(),
            "its own package is not an outsider: {:?}",
            reach.others
        );
        assert_eq!(reach.yours.len(), 1, "yours are unchanged");
    }

    #[test]
    fn the_members_reached_carry_what_they_do_and_give() {
        let (world, value) = toy();
        let reach = reach_of(&world, value, &mut reader);
        let as_str = reach.reached_member("as_str").expect("as_str is reached");
        assert_eq!(as_str.effect, Effect::Reads);
        assert!(
            as_str
                .gives
                .as_deref()
                .is_some_and(|gives| gives.contains("maybe")),
            "{:?}",
            as_str.gives
        );
    }
}
