use super::*;
use crate::graph::model::{Module, Node, Package};

fn world(nodes: Vec<Node>) -> World {
    World::new(
        vec![Package {
            name: "demo".into(),
            version: "0.1".into(),
            yours: true,
            external: false,
            deps: Vec::new(),
        }],
        vec![Module {
            pkg: 0,
            path: "".into(),
            file: "src/lib.rs".into(),
        }],
        nodes,
        Vec::new(),
    )
    .expect("synthetic world")
}
fn item(kind: Kind, name: &str) -> Node {
    let mut n = Node::new(kind, name.to_owned(), 0, 0);
    n.vis = Some("pub".into());
    n.file = Some("src/lib.rs".into());
    n.non_exhaustive = true;
    n
}
fn call(name: &str, owner: Option<NodeId>, recv: bool, params: &[&str], ret: &str) -> Node {
    let mut n = item(if recv { Kind::Method } else { Kind::Function }, name);
    n.parent = owner;
    n.recv = recv.then(|| "reads".into());
    n.params = params.iter().map(|p| (*p).into()).collect();
    n.ret = Some(ret.to_owned().into());
    n
}
fn road_world() -> World {
    world(vec![
        item(Kind::Struct, "Invocation"),
        item(Kind::Struct, "Grammar"),
        call("grammar", Some(0), true, &[], "Option<Grammar>"),
        call("aliases", Some(1), true, &[], "Vec<String>"),
        call("maybe_name", Some(1), true, &[], "Option<String>"),
    ])
}

#[test]
fn arrows_and_words_select_shape_search() {
    for q in [
        "path -> text",
        "path → text",
        "path => text",
        "takes path gives text",
        "gives text",
    ] {
        assert!(is_shape(q), "{q}");
    }
    assert!(!is_shape("Value"));
}

#[test]
fn named_search_accepts_a_package_prefix_and_caches_the_same_result() {
    let w = world(vec![
        item(Kind::Struct, "Value"),
        item(Kind::Struct, "Values"),
    ]);
    let d = Discovery::new(&w);
    let a = d.query(&w, "demo Value");
    let b = d.query(&w, "demo Value");
    assert!(Rc::ptr_eq(&a, &b));
    assert_eq!(a.lit, [0, 1]);
    assert_eq!(a.rows[0].node, 0);
    assert_eq!(a.packages, 1);
    assert!(d.query(&w, "").lit.is_empty());
    assert!(d.query(&w, "missing::Value").lit.is_empty());
}

#[test]
fn shape_inputs_are_unordered_and_overloads_fold() {
    let w = world(vec![
        item(Kind::Struct, "Span"),
        call(
            "new",
            Some(0),
            false,
            &["text: String", "size: usize"],
            "Self",
        ),
        call("new", Some(0), false, &["text: &str", "size: u32"], "Self"),
    ]);
    let d = Discovery::new(&w);
    let a = d.query(&w, "number, text -> Span");
    assert_eq!(a.lit, [1]);
    assert_eq!(a.rows[0].shape, "(text, a number) → Span");
    assert_eq!(d.query(&w, "takes text and number gives Span").lit, a.lit);
    assert!(d.query(&w, "text, text -> Span").lit.is_empty());
}

#[test]
fn rust_wrappers_match_plain_words_and_optional_answers_rank_first() {
    let w = world(vec![
        call(
            "read",
            None,
            false,
            &["path: PathBuf"],
            "Result<Option<String>, Error>",
        ),
        call("name", None, false, &["path: &Path"], "String"),
    ]);
    let d = Discovery::new(&w);
    let a = d.query(&w, "path -> maybe text");
    assert_eq!(a.lit, [0, 1]);
    assert!(a.rows[0].shape.ends_with("maybe text or fails"));
    assert_eq!(d.query(&w, "&Path -> Option<String>").lit, a.lit);
}

#[test]
fn a_concrete_value_matches_its_derived_external_or_written_traits() {
    let mut schema = item(Kind::Struct, "WireSchema");
    schema.impls_ext.push("serde::Serialize".into());
    let mut stringify = call(
        "to_string",
        None,
        false,
        &["value: &T"],
        "Result<String, Error>",
    );
    stringify.generics = Some("T: Serialize".into());
    let w = world(vec![
        schema,
        stringify,
        call("own_text", None, false, &["value: WireSchema"], "String"),
    ]);
    let d = Discovery::new(&w);
    let a = d.query(&w, "WireSchema -> text");
    assert_eq!(a.lit, [2, 1]);
    assert!(a.chains.is_empty());
}

#[test]
fn the_road_consumes_your_value_and_folds_calls_into_type_places() {
    let w = road_world();
    let d = Discovery::new(&w);
    let a = d.query(&w, "Invocation -> list of text");
    assert!(a.rows.is_empty());
    assert_eq!(a.chains.len(), 1);
    let c = &a.chains[0];
    assert_eq!(c.path, [0, 2, 3]);
    assert_eq!(c.steps.len(), 2);
    assert_eq!(c.stops.iter().map(|s| s.node).collect::<Vec<_>>(), [0, 1]);
    assert!(c.stops[0].yours);
    assert_eq!(c.stops[0].label, "Invocation · grammar");
    assert_eq!(c.stops[1].label, "Grammar · aliases");
    assert_eq!(c.code, "invocation.grammar()?.aliases()");
    assert_eq!(
        c.brief,
        "from Invocation › grammar › aliases → list of text"
    );
}

#[test]
fn only_intermediate_optional_calls_are_unwrapped() {
    let w = road_world();
    let d = Discovery::new(&w);
    let a = d.query(&w, "Invocation -> maybe text");
    let c = a
        .chains
        .iter()
        .find(|c| c.steps.last().is_some_and(|s| s.node == 4))
        .expect("optional answer road");
    assert_eq!(c.code, "invocation.grammar()?.maybe_name()");
    assert!(c.steps.last().expect("last").maybe);
}

#[test]
fn variants_are_places_but_never_hollow_method_detours() {
    let mut variant = item(Kind::Variant, "Wrapped").member_of(1);
    variant.ty = Some("Input".into());
    variant.shape = Some("tuple".into());
    let w = world(vec![
        item(Kind::Struct, "Input"),
        item(Kind::Enum, "Value"),
        variant,
        call("as_text", Some(1), true, &[], "Option<String>"),
    ]);
    let d = Discovery::new(&w);
    assert!(d.query(&w, "Input -> text").chains.is_empty());
}

#[test]
fn constructor_getter_echoes_and_identity_views_are_not_roads() {
    let w = world(vec![
        item(Kind::Struct, "Input"),
        item(Kind::Struct, "Head"),
        call(
            "new",
            Some(1),
            false,
            &["input: Input", "sequence: usize"],
            "Self",
        ),
        call("sequence", Some(1), true, &[], "usize"),
        call("clone", Some(0), true, &[], "Head"),
        call("text", Some(1), true, &[], "String"),
    ]);
    let d = Discovery::new(&w);
    assert!(d.query(&w, "Input -> number").chains.is_empty());
    let a = d.query(&w, "Input -> text");
    assert_eq!(a.chains.len(), 1);
    assert!(
        a.chains
            .iter()
            .all(|c| c.steps.iter().all(|s| s.verb != "clone"))
    );
}

