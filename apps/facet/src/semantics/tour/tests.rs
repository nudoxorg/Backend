//! Start here against the prototype. `tests/engines.golden` is `tour.js`
//! and `fails.js` run headless over the pinned `tests/fixtures/engines.json`
//! (`node apps/facet/src/semantics/tests/fixtures/engines.mjs golden`); the
//! trim that made the fixture checked every golden line against the full
//! world first. Symbols are named `qual::name:line`, so ids never leak.

use super::{MAX_STOPS, Role, Tour, Why, of, parts_of, plain, use_of};
use crate::graph::model::{NodeId, World};
use crate::semantics::fails::Fails;
use crate::semantics::names::Names;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Instant;

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/semantics/tests")
}

/// The pinned world, with the full world's importance.
pub(crate) fn world() -> &'static World {
    static WORLD: OnceLock<World> = OnceLock::new();
    WORLD.get_or_init(|| {
        let bytes = std::fs::read(dir().join("fixtures/engines.json")).expect("engines.json");
        let mut world = World::from_json(&bytes).expect("the fixture parses");
        let json: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        #[allow(clippy::cast_possible_truncation)]
        let imp: Vec<f32> = json["imp"].as_array().expect("imp").iter().map(|v| v.as_f64().unwrap_or(0.0) as f32).collect();
        assert_eq!(imp.len(), world.len());
        world.importance = imp;
        world
    })
}

/// Its name index.
pub(crate) fn names() -> &'static Names {
    static NAMES: OnceLock<Names> = OnceLock::new();
    NAMES.get_or_init(|| Names::new(world()))
}

/// A symbol's golden name: `qual::name:line`.
pub(crate) fn key(w: &World, i: NodeId) -> String {
    format!("{}::{}:{}", w.qual(i), w.node(i).name, w.node(i).line)
}

/// `key` or `-`.
pub(crate) fn key_or(w: &World, i: Option<NodeId>) -> String {
    i.map_or_else(|| "-".to_owned(), |i| key(w, i))
}

/// The symbol a golden name names.
pub(crate) fn id(name: &str) -> NodeId {
    static IDS: OnceLock<HashMap<String, NodeId>> = OnceLock::new();
    let ids = IDS.get_or_init(|| {
        let w = world();
        let mut ids = HashMap::new();
        for i in 0..u32::try_from(w.len()).unwrap_or(0) {
            assert!(ids.insert(key(w, i), i).is_none(), "{} is not unique", key(w, i));
        }
        ids
    });
    *ids.get(name).unwrap_or_else(|| panic!("no {name} in the fixture"))
}

/// The golden's lines of one kind, split at `|` (the kind dropped).
pub(crate) fn golden(kind: &str) -> Vec<Vec<String>> {
    static TEXT: OnceLock<String> = OnceLock::new();
    let text = TEXT.get_or_init(|| std::fs::read_to_string(dir().join("engines.golden")).expect("engines.golden"));
    let lines: Vec<Vec<String>> = text
        .lines()
        .filter_map(|l| l.strip_prefix(kind).and_then(|r| r.strip_prefix('|')))
        .map(|l| l.split('|').map(str::to_owned).collect())
        .collect();
    assert!(!lines.is_empty(), "no `{kind}` lines in the golden");
    lines
}

/// Every mismatch, or none: `(what, want, got)`.
pub(crate) fn agree(what: &str, rows: Vec<(String, String, String)>) {
    let total = rows.len();
    let bad: Vec<String> =
        rows.into_iter().filter(|(_, want, got)| want != got).map(|(k, want, got)| format!("{k}\n  want {want}\n  got  {got}")).collect();
    assert!(bad.is_empty(), "{what}: {} of {total} differ from the prototype\n{}", bad.len(), bad.join("\n"));
}

fn package(name: &str) -> u32 {
    let w = world();
    let at = w.packages.iter().position(|p| p.name.as_ref() == name).unwrap_or_else(|| panic!("no package {name}"));
    u32::try_from(at).unwrap_or(u32::MAX)
}

fn tour_of(name: &str) -> Tour {
    of(world(), names(), package(name))
}

/// The golden's spelling of a tour: `key=role=why;…`.
fn spelled(w: &World, t: &Tour) -> String {
    t.stops.iter().map(|s| format!("{}={}={}", key(w, s.node), s.role.text(), s.why.text(w))).collect::<Vec<_>>().join(";")
}

/// A stop's name as the plate prints it (`SmallVec::new`).
fn full_name(w: &World, i: NodeId) -> String {
    let n = w.node(i);
    n.parent.map_or_else(|| n.name.to_string(), |p| format!("{}::{}", w.node(p).name, n.name))
}

#[test]
fn every_tour_is_the_prototypes() {
    let rows = golden("tour")
        .into_iter()
        .map(|g| {
            let t = tour_of(&g[0]);
            (g[0].clone(), format!("{}|{}", g[1], g[2]), format!("{}|{}", t.items, spelled(world(), &t)))
        })
        .collect();
    agree("tours", rows);
}

#[test]
fn every_stop_reads_as_the_plate_does() {
    let w = world();
    let mut got: HashMap<String, Vec<String>> = HashMap::new();
    let rows = golden("stop")
        .into_iter()
        .map(|g| {
            let lines = got.entry(g[0].clone()).or_insert_with(|| {
                tour_of(&g[0])
                    .stops
                    .iter()
                    .map(|s| {
                        let lede = w.node(s.node).doc.as_deref().map(plain).unwrap_or_default();
                        format!("{}|{}|{}|{}", full_name(w, s.node), s.role.text(), s.why.text(w), lede)
                    })
                    .collect()
            });
            let line = if lines.is_empty() { String::new() } else { lines.remove(0) };
            (g[0].clone(), g[1..].join("|"), line)
        })
        .collect();
    agree("stops", rows);
}

