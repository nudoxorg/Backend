//! Exercises the unified semantic IR through its public compiler, render, graph, and VCS views.

use allocation_counter::{AllocationInfo, measure};
use compiler_ir::{
    AtomId, BorrowedTree, ComputedState, ComputedType, ConcreteState, ConcreteType, Confidence,
    CorePayloadHash, DeclarationFamilyId, Diff, DocInput, EntityAuthorityFacts, EntityChange,
    EntityVersion, FactAvailability, FrontendTree, GuardedType, Ir, IrBuilder, ItemKind,
    LanguageExtensionInput, LinkChangeKind, LinkKind, MappedModifier, OccurrenceAuthorityFacts,
    PackageLineage, ParentageAuthority, SemanticCoreReader, SemanticDiff, SemanticEntityChange,
    SemanticImageView, SemanticLinkChangeKind, SemanticSnapshot, Snapshot, SourceIdentity,
    SourceSpan, TreeEntityId, TreeItemInput, TreeLinkInput, TreeLinkTarget, TypeExpr, TypeHeader,
    TypePairPayload, TypeParameter, TypeParameterBound, TypeParameterInference, TypeParameterKind,
    TypeParameterRequirements, TypeQuadPayload, TypeScriptFacts, TypeTriplePayload, UnknownState,
    UnknownType, UnrepresentedAuthorityOwner, Variance, VariantFingerprint, Visibility,
    encode_full_semantic_image, full_semantic_image_len,
};
use compiler_vocabulary::{
    CompileRecipeFact, LanguageProfile, NativeTool, PackageUrl, RustEdition, Stage,
};
use core::mem::{size_of, size_of_val};
use core::{fmt, hint::black_box};
use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};

fn version(identity: u8, payload: u8) -> EntityVersion {
    EntityVersion {
        family: DeclarationFamilyId::from_raw([identity; 16]),
        variant: VariantFingerprint::from_raw([identity; 16]),
        core_payload: CorePayloadHash::from_raw([payload; 16]),
    }
}

fn unavailable_authority() -> EntityAuthorityFacts {
    EntityAuthorityFacts {
        // Test rows choose a concrete visibility value, so that value is an
        // explicit observed fact even when every optional plane is absent.
        visibility: FactAvailability::Captured,
        ..EntityAuthorityFacts::default()
    }
}

struct FixedText<const N: usize> {
    bytes: [u8; N],
    len: usize,
}

impl<const N: usize> FixedText<N> {
    const fn new() -> Self {
        Self {
            bytes: [0; N],
            len: 0,
        }
    }

    fn clear(&mut self) {
        self.len = 0;
    }
}

impl<const N: usize> fmt::Write for FixedText<N> {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let end = self.len.checked_add(value.len()).ok_or(fmt::Error)?;
        let output = self.bytes.get_mut(self.len..end).ok_or(fmt::Error)?;
        output.copy_from_slice(value.as_bytes());
        self.len = end;
        Ok(())
    }
}

struct NativeTree<'tree> {
    versions: &'tree [EntityVersion],
    names: &'tree [&'tree [u8]],
}

impl FrontendTree for NativeTree<'_> {
    fn versions(&self) -> &[EntityVersion] {
        self.versions
    }

    fn items(&self) -> impl ExactSizeIterator<Item = TreeItemInput<'_>> {
        self.names.iter().copied().map(|name| TreeItemInput {
            name,
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            authority: unavailable_authority(),
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        })
    }

    fn links(&self) -> impl ExactSizeIterator<Item = TreeLinkInput> {
        core::iter::empty()
    }
}

#[test]
fn native_frontend_stream_needs_no_compatibility_row_array() -> Result<(), compiler_ir::BuildError>
{
    let versions = [version(1, 1), version(2, 1)];
    let names = [b"one".as_slice(), b"two".as_slice()];
    let mut builder = IrBuilder::new();
    builder.add_frontend_tree(&NativeTree {
        versions: &versions,
        names: &names,
    })?;
    let ir = builder.finish()?;
    assert_eq!(ir.entity_count(), 2);
    assert_eq!(
        ir.item(compiler_ir::EntityId::new(1))
            .map(|item| item.name()),
        Some(b"two".as_slice())
    );
    Ok(())
}

#[test]
fn authority_captured_empty_and_unavailable_remain_distinct() -> Result<(), compiler_ir::BuildError>
{
    let versions = [version(1, 1), version(2, 1)];
    let items = [
        TreeItemInput {
            name: b"captured-empty",
            kind: ItemKind::Module,
            visibility: Visibility::Private,
            authority: EntityAuthorityFacts {
                members: FactAvailability::Captured,
                documentation: FactAvailability::Captured,
                attributes: FactAvailability::Captured,
                language_extension: FactAvailability::Captured,
                visibility: FactAvailability::Captured,
                ..unavailable_authority()
            },
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        },
        TreeItemInput {
            name: b"unavailable-empty",
            kind: ItemKind::Module,
            visibility: Visibility::Private,
            authority: unavailable_authority(),
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        },
    ];
    let mut builder = IrBuilder::new();
    builder.add_borrowed_tree(BorrowedTree {
        versions: &versions,
        items: &items,
        links: &[],
    })?;
    let ir = builder.finish()?;
    let facts = ir.entity_authority_columns();
    assert_eq!(
        ir.item(compiler_ir::EntityId::new(0))
            .expect("first")
            .members(),
        &[]
    );
    assert_eq!(
        ir.item(compiler_ir::EntityId::new(1))
            .expect("second")
            .members(),
        &[]
    );
    assert_eq!(
        facts.members,
        &[FactAvailability::Captured, FactAvailability::Unavailable]
    );
    assert_eq!(
        facts.documentation,
        &[FactAvailability::Captured, FactAvailability::Unavailable]
    );
    assert_eq!(
        facts.attributes,
        &[FactAvailability::Captured, FactAvailability::Unavailable]
    );
    assert_eq!(
        facts.language_extension,
        &[FactAvailability::Captured, FactAvailability::Unavailable]
    );
    Ok(())
}

