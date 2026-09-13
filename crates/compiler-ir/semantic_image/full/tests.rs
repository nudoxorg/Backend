//! Unrun hostile falsifiers for completed private full-image common planes.

use alloc::vec::Vec;

use crate::{
    BorrowedTree, Confidence, CorePayloadHash, DeclarationFamilyId, DocInput, EntityAuthorityFacts,
    EntityVersion, ExternalEntityRef, ExternalFragmentId, ExternalTarget, FactAvailability, Ir,
    IrBuilder, ItemKind, LanguageExtensionInput, LinkKind, OccurrenceAuthorityFacts,
    ParentageAuthority, RustFacts, RustOwnership, SourceSpan, TreeEntityId, TreeItemInput,
    TreeLinkInput, TreeLinkTarget, VariantFingerprint, Visibility,
};
use compiler_vocabulary::{LanguageProfile, RustEdition};

use super::*;

fn version(value: u8) -> EntityVersion {
    EntityVersion {
        family: DeclarationFamilyId::from_raw([value; 16]),
        variant: VariantFingerprint::from_raw([value.wrapping_add(1); 16]),
        core_payload: CorePayloadHash::from_raw([value.wrapping_add(2); 16]),
    }
}

fn authority() -> EntityAuthorityFacts {
    EntityAuthorityFacts {
        parentage: ParentageAuthority::Root,
        source: FactAvailability::Captured,
        source_file: FactAvailability::Captured,
        members: FactAvailability::Captured,
        documentation: FactAvailability::Captured,
        attributes: FactAvailability::Captured,
        visibility: FactAvailability::Captured,
        ..EntityAuthorityFacts::default()
    }
}

/// Reverses every builder-local coordinate while retaining exact docs,
/// external endpoint, source evidence, member order, and occurrence truth.
fn full_image(reverse: bool) -> Result<Ir, crate::BuildError> {
    let mut builder = IrBuilder::new();
    if reverse {
        builder.intern_atom(b"noise-z")?;
        builder.intern_atom(b"noise-a")?;
    } else {
        builder.intern_atom(b"noise-a")?;
        builder.intern_atom(b"noise-z")?;
    }
    let file = builder.intern_atom(b"src/graph.rs")?;
    let display = builder.intern_atom(b"Remote")?;
    let external = builder.intern_external(ExternalTarget::FragmentEntity {
        target: ExternalEntityRef::bind(
            ExternalFragmentId::from_canonical_bytes(b"full-plan-remote"),
            9,
        ),
        display,
    })?;

    let local_docs = [
        DocInput::Text("alpha"),
        DocInput::Code("T"),
        DocInput::Link {
            label: "local",
            target: TreeLinkTarget::Local(TreeEntityId::new(if reverse { 0 } else { 1 })),
        },
        DocInput::Link {
            label: "remote",
            target: TreeLinkTarget::External(external),
        },
        DocInput::SoftBreak,
        DocInput::HardBreak,
    ];
    let other_docs = [DocInput::Text("beta")];
    let attributes: [&[u8]; 2] = [b"first", b"second"];
    let forward_members = [TreeEntityId::new(1)];
    let reverse_members = [TreeEntityId::new(0)];
    let alpha = TreeItemInput {
        name: b"alpha",
        kind: ItemKind::Function,
        visibility: Visibility::Public,
        authority: authority(),
        parent: None,
        semantic_type: None,
        members: &forward_members,
        docs: &local_docs,
        attributes: &attributes,
        source: SourceSpan::new(file, 2, 9),
        extension: None,
    };
    let beta = TreeItemInput {
        name: b"beta",
        kind: ItemKind::Record,
        visibility: Visibility::Private,
        authority: authority(),
        parent: None,
        semantic_type: None,
        members: &[],
        docs: &other_docs,
        attributes: &[],
        source: SourceSpan::new(file, 11, 17),
        extension: None,
    };
    let forward_links = [
        TreeLinkInput {
            from: TreeEntityId::new(0),
            target: TreeLinkTarget::Local(TreeEntityId::new(1)),
            kind: LinkKind::Calls,
            confidence: Confidence::Compiler,
            authority: OccurrenceAuthorityFacts {
                source: FactAvailability::Captured,
            },
            source: SourceSpan::new(file, 3, 8),
        },
        TreeLinkInput {
            from: TreeEntityId::new(1),
            target: TreeLinkTarget::External(external),
            kind: LinkKind::Documents,
            confidence: Confidence::Syntactic,
            authority: OccurrenceAuthorityFacts {
                source: FactAvailability::Captured,
            },
            source: SourceSpan::new(file, 12, 16),
        },
    ];
    let reverse_links = [
        TreeLinkInput {
            from: TreeEntityId::new(1),
            target: TreeLinkTarget::Local(TreeEntityId::new(0)),
            ..forward_links[0]
        },
        TreeLinkInput {
            from: TreeEntityId::new(0),
            target: TreeLinkTarget::External(external),
            ..forward_links[1]
        },
    ];
    if reverse {
        let alpha = TreeItemInput {
            members: &reverse_members,
            ..alpha
        };
        let versions = [version(2), version(1)];
        builder.add_borrowed_tree(BorrowedTree {
            versions: &versions,
            items: &[beta, alpha],
            links: &reverse_links,
        })?;
    } else {
        let versions = [version(1), version(2)];
        builder.add_borrowed_tree(BorrowedTree {
            versions: &versions,
            items: &[alpha, beta],
            links: &forward_links,
        })?;
    }
    builder.finish()
}

