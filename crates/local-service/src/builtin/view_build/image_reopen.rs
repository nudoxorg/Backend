//! One validation of a semantic image, which view publication used to repeat.

use super::semantic::{SemanticRowSink, append_image_rows};
use backend_semantic::ir::{
    BorrowedTree, CorePayloadHash, DeclarationFamilyId, DeclarationIdentity, EntityAuthorityFacts,
    EntityVersion, FactAvailability, IrBuilder, ItemKind, ParentageAuthority, SemanticImageView,
    SourceIdentity, TreeEntityId, TreeItemInput, VariantFingerprint, Visibility,
    encode_full_semantic_image, full_semantic_image_len,
};
use backend_semantic::vocabulary::{
    CompileRecipeFact, LanguageProfile, NativeTool, PackageUrl, RustEdition, Stage,
};
use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};

const FIXTURE_PATH: &str = "src/worker.rs";

/// Times validating one semantic image once, validating it three times, and
/// projecting its rows after one validation.
///
/// The image is built before the timer. `once` is one [`SemanticImageView::reopen`].
/// `triple` is three reopens, the validation view publication used to pay per
/// image. `project` reopens once and appends that image's rows.
#[allow(clippy::expect_used, clippy::print_stdout)]
pub(super) fn measure_semantic_image_reopen() {
    const SAMPLES: usize = 32;
    const WARMUPS: usize = 4;
    let bytes = fixture_semantic_image(FIXTURE_PATH).expect("fixture image");
    let view = SemanticImageView::reopen(&bytes).expect("reopen");
    let projected = project_rows(&bytes);
    let named = projected.iter().any(|row| row.label.contains("Worker"));
    named
        .then_some(())
        .expect("fixture image did not project Worker");
    drop(view);

    let once = sample(WARMUPS, SAMPLES, || {
        SemanticImageView::reopen(&bytes)
            .expect("reopen")
            .as_ref()
            .len()
    });
    let triple = sample(WARMUPS, SAMPLES, || {
        let first = SemanticImageView::reopen(&bytes).expect("reopen");
        let second = SemanticImageView::reopen(&bytes).expect("reopen");
        let third = SemanticImageView::reopen(&bytes).expect("reopen");
        first.as_ref().len() + second.as_ref().len() + third.as_ref().len()
    });
    let project = sample(WARMUPS, SAMPLES, || project_rows(&bytes).len());
    let (once_median, once_p95) = percentiles(&once);
    let (triple_median, triple_p95) = percentiles(&triple);
    let (project_median, project_p95) = percentiles(&project);
    println!(
        "semantic_image_reopen bytes={} once_median_ns={once_median} once_p95_ns={once_p95} triple_median_ns={triple_median} triple_p95_ns={triple_p95} project_median_ns={project_median} project_p95_ns={project_p95}",
        bytes.len()
    );
}

#[allow(clippy::expect_used)]
fn project_rows(bytes: &[u8]) -> Vec<backend_engine::Row> {
    let view = SemanticImageView::reopen(bytes).expect("reopen");
    let project = super::super::IndexedProject {
        package: backend_engine::package_key("fixture"),
        label: "fixture".to_owned(),
        files: std::sync::Arc::<[[u8; 32]]>::from([]),
    };
    let head = super::super::genesis().expect("genesis");
    let (initial, _) =
        super::super::initial_view_for_workspace(&head.snapshot()).expect("initial view");
    let mut symbols = std::collections::BTreeSet::new();
    let mut rows = Vec::new();
    let mut remaining = usize::MAX;
    let mut sink = SemanticRowSink {
        initial: &initial,
        symbols: &mut symbols,
        rows: &mut rows,
        capacity: 64,
        remaining_bytes: &mut remaining,
        stale: false,
        path: FIXTURE_PATH,
        site_declarations: &[],
    };
    append_image_rows(
        &view,
        &project,
        LanguageProfile::Rust(RustEdition::Rust2024),
        &mut sink,
    )
    .expect("project");
    rows
}