#[test]
fn authority_parentage_states_are_closed_and_exact() -> Result<(), compiler_ir::BuildError> {
    let versions = [version(1, 1), version(2, 1), version(3, 1), version(4, 1)];
    let items = [
        TreeItemInput {
            name: b"root",
            kind: ItemKind::Module,
            visibility: Visibility::Public,
            authority: EntityAuthorityFacts {
                parentage: ParentageAuthority::Root,
                ..unavailable_authority()
            },
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        },
        TreeItemInput {
            name: b"child",
            kind: ItemKind::Record,
            visibility: Visibility::Public,
            authority: EntityAuthorityFacts {
                parentage: ParentageAuthority::Bound(versions[0].identity()),
                ..unavailable_authority()
            },
            parent: Some(TreeEntityId::new(0)),
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        },
        TreeItemInput {
            name: b"foreign-owner",
            kind: ItemKind::Record,
            visibility: Visibility::Public,
            authority: EntityAuthorityFacts {
                parentage: ParentageAuthority::UnrepresentedAuthorityOwner(
                    UnrepresentedAuthorityOwner::new([0x55; 16]),
                ),
                ..unavailable_authority()
            },
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        },
        TreeItemInput {
            name: b"unknown-owner",
            kind: ItemKind::Record,
            visibility: Visibility::Public,
            authority: unavailable_authority(),
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        },
    ];
    let mut builder = IrBuilder::new();
    builder.add_borrowed_tree(BorrowedTree {
        versions: &versions,
        items: &items,
        links: &[],
    })?;
    let ir = builder.finish()?;
    let parentage = ir.entity_authority_columns().parentage;
    assert_eq!(parentage[0], ParentageAuthority::Root);
    assert_eq!(
        parentage[1],
        ParentageAuthority::Bound(versions[0].identity())
    );
    assert_eq!(
        parentage[2],
        ParentageAuthority::UnrepresentedAuthorityOwner(UnrepresentedAuthorityOwner::new(
            [0x55; 16]
        ))
    );
    assert_eq!(parentage[3], ParentageAuthority::Unavailable);
    Ok(())
}

#[test]
fn authority_parentage_and_source_conflicts_retain_exact_facts()
-> Result<(), compiler_ir::BuildError> {
    let versions = [version(1, 1), version(2, 1)];
    let items = [
        TreeItemInput {
            name: b"parent",
            kind: ItemKind::Module,
            visibility: Visibility::Public,
            authority: unavailable_authority(),
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        },
        TreeItemInput {
            name: b"bad-child",
            kind: ItemKind::Record,
            visibility: Visibility::Public,
            authority: EntityAuthorityFacts {
                parentage: ParentageAuthority::Root,
                ..unavailable_authority()
            },
            parent: Some(TreeEntityId::new(0)),
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        },
    ];
    let mut builder = IrBuilder::new();
    builder.add_borrowed_tree(BorrowedTree {
        versions: &versions,
        items: &items,
        links: &[],
    })?;
    assert!(matches!(
        builder.finish(),
        Err(compiler_ir::BuildError::AuthorityFacts {
            entity,
            cause: compiler_ir::AuthorityFactFault::Parentage {
                claimed: ParentageAuthority::Root,
                local_parent: Some(identity),
            },
        }) if entity == compiler_ir::EntityId::new(1) && identity == versions[0].identity()
    ));

    let mut builder = IrBuilder::new();
    let source = builder.intern_atom(b"source.rs")?;
    let Some(span) = SourceSpan::new(source, 0, 1) else {
        return Ok(());
    };
    builder.add_borrowed_tree(BorrowedTree {
        versions: &versions[..1],
        items: &[TreeItemInput {
            name: b"bad-source",
            kind: ItemKind::Constant,
            visibility: Visibility::Public,
            authority: EntityAuthorityFacts {
                source: FactAvailability::Captured,
                source_file: FactAvailability::Unavailable,
                ..unavailable_authority()
            },
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: Some(span),
            extension: None,
        }],
        links: &[],
    })?;
    assert!(matches!(
        builder.finish(),
        Err(compiler_ir::BuildError::AuthorityFacts {
            entity,
            cause: compiler_ir::AuthorityFactFault::SourceFileWithoutSource {
                source: FactAvailability::Captured,
                source_file: FactAvailability::Unavailable,
            },
        }) if entity == compiler_ir::EntityId::new(0)
    ));
    Ok(())
}