#[test]
fn plain_values_alone_and_four_calls_get_no_road() {
    let w = world(vec![
        item(Kind::Struct, "Alpha"),
        item(Kind::Struct, "Bravo"),
        item(Kind::Struct, "Charlie"),
        item(Kind::Struct, "Delta"),
        call("a", None, false, &["text: String"], "Alpha"),
        call("b", None, false, &["a: Alpha"], "Bravo"),
        call("c", None, false, &["b: Bravo"], "Charlie"),
        call("d", None, false, &["c: Charlie"], "Delta"),
        call("text", None, false, &["d: Delta"], "String"),
    ]);
    let d = Discovery::new(&w);
    assert!(d.query(&w, "text -> Delta").chains.is_empty());
    assert!(d.query(&w, "Alpha -> text").chains.is_empty());
    assert_eq!(d.query(&w, "Alpha -> Charlie").chains[0].steps.len(), 2);
}

#[test]
fn a_side_input_that_only_your_value_can_make_is_not_opaque() {
    let w = world(vec![
        item(Kind::Struct, "Input"),
        item(Kind::Struct, "Middle"),
        item(Kind::Struct, "Side"),
        call("middle", None, false, &["input: Input"], "Middle"),
        call("side", None, false, &["input: Input"], "Side"),
        call("finish", Some(1), true, &["side: Side"], "String"),
    ]);
    let d = Discovery::new(&w);
    let a = d.query(&w, "Input -> text");
    let c = a.chains.first().expect("seeded side input");
    assert_eq!(c.cost, 3.3);
    assert!(c.code.contains("side(input)"));
    assert!(c.rail.contains("+ Side side"));
}

#[test]
fn constellation_is_bounded_and_membership_is_cached() {
    let nodes = (0..450)
        .map(|i| item(Kind::Struct, &format!("Match{i}")))
        .collect();
    let w = world(nodes);
    let d = Discovery::new(&w);
    let a = d.query(&w, "Match");
    assert_eq!(a.lit.len(), LIT);
    assert_eq!(a.lit_set.len(), LIT);
    assert_eq!(a.rows.len(), ROWS);
    assert!(a.lit.iter().all(|i| a.lit_set.contains(i)));
}

#[test]
fn pinned_recipe_fixture_has_deterministic_cached_queries() {
    let bytes = include_bytes!("../../semantics/tests/fixtures/recipes.json");
    let w = World::from_json(bytes).expect("pinned fixture");
    let begin = std::time::Instant::now();
    let d = Discovery::new(&w);
    let build = begin.elapsed();
    let begin = std::time::Instant::now();
    let a = d.query(&w, "number -> Value");
    let query = begin.elapsed();
    assert!(!a.rows.is_empty());
    assert_eq!(a.as_ref(), d.query(&w, "number -> Value").as_ref());
    eprintln!(
        "discovery pinned {} nodes: build {build:?}, cold query {query:?}",
        w.len()
    );
}

#[test]
fn generic_associated_outputs_never_match_unrelated_same_named_types() {
    let mut visit = call("visit", None, false, &["value: V"], "V::Value");
    visit.generics = Some("V: Visitor".into());
    let mut project = call(
        "project",
        None,
        false,
        &["value: T"],
        "<T as Source>::Value",
    );
    project.generics = Some("T: Source".into());
    let w = world(vec![item(Kind::Struct, "Value"), visit, project]);
    let d = Discovery::new(&w);
    assert!(d.query(&w, "gives Value").lit.is_empty());
    assert_eq!(
        d.recipes
            .table()
            .iter()
            .filter(|e| matches!(e.how, How::Call))
            .map(|e| e.out.as_str())
            .collect::<Vec<_>>(),
        ["any", "any"]
    );
}

#[test]
fn prepared_discovery_is_send_and_reconstructs_identical_answers() {
    fn assert_send<T: Send>() {}
    assert_send::<PreparedDiscovery>();
    let w = road_world();
    let direct = Discovery::new(&w);
    let loaded = Discovery::from_prepared(Discovery::prepare(&w));
    assert_eq!(
        direct.query(&w, "Invocation -> list of text"),
        loaded.query(&w, "Invocation -> list of text")
    );
}

#[test]
fn variants_can_start_a_real_three_call_conversion() {
    let mut variant = item(Kind::Variant, "Rust").member_of(1);
    variant.ty = Some("RustEdition".into());
    variant.shape = Some("tuple".into());
    let w = world(vec![
        item(Kind::Struct, "RustEdition"),
        item(Kind::Enum, "LanguageProfile"),
        item(Kind::Struct, "Language"),
        variant,
        call(
            "from",
            Some(2),
            false,
            &["profile: LanguageProfile"],
            "Self",
        ),
        call("name", Some(2), true, &[], "String"),
    ]);
    let d = Discovery::new(&w);
    let a = d.query(&w, "RustEdition -> text");
    assert_eq!(a.chains.len(), 1);
    let c = &a.chains[0];
    assert_eq!(c.steps.len(), 3);
    assert_eq!(c.cost, 3.0);
    assert_eq!(
        c.code,
        "let language = Language::from(LanguageProfile::Rust(rust_edition));\nlanguage.name()"
    );
    assert_eq!(
        c.stops.last().expect("answer place").label,
        "Language · from › name"
    );
}

#[test]
fn a_trait_can_carry_your_value_along_the_spine() {
    let mut schema = item(Kind::Struct, "WireSchema");
    schema.derives.push("Serialize".into());
    let mut encode = call("encode", None, false, &["schema: &T"], "Encoded");
    encode.generics = Some("T: Serialize".into());
    let w = world(vec![
        schema,
        item(Kind::Struct, "Encoded"),
        encode,
        call("text", Some(1), true, &[], "String"),
    ]);
    let d = Discovery::new(&w);
    let a = d.query(&w, "WireSchema -> text");
    let c = a.chains.first().expect("trait-carried road");
    assert_eq!(c.from, "#0");
    assert_eq!(c.via.as_deref(), Some("any Serialize"));
    assert_eq!(c.code, "encode(schema).text()");
    assert!(c.rail.starts_with("from your WireSchema, a Serialize"));
}

#[test]
fn tuples_and_rust_or_plain_maps_use_the_shared_key_vocabulary() {
    let w = world(vec![call(
        "map",
        None,
        false,
        &["map: HashMap<String, u32>"],
        "(String, usize)",
    )]);
    let d = Discovery::new(&w);
    assert_eq!(
        d.query(&w, "map text to number -> (String, usize)").lit,
        [0]
    );
    assert_eq!(
        d.query(&w, "HashMap<&str, u64> -> (String, usize)").lit,
        [0]
    );
}