fn pool_shape(
    pool: &super::model::TerminalPoolPlan,
    domain: super::model::TerminalPoolDomain,
) -> Vec<Vec<u8>> {
    pool.order
        .iter()
        .copied()
        .map(|raw| {
            pool.key(raw, domain)
                .expect("planned terminal key")
                .to_vec()
        })
        .collect()
}

#[test]
fn terminal_pools_entity_rows_and_graph_evidence_are_coordinate_free()
-> Result<(), crate::BuildError> {
    let forward = full_image(false)?;
    let reverse = full_image(true)?;
    let forward_plan = FullSemanticPlan::build(&forward).expect("forward full plan");
    let reverse_plan = FullSemanticPlan::build(&reverse).expect("reverse full plan");

    assert_eq!(
        pool_shape(
            &forward_plan.terminal.members,
            super::model::TerminalPoolDomain::EntityList
        ),
        pool_shape(
            &reverse_plan.terminal.members,
            super::model::TerminalPoolDomain::EntityList
        ),
    );
    assert_eq!(
        pool_shape(
            &forward_plan.terminal.docs,
            super::model::TerminalPoolDomain::Documentation
        ),
        pool_shape(
            &reverse_plan.terminal.docs,
            super::model::TerminalPoolDomain::Documentation
        ),
    );
    let forward_rows = forward_plan
        .entities
        .rows
        .iter()
        .map(|row| (row.semantic_type, row.members, row.docs, row.attributes))
        .collect::<Vec<_>>();
    let reverse_rows = reverse_plan
        .entities
        .rows
        .iter()
        .map(|row| (row.semantic_type, row.members, row.docs, row.attributes))
        .collect::<Vec<_>>();
    assert_eq!(forward_rows, reverse_rows);

    let graph_shape = |ir: &Ir, plan: &FullSemanticPlan<'_>| {
        plan.graph
            .relations
            .iter()
            .map(|id| ir.link(*id).expect("planned relation"))
            .map(|row| {
                (
                    row.kind,
                    row.confidence,
                    row.source.map(|span| {
                        (
                            ir.atom(span.file())
                                .expect("validated graph source atom")
                                .to_vec(),
                            span.start(),
                            span.end(),
                        )
                    }),
                )
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        graph_shape(&forward, &forward_plan),
        graph_shape(&reverse, &reverse_plan)
    );
    assert_eq!(forward_plan.graph.occurrences.len(), 2);
    assert_eq!(reverse_plan.graph.occurrences.len(), 2);
    Ok(())
}

#[test]
fn docs_keep_local_and_external_targets_and_occurrence_authority_is_not_relation_source()
-> Result<(), crate::BuildError> {
    let ir = full_image(false)?;
    let plan = FullSemanticPlan::build(&ir).expect("full plan");
    let docs = pool_shape(
        &plan.terminal.docs,
        super::model::TerminalPoolDomain::Documentation,
    );
    // The alpha list has two explicit link fragments (one local, one exact
    // external endpoint) in addition to text/code/breaks; neither is folded
    // into a generic text key.
    assert!(docs.iter().any(|key| key.len() > 24));
    let occurrences = plan
        .graph
        .occurrences
        .iter()
        .copied()
        .map(|id| {
            let occurrence = ir.link_occurrence(id).expect("planned occurrence");
            let authority = ir.occurrence_authority_columns().source[id.index()];
            (occurrence.confidence, occurrence.source, authority)
        })
        .collect::<Vec<_>>();
    assert!(occurrences.iter().all(|(_, source, authority)| {
        source.is_some() && *authority == FactAvailability::Captured
    }));
    Ok(())
}

#[test]
fn shared_sparse_extension_fact_binds_each_canonical_entity_without_duplication()
-> Result<(), crate::BuildError> {
    let mut builder = IrBuilder::new();
    builder.set_language_profile(LanguageProfile::Rust(RustEdition::Rust2024))?;
    let facts = RustFacts {
        ownership: RustOwnership::Value,
        lifetimes: builder.intern_attributes(&[])?,
        where_clauses: builder.intern_type_parameters(&[])?,
        macros: builder.intern_attributes(&[])?,
    };
    let authority = EntityAuthorityFacts {
        parentage: ParentageAuthority::Root,
        members: FactAvailability::Captured,
        documentation: FactAvailability::Captured,
        attributes: FactAvailability::Captured,
        visibility: FactAvailability::Captured,
        language_extension: FactAvailability::Captured,
        ..EntityAuthorityFacts::default()
    };
    let first = TreeItemInput {
        name: b"first",
        kind: ItemKind::Record,
        visibility: Visibility::Private,
        authority,
        parent: None,
        semantic_type: None,
        members: &[],
        docs: &[],
        attributes: &[],
        source: None,
        extension: Some(LanguageExtensionInput::Rust(&facts)),
    };
    let second = TreeItemInput {
        name: b"second",
        ..first
    };
    let versions = [version(1), version(2)];
    builder.add_borrowed_tree(BorrowedTree {
        versions: &versions,
        items: &[second, first],
        links: &[],
    })?;
    let ir = builder.finish()?;
    let plan = FullSemanticPlan::build(&ir).expect("sparse Rust plane is planned");
    assert_eq!(plan.extensions.rust.order.len(), 1);
    assert_eq!(plan.extensions.rust.bindings.len(), 2);
    assert_ne!(
        plan.extensions.rust.bindings[0].entity,
        plan.extensions.rust.bindings[1].entity
    );
    assert_eq!(
        plan.extensions.rust.bindings[0].fact,
        plan.extensions.rust.bindings[1].fact
    );
    Ok(())
}