fn measured_stream_build(count: usize) -> AllocationInfo {
    let versions = (0..count)
        .map(|index| {
            let bytes = index.to_le_bytes();
            EntityVersion {
                family: DeclarationFamilyId::from_canonical_bytes(&bytes),
                variant: VariantFingerprint::from_canonical_bytes(&bytes),
                core_payload: CorePayloadHash::from_canonical_bytes(&bytes),
            }
        })
        .collect::<Vec<_>>();
    let names = vec![b"same-name".as_slice(); count];
    let tree = NativeTree {
        versions: &versions,
        names: &names,
    };
    let mut result = None;
    let allocations = measure(|| {
        let mut builder = IrBuilder::new();
        result = Some(
            builder
                .add_frontend_tree(&tree)
                .and_then(|_| builder.finish())
                .map(|ir| ir.entity_count()),
        );
    });
    let entity_count = result
        .expect("measurement executed")
        .expect("stream build succeeds");
    assert_eq!(entity_count, count);
    allocations
}

#[test]
fn exact_reservation_keeps_builder_allocation_calls_constant_as_rows_scale() {
    let one = measured_stream_build(1);
    let sixty_four = measured_stream_build(64);
    assert_eq!(one.count_current, 0);
    assert_eq!(sixty_four.count_current, 0);
    assert_eq!(
        sixty_four.count_total, one.count_total,
        "one={one:?}, sixty_four={sixty_four:?}"
    );
    assert!(one.count_total <= 16, "{one:?}");
    assert!(
        sixty_four.count_total <= one.count_total.saturating_add(2),
        "one={one:?}, sixty_four={sixty_four:?}"
    );
    assert!(
        sixty_four.bytes_total <= one.bytes_total.saturating_mul(128),
        "one={one:?}, sixty_four={sixty_four:?}"
    );
}

#[test]
fn transparent_state_terms_pack_directly_without_interning_allocations() {
    assert_eq!(
        size_of::<GuardedType<ConcreteState>>(),
        size_of::<ConcreteType>()
    );
    assert_eq!(
        size_of::<GuardedType<ComputedState>>(),
        size_of::<ComputedType>()
    );
    assert_eq!(
        size_of::<GuardedType<UnknownState>>(),
        size_of::<UnknownType>()
    );

    let versions = (0_usize..64)
        .map(|index| {
            let bytes = index.to_le_bytes();
            EntityVersion {
                family: DeclarationFamilyId::from_canonical_bytes(&bytes),
                variant: VariantFingerprint::from_canonical_bytes(&bytes),
                core_payload: CorePayloadHash::from_canonical_bytes(&bytes),
            }
        })
        .collect::<Vec<_>>();
    let names = vec![b"typed".as_slice(); versions.len()];
    let tree = NativeTree {
        versions: &versions,
        names: &names,
    };
    let mut builder = IrBuilder::new();
    let reservation = measure(|| builder.reserve_types(versions.len()));
    assert_eq!(reservation.count_total, 2, "{reservation:?}");
    assert_eq!(reservation.bytes_total, 1_536, "{reservation:?}");
    let interning = measure(|| {
        for raw in 0..versions.len() {
            let id = builder
                .intern_guarded(GuardedType::concrete(ConcreteType::Nominal(
                    compiler_ir::EntityId::new(raw as u32),
                )))
                .expect("reserved packed type");
            assert_eq!(id.erase().raw, raw as u32);
        }
    });
    assert_eq!(interning, AllocationInfo::default());
    builder
        .add_frontend_tree(&tree)
        .expect("valid entity range");
    let ir = builder.finish().expect("nominal targets become valid");
    let types = ir.storage_columns().types;
    assert_eq!(types.headers.len(), versions.len());
    assert!(types.pairs.is_empty());
    assert!(types.triples.is_empty());
    assert!(types.quads.is_empty());
    assert_eq!(size_of_val(types.headers), versions.len() * 8);
}