#[test]
fn the_briefs_roads() {
    let w = world();
    let road = |name: &str| tour_of(name).stops.iter().map(|s| full_name(w, s.node)).collect::<Vec<_>>().join(" → ");
    assert_eq!(road("toml"), "from_str → Value → Table → Array → Index → Error");
    assert_eq!(road("serde_json"), "from_slice → Value → Map → Number → Read → Error");
    assert_eq!(road("serde_core"), "Serialize → Deserialize → Error");
    assert_eq!(road("smallvec"), "SmallVec::new → SmallVec → ExtendFromSlice");
    // serde_core's idea is a trait: its heart carries too little weight to stay.
    let serde = tour_of("serde_core");
    assert_eq!(serde.stops.iter().map(|s| s.role).collect::<Vec<_>>(), [Role::Idea, Role::OtherHalf, Role::WhenItFails]);
    assert_eq!(serde.stops[0].why, Why::Doers(486));
    // smallvec has no free function used outside: the door is the heart's maker.
    assert_eq!(tour_of("smallvec").stops[0].why, Why::GetOne);
    assert!(tour_of("toml").shown() && tour_of("smallvec").shown());
    assert!(tour_of("toml").stops.len() <= MAX_STOPS);
}

#[test]
fn use_is_distinct_outside_items() {
    let w = world();
    let rows = golden("use")
        .into_iter()
        .map(|g| {
            let i = id(&g[0]);
            let u = use_of(w, i, w.node(i).pkg);
            (g[0].clone(), format!("{}|{}", g[1], g[2]), format!("{}|{}", u.n, u.yours))
        })
        .collect();
    agree("use", rows);
}

#[test]
fn parts_are_what_the_type_keys_name() {
    let w = world();
    let rows = golden("parts")
        .into_iter()
        .map(|g| {
            let parts = parts_of(w, names(), id(&g[0])).into_iter().map(|t| key(w, t)).collect::<Vec<_>>().join(",");
            (g[0].clone(), g.get(1).cloned().unwrap_or_default(), parts)
        })
        .collect();
    agree("parts", rows);
}

#[test]
fn plain_drops_marks_and_link_targets() {
    assert_eq!(plain("Deserializes a [`Value`](crate::Value) from **text**."), "Deserializes a Value from text.");
    assert_eq!(plain("See [`Table`] and [Array], `Index` and __it__."), "See Table and Array, Index and it.");
    // A bracket with no target and no closing is left as written.
    assert_eq!(plain("a [b and [c](d) e"), "a b and [c e");
    assert_eq!(plain("[]() and [`]` ***"), "[]() and [`]` *".replace('`', ""));
}

/// The whole world, off by default: times the tour of every package and the
/// fails of every callable (run it `--release`), and writes the answers in
/// `engines.mjs full`'s format to `$ENGINES_OUT` for a parity diff. It asserts
/// nothing about values: the world moves.
#[test]
#[ignore = "reads the live prototype world; run by hand"]
fn whole_world() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../Nudox-Design-System/v4/graph/world.json");
    let bytes = std::fs::read(path).expect("world.json");
    let t0 = Instant::now();
    let w = World::from_json(&bytes).expect("world.json parses");
    let names = Names::new(&w);
    let t_world = t0.elapsed();
    let t0 = Instant::now();
    let tours: Vec<Tour> = (0..u32::try_from(w.packages.len()).unwrap_or(0)).map(|pk| of(&w, &names, pk)).collect();
    let t_tours = t0.elapsed();
    let callables: Vec<NodeId> = (0..u32::try_from(w.len()).unwrap_or(0))
        .filter(|&j| matches!(w.node(j).kind, crate::graph::Kind::Function | crate::graph::Kind::Method))
        .collect();
    let t0 = Instant::now();
    let fails = Fails::new(&w, &names);
    let answers: Vec<_> = callables.iter().map(|&j| fails.fails_of(j)).collect();
    let t_fails = t0.elapsed();
    eprintln!(
        "world {} nodes: parse + names {t_world:.1?}; tours of {} packages {t_tours:.1?}; fails of {} callables {t_fails:.1?}",
        w.len(),
        tours.len(),
        callables.len()
    );
    let Ok(out) = std::env::var("ENGINES_OUT") else { return };
    let mut s = String::new();
    for (pk, t) in tours.iter().enumerate() {
        let _ = writeln!(s, "tour|{}|{}|{}", w.packages[pk].name, t.items, spelled(&w, t));
    }
    for (&j, r) in callables.iter().zip(&answers) {
        let _ = match r {
            Some(r) => writeln!(s, "fails|{}|{}|{}|{}", key(&w, j), key(&w, r.error), r.all, crate::semantics::fails::tests::given(&w, &r.kinds)),
            None => writeln!(s, "fails|{}|-", key(&w, j)),
        };
    }
    crate::semantics::fails::tests::errors_in(&w, &fails, &mut s);
    for (&j, r) in callables.iter().zip(&answers) {
        if r.is_some()
            && let Some(line) = fails.kinds_line(j)
        {
            let _ = writeln!(s, "line|{}|{}", key(&w, j), line.words(&w));
        }
    }
    std::fs::write(out, s).expect("write");
}