/// Informational timings; assertions never depend on mutable extracted data.
#[test]
#[ignore = "full-world startup and input timing; run explicitly after integration"]
fn full_world_startup_and_first_input_cost() {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../Nudox-Design-System/v4/graph/world.json");
    let bytes = std::fs::read(path).expect("prototype snapshot");
    let w = World::from_json(&bytes).expect("world");
    let start = std::time::Instant::now();
    let prepared = Discovery::prepare(&w);
    let build = start.elapsed();
    let start = std::time::Instant::now();
    let d = Discovery::from_prepared(prepared);
    let attach = start.elapsed();
    for q in [
        "toml Value",
        "path -> maybe text",
        "Invocation -> list of text",
        "RustEdition -> text",
    ] {
        let start = std::time::Instant::now();
        let first = d.query(&w, q);
        let cold = start.elapsed();
        let start = std::time::Instant::now();
        let second = d.query(&w, q);
        let warm = start.elapsed();
        assert!(Rc::ptr_eq(&first, &second));
        eprintln!(
            "query {q:?}: cold {cold:?}, cached {warm:?}, {} direct/{} chains",
            first.lit.len(),
            first.chains.len()
        );
    }
    let start = std::time::Instant::now();
    let baseline = crate::semantics::names::Names::new(&w);
    let name_build = start.elapsed();
    let package = (0..u32::try_from(w.packages.len()).unwrap())
        .max_by_key(|&package| d.package_items(package).len())
        .unwrap();
    let start = std::time::Instant::now();
    let tour = d.package_tour(package).expect("prepared package tour");
    let tour_build = start.elapsed();
    eprintln!(
        "largest package first tour: {tour_build:?}, {} indexed candidates/{} eligible items/{} stops; old action Names build {name_build:?}",
        d.package_items(package).len(),
        tour.items,
        tour.stops.len()
    );
    std::hint::black_box(baseline);
    eprintln!(
        "discovery full {} nodes: prepare {build:?}, attach {attach:?}",
        w.len()
    );
}

fn generated_road(seed: usize) -> World {
    let calls = 2 + seed % 2;
    let mut nodes: Vec<_> = (0..calls)
        .map(|a| item(Kind::Struct, &format!("Place{seed}_{a}")))
        .collect();
    for a in 0..calls {
        let ret = if a + 1 == calls {
            "String".to_owned()
        } else {
            format!("Place{seed}_{}", a + 1)
        };
        let params = if a == 0 && seed % 5 == 0 {
            vec!["count: usize"]
        } else {
            Vec::new()
        };
        nodes.push(call(
            &format!("step{a}"),
            Some(NodeId::try_from(a).expect("small fixture")),
            true,
            &params,
            &ret,
        ));
    }
    world(nodes)
}

fn permuted(w: &World, shift: usize) -> World {
    let len = w.len();
    let old_order: Vec<_> = (0..len).map(|a| (a + shift) % len).rev().collect();
    let mut new_id = vec![0; len];
    for (a, &old) in old_order.iter().enumerate() {
        new_id[old] = NodeId::try_from(a).expect("small fixture");
    }
    let nodes = old_order
        .into_iter()
        .map(|old| {
            let mut node = w.nodes[old].clone();
            node.parent = node.parent.map(|p| new_id[p as usize]);
            for imp in &mut node.impls {
                imp.trait_ = imp.trait_.map(|i| new_id[i as usize]);
            }
            node
        })
        .collect();
    let edges = w
        .edges
        .iter()
        .map(|e| crate::graph::Edge {
            from: new_id[e.from as usize],
            to: new_id[e.to as usize],
            rel: e.rel,
        })
        .collect();
    World::new(w.packages.clone(), w.modules.clone(), nodes, edges).expect("permuted world")
}

fn semantic_answers(w: &World, search: &Search) -> (Vec<String>, Vec<(String, String)>) {
    (
        search.rows.iter().map(|row| row.label.clone()).collect(),
        search
            .chains
            .iter()
            .map(|chain| {
                (
                    format!(
                        "{} → {}",
                        recipes::key_words(w, &chain.from),
                        recipes::key_words(w, &chain.output)
                    ),
                    chain.code.clone(),
                )
            })
            .collect(),
    )
}

#[test]
fn generated_graph_permutations_preserve_the_same_semantic_roads() {
    for seed in 0..32 {
        let w = generated_road(seed);
        let q = format!("Place{seed}_0 -> text");
        let expected = Discovery::new(&w).query(&w, &q);
        assert_eq!(expected.chains.len(), 1, "seed {seed}");
        for shift in [0, 1, w.len() / 2] {
            let p = permuted(&w, shift);
            let got = Discovery::new(&p).query(&p, &q);
            assert_eq!(
                semantic_answers(&w, &expected),
                semantic_answers(&p, &got),
                "seed {seed}, shift {shift}"
            );
            assert_eq!(expected.chains[0].cost, got.chains[0].cost);
        }
    }
}

#[test]
fn generated_irrelevant_producers_do_not_change_best_answers() {
    for seed in 0..32 {
        let w = generated_road(seed);
        let q = format!("Place{seed}_0 -> text");
        let expected = Discovery::new(&w).query(&w, &q);
        let mut nodes = w.nodes.clone();
        let outsider = NodeId::try_from(nodes.len()).expect("small fixture");
        nodes.push(item(Kind::Struct, "Unrelated"));
        for a in 0..12 {
            nodes.push(call(
                &format!("unrelated_{a}"),
                Some(outsider),
                false,
                &["number: usize"],
                "Self",
            ));
        }
        let larger = world(nodes);
        let got = Discovery::new(&larger).query(&larger, &q);
        assert_eq!(
            semantic_answers(&w, &expected),
            semantic_answers(&larger, &got),
            "seed {seed}"
        );
    }
}

#[test]
fn generated_chains_consume_the_input_never_cycle_and_can_build_every_side_input() {
    for seed in 0..48 {
        let w = generated_road(seed);
        let d = Discovery::new(&w);
        let q = format!("Place{seed}_0 -> text");
        let result = d.query(&w, &q);
        let have = HashSet::from(["#0".to_owned()]);
        let r0 = d.recipes.run_with_inputs(&w, PUBLIC, &have);
        for chain in &result.chains {
            assert_eq!(chain.from, "#0");
            assert!(chain.cost <= 3.6);
            assert!(chain.steps.len() <= 3);
            assert_eq!(chain.steps.first().expect("spine").input, chain.from);
            let mut consumed = chain.from.clone();
            let mut visited = HashSet::from([consumed.clone()]);
            for step in &chain.steps {
                assert_eq!(step.input, consumed, "seed {seed}, {}", step.verb);
                assert!(
                    visited.insert(step.output.clone()),
                    "cycle at {}",
                    step.output
                );
                let e = d
                    .recipes
                    .table()
                    .iter()
                    .find(|e| e.node == step.node)
                    .expect("real producer");
                let spine = e
                    .ins
                    .iter()
                    .position(|k| k == &step.input)
                    .expect("consumed input");
                for (a, k) in e.ins.iter().enumerate() {
                    if a != spine {
                        assert!(
                            r0.cost.get(k).is_some_and(|c| c.is_finite()),
                            "unbuildable {k}"
                        );
                    }
                }
                consumed = step.output.clone();
            }
            assert_eq!(consumed, chain.output);
        }
        assert!(!result.chains.is_empty(), "seed {seed}");
    }
}

#[test]
fn generated_overload_folding_is_stable_under_node_and_parameter_order() {
    for seed in 0..16 {
        let mut nodes = vec![item(Kind::Struct, "Span")];
        for a in 0..8 {
            nodes.push(call(
                "new",
                Some(0),
                false,
                &[
                    if a % 2 == 0 {
                        "value: String"
                    } else {
                        "value: &str"
                    },
                    "number: usize",
                ],
                "Self",
            ));
        }
        let w = world(nodes);
        let p = permuted(&w, seed % w.len());
        for q in ["text, number -> Span", "number, text -> Span"] {
            let expected = Discovery::new(&w).query(&w, q);
            let got = Discovery::new(&p).query(&p, q);
            assert_eq!(expected.rows.len(), 1);
            assert_eq!(semantic_answers(&w, &expected), semantic_answers(&p, &got));
            assert_eq!(expected.rows[0].shape, got.rows[0].shape);
        }
    }
}