#[test]
fn borrowed_tree_keeps_binary_atoms_and_renders_computed_typescript()
-> Result<(), compiler_ir::BuildError> {
    let mut builder = IrBuilder::new();
    let raw = builder.intern_atom(&[0xff, b'N'])?;
    assert_eq!(raw, builder.intern_atom(&[0xff, b'N'])?);

    let versions = [version(2, 1), version(1, 1)];
    builder.set_language_profile(compiler_vocabulary::LanguageProfile::TypeScript(
        compiler_vocabulary::TypeScriptSource::TypeScript,
    ))?;
    let mut tree = builder.reserve_tree(&versions)?;
    let entities = tree.entities();
    let string = tree.intern_concrete(ConcreteType::Builtin(compiler_ir::BuiltinType::String))?;
    let key_name = tree.intern_atom(b"K")?;
    let keys = tree.intern_computed(ComputedType::KeyOf(string.erase()))?;
    let mapped = tree.intern_computed(ComputedType::Mapped {
        parameter: key_name,
        constraint: keys.erase(),
        name_as: Some(string.erase()),
        value: string.erase(),
        readonly: MappedModifier::Add,
        optional: MappedModifier::Remove,
    })?;
    let parameter_bounds =
        tree.intern_type_parameter_bounds(&[TypeParameterBound::Type(keys.erase())])?;
    let parameters = tree.intern_type_parameters(&[TypeParameter {
        name: key_name,
        bounds: parameter_bounds,
        default: None,
        variance: Variance::Invariant,
        kind: TypeParameterKind::Type {
            inference: TypeParameterInference::Ordinary,
        },
        requirements: TypeParameterRequirements::none(),
    }])?;
    let members = [TreeEntityId::new(1)];
    let docs = [
        DocInput::Text("See "),
        DocInput::Link {
            label: "field",
            target: TreeLinkTarget::Local(TreeEntityId::new(1)),
        },
    ];
    let typescript = TypeScriptFacts {
        type_parameters: parameters,
        declared: Some(string.erase()),
        observed: Some(mapped.erase()),
    };
    let items = [
        TreeItemInput {
            name: &[0xff, b'N'],
            kind: ItemKind::TypeAlias,
            visibility: Visibility::Public,
            authority: EntityAuthorityFacts {
                semantic_type: FactAvailability::Captured,
                members: FactAvailability::Captured,
                documentation: FactAvailability::Captured,
                language_extension: FactAvailability::Captured,
                ..unavailable_authority()
            },
            parent: None,
            semantic_type: Some(mapped.erase()),
            members: &members,
            docs: &docs,
            attributes: &[],
            source: None,
            extension: Some(LanguageExtensionInput::TypeScript(&typescript)),
        },
        TreeItemInput {
            name: b"field",
            kind: ItemKind::Field,
            visibility: Visibility::Public,
            authority: EntityAuthorityFacts {
                parentage: ParentageAuthority::Bound(versions[0].identity()),
                semantic_type: FactAvailability::Captured,
                ..unavailable_authority()
            },
            parent: Some(TreeEntityId::new(0)),
            semantic_type: Some(string.erase()),
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        },
    ];
    let links = [TreeLinkInput {
        from: TreeEntityId::new(1),
        target: TreeLinkTarget::Local(TreeEntityId::new(0)),
        kind: LinkKind::TypeReference,
        confidence: Confidence::Compiler,
        authority: OccurrenceAuthorityFacts::default(),
        source: None,
    }];
    tree.commit(&items, &links)?;
    let ir = builder.finish()?;

    assert_eq!(ir.atom(raw), Some(&[0xff, b'N'][..]));
    assert_eq!(
        ir.typed_type(mapped),
        Some(ComputedType::Mapped {
            parameter: key_name,
            constraint: keys.erase(),
            name_as: Some(string.erase()),
            value: string.erase(),
            readonly: MappedModifier::Add,
            optional: MappedModifier::Remove,
        })
    );
    assert_eq!(
        ir.language_extensions()
            .typescript
            .get(entities.start())
            .copied(),
        Some(TypeScriptFacts {
            type_parameters: parameters,
            declared: Some(string.erase()),
            observed: Some(mapped.erase()),
        })
    );
    assert_eq!(
        ir.signature(entities.get(TreeEntityId::new(0)).expect("reserved entity"))
            .map(|value| value.to_string()),
        Some("pub type %FFN = { +readonly [K in keyof str as str]-?: str }".into()),
    );
    assert_eq!(
        ir.display_docs(entities.get(TreeEntityId::new(0)).expect("reserved entity"))
            .map(|value| value.to_string()),
        Some("See [field](field.html)".into()),
    );
    let field = entities.get(TreeEntityId::new(1)).expect("reserved entity");
    assert_eq!(ir.links_from(field).len(), 1);
    assert_eq!(ir.links_to(entities.start()).len(), 1);
    assert_eq!(
        ir.items_of_kind(ItemKind::Field)
            .map(|item| item.id())
            .collect::<Vec<_>>(),
        vec![field]
    );
    assert_eq!(
        ir.items_named(b"field")
            .map(|item| item.id())
            .collect::<Vec<_>>(),
        vec![field]
    );
    assert_eq!(
        ir.canonical_items().next().map(|item| item.id()),
        Some(field)
    );
    Ok(())
}

#[test]
fn image_provenance_rebind_is_coordinate_free_and_non_mutating()
-> Result<(), compiler_ir::BuildError> {
    let source = SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"semantic-image"),
        byte_len: 14,
    };
    let recipe = CompileRecipeFact::derive(
        LanguageProfile::Rust(RustEdition::Rust2024),
        Stage::LowerIr,
        NativeTool::Rustc,
        source.identity,
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"semantic-image-toolchain"),
    );
    let lineage = PackageLineage::new("cargo", "authority-image")
        .expect("fixed authority image lineage is valid");
    let mut builder = IrBuilder::new();
    builder.set_image_provenance(source, recipe, lineage, "src/lib.rs")?;
    // Equal claims are idempotent and cannot grow the image atom arena.
    builder.set_image_provenance(source, recipe, lineage, "src/lib.rs")?;
    assert!(matches!(
        builder.set_image_provenance(source, recipe, lineage, "src/other.rs"),
        Err(compiler_ir::BuildError::ImageProvenanceRebind { existing, requested })
            if existing.source == source
                && existing.recipe == recipe
                && requested.source == source
                && requested.recipe == recipe
                && existing.scope != requested.scope
    ));
    let ir = builder.finish()?;
    let compiler_ir::ImageProvenance::Captured {
        source: retained_source,
        recipe: retained_recipe,
        scope,
        ..
    } = ir.image_provenance()
    else {
        panic!("compiled image provenance must remain captured");
    };
    assert_eq!(retained_source, source);
    assert_eq!(retained_recipe, recipe);
    assert_eq!(ir.atom(scope.ecosystem), Some(b"cargo".as_slice()));
    assert_eq!(ir.atom(scope.package), Some(b"authority-image".as_slice()));
    assert_eq!(ir.atom(scope.path), Some(b"src/lib.rs".as_slice()));
    Ok(())
}

