mod helpers;

use helpers::*;

// TODO: make this nicer to look at

#[tokio::test]
async fn build_ir_package_1() {
    let registry = Registry::new(ExampleResolver::new());

    registry.build_package_ir(PackageMeta { id: pkg(1) }, symbol("package_1"), |b| {
        build_package(b, build_package_1)
    });

    let id_iter = registry.iter().map(|idx| registry.resolve_id(idx));

    let entry_iter = async_to_sync_iter(registry.iter().map(|idx| registry.resolve_entry(idx)))
        .await
        .map(|r| r.expect("failed to resolve idx"));

    let (expected_ids, expected_entries) =
        expected_package_1("package_1", pkg(1), |id| registry.resolve_idx(id));

    itertools::assert_equal(id_iter, &expected_ids);
    itertools::assert_equal(entry_iter, &expected_entries);
}

#[tokio::test]
async fn build_ir_package_2() {
    let registry = Registry::new(ExampleResolver::new());

    registry.build_package_ir(PackageMeta { id: pkg(2) }, symbol("package_2"), |b| {
        build_package(b, build_package_2)
    });

    let id_iter = registry.iter().map(|idx| registry.resolve_id(idx));

    let entry_iter = async_to_sync_iter(registry.iter().map(|idx| registry.resolve_entry(idx)))
        .await
        .map(|r| r.expect("failed to resolve idx"));

    let (expected_ids, expected_entries) =
        expected_package_2("package_2", pkg(2), |id| registry.resolve_idx(id));

    itertools::assert_equal(id_iter, &expected_ids);
    itertools::assert_equal(entry_iter, &expected_entries);
}

#[tokio::test]
async fn build_both_ir_packages() {
    let registry = Registry::new(ExampleResolver::new());

    registry.build_package_ir(PackageMeta { id: pkg(1) }, symbol("package_1"), |b| {
        build_package(b, build_package_1)
    });

    registry.build_package_ir(PackageMeta { id: pkg(2) }, symbol("package_2"), |b| {
        build_package(b, build_package_2)
    });

    let id_iter = registry.iter().map(|idx| registry.resolve_id(idx));

    let entry_iter = async_to_sync_iter(registry.iter().map(|idx| registry.resolve_entry(idx)))
        .await
        .map(|r| r.expect("failed to resolve idx"));

    let (ids_1, entries_1) = expected_package_1("package_1", pkg(1), |id| registry.resolve_idx(id));
    let (ids_2, entries_2) = expected_package_2("package_2", pkg(2), |id| registry.resolve_idx(id));

    itertools::assert_equal(id_iter, std::iter::chain(&ids_1, &ids_2));
    itertools::assert_equal(entry_iter, std::iter::chain(&entries_1, &entries_2));
}