#[test]
fn worker_results_share_indexes_and_enter_the_same_bounded_cache() {
    fn assert_send<T: Send>() {}
    assert_send::<Search>();
    let w = road_world();
    let d = Discovery::new(&w);
    let prepared = d.prepared();
    let again = prepared.clone();
    assert!(Arc::ptr_eq(&prepared.names, &again.names));
    assert!(Arc::ptr_eq(&prepared.by_in, &again.by_in));
    assert!(d.cached("Invocation -> list of text").is_none());
    let worker = Discovery::query_prepared(prepared, &w, "Invocation -> list of text");
    let kept = d.remember("Invocation -> list of text", worker);
    assert!(Rc::ptr_eq(
        &kept,
        &d.cached("Invocation -> list of text").expect("cached")
    ));
    let old = kept.clone();
    for a in 0..CACHE {
        let _ = d.query(&w, &format!("missing{a}"));
    }
    assert!(d.cached("Invocation -> list of text").is_none());
    assert_eq!(
        old.as_ref(),
        d.query(&w, "Invocation -> list of text").as_ref()
    );
}

#[test]
fn every_required_trait_must_hold_for_a_supplied_generic_value() {
    let mut schema = item(Kind::Struct, "Schema");
    schema.derives.push("Serialize".into());
    let mut encode = call("encode", None, false, &["value: T"], "Encoded");
    encode.generics = Some("T: Serialize + Special".into());
    let mut stringify = call("stringify", None, false, &["value: T"], "String");
    stringify.generics = encode.generics.clone();
    let w = world(vec![
        schema.clone(),
        item(Kind::Struct, "Encoded"),
        encode.clone(),
        stringify.clone(),
        call("text", Some(1), true, &[], "String"),
    ]);
    let d = Discovery::new(&w);
    let missing = d.query(&w, "Schema -> text");
    assert!(missing.rows.is_empty());
    assert!(missing.chains.is_empty());
    schema.derives.push("Special".into());
    let both = world(vec![
        schema,
        item(Kind::Struct, "Encoded"),
        encode,
        stringify,
        call("text", Some(1), true, &[], "String"),
    ]);
    let matched = Discovery::new(&both).query(&both, "Schema -> text");
    assert_eq!(matched.rows[0].label, "stringify");
    assert_eq!(matched.chains.len(), 1);
}

#[test]
fn same_named_traits_in_different_packages_are_distinct_capabilities() {
    let mut schema = item(Kind::Struct, "Schema");
    schema.impls.push(crate::graph::model::Impl {
        trait_: Some(2),
        line: 0,
        members: Vec::new(),
        generic: false,
    });
    let trait_a = item(Kind::Trait, "Codec");
    let mut trait_b = item(Kind::Trait, "Codec");
    trait_b.pkg = 1;
    trait_b.module = 1;
    let mut encode = call("encode", None, false, &["value: T"], "String");
    encode.generics = Some("T: a::Codec".into());
    let packages = vec![
        Package {
            name: "a".into(),
            version: "1".into(),
            yours: true,
            external: false,
            deps: vec![1],
        },
        Package {
            name: "b".into(),
            version: "1".into(),
            yours: false,
            external: true,
            deps: Vec::new(),
        },
    ];
    let modules = vec![
        Module {
            pkg: 0,
            path: "".into(),
            file: "src/lib.rs".into(),
        },
        Module {
            pkg: 1,
            path: "".into(),
            file: "src/lib.rs".into(),
        },
    ];
    let wrong = World::new(
        packages.clone(),
        modules.clone(),
        vec![
            schema.clone(),
            trait_a.clone(),
            trait_b.clone(),
            encode.clone(),
        ],
        Vec::new(),
    )
    .expect("distinct traits");
    assert!(
        Discovery::new(&wrong)
            .query(&wrong, "Schema -> text")
            .rows
            .is_empty()
    );
    schema.impls[0].trait_ = Some(1);
    let right = World::new(
        packages,
        modules,
        vec![schema, trait_a, trait_b, encode],
        Vec::new(),
    )
    .expect("correct trait");
    assert_eq!(
        Discovery::new(&right).query(&right, "Schema -> text").rows[0].label,
        "encode"
    );
}

#[test]
fn qualified_external_capabilities_do_not_collapse_by_final_name() {
    let mut schema = item(Kind::Struct, "Schema");
    schema.impls_ext.push("b::Codec".into());
    let mut wrong = call("wrong", None, false, &["value: T"], "String");
    wrong.generics = Some("T: a::Codec".into());
    let mut right = call("right", None, false, &["value: T"], "String");
    right.generics = Some("T: b::Codec".into());
    let w = world(vec![schema, wrong, right]);
    let d = Discovery::new(&w);
    assert_eq!(
        d.query(&w, "Schema -> text")
            .rows
            .iter()
            .map(|r| r.label.as_str())
            .collect::<Vec<_>>(),
        ["right"]
    );
}

#[test]
fn generated_input_assignment_matches_exhaustive_optima_and_is_permutation_invariant() {
    fn optimum(weights: &[Vec<u8>], row: usize, used: u32) -> Option<f64> {
        if row == weights.len() {
            return Some(0.0);
        }
        weights[row]
            .iter()
            .enumerate()
            .filter(|(column, weight)| **weight > 0 && used & (1 << column) == 0)
            .filter_map(|(column, weight)| {
                optimum(weights, row + 1, used | (1 << column))
                    .map(|tail| tail + f64::from(*weight))
            })
            .max_by(f64::total_cmp)
    }
    for seed in 0..256usize {
        let weights: Vec<Vec<u8>> = (0..3)
            .map(|row| {
                (0..4)
                    .map(|column| {
                        u8::try_from((seed * 17 + row * 11 + column * 7 + (seed >> row)) % 3)
                            .expect("weight")
                    })
                    .collect()
            })
            .collect();
        let keys: Vec<_> = (0..4).map(|a| format!("slot{a}")).collect();
        let ins: Vec<_> = weights
            .iter()
            .map(|row| Words {
                keys: row
                    .iter()
                    .enumerate()
                    .filter(|(_, w)| **w == 2)
                    .map(|(j, _)| keys[j].clone())
                    .collect(),
                maybe: false,
            })
            .collect();
        let via: Vec<HashSet<_>> = weights
            .iter()
            .map(|row| {
                row.iter()
                    .enumerate()
                    .filter(|(_, w)| **w == 1)
                    .map(|(j, _)| keys[j].clone())
                    .collect()
            })
            .collect();
        let permitted = vec![vec![true; 4]; 3];
        let expected = optimum(&weights, 0, 0).map(|score| score - 0.6);
        assert_eq!(
            input_score(&ins, &via, &keys, &permitted),
            expected,
            "seed {seed}"
        );
        for order in [[0, 1, 2], [2, 0, 1], [1, 2, 0], [2, 1, 0]] {
            let reordered: Vec<_> = order.iter().map(|&a| ins[a].clone()).collect();
            let traits: Vec<_> = order.iter().map(|&a| via[a].clone()).collect();
            assert_eq!(
                input_score(&reordered, &traits, &keys, &permitted),
                expected,
                "seed {seed}, {order:?}"
            );
        }
    }
}