#[test]
fn full_image_reopens_the_exact_version_qualifier_and_subpath_coordinate()
-> Result<(), Box<dyn std::error::Error>> {
    let source = SourceIdentity {
        identity: ContentId::<SourceFactDomain>::from_canonical_bytes(b"package-coordinate"),
        byte_len: 18,
    };
    let recipe = CompileRecipeFact::derive(
        LanguageProfile::Rust(RustEdition::Rust2024),
        Stage::LowerIr,
        NativeTool::Rustc,
        source.identity,
        ContentId::<ToolchainDomain>::from_canonical_bytes(b"package-coordinate-toolchain"),
    );
    let coordinate =
        PackageUrl::parse("pkg:cargo/acme/widget@1.2.3?source=registry#src/lib".to_owned())
            .map_err(|error| format!("package coordinate rejected: {error}"))?;
    let mut builder = IrBuilder::new();
    builder.set_image_provenance_for_package(source, recipe, &coordinate, "src/lib.rs")?;
    let ir = builder.finish()?;
    let mut bytes = vec![0; full_semantic_image_len(&ir)?];
    encode_full_semantic_image(&ir, &mut bytes)?;
    let reopened = SemanticImageView::reopen(&bytes)?;
    let compiler_ir::ImageProvenance::Captured { scope, .. } = reopened.image_facts().provenance
    else {
        return Err("package image lost captured provenance".into());
    };
    let coordinate_atom = scope
        .coordinate
        .ok_or("package image lost its exact coordinate")?;
    assert_eq!(
        reopened.atom(coordinate_atom),
        Some(coordinate.as_str().as_bytes())
    );
    Ok(())
}

#[test]
fn logical_links_are_unique_while_occurrence_sites_remain_exact()
-> Result<(), compiler_ir::BuildError> {
    let mut builder = IrBuilder::new();
    let source_file = builder.intern_atom(b"fixture.rs")?;
    let versions = [version(1, 1), version(2, 1)];
    let items = [
        TreeItemInput {
            name: b"source",
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            authority: unavailable_authority(),
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        },
        TreeItemInput {
            name: b"target",
            kind: ItemKind::Function,
            visibility: Visibility::Public,
            authority: unavailable_authority(),
            parent: None,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        },
    ];
    let links = [
        TreeLinkInput {
            from: TreeEntityId::new(0),
            target: TreeLinkTarget::Local(TreeEntityId::new(1)),
            kind: LinkKind::Calls,
            confidence: Confidence::Heuristic,
            authority: OccurrenceAuthorityFacts {
                source: FactAvailability::Captured,
            },
            source: SourceSpan::new(source_file, 8, 14),
        },
        TreeLinkInput {
            from: TreeEntityId::new(0),
            target: TreeLinkTarget::Local(TreeEntityId::new(1)),
            kind: LinkKind::Calls,
            confidence: Confidence::Compiler,
            authority: OccurrenceAuthorityFacts {
                source: FactAvailability::Captured,
            },
            source: SourceSpan::new(source_file, 22, 28),
        },
        TreeLinkInput {
            from: TreeEntityId::new(0),
            target: TreeLinkTarget::Local(TreeEntityId::new(1)),
            kind: LinkKind::Calls,
            confidence: Confidence::Compiler,
            authority: OccurrenceAuthorityFacts {
                source: FactAvailability::Captured,
            },
            source: SourceSpan::new(source_file, 16, 20),
        },
        TreeLinkInput {
            from: TreeEntityId::new(0),
            target: TreeLinkTarget::Local(TreeEntityId::new(1)),
            kind: LinkKind::Calls,
            confidence: Confidence::Syntactic,
            authority: OccurrenceAuthorityFacts {
                source: FactAvailability::Captured,
            },
            source: SourceSpan::new(source_file, 36, 42),
        },
    ];
    builder.add_borrowed_tree(BorrowedTree {
        versions: &versions,
        items: &items,
        links: &links,
    })?;
    let ir = builder.finish()?;
    let retained = ir
        .links_from(compiler_ir::EntityId::new(0))
        .collect::<Vec<_>>();
    assert_eq!(retained.len(), 1);
    assert_eq!(retained[0].1.confidence, Confidence::Compiler);
    assert_eq!(retained[0].1.source.map(SourceSpan::start), Some(16));
    let occurrences = ir
        .link_occurrences_from(compiler_ir::EntityId::new(0))
        .collect::<Vec<_>>();
    assert_eq!(occurrences.len(), 4);
    assert!(
        occurrences
            .iter()
            .all(|(_, occurrence)| occurrence.link == retained[0].0)
    );
    assert_eq!(
        occurrences
            .iter()
            .map(|(_, occurrence)| occurrence.source.map(SourceSpan::start))
            .collect::<Vec<_>>(),
        vec![Some(8), Some(16), Some(22), Some(36)]
    );
    assert_eq!(
        occurrences
            .iter()
            .map(|(_, occurrence)| occurrence.confidence)
            .collect::<Vec<_>>(),
        vec![
            Confidence::Heuristic,
            Confidence::Compiler,
            Confidence::Compiler,
            Confidence::Syntactic,
        ]
    );
    assert_eq!(
        ir.occurrence_authority_columns().source,
        &[
            FactAvailability::Captured,
            FactAvailability::Captured,
            FactAvailability::Captured,
            FactAvailability::Captured,
        ]
    );
    let mut reversed_builder = IrBuilder::new();
    let reversed_file = reversed_builder.intern_atom(b"fixture.rs")?;
    let reversed_links = [
        TreeLinkInput {
            from: TreeEntityId::new(0),
            target: TreeLinkTarget::Local(TreeEntityId::new(1)),
            kind: LinkKind::Calls,
            confidence: Confidence::Syntactic,
            authority: OccurrenceAuthorityFacts {
                source: FactAvailability::Captured,
            },
            source: SourceSpan::new(reversed_file, 36, 42),
        },
        TreeLinkInput {
            from: TreeEntityId::new(0),
            target: TreeLinkTarget::Local(TreeEntityId::new(1)),
            kind: LinkKind::Calls,
            confidence: Confidence::Compiler,
            authority: OccurrenceAuthorityFacts {
                source: FactAvailability::Captured,
            },
            source: SourceSpan::new(reversed_file, 16, 20),
        },
        TreeLinkInput {
            from: TreeEntityId::new(0),
            target: TreeLinkTarget::Local(TreeEntityId::new(1)),
            kind: LinkKind::Calls,
            confidence: Confidence::Compiler,
            authority: OccurrenceAuthorityFacts {
                source: FactAvailability::Captured,
            },
            source: SourceSpan::new(reversed_file, 22, 28),
        },
        TreeLinkInput {
            from: TreeEntityId::new(0),
            target: TreeLinkTarget::Local(TreeEntityId::new(1)),
            kind: LinkKind::Calls,
            confidence: Confidence::Heuristic,
            authority: OccurrenceAuthorityFacts {
                source: FactAvailability::Captured,
            },
            source: SourceSpan::new(reversed_file, 8, 14),
        },
    ];
    reversed_builder.add_borrowed_tree(BorrowedTree {
        versions: &versions,
        items: &items,
        links: &reversed_links,
    })?;
    let reversed = reversed_builder.finish()?;
    let reversed_relation = reversed
        .links_from(compiler_ir::EntityId::new(0))
        .next()
        .ok_or(compiler_ir::BuildError::Dangling {
            space: compiler_ir::SemanticSpace::Link,
            raw: 0,
        })?
        .1;
    assert_eq!(reversed_relation.confidence, Confidence::Compiler);
    assert_eq!(reversed_relation.source.map(SourceSpan::start), Some(16));
    Ok(())
}

