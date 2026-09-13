//! Retained recipe graph admission tests.

use super::*;

#[test]
fn retained_manifest_graph_rejects_cross_manifest_cycle_transactionally() {
    let first_recipe = recipe(30);
    let second_recipe = recipe(31);
    let first = DependencyManifest::new(vec![DependencyFact::recipe(
        RecipeDependencyFact::new(first_recipe, second_recipe).expect("edge"),
    )])
    .expect("manifest");
    let second = DependencyManifest::new(vec![DependencyFact::recipe(
        RecipeDependencyFact::new(second_recipe, first_recipe).expect("edge"),
    )])
    .expect("manifest");

    let mut graph = RetainedDependencyGraph::default();
    let first_version = graph.register_manifest(&first).expect("first manifest");
    assert!(graph.depends_on(first_recipe, second_recipe));
    assert_eq!(
        graph.register_manifest(&second),
        Err(SemanticError::DependencyCycle)
    );
    assert_eq!(graph.manifest_count(), 1);
    assert_eq!(graph.edge_count(), 1);
    assert_eq!(graph.manifest_reference_count(first_version), 1);
    assert!(graph.release_manifest(first_version));
    assert_eq!(graph.manifest_count(), 0);
    assert_eq!(graph.edge_count(), 0);
}

#[test]
fn retained_reader_manifest_cycle_admission_rolls_back_global_and_local_state() {
    let first_recipe = recipe(32);
    let second_recipe = recipe(33);
    let first = DependencyManifest::new(vec![DependencyFact::recipe(
        RecipeDependencyFact::new(first_recipe, second_recipe).expect("edge"),
    )])
    .expect("manifest");
    let second = DependencyManifest::new(vec![DependencyFact::recipe(
        RecipeDependencyFact::new(second_recipe, first_recipe).expect("edge"),
    )])
    .expect("manifest");

    let first_reader = work(32);
    let second_reader = work(33);
    let mut readers = RetainedReaders::default();
    let first_version = readers
        .register_manifest(first_reader, &first)
        .expect("first manifest");
    assert_eq!(readers.reader_count(), 1);
    assert_eq!(readers.dependency_graph().manifest_count(), 1);
    assert_eq!(readers.dependency_graph().edge_count(), 1);

    assert_eq!(
        readers.register_manifest(second_reader, &second),
        Err(SemanticError::DependencyCycle)
    );
    assert_eq!(readers.reader_count(), 1);
    assert_eq!(readers.registration_count(), 0);
    assert_eq!(readers.dependency_graph().manifest_count(), 1);
    assert_eq!(readers.dependency_graph().edge_count(), 1);
    assert_eq!(
        readers
            .dependency_graph()
            .manifest_reference_count(first_version),
        1
    );

    assert_eq!(readers.unregister_reader(first_reader), 0);
    assert_eq!(readers.dependency_graph().manifest_count(), 0);
    assert_eq!(readers.dependency_graph().edge_count(), 0);
}

#[test]
fn sparse_graph_admission_scales_with_reachable_region() {
    let mut graph = RetainedDependencyGraph::default();
    for seed in 0..256_u32 {
        let recipe = wide_recipe(seed * 2);
        let dependency = wide_recipe(seed * 2 + 1);
        let manifest = DependencyManifest::new(vec![DependencyFact::recipe(
            RecipeDependencyFact::new(recipe, dependency).expect("edge"),
        )])
        .expect("manifest");
        graph.register_manifest(&manifest).expect("sparse edge");
    }
    let entry = wide_recipe(10_000);
    let reachable = wide_recipe(0);
    let manifest = DependencyManifest::new(vec![DependencyFact::recipe(
        RecipeDependencyFact::new(entry, reachable).expect("edge"),
    )])
    .expect("manifest");
    let version = graph
        .register_manifest_budgeted(
            &manifest,
            GraphAdmissionBudget {
                max_edges: 1,
                max_nodes: 4,
                max_edge_probes: 4,
            },
        )
        .expect("sparse admission");
    let counters = graph.admission_counters();
    assert!(counters.nodes_visited <= 3);
    assert!(counters.edge_probes <= 2);
    assert_eq!(counters.edges_checked, 1);

    let before = graph.edge_count();
    assert_eq!(
        graph.register_manifest_budgeted(
            &DependencyManifest::new(vec![DependencyFact::recipe(
                RecipeDependencyFact::new(wide_recipe(10_001), wide_recipe(10_002)).expect("edge"),
            )])
            .expect("manifest"),
            GraphAdmissionBudget {
                max_edges: 1,
                max_nodes: 0,
                max_edge_probes: 0,
            },
        ),
        Err(SemanticError::ReuseWorkLimit)
    );
    assert_eq!(graph.edge_count(), before);
    assert!(graph.release_manifest(version));
}