#[test]
fn presentation_plain_words_never_erase_additional_generic_requirements() {
    let mut displayed = call("displayed", None, false, &["value: T"], "Encoded");
    displayed.generics = Some("T: Display + Special".into());
    let mut borrowed = call("borrowed", None, false, &["value: T"], "Encoded");
    borrowed.generics = Some("T: AsRef<str> + Special".into());
    let mut simple = call("simple", None, false, &["value: T"], "Encoded");
    simple.generics = Some("T: Display".into());
    let w = world(vec![
        item(Kind::Struct, "Encoded"),
        item(Kind::Trait, "Special"),
        displayed,
        borrowed,
        simple,
    ]);
    let d = Discovery::new(&w);
    let result = d.query(&w, "text -> Encoded");
    assert_eq!(
        result
            .rows
            .iter()
            .map(|row| row.label.as_str())
            .collect::<Vec<_>>(),
        ["simple"]
    );
}

#[test]
fn a_custom_display_trait_is_not_the_standard_text_capability() {
    let mut encode = call("encode", None, false, &["value: T"], "String");
    encode.generics = Some("T: Display".into());
    let w = world(vec![item(Kind::Trait, "Display"), encode]);
    assert!(Discovery::new(&w).query(&w, "text -> text").rows.is_empty());
}

#[test]
fn an_intermediate_plain_output_cannot_bypass_an_additional_trait_bound() {
    let mut encode = call("encode", None, false, &["value: T"], "Encoded");
    encode.generics = Some("T: Display + Special".into());
    let w = world(vec![
        item(Kind::Struct, "Raw"),
        item(Kind::Struct, "Encoded"),
        item(Kind::Trait, "Special"),
        call("text", Some(0), true, &[], "String"),
        encode,
    ]);
    let d = Discovery::new(&w);
    assert!(d.query(&w, "text -> Encoded").rows.is_empty());
    assert!(d.query(&w, "Raw -> Encoded").chains.is_empty());
}

#[test]
fn unmet_generic_bounds_cannot_fabricate_a_side_input() {
    let mut encode = call("encode", None, false, &["value: T"], "Side");
    encode.generics = Some("T: Display + Special".into());
    let w = world(vec![
        item(Kind::Struct, "Raw"),
        item(Kind::Struct, "Middle"),
        item(Kind::Struct, "Side"),
        item(Kind::Trait, "Special"),
        call("middle", Some(0), true, &[], "Middle"),
        encode,
        call("finish", Some(1), true, &["side: Side"], "String"),
    ]);
    let d = Discovery::new(&w);
    assert!(d.query(&w, "Raw -> text").chains.is_empty());
}

#[test]
fn repeated_generic_inputs_must_unify_to_one_concrete_type() {
    let mut alpha = item(Kind::Struct, "Alpha");
    alpha.derives.push("Serialize".into());
    let mut beta = item(Kind::Struct, "Beta");
    beta.derives.push("Serialize".into());
    let mut merge = call("merge", None, false, &["left: T", "right: T"], "String");
    merge.generics = Some("T: Serialize".into());
    let w = world(vec![alpha, beta, merge]);
    let d = Discovery::new(&w);
    assert!(d.query(&w, "Alpha, Beta -> text").rows.is_empty());
    assert_eq!(d.query(&w, "Alpha, Alpha -> text").rows[0].label, "merge");
    assert_eq!(d.query(&w, "Beta, Beta -> text").rows[0].label, "merge");
}

#[test]
fn erased_nominal_arguments_never_prove_inputs_outputs_or_side_recipes() {
    let mut wrapper = item(Kind::Struct, "Wrapper");
    wrapper.generics = Some("T".into());
    let mut encode = call("encode", None, false, &["value: Wrapper<T>"], "Encoded");
    encode.generics = Some("T: Special".into());
    let w = world(vec![
        item(Kind::Struct, "Raw"),
        item(Kind::Struct, "Alpha"),
        item(Kind::Struct, "Beta"),
        wrapper,
        item(Kind::Struct, "Encoded"),
        item(Kind::Struct, "Side"),
        encode,
        call("wrap_alpha", None, false, &["raw: Raw"], "Wrapper<Alpha>"),
        call("make_side", None, false, &["value: Wrapper<Alpha>"], "Side"),
        call(
            "use_side",
            None,
            false,
            &["raw: Raw", "side: Side"],
            "Encoded",
        ),
    ]);
    let d = Discovery::new(&w);
    for q in ["Wrapper<Beta> -> Encoded", "Raw -> Wrapper<Beta>"] {
        let result = d.query(&w, q);
        assert!(result.rows.is_empty(), "{q}: {:?}", result.rows);
        assert!(result.chains.is_empty(), "{q}: {:?}", result.chains);
    }
    let side = d.query(&w, "Raw -> Encoded");
    assert!(side.chains.is_empty());
    assert_eq!(side.rows[0].label, "use_side");
    assert!(side.rows[0].shape.contains("Side"));
    assert_eq!(d.query(&w, "wrap_alpha").rows[0].node, 7);
}

#[test]
fn proof_status_tracks_preserved_generic_unknown_and_erased_representations() {
    use super::proof::TypeProof;
    let mut alpha = item(Kind::Struct, "Alpha");
    alpha.derives.push("Special".into());
    let mut wrapper = item(Kind::Struct, "Wrapper");
    wrapper.generics = Some("T".into());
    let mut generic = call("generic", None, false, &["value: T"], "String");
    generic.generics = Some("T: Special".into());
    let mut nested = call("nested", None, false, &["value: Vec<T>"], "String");
    nested.generics = Some("T: Special".into());
    let mut projection = call("projection", None, false, &["value: Alpha"], "T::Value");
    projection.generics = Some("T: Special".into());
    let mut generic_output = call("generic_output", None, false, &["value: T"], "T");
    generic_output.generics = Some("T: Special".into());
    let mut asynchronous = call("asynchronous", None, false, &["value: Alpha"], "String");
    asynchronous.quals.push("async".into());
    let w = world(vec![
        alpha,
        wrapper,
        item(Kind::Trait, "Special"),
        call("plain", None, false, &["value: Alpha"], "String"),
        generic,
        call("applied", None, false, &["value: Wrapper<Alpha>"], "String"),
        projection,
        nested,
        call("extent", None, false, &["value: [u8; 4]"], "String"),
        call(
            "callback",
            None,
            false,
            &["value: fn(Alpha) -> String"],
            "String",
        ),
        asynchronous,
        call(
            "structural",
            None,
            false,
            &["value: Vec<Alpha>"],
            "HashMap<String, Alpha>",
        ),
        generic_output,
    ]);
    let d = Discovery::new(&w);
    let states: Vec<_> = d
        .recipes
        .table()
        .iter()
        .enumerate()
        .map(|(q, e)| (w.node(e.node).name.as_ref(), d.proof[q]))
        .collect();
    assert_eq!(
        states,
        [
            ("plain", TypeProof::Proven),
            ("generic", TypeProof::Generic),
            ("applied", TypeProof::Erased),
            ("projection", TypeProof::Unknown),
            ("nested", TypeProof::Erased),
            ("extent", TypeProof::Erased),
            ("callback", TypeProof::Erased),
            ("asynchronous", TypeProof::Erased),
            ("structural", TypeProof::Proven),
            ("generic_output", TypeProof::Erased),
        ]
    );
    assert_eq!(
        d.query(&w, "list of Alpha -> map text to Alpha").rows[0].label,
        "structural"
    );
    assert!(
        d.query(&w, "Alpha -> text")
            .rows
            .iter()
            .any(|r| r.label == "generic")
    );
}