#[test]
fn occurrence_source_authority_mismatch_retains_the_exact_site()
-> Result<(), compiler_ir::BuildError> {
    let mut builder = IrBuilder::new();
    let source_file = builder.intern_atom(b"occurrence.rs")?;
    let versions = [version(1, 1)];
    let items = [TreeItemInput {
        name: b"owner",
        kind: ItemKind::Function,
        visibility: Visibility::Public,
        authority: unavailable_authority(),
        parent: None,
        semantic_type: None,
        members: &[],
        docs: &[],
        attributes: &[],
        source: None,
        extension: None,
    }];
    let links = [TreeLinkInput {
        from: TreeEntityId::new(0),
        target: TreeLinkTarget::Local(TreeEntityId::new(0)),
        kind: LinkKind::Calls,
        confidence: Confidence::Syntactic,
        authority: OccurrenceAuthorityFacts::default(),
        source: SourceSpan::new(source_file, 0, 1),
    }];
    builder.add_borrowed_tree(BorrowedTree {
        versions: &versions,
        items: &items,
        links: &links,
    })?;
    assert!(matches!(
        builder.finish(),
        Err(compiler_ir::BuildError::OccurrenceAuthorityFacts {
            occurrence,
            cause: compiler_ir::AuthorityFactFault::Availability {
                plane: compiler_ir::AuthorityFactPlane::OccurrenceSource,
                claimed: FactAvailability::Unavailable,
                present: true,
            },
        }) if occurrence == compiler_ir::LinkOccurrenceId::new(0)
    ));
    Ok(())
}

