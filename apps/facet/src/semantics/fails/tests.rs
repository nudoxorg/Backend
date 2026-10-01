//! How it fails against the prototype (`tests/engines.golden`, see the tour
//! tests for how it is made), plus hand-built worlds for the rules the real
//! world never exercises.

use super::{Fails, Given};
use crate::graph::model::{Edge, Kind, Module, Node, NodeId, Package, Rel, World};
use crate::semantics::names::Names;
use crate::semantics::tour::tests::{agree, golden, id, key, key_or, names, world};
use std::fmt::Write as _;

/// The golden's spelling of what a callable gives: `Kind@via,…`.
pub(crate) fn given(w: &World, kinds: &[Given]) -> String {
    kinds.iter().map(|g| format!("{}@{}", w.node(g.kind).name, key_or(w, g.via))).collect::<Vec<_>>().join(",")
}

/// Makers, reach and section lines for every error with kinds that
/// something fails with (`engines.mjs full`'s order).
pub(crate) fn errors_in(w: &World, fails: &Fails<'_>, s: &mut String) {
    for e in 0..u32::try_from(w.len()).unwrap_or(0) {
        if fails.kinds(e).is_empty() {
            continue;
        }
        let reach = fails.reach_of(e);
        if reach.can == 0 {
            continue;
        }
        for m in fails.makers_of(e) {
            let _ = writeln!(s, "makers|{}|{}|{}", key(w, e), w.node(m.kind).name, makers(w, &m.makers));
        }
        let _ = writeln!(s, "reach|{}|{}|{}", key(w, e), reach.can, told(w, &reach.told));
        let _ = writeln!(s, "section|{}|{}", key(w, e), section(w, fails, e));
    }
}

fn makers(w: &World, list: &[NodeId]) -> String {
    list.iter().map(|&j| format!("{}{}", if w.yours(j) { "*" } else { "" }, key(w, j))).collect::<Vec<_>>().join(",")
}

fn told(w: &World, list: &[NodeId]) -> String {
    list.iter().map(|&v| w.node(v).name.to_string()).collect::<Vec<_>>().join(",")
}

fn section(w: &World, fails: &Fails<'_>, e: NodeId) -> String {
    fails.how_it_fails(e).map_or_else(|| "|-".to_owned(), |s| format!("{}|{}", s.words(w), s.column))
}

/// `(what, want, got)` rows for [`agree`].
type Rows = Vec<(String, String, String)>;

/// One engine for the golden's asking order (ascending, per error).
fn asked_in_order() -> (Fails<'static>, Rows) {
    let w = world();
    let fails = Fails::new(w, names());
    let rows = golden("fails")
        .into_iter()
        .map(|g| {
            let r = fails.fails_of(id(&g[0])).expect("it returns an error");
            (g[0].clone(), g[1..].join("|"), format!("{}|{}|{}", key(w, r.error), r.all, given(w, &r.kinds)))
        })
        .collect();
    (fails, rows)
}

#[test]
fn every_error_of_is_the_prototypes() {
    let w = world();
    let fails = Fails::new(w, names());
    let rows = golden("error").into_iter().map(|g| (g[0].clone(), g[1].clone(), key_or(w, fails.error_of(id(&g[0]))))).collect();
    agree("error_of", rows);
}

#[test]
fn every_fails_of_is_the_prototypes() {
    agree("fails_of, asked in node order", asked_in_order().1);
}

#[test]
fn asked_the_other_way_round_cycles_answer_differently() {
    let w = world();
    let fails = Fails::new(w, names());
    let rows = golden("fails-desc")
        .into_iter()
        .map(|g| (g[0].clone(), g[1].clone(), fails.fails_of(id(&g[0])).map(|r| given(w, &r.kinds)).unwrap_or_default()))
        .collect();
    agree("fails_of, asked in reverse node order", rows);
    // The prototype's memo lets a cycle see a partial set, so the page for
    // emit_wildcard depends on which pages were opened before it.
    let at = id("semantic::ir::semantic_render::canonical::emit_wildcard:758");
    let up = Fails::new(w, names());
    let asc: Vec<_> = golden("fails").iter().map(|g| id(&g[0])).filter(|&j| up.error_of(j) == up.error_of(at)).collect();
    for j in asc {
        let _ = up.fails_of(j);
    }
    let down = Fails::new(w, names());
    let mut desc: Vec<_> = golden("fails").iter().map(|g| id(&g[0])).filter(|&j| down.error_of(j) == down.error_of(at)).collect();
    desc.reverse();
    for j in desc {
        let _ = down.fails_of(j);
    }
    let words = |f: &Fails<'_>| f.kinds_line(at).map(|l| l.words(w)).unwrap_or_default();
    assert_eq!(words(&up), "OutputWrite through write_text 1 of its 8 kinds");
    assert_eq!(words(&down), "OutputWrite through write_text · TraversalLimit or MissingReference through unary 3 of its 8 kinds");
}