#[test]
fn qualified_nominal_types_must_agree_with_the_retained_node_identity() {
    let mut a = item(Kind::Struct, "Token");
    a.module = 1;
    let mut b = item(Kind::Struct, "Token");
    b.module = 2;
    let mut local = call("local", None, false, &["value: Token"], "String");
    local.module = 1;
    let mut w = World::new(
        vec![Package {
            name: "demo".into(),
            version: "0.1".into(),
            yours: true,
            external: false,
            deps: Vec::new(),
        }],
        ["", "a", "b"]
            .into_iter()
            .map(|path| Module {
                pkg: 0,
                path: path.into(),
                file: "src/lib.rs".into(),
            })
            .collect(),
        vec![
            a,
            b,
            call("from_a", None, false, &["value: a::Token"], "String"),
            call("from_b", None, false, &["value: b::Token"], "String"),
            local,
        ],
        Vec::new(),
    )
    .unwrap();
    w.importance[1] = 100.0;
    let d = Discovery::new(&w);
    assert_eq!(
        d.query(&w, "b::Token -> text")
            .rows
            .iter()
            .map(|r| r.label.as_str())
            .collect::<Vec<_>>(),
        ["from_b"]
    );
    assert!(d.query(&w, "a::Token -> text").rows.is_empty());
    assert_eq!(d.query(&w, "from_a").rows[0].node, 2);
}

#[test]
fn custom_canonical_constructor_names_do_not_inherit_standard_type_proofs() {
    let mut vec = item(Kind::Struct, "Vec");
    vec.generics = Some("T".into());
    let w = world(vec![
        item(Kind::Struct, "Alpha"),
        vec,
        call("custom", None, false, &["value: Vec<Alpha>"], "String"),
        call(
            "standard",
            None,
            false,
            &["value: std::vec::Vec<Alpha>"],
            "String",
        ),
    ]);
    let d = Discovery::new(&w);
    assert_eq!(
        d.query(&w, "list of Alpha -> text")
            .rows
            .iter()
            .map(|r| r.label.as_str())
            .collect::<Vec<_>>(),
        ["standard"]
    );
    assert_eq!(d.query(&w, "custom").rows[0].node, 2);
}

#[test]
fn nominal_nodes_cannot_be_mistaken_for_heuristic_generic_keys() {
    let w = world(vec![
        item(Kind::Struct, "A"),
        item(Kind::Struct, "Beta"),
        call("only_a", None, false, &["value: A"], "String"),
    ]);
    let d = Discovery::new(&w);
    assert!(d.query(&w, "Beta -> text").rows.is_empty());
    assert!(d.query(&w, "A -> text").rows.is_empty());
    assert_eq!(d.query(&w, "only_a").rows[0].node, 2);
}

#[test]
fn generated_erased_arguments_remain_unproved_under_structural_composition() {
    for seed in 0..24 {
        let alpha = format!("Alpha{seed}");
        let beta = format!("Beta{seed}");
        let erased = |inner: &str| match seed % 6 {
            0 => format!("Wrapper<{inner}>"),
            1 => format!("&Wrapper<{inner}>"),
            2 => format!("Option<Wrapper<{inner}>>"),
            3 => format!("Vec<Wrapper<{inner}>>"),
            4 => format!("HashMap<String, Wrapper<{inner}>>"),
            _ => format!("(usize, Wrapper<{inner}>)"),
        };
        let w = world(vec![
            item(Kind::Struct, "Raw"),
            item(Kind::Struct, &alpha),
            item(Kind::Struct, &beta),
            item(Kind::Struct, "Wrapper"),
            call("make", None, false, &["raw: Raw"], &erased(&alpha)),
        ]);
        let d = Discovery::new(&w);
        let query = format!("Raw -> {}", erased(&beta));
        let result = d.query(&w, &query);
        assert!(
            result.rows.is_empty() && result.chains.is_empty(),
            "seed {seed}"
        );
        assert_eq!(d.query(&w, "make").rows[0].node, 4);
    }
}

#[test]
fn erased_constructor_inputs_cannot_supply_chain_riders() {
    use super::proof::TypeProof;
    let mut wrapper = item(Kind::Struct, "Wrapper");
    wrapper.generics = Some("T".into());
    wrapper.derives.push("Default".into());
    let mut side = item(Kind::Struct, "Side");
    side.non_exhaustive = false;
    let mut field = item(Kind::Field, "wrapped");
    field.parent = Some(3);
    field.ty = Some("Wrapper<Alpha>".into());
    let mut choice = item(Kind::Enum, "Choice");
    choice.generics = Some("T".into());
    let mut variant = item(Kind::Variant, "Wrapped");
    variant.parent = Some(5);
    variant.shape = Some("tuple".into());
    variant.ty = Some("Wrapper<Alpha>".into());
    let w = world(vec![
        item(Kind::Struct, "Raw"),
        item(Kind::Struct, "Alpha"),
        wrapper,
        side,
        field,
        choice,
        variant,
        call("finish", None, false, &["raw: Raw", "side: Side"], "String"),
    ]);
    let d = Discovery::new(&w);
    for (q, producer) in d.recipes.table().iter().enumerate() {
        if matches!(producer.how, How::Literal | How::Default | How::Variant) {
            assert_eq!(
                d.proof[q],
                TypeProof::Erased,
                "{}",
                w.node(producer.node).name
            );
        }
    }
    assert!(d.query(&w, "Raw -> text").chains.is_empty());
}

#[test]
fn relative_trait_paths_are_scoped_to_their_package_and_module() {
    let packages: Vec<_> = ["one", "two"]
        .into_iter()
        .map(|name| Package {
            name: name.into(),
            version: "0.1".into(),
            yours: true,
            external: false,
            deps: Vec::new(),
        })
        .collect();
    let modules: Vec<_> = [(0, ""), (1, ""), (0, "a"), (0, "b"), (0, "a::child")]
        .into_iter()
        .map(|(pkg, path)| Module {
            pkg,
            path: path.into(),
            file: "src/lib.rs".into(),
        })
        .collect();
    for (source_pkg, source_module, call_pkg, call_module, source, required, matches) in [
        (0, 0, 1, 1, "crate::Codec", "crate::Codec", false),
        (0, 2, 0, 3, "crate::Codec", "crate::Codec", true),
        (0, 2, 0, 3, "self::Codec", "self::Codec", false),
        (0, 2, 0, 2, "self::Codec", "self::Codec", true),
        (0, 2, 0, 4, "self::Codec", "super::Codec", true),
        (0, 0, 0, 0, "crate::Codec", "super::Codec", false),
    ] {
        let mut schema = item(Kind::Struct, "Schema");
        schema.pkg = source_pkg;
        schema.module = source_module;
        schema.impls_ext.push(source.into());
        let mut encode = call("encode", None, false, &["value: T"], "String");
        encode.pkg = call_pkg;
        encode.module = call_module;
        encode.generics = Some(format!("T: {required}").into());
        let w = World::new(
            packages.clone(),
            modules.clone(),
            vec![schema, encode],
            Vec::new(),
        )
        .unwrap();
        let d = Discovery::new(&w);
        assert_eq!(
            !d.query(&w, "Schema -> text").rows.is_empty(),
            matches,
            "source {source_pkg}/{source_module}/{source}, required {call_pkg}/{call_module}/{required}"
        );
    }
}