#[test]
fn vcs_diffs_the_same_ir_without_lowering_or_archiving() -> Result<(), compiler_ir::BuildError> {
    let before = simple_ir(
        &[version(1, 1), version(2, 1)],
        &[Some(TreeEntityId::new(0))],
        true,
    )?;
    let after = simple_ir(
        &[version(1, 1), version(2, 2), version(3, 1)],
        &[None, None],
        false,
    )?;
    let before = Snapshot {
        generation: compiler_ir::GenerationId::from_raw([1; 32]),
        ir: &before,
    };
    let after = Snapshot {
        generation: compiler_ir::GenerationId::from_raw([2; 32]),
        ir: &after,
    };
    let diff = Diff::between(before, after);
    let entity_changes = diff.entities.collect::<Vec<_>>();
    assert!(matches!(
        entity_changes.as_slice(),
        [
            EntityChange::Retained { .. },
            EntityChange::Introduced { .. },
        ]
    ));
    let link_changes = diff.links.map(|change| change.kind).collect::<Vec<_>>();
    assert_eq!(link_changes, vec![LinkChangeKind::Removed]);

    let before = SemanticSnapshot {
        generation: compiler_ir::GenerationId::from_raw([1; 32]),
        reader: before.ir,
    };
    let after = SemanticSnapshot {
        generation: compiler_ir::GenerationId::from_raw([2; 32]),
        reader: after.ir,
    };
    let semantic_diff = SemanticDiff::between(before, after);
    assert!(matches!(
        semantic_diff.entities.collect::<Vec<_>>().as_slice(),
        [
            SemanticEntityChange::Retained { .. },
            SemanticEntityChange::Introduced { .. }
        ]
    ));
    assert_eq!(
        semantic_diff
            .links
            .map(|change| change.kind)
            .collect::<Vec<_>>(),
        vec![SemanticLinkChangeKind::Removed]
    );

    let mut before_bytes = vec![0; full_semantic_image_len(before.reader).expect("before length")];
    encode_full_semantic_image(before.reader, &mut before_bytes).expect("encode before");
    let before_image = SemanticImageView::reopen(&before_bytes).expect("reopen before");
    let mut after_bytes = vec![0; full_semantic_image_len(after.reader).expect("after length")];
    encode_full_semantic_image(after.reader, &mut after_bytes).expect("encode after");
    let after_image = SemanticImageView::reopen(&after_bytes).expect("reopen after");
    let before = SemanticSnapshot {
        generation: compiler_ir::GenerationId::from_raw([1; 32]),
        reader: &before_image,
    };
    let after = SemanticSnapshot {
        generation: compiler_ir::GenerationId::from_raw([2; 32]),
        reader: &after_image,
    };
    let reopened_diff = SemanticDiff::between(before, after);
    assert!(matches!(
        reopened_diff.entities.collect::<Vec<_>>().as_slice(),
        [
            SemanticEntityChange::Retained { .. },
            SemanticEntityChange::Introduced { .. }
        ]
    ));
    assert_eq!(
        reopened_diff
            .links
            .map(|change| change.kind)
            .collect::<Vec<_>>(),
        vec![SemanticLinkChangeKind::Removed]
    );
    Ok(())
}

fn simple_ir(
    versions: &[EntityVersion],
    child_parents: &[Option<TreeEntityId>],
    link: bool,
) -> Result<Ir, compiler_ir::BuildError> {
    let mut builder = IrBuilder::new();
    let mut items = Vec::with_capacity(versions.len());
    items.push(TreeItemInput {
        name: b"root",
        kind: ItemKind::Module,
        visibility: Visibility::Public,
        authority: unavailable_authority(),
        parent: None,
        semantic_type: None,
        members: &[],
        docs: &[],
        attributes: &[],
        source: None,
        extension: None,
    });
    for (index, parent) in child_parents.iter().enumerate() {
        items.push(TreeItemInput {
            name: if index == 0 { b"child" } else { b"added" },
            kind: ItemKind::Record,
            visibility: Visibility::Public,
            authority: parent
                .map(|parent| EntityAuthorityFacts {
                    parentage: ParentageAuthority::Bound(versions[parent.index()].identity()),
                    ..unavailable_authority()
                })
                .unwrap_or_else(unavailable_authority),
            parent: *parent,
            semantic_type: None,
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        });
    }
    let links = [TreeLinkInput {
        from: TreeEntityId::new(0),
        target: TreeLinkTarget::Local(TreeEntityId::new(1)),
        kind: LinkKind::TypeReference,
        confidence: Confidence::Compiler,
        authority: OccurrenceAuthorityFacts::default(),
        source: None,
    }];
    builder.add_borrowed_tree(BorrowedTree {
        versions,
        items: &items,
        links: if link { &links } else { &[] },
    })?;
    builder.finish()
}

#[test]
fn unknown_types_are_neither_concrete_nor_computed() -> Result<(), compiler_ir::BuildError> {
    let mut builder = IrBuilder::new();
    let ty = builder.intern_type(TypeExpr::Unknown(UnknownType::new(
        compiler_ir::UnknownReason::UnresolvedLocalName,
    )))?;
    let versions = [version(1, 1)];
    builder.add_borrowed_tree(BorrowedTree {
        versions: &versions,
        items: &[TreeItemInput {
            name: b"value",
            kind: ItemKind::Constant,
            visibility: Visibility::Public,
            authority: EntityAuthorityFacts {
                semantic_type: FactAvailability::Captured,
                ..unavailable_authority()
            },
            parent: None,
            semantic_type: Some(ty),
            members: &[],
            docs: &[],
            attributes: &[],
            source: None,
            extension: None,
        }],
        links: &[],
    })?;
    let ir = builder.finish()?;
    assert_eq!(
        ir.display_type(ty).map(|value| value.to_string()),
        Some("?unresolved-local".into())
    );
    assert!(
        ir.ty(ty)
            .is_some_and(|value| !value.is_computed() && value.concrete().is_none())
    );
    Ok(())
}