#[test]
fn every_line_and_section_is_the_prototypes() {
    let w = world();
    let (fails, _) = asked_in_order();
    let lines = golden("line")
        .into_iter()
        .map(|g| (g[0].clone(), g[1].clone(), fails.kinds_line(id(&g[0])).map(|l| l.words(w)).unwrap_or_default()))
        .collect();
    agree("kinds lines", lines);
    let makers_rows = golden("makers")
        .into_iter()
        .map(|g| {
            let e = id(&g[0]);
            let got = fails.makers_of(e).into_iter().find(|m| w.node(m.kind).name.as_ref() == g[1]).map(|m| makers(w, &m.makers));
            (format!("{} {}", g[0], g[1]), g[2].clone(), got.unwrap_or_default())
        })
        .collect();
    agree("makers", makers_rows);
    let reach = golden("reach")
        .into_iter()
        .map(|g| {
            let r = fails.reach_of(id(&g[0]));
            (g[0].clone(), format!("{}|{}", g[1], g[2]), format!("{}|{}", r.can, told(w, &r.told)))
        })
        .collect();
    agree("reach", reach);
    let sections = golden("section").into_iter().map(|g| (g[0].clone(), g[1..].join("|"), section(w, &fails, id(&g[0])))).collect();
    agree("sections", sections);
}

#[test]
fn the_briefs_answers() {
    let w = world();
    let fails = Fails::new(w, names());
    let line = |k: &str| fails.kinds_line(id(k)).map(|l| l.words(w)).unwrap_or_default();
    assert_eq!(line("runtime::WorkspacePaths::discover:126"), "Io, InvalidPath or WorkspaceProjectMismatch 3 of its 10 kinds");
    assert_eq!(
        line("runtime::ensure_locald:356"),
        "Io, Spawn, DaemonExited or StartTimeout · MissingExecutable through locald_executable 5 of its 10 kinds"
    );
    let connect = fails.fails_of(id("client::Session::connect:322")).expect("it fails with ClientError");
    let via = id("client::UnixCommandTransport::connect:188");
    assert_eq!(given(w, &connect.kinds), format!("Transport@{k},Io@{k},Disconnected@{k}", k = key(w, via)));
    let e = id("runtime::RuntimeError:781");
    let reach = fails.reach_of(e);
    assert_eq!((reach.can, told(w, &reach.told).as_str()), (12, "Io"));
    let s = fails.how_it_fails(e).expect("RuntimeError has a section");
    assert_eq!(s.foot(), "12 calls in this world can fail with it · your code tells 1 of its 10 kinds apart");
    assert_eq!((s.rows[0].more, s.rows[0].told, s.column), (4, true, 25));
    // RuntimeError's fmt reads every kind but returns fmt::Result: it never
    // returns the error, so it is not a maker with or without the receiver rule.
    let fmt = id("runtime::RuntimeError::fmt:826");
    assert_eq!(fails.error_of(fmt), None);
    assert!(fails.makers_of(e).iter().all(|m| !m.makers.contains(&fmt)));
}