#[test]
fn generic_declarations_do_not_prove_conditional_instance_capabilities() {
    let mut wrapper = item(Kind::Struct, "Wrapper");
    wrapper.generics = Some("T".into());
    wrapper.derives.push("Serialize".into());
    wrapper.impls_ext.push("external::Codec".into());
    let mut serialize = call("serialize", None, false, &["value: U"], "String");
    serialize.generics = Some("U: Serialize".into());
    let mut encode = call("encode", None, false, &["value: U"], "String");
    encode.generics = Some("U: external::Codec".into());
    let w = world(vec![
        wrapper,
        item(Kind::Struct, "Alpha"),
        serialize,
        encode,
    ]);
    let d = Discovery::new(&w);
    for q in ["Wrapper<Alpha> -> text", "Wrapper -> text"] {
        let result = d.query(&w, q);
        assert!(result.rows.is_empty() && result.chains.is_empty(), "{q}");
    }
    assert_eq!(d.query(&w, "Wrapper").rows[0].node, 0);
}

#[test]
fn unrepresented_hasher_allocator_and_error_arguments_cannot_claim_type_identity() {
    let w = world(vec![
        item(Kind::Struct, "Alpha"),
        item(Kind::Struct, "Encoded"),
        item(Kind::Struct, "HasherA"),
        item(Kind::Struct, "HasherB"),
        call(
            "extra_hasher",
            None,
            false,
            &["value: std::collections::HashMap<String, u32, HasherA>"],
            "Encoded",
        ),
        call(
            "plain_map",
            None,
            false,
            &["value: std::collections::HashMap<String, u32>"],
            "Encoded",
        ),
        call(
            "extra_allocator",
            None,
            false,
            &["value: Vec<Alpha, HasherA>"],
            "Encoded",
        ),
        call(
            "fallible",
            None,
            false,
            &["value: Alpha"],
            "Result<String, ErrorA>",
        ),
        call(
            "nested_error",
            None,
            false,
            &["value: Alpha"],
            "Vec<Result<String, ErrorA>>",
        ),
        call(
            "requires_result",
            None,
            false,
            &["value: Result<Alpha, ErrorA>"],
            "Encoded",
        ),
    ]);
    let d = Discovery::new(&w);
    for q in [
        "HashMap<String, u32, HasherB> -> Encoded",
        "Vec<Alpha, HasherB> -> Encoded",
        "Result<Alpha, ErrorB> -> Encoded",
        "Alpha -> Result<String, ErrorB>",
    ] {
        let result = d.query(&w, q);
        assert!(result.rows.is_empty() && result.chains.is_empty(), "{q}");
    }
    assert_eq!(
        d.query(&w, "map text to number -> Encoded").rows[0].label,
        "plain_map"
    );
    assert_eq!(d.query(&w, "Alpha -> text").rows[0].label, "fallible");
    assert!(
        d.query(&w, "Alpha -> text").rows[0]
            .shape
            .ends_with("or fails")
    );
    assert!(d.query(&w, "Alpha -> list of text").rows.is_empty());
    assert!(d.query(&w, "Alpha -> Encoded").rows.is_empty());
}

#[test]
fn semantic_names_and_package_items_are_shared_prepared_action_indexes() {
    let w = road_world();
    let d = Discovery::new(&w);
    let prepared = d.prepared();
    assert!(Arc::ptr_eq(&d.semantic_names, &prepared.semantic_names));
    assert!(Arc::ptr_eq(&d.package_items, &prepared.package_items));
    let worker = Discovery::from_prepared(prepared);
    assert!(Arc::ptr_eq(&d.semantic_names, &worker.semantic_names));
    assert_eq!(d.semantic_names().candidates("Invocation"), [0]);
    assert_eq!(d.package_items(0), [0, 1]);
    assert!(d.package_items(999).is_empty());
}

#[test]
fn qualified_shape_names_compare_complete_segments_instead_of_substrings() {
    let packages: Vec<_> = ["ab", "b"]
        .into_iter()
        .map(|name| Package {
            name: name.into(),
            version: "0.1".into(),
            yours: true,
            external: false,
            deps: Vec::new(),
        })
        .collect();
    let modules: Vec<_> = [0, 1]
        .into_iter()
        .map(|pkg| Module {
            pkg,
            path: "".into(),
            file: "src/lib.rs".into(),
        })
        .collect();
    let a = item(Kind::Struct, "Thing");
    let mut b = item(Kind::Struct, "Thing");
    b.pkg = 1;
    b.module = 1;
    let from_a = call("from_ab", None, false, &["value: Thing"], "String");
    let mut from_b = call("from_b", None, false, &["value: Thing"], "String");
    from_b.pkg = 1;
    from_b.module = 1;
    let w = World::new(packages, modules, vec![a, b, from_a, from_b], Vec::new()).unwrap();
    let d = Discovery::new(&w);
    assert_eq!(
        d.query(&w, "b::Thing -> text")
            .rows
            .iter()
            .map(|r| r.label.as_str())
            .collect::<Vec<_>>(),
        ["from_b"]
    );

    let mut beta = item(Kind::Struct, "Thing");
    beta.module = 1;
    let mut b = item(Kind::Struct, "Thing");
    b.module = 2;
    let mut w = World::new(
        vec![Package {
            name: "demo".into(),
            version: "0.1".into(),
            yours: true,
            external: false,
            deps: Vec::new(),
        }],
        ["", "a::beta", "a::b"]
            .into_iter()
            .map(|path| Module {
                pkg: 0,
                path: path.into(),
                file: "src/lib.rs".into(),
            })
            .collect(),
        vec![
            beta,
            b,
            call(
                "from_beta",
                None,
                false,
                &["value: a::beta::Thing"],
                "String",
            ),
            call("from_b", None, false, &["value: a::b::Thing"], "String"),
        ],
        Vec::new(),
    )
    .unwrap();
    w.importance[1] = 100.0;
    let d = Discovery::new(&w);
    assert_eq!(
        d.query(&w, "a::b::Thing -> text")
            .rows
            .iter()
            .map(|r| r.label.as_str())
            .collect::<Vec<_>>(),
        ["from_b"]
    );
}