#[test]
fn unknown_spelling_is_an_atom_coordinate_not_a_renderer_fallback() {
    let mut builder = IrBuilder::new();
    assert!(matches!(
        builder.intern_type(TypeExpr::Unknown(
            UnknownType::new(compiler_ir::UnknownReason::NoIrRepresentation)
                .with_spelling(AtomId::new(0)),
        )),
        Err(compiler_ir::BuildError::Dangling {
            space: compiler_ir::SemanticSpace::Atom,
            raw: 0,
        })
    ));
}

#[test]
fn every_hot_borrowed_view_is_allocation_free() -> Result<(), compiler_ir::BuildError> {
    let ir = simple_ir(
        &[version(1, 1), version(2, 1)],
        &[Some(TreeEntityId::new(0))],
        true,
    )?;
    let generation = compiler_ir::GenerationId::from_canonical_bytes(b"allocation-proof");
    let snapshot = Snapshot {
        generation,
        ir: &ir,
    };
    let mut rendered = FixedText::<4096>::new();
    let allocations = measure(|| {
        black_box(ir.items().count());
        black_box(ir.canonical_items().count());
        black_box(ir.items_of_kind(ItemKind::Record).count());
        black_box(ir.items_named(b"child").count());
        black_box(ir.links_from(compiler_ir::EntityId::new(0)).count());
        black_box(ir.links_to(compiler_ir::EntityId::new(1)).count());
        black_box(ir.stable_links().count());
        black_box(ir.signature(compiler_ir::EntityId::new(0)));
        black_box(ir.entity_columns());
        black_box(ir.graph_columns());
        black_box(ir.vcs_columns());
        black_box(ir.storage_columns());
        black_box(ir.language_extensions().typescript);
        black_box(ir.embedding_text(
            compiler_ir::EntityId::new(0),
            compiler_ir::EmbeddingProfile::CONTEXTUAL,
        ));
        black_box(Diff::between(snapshot, snapshot).entities.count());
        black_box(Diff::between(snapshot, snapshot).links.count());
        use core::fmt::Write as _;
        rendered.clear();
        write!(
            rendered,
            "{}",
            ir.signature(compiler_ir::EntityId::new(0))
                .expect("signature")
        )
        .expect("fixed signature buffer");
        rendered.clear();
        write!(
            rendered,
            "{}",
            ir.embedding_text(
                compiler_ir::EntityId::new(0),
                compiler_ir::EmbeddingProfile::CONTEXTUAL,
            )
            .expect("embedding display")
        )
        .expect("fixed embedding buffer");
    });
    assert_eq!(allocations, AllocationInfo::default());
    assert_eq!(
        size_of::<compiler_ir::OptionalId<compiler_ir::Entity>>(),
        size_of::<u32>()
    );
    assert!(
        size_of::<Option<compiler_ir::EntityId>>()
            > size_of::<compiler_ir::OptionalId<compiler_ir::Entity>>()
    );
    let soa_width = size_of::<compiler_ir::AtomId>()
        + size_of::<ItemKind>()
        + size_of::<Visibility>()
        + size_of::<compiler_ir::OptionalId<compiler_ir::Entity>>()
        + size_of::<compiler_ir::OptionalId<compiler_ir::Type>>()
        + size_of::<compiler_ir::EntityListId>()
        + size_of::<compiler_ir::DocId>()
        + size_of::<compiler_ir::AtomListId>();
    assert_eq!(soa_width, 27);
    assert!(size_of::<compiler_ir::Item>() > soa_width);
    assert_eq!(size_of::<u32>(), 4);
    assert_eq!(size_of::<compiler_ir::TypeTag>(), 1);
    assert_eq!(size_of::<TypeHeader>(), 8);
    assert_eq!(size_of::<TypePairPayload>(), 8);
    assert_eq!(size_of::<TypeTriplePayload>(), 12);
    assert_eq!(size_of::<TypeQuadPayload>(), 16);
    // Family, variant, and core-payload hashes are independent identities.
    assert_eq!(size_of::<EntityVersion>(), 48);
    assert!(size_of::<TypeExpr>() >= 3 * size_of::<TypeHeader>());
    assert_eq!(ir.storage_columns().types.headers.len(), 0);
    assert!(size_of::<Option<TypeScriptFacts>>() > size_of::<u32>());
    let graph = ir.graph_columns();
    let graph_hot_width = size_of_val(&graph.from[0])
        + size_of_val(&graph.targets[0])
        + size_of_val(&graph.kinds[0])
        + size_of_val(&graph.confidence[0])
        + size_of::<u32>();
    assert_eq!(graph_hot_width, 18);
    assert!(graph_hot_width < size_of::<compiler_ir::Link>());
    let columns = ir.entity_columns();
    assert_eq!(columns.names.len(), ir.entity_count());
    assert_eq!(columns.parents.len(), ir.entity_count());
    assert_eq!(
        ir.language_extensions().typescript.ids.row_count(),
        ir.entity_count()
    );
    assert!(
        ir.language_extensions()
            .typescript
            .ids
            .ordinals()
            .is_empty()
    );
    assert_eq!(ir.source_columns().row_count(), ir.entity_count());
    assert!(ir.source_columns().files.is_empty());
    assert_eq!(
        columns.names.as_ptr(),
        ir.storage_columns().entities.names.as_ptr()
    );
    Ok(())
}