/// A hand-built world: `lib` with `E { A, B }`; `E::check(&self) ->
/// Result<(), E>` reads both kinds; `open() -> Result<(), E>` builds A and
/// calls `step`, which builds B and calls `open` back (a cycle); `mine` (your
/// package) mentions B while returning a bool; `alias_user() -> Result<u8>`
/// through the package's `type Result<T> = Result<T, E>` (written in `ty`).
fn small() -> (World, [NodeId; 8]) {
    let packages = vec![
        Package { name: "lib".into(), version: "1.0.0".into(), yours: false, external: true, deps: vec![] },
        Package { name: "backend-app".into(), version: "0.1.0".into(), yours: true, external: false, deps: vec![0] },
    ];
    let modules = vec![
        Module { pkg: 0, path: "".into(), file: "src/lib.rs".into() },
        Module { pkg: 1, path: "".into(), file: "src/main.rs".into() },
    ];
    let ret = |mut n: Node, r: &str| {
        n.ret = Some(r.to_owned().into());
        n
    };
    let mut check = ret(Node::new(Kind::Method, "check", 0, 0).member_of(0), "Result<(), E>");
    check.recv = Some("reads".into());
    let mut alias = Node::new(Kind::Type, "Result", 0, 0);
    alias.ty = Some("core::result::Result<T, E>".into());
    let nodes = vec![
        Node::new(Kind::Enum, "E", 0, 0),                              // 0
        Node::new(Kind::Variant, "A", 0, 0).member_of(0),              // 1
        Node::new(Kind::Variant, "B", 0, 0).member_of(0),              // 2
        check,                                                         // 3
        ret(Node::new(Kind::Function, "open", 0, 0), "Result<(), E>"), // 4
        ret(Node::new(Kind::Function, "step", 0, 0), "Result<u8, E>"), // 5
        ret(Node::new(Kind::Function, "mine", 1, 1), "bool"),          // 6
        ret(Node::new(Kind::Function, "alias_user", 0, 0), "Result<u8>"), // 7
        alias,                                                         // 8
    ];
    let edges = vec![
        Edge { from: 3, to: 1, rel: Rel::USES },
        Edge { from: 3, to: 2, rel: Rel::USES },
        Edge { from: 4, to: 1, rel: Rel::USES },
        Edge { from: 4, to: 5, rel: Rel::CALLS },
        Edge { from: 5, to: 2, rel: Rel::CALLS },
        Edge { from: 5, to: 4, rel: Rel::CALLS },
        Edge { from: 6, to: 2, rel: Rel::USES },
    ];
    let w = World::new(packages, modules, nodes, edges).expect("a small world");
    (w, [0, 1, 2, 3, 4, 5, 6, 7])
}

#[test]
fn a_method_of_the_error_that_reads_it_is_not_a_maker() {
    let (w, [e, a, b, check, open, step, _, _]) = small();
    let names = Names::new(&w);
    let fails = Fails::new(&w, &names);
    assert_eq!(fails.error_of(check), Some(e), "check returns E…");
    let makers: Vec<_> = fails.makers_of(e).into_iter().map(|m| (m.kind, m.makers)).collect();
    assert_eq!(makers, vec![(a, vec![open]), (b, vec![step])], "…but only reads it");
}

#[test]
fn kinds_spread_through_calls_and_a_cycle_sees_what_is_known() {
    let (w, [e, a, b, _, open, step, mine, _]) = small();
    let names = Names::new(&w);
    let fails = Fails::new(&w, &names);
    let open_gives = fails.fails_of(open).expect("open fails with E");
    assert_eq!(open_gives.kinds, vec![Given { kind: a, via: None }, Given { kind: b, via: Some(step) }]);
    // step was filled while open was still in progress: it saw only A.
    let step_gives = fails.fails_of(step).expect("step fails with E");
    assert_eq!(step_gives.kinds, vec![Given { kind: b, via: None }, Given { kind: a, via: Some(open) }]);
    assert_eq!(fails.kinds_line(open).map(|l| l.words(&w)).as_deref(), Some("A · B through step any of its 2 kinds"));
    // Your code mentions B without returning E: it tells B apart.
    assert_eq!(fails.reach_of(e), super::Reach { can: 4, told: vec![b] });
    assert_eq!(fails.fails_of(mine), None);
}

#[test]
fn a_one_argument_result_reads_the_packages_alias() {
    let (w, [e, _, _, _, _, _, _, alias_user]) = small();
    let names = Names::new(&w);
    let fails = Fails::new(&w, &names);
    assert_eq!(fails.error_of(alias_user), Some(e));
}

#[test]
fn return_types_parse_as_fails_js_reads_them() {
    use super::{alias_args, plain_name, result_args};
    assert_eq!(result_args("Result<Self, RuntimeError>"), Some("Self, RuntimeError"));
    assert_eq!(result_args(" std::result::Result <T, E> "), Some("T, E"));
    assert_eq!(result_args("io::Result<()>"), Some("()"));
    assert_eq!(result_args(":::Result<T>"), Some("T"));
    assert_eq!(result_args("::Result<T>"), None);
    assert_eq!(result_args("MyResult<T, E>"), None);
    assert_eq!(result_args("fmt::Result"), None);
    assert_eq!(result_args("Result<T, E>\n"), Some("T, E"));
    assert_eq!(result_args("Option<Result<T, E>>"), None);
    assert_eq!(alias_args("std::result::Result<T, Error>"), Some("T, Error"));
    assert_eq!(alias_args("Box<Result<T, E>>"), Some("T, E>"));
    assert_eq!(plain_name("&'static io::Error<T>"), "Error");
    assert_eq!(plain_name(" &  Error "), "Error");
}