#[test]
fn trait_identity_includes_package_version_and_selected_dependency() {
    let mut packages: Vec<_> = ["1", "2"]
        .into_iter()
        .map(|version| Package {
            name: "foo".into(),
            version: version.into(),
            yours: true,
            external: false,
            deps: Vec::new(),
        })
        .collect();
    let mut modules: Vec<_> = [0, 1]
        .into_iter()
        .map(|pkg| Module {
            pkg,
            path: "".into(),
            file: "src/lib.rs".into(),
        })
        .collect();
    let mut schema = item(Kind::Struct, "Schema");
    schema.impls_ext.push("crate::Codec".into());
    let mut encode = call("encode", None, false, &["value: T"], "String");
    encode.pkg = 1;
    encode.module = 1;
    encode.generics = Some("T: crate::Codec".into());
    let w = World::new(
        packages.clone(),
        modules.clone(),
        vec![schema.clone(), encode.clone()],
        Vec::new(),
    )
    .unwrap();
    assert!(
        Discovery::new(&w)
            .query(&w, "Schema -> text")
            .rows
            .is_empty()
    );
    encode.pkg = 0;
    encode.module = 0;
    let w = World::new(
        packages.clone(),
        modules.clone(),
        vec![schema.clone(), encode.clone()],
        Vec::new(),
    )
    .unwrap();
    assert_eq!(
        Discovery::new(&w).query(&w, "Schema -> text").rows[0].label,
        "encode"
    );

    packages.extend(
        ["one", "two"]
            .into_iter()
            .enumerate()
            .map(|(a, name)| Package {
                name: name.into(),
                version: "1".into(),
                yours: true,
                external: false,
                deps: vec![u32::try_from(a).unwrap()],
            }),
    );
    modules.extend([2, 3].into_iter().map(|pkg| Module {
        pkg,
        path: "".into(),
        file: "src/lib.rs".into(),
    }));
    schema.pkg = 2;
    schema.module = 2;
    schema.impls_ext = vec!["foo::Codec".into()];
    encode.pkg = 3;
    encode.module = 3;
    encode.generics = Some("T: foo::Codec".into());
    let w = World::new(
        packages.clone(),
        modules.clone(),
        vec![schema.clone(), encode.clone()],
        Vec::new(),
    )
    .unwrap();
    assert!(
        Discovery::new(&w)
            .query(&w, "Schema -> text")
            .rows
            .is_empty()
    );
    packages[3].deps = vec![0];
    let w = World::new(packages, modules, vec![schema, encode], Vec::new()).unwrap();
    assert_eq!(
        Discovery::new(&w).query(&w, "Schema -> text").rows[0].label,
        "encode"
    );
}

#[test]
fn crate_type_paths_accept_normalized_full_and_short_package_names() {
    let w = World::new(
        vec![Package {
            name: "backend-demo".into(),
            version: "1".into(),
            yours: true,
            external: false,
            deps: Vec::new(),
        }],
        vec![Module {
            pkg: 0,
            path: "".into(),
            file: "src/lib.rs".into(),
        }],
        vec![
            item(Kind::Struct, "Thing"),
            call("consume", None, false, &["value: crate::Thing"], "String"),
        ],
        Vec::new(),
    )
    .unwrap();
    let d = Discovery::new(&w);
    for q in ["backend_demo::Thing -> text", "demo::Thing -> text"] {
        assert_eq!(d.query(&w, q).rows[0].label, "consume", "{q}");
    }
}

#[test]
fn raw_pointer_identity_is_not_owned_value_proof() {
    let w = world(vec![
        item(Kind::Struct, "Alpha"),
        item(Kind::Struct, "Raw"),
        call("inspect", None, false, &["value: *const Alpha"], "String"),
        call("pointer", None, false, &["raw: Raw"], "*mut Alpha"),
        call("finish", None, false, &["value: Alpha"], "String"),
    ]);
    let d = Discovery::new(&w);
    assert!(!d.query(&w, "Alpha -> text").lit.contains(&2));
    assert!(d.query(&w, "*const Alpha -> text").lit.is_empty());
    assert!(d.query(&w, "Raw -> Alpha").lit.is_empty());
    assert!(d.query(&w, "Raw -> text").chains.is_empty());
    assert_eq!(d.query(&w, "inspect").rows[0].node, 2);
    assert_eq!(d.query(&w, "pointer").rows[0].node, 3);
}

#[test]
fn unsafe_calls_are_names_but_not_safe_callable_proofs() {
    let mut unsafe_call = call("inspect_unsafe", None, false, &["value: Alpha"], "String");
    unsafe_call.quals.push("unsafe".into());
    let w = world(vec![item(Kind::Struct, "Alpha"), unsafe_call]);
    let d = Discovery::new(&w);
    assert!(d.query(&w, "Alpha -> text").lit.is_empty());
    assert_eq!(d.query(&w, "inspect_unsafe").rows[0].node, 1);
}

#[test]
fn shape_complexity_is_rejected_before_recursive_parsing() {
    let w = road_world();
    let d = Discovery::new(&w);
    for query in [
        format!("{}Invocation -> text", "maybe ".repeat(10_000)),
        format!(
            "{}Invocation{} -> text",
            "Option<".repeat(33),
            ">".repeat(33)
        ),
        format!("{}Invocation -> text", "&".repeat(33)),
        format!("{}Invocation -> text", "maybe ".repeat(128)),
        format!("{}Invocation -> text", ".".repeat(4000)),
        format!("{}Invocation -> text", ")".repeat(4000)),
    ] {
        let result = d.query(&w, &query);
        assert!(
            result.issue.is_some(),
            "must reject before recursive parsing"
        );
        assert!(result.rows.is_empty() && result.chains.is_empty());
    }
    for query in [
        format!("{}Invocation -> text", "maybe ".repeat(124)),
        format!(
            "{}Invocation{} -> text",
            "Option<".repeat(32),
            ">".repeat(32)
        ),
        format!("{}Invocation -> text", "&".repeat(32)),
    ] {
        assert!(
            d.query(&w, &query).issue.is_none(),
            "near-budget shape remains searchable"
        );
    }
    assert!(
        d.query(&w, &"maybe ".repeat(10_000)).issue.is_none(),
        "names have no grammar cap"
    );
    assert!(d.cache_len() <= 8);
}

#[test]
fn prepared_focus_facts_preserve_member_callers_and_tours() {
    let w = crate::graph::model::tests::tiny();
    let d = Discovery::new(&w);
    for (at, _) in w.nodes.iter().enumerate() {
        let i = at as NodeId;
        let facts = d.focus_facts(i).expect("facts for every node");
        let used = w.used_in(i);
        assert_eq!(facts.used, used.len());
        assert_eq!(
            facts.yours,
            used.iter().filter(|&&caller| w.yours(caller)).count()
        );
        assert_eq!(facts.caps, crate::semantics::page::caps_of(&w, i));
        let counts = [Kind::Variant, Kind::Field, Kind::Method].map(|kind| {
            w.kids(i)
                .iter()
                .filter(|&&child| w.node(child).kind == kind)
                .count()
        });
        assert_eq!(facts.counts, counts);
    }
    for package in 0..w.packages.len() {
        assert_eq!(
            d.package_tour(package as u32),
            Some(&crate::semantics::tour::of(
                &w,
                d.semantic_names(),
                package as u32
            ))
        );
    }
    assert!(d.focus_facts(NodeId::MAX).is_none());
    assert!(d.package_tour(u32::MAX).is_none());
    let attached = Discovery::from_prepared(d.prepared());
    assert!(Arc::ptr_eq(&d.focus_facts, &attached.focus_facts));
    assert!(Arc::ptr_eq(&d.package_tours, &attached.package_tours));
}

#[test]
fn explicit_array_queries_do_not_erase_length_identity() {
    let w = world(vec![
        item(Kind::Struct, "Alpha"),
        item(Kind::Struct, "Beta"),
        call("collect", None, false, &["value: Alpha"], "Vec<Beta>"),
        call("consume", None, false, &["values: Vec<Alpha>"], "String"),
    ]);
    let d = Discovery::new(&w);
    assert_eq!(d.query(&w, "Alpha -> list of Beta").lit, [2]);
    assert_eq!(d.query(&w, "list of Alpha -> text").lit, [3]);
    for query in [
        "Alpha -> [Beta; 3]",
        "[Alpha; 3] -> text",
        "Alpha -> Option<[Beta; 3]>",
    ] {
        let result = d.query(&w, query);
        assert!(result.lit.is_empty() && result.chains.is_empty(), "{query}");
    }
}