/// Builds one small Rust semantic image rooted at `path`.
pub(super) fn fixture_semantic_image(path: &str) -> Result<Vec<u8>, String> {
    let source = SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"fixture-source"),
        byte_len: 14,
    };
    let recipe = CompileRecipeFact::derive(
        LanguageProfile::Rust(RustEdition::Rust2024),
        Stage::LowerIr,
        NativeTool::Rustc,
        source.identity,
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"fixture-toolchain"),
    );
    let coordinate = PackageUrl::parse("pkg:cargo/fixture@1.0.0".to_owned())
        .map_err(|error| format!("fixture coordinate: {error:?}"))?;
    let mut builder = IrBuilder::new();
    builder
        .set_image_provenance_for_package(source, recipe, &coordinate, path)
        .map_err(|error| error.to_string())?;
    let struct_id = TreeEntityId::new(0);
    let enum_id = TreeEntityId::new(2);
    let items = [
        TreeItemInput {
            name: b"Worker",
            kind: ItemKind::Record,
            visibility: Visibility::Public,
            authority: fixture_authority(ParentageAuthority::Root),
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        },
        TreeItemInput {
            name: b"name",
            kind: ItemKind::Field,
            visibility: Visibility::Public,
            authority: fixture_authority(fixture_identity(1)),
            parent: Some(struct_id),
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        },
        TreeItemInput {
            name: b"Event",
            kind: ItemKind::Enum,
            visibility: Visibility::Public,
            authority: fixture_authority(ParentageAuthority::Root),
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        },
        TreeItemInput {
            name: b"Started",
            kind: ItemKind::Variant,
            visibility: Visibility::Public,
            authority: fixture_authority(fixture_identity(3)),
            parent: Some(enum_id),
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        },
    ];
    builder
        .add_borrowed_tree(BorrowedTree {
            versions: &[
                fixture_version(1),
                fixture_version(2),
                fixture_version(3),
                fixture_version(4),
            ],
            items: &items,
            links: &[],
        })
        .map_err(|error| error.to_string())?;
    let ir = builder.finish().map_err(|error| error.to_string())?;
    let mut bytes = vec![0; full_semantic_image_len(&ir).map_err(|error| error.to_string())?];
    encode_full_semantic_image(&ir, &mut bytes).map_err(|error| error.to_string())?;
    Ok(bytes)
}

fn fixture_version(identity: u8) -> EntityVersion {
    EntityVersion {
        family: DeclarationFamilyId::from_raw([identity; 16]),
        variant: VariantFingerprint::from_raw([identity; 16]),
        core_payload: CorePayloadHash::from_raw([identity; 16]),
    }
}

fn fixture_authority(parentage: ParentageAuthority) -> EntityAuthorityFacts {
    EntityAuthorityFacts {
        parentage,
        visibility: FactAvailability::Captured,
        ..EntityAuthorityFacts::default()
    }
}

fn fixture_identity(identity: u8) -> ParentageAuthority {
    ParentageAuthority::Bound(DeclarationIdentity {
        family: DeclarationFamilyId::from_raw([identity; 16]),
        variant: VariantFingerprint::from_raw([identity; 16]),
    })
}

fn sample<T>(warmups: usize, samples: usize, mut body: impl FnMut() -> T) -> Vec<u128> {
    for _ in 0..warmups {
        let _ = body();
    }
    let mut samples_ns = Vec::with_capacity(samples);
    for _ in 0..samples {
        let started = std::time::Instant::now();
        let _ = body();
        samples_ns.push(started.elapsed().as_nanos());
    }
    samples_ns
}

#[allow(clippy::indexing_slicing)]
fn percentiles(samples: &[u128]) -> (u128, u128) {
    let mut ordered = samples.to_vec();
    ordered.sort_unstable();
    let median = ordered[ordered.len() / 2];
    let p95 = ordered[ordered.len() * 95 / 100];
    (median, p95)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    #[test]
    fn the_reopen_fixture_projects_the_struct_and_its_field() {
        let bytes = super::fixture_semantic_image(super::FIXTURE_PATH).expect("image");
        let rows = super::project_rows(&bytes);
        assert!(rows.iter().any(|row| row.label.contains("Worker")));
        assert!(rows.iter().any(|row| row.label.contains("name")));
        assert!(rows.iter().any(|row| row.label.contains("Event")));
        assert!(rows.iter().any(|row| row.label.contains("Started")));
    }
}
