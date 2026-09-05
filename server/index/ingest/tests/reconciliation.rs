use compiler_ir::{
    BorrowedTree, CorePayloadHash, DeclarationFamilyId, EntityAuthorityFacts, EntityVersion,
    FactAvailability, IrBuilder, ItemKind, ParentageAuthority, SemanticCoreReader,
    SemanticImageIdentity, SemanticImageView, SemanticReader, SourceSpan, TreeItemInput,
    VariantFingerprint, Visibility, encode_full_semantic_image, full_semantic_image_len,
};
use heart_identity::{CompilePublicationDomain, ContentId, GenerationId};
use server_index_ingest::{
    Checkpoint, IngestedVersion, IngestedVersionFault, IngestionOrigin, ReconciliationFault,
    ReconciliationOperation, reconcile_into,
};
use server_index_vocabulary::{
    CanonicalEntityLocator, IndexLocatorFacts, IndexSnapshotId, PackageCoordinate, PackageVersion,
    SemanticImageExtent, SemanticImageLocator, VerifiedSemanticPublication,
};

fn image() -> Result<(&'static SemanticImageView<'static>, SemanticImageLocator), String> {
    image_with_seed(1)
}

fn image_with_seed(
    seed: u8,
) -> Result<(&'static SemanticImageView<'static>, SemanticImageLocator), String> {
    let mut builder = IrBuilder::new();
    let file = builder
        .intern_atom(b"fixture.rs")
        .map_err(|e| format!("{e:?}"))?;
    let version = EntityVersion {
        family: DeclarationFamilyId::from_raw([seed; 16]),
        variant: VariantFingerprint::from_raw([seed.wrapping_add(1); 16]),
        core_payload: CorePayloadHash::from_raw([seed.wrapping_add(2); 16]),
    };
    let version_two = EntityVersion {
        family: DeclarationFamilyId::from_raw([seed.wrapping_add(3); 16]),
        variant: VariantFingerprint::from_raw([seed.wrapping_add(4); 16]),
        core_payload: CorePayloadHash::from_raw([seed.wrapping_add(5); 16]),
    };
    let item = TreeItemInput {
        name: b"entry",
        kind: ItemKind::Module,
        visibility: Visibility::Public,
        authority: EntityAuthorityFacts {
            parentage: ParentageAuthority::Root,
            visibility: FactAvailability::Captured,
            source: FactAvailability::Captured,
            source_file: FactAvailability::Captured,
            ..EntityAuthorityFacts::default()
        },
        parent: None,
        semantic_type: None,
        members: &[],
        docs: &[],
        attributes: &[],
        source: SourceSpan::new(file, 1, 3),
        extension: None,
    };
    let item_two = TreeItemInput {
        name: b"second",
        ..item
    };
    let versions = [version, version_two];
    let items = [item, item_two];
    builder
        .add_borrowed_tree(BorrowedTree {
            versions: &versions,
            items: &items,
            links: &[],
        })
        .map_err(|e| format!("{e:?}"))?;
    let ir = builder.finish().map_err(|e| format!("{e:?}"))?;
    let length = full_semantic_image_len(&ir).map_err(|e| format!("{e:?}"))?;
    let mut bytes = vec![0; length];
    encode_full_semantic_image(&ir, &mut bytes).map_err(|e| format!("{e:?}"))?;
    let leaked: &'static [u8] = Box::leak(bytes.into_boxed_slice());
    let view = Box::leak(Box::new(
        SemanticImageView::reopen(leaked).map_err(|e| format!("{e:?}"))?,
    ));
    let encoded_len = u32::try_from(view.as_ref().len())
        .map_err(|_| "fixture image exceeds a u32 extent".to_owned())?;
    let locator = SemanticImageLocator::new(
        SemanticImageIdentity::from_encoded_bytes(view.as_ref()),
        SemanticImageExtent::new(0, encoded_len).map_err(|e| format!("{e:?}"))?,
    );
    Ok((view, locator))
}

fn facts() -> IndexLocatorFacts {
    IndexLocatorFacts::new(
        GenerationId::from_canonical_bytes(b"generation"),
        IndexSnapshotId::from_canonical_bytes(b"snapshot"),
        ContentId::<CompilePublicationDomain>::from_canonical_bytes(b"publication"),
    )
}

fn publication(
    view: &SemanticImageView<'_>,
    image: SemanticImageLocator,
    facts: IndexLocatorFacts,
) -> Result<VerifiedSemanticPublication, String> {
    VerifiedSemanticPublication::verify_reopened(facts, image, view).map_err(|e| format!("{e:?}"))
}

fn entities(
    view: &'static SemanticImageView<'static>,
    image: SemanticImageLocator,
) -> Result<&'static [server_index_vocabulary::VerifiedCanonicalEntityLocator], String> {
    Ok(Box::leak(
        view.canonical_entities()
            .enumerate()
            .map(|(ordinal, entity)| {
                CanonicalEntityLocator::new(
                    image,
                    u32::try_from(ordinal).map_err(|_| "fixture ordinal")?,
                    entity.version.identity(),
                )
                .verify_reopened(view)
                .map_err(|e| format!("{e:?}"))
            })
            .collect::<Result<Vec<_>, _>>()?
            .into_boxed_slice(),
    ))
}

fn row<'a>(
    name: &'a str,
    view: &'static SemanticImageView<'static>,
    image: SemanticImageLocator,
) -> Result<IngestedVersion<'a, 'static>, String> {
    let lineage = compiler_ir::PackageLineage::new("cargo", name).map_err(|e| format!("{e:?}"))?;
    IngestedVersion::new(
        IngestionOrigin::RegistryUpdate,
        PackageCoordinate::new(
            lineage,
            PackageVersion::new("1").map_err(|e| format!("{e:?}"))?,
        ),
        publication(view, image, facts())?,
        entities(view, image)?,
    )
    .map_err(|e| format!("{e:?}"))
}

fn operation_payload<'r, 'c, 'e>(
    op: &ReconciliationOperation<'r, 'c, 'e>,
) -> (
    u8,
    PackageCoordinate<'c>,
    SemanticImageLocator,
    IndexLocatorFacts,
) {
    match op {
        ReconciliationOperation::Insert(row) => {
            (0, row.coordinate(), row.image(), row.locator_facts())
        }
        ReconciliationOperation::Replace { desired, .. } => (
            1,
            desired.coordinate(),
            desired.image(),
            desired.locator_facts(),
        ),
        ReconciliationOperation::Unchanged(row) => {
            (2, row.coordinate(), row.image(), row.locator_facts())
        }
        ReconciliationOperation::Rebind { desired, .. } => (
            3,
            desired.coordinate(),
            desired.image(),
            desired.locator_facts(),
        ),
        ReconciliationOperation::Remove(row) => {
            (4, row.coordinate(), row.image(), row.locator_facts())
        }
    }
}

#[test]
fn canonical_source_entities_and_checkpoint_contracts_hold() -> Result<(), String> {
    let (view, image) = image()?;
    let first = view.canonical_entities().next().ok_or("no entity")?;
    let Some(source) = first.source else {
        return Err("source span was not retained in image".into());
    };
    if view.atom(source.file()) != Some(b"fixture.rs".as_slice())
        || source.start() != 1
        || source.end() != 3
    {
        return Err("wrong source span".into());
    }
    let declaration = first.version.identity();
    let verified = CanonicalEntityLocator::new(image, 0, declaration)
        .verify_reopened(view)
        .map_err(|e| format!("{e:?}"))?;
    let declaration_two = view
        .canonical_entities()
        .nth(1)
        .ok_or("no second entity")?
        .version
        .identity();
    let verified_two = CanonicalEntityLocator::new(image, 1, declaration_two)
        .verify_reopened(view)
        .map_err(|e| format!("{e:?}"))?;
    let coordinate = row("verified", view, image)?.coordinate();
    let reversed = [verified_two, verified];
    if !matches!(
        IngestedVersion::new(
            IngestionOrigin::RemotePull,
            coordinate,
            publication(view, image, facts())?,
            &reversed
        ),
        Err(IngestedVersionFault::EntityLocatorOutOfOrder { .. })
    ) {
        return Err("reversed locators accepted".into());
    }
    let verified_entities = [verified, verified_two];
    let with_entity = IngestedVersion::new(
        IngestionOrigin::RegistryUpdate,
        coordinate,
        publication(view, image, facts())?,
        &verified_entities,
    )
    .map_err(|e| format!("{e:?}"))?;
    if with_entity.entities().len() != 2 {
        return Err("verified locator was lost".into());
    }
    let version = row("fixture", view, image)?;
    let pull = IngestedVersion::new(
        IngestionOrigin::RemotePull,
        version.coordinate(),
        publication(view, image, facts())?,
        entities(view, image)?,
    )
    .map_err(|e| format!("{e:?}"))?;
    let scrape = IngestedVersion::new(
        IngestionOrigin::Scrape,
        version.coordinate(),
        publication(view, image, facts())?,
        entities(view, image)?,
    )
    .map_err(|e| format!("{e:?}"))?;
    if version.coordinate() != pull.coordinate()
        || pull.image() != scrape.image()
        || pull.origin() != IngestionOrigin::RemotePull
        || scrape.origin() != IngestionOrigin::Scrape
    {
        return Err("origins changed typed record".into());
    }
    if !matches!(
        IngestedVersion::new(
            IngestionOrigin::RegistryUpdate,
            coordinate,
            publication(view, image, facts())?,
            &[],
        ),
        Err(IngestedVersionFault::EntityCountMismatch {
            expected: 2,
            observed: 0,
        })
    ) {
        return Err("incomplete reopened image was admitted".into());
    }
    let mut output = [ReconciliationOperation::Remove(&version); 1];
    let count = reconcile_into(&[], &[version], &mut output).map_err(|e| format!("{e:?}"))?;
    if count != 1 {
        return Err("empty version was lost".into());
    }
    let old = Checkpoint {
        sequence: 1,
        page: 2,
    };
    let pending = old
        .prepare_next(Checkpoint {
            sequence: 1,
            page: 3,
        })
        .map_err(|e| format!("{e:?}"))?;
    let Err(failure) = pending.advance_after_durable_apply(|| Err::<(), _>("not durable")) else {
        return Err("failed apply advanced checkpoint".into());
    };
    if failure.current != old {
        return Err("failed apply lost current checkpoint".into());
    }
    if old.prepare_next(old).is_ok() {
        return Err("non-monotonic checkpoint accepted".into());
    }
    let pending = old
        .prepare_next(Checkpoint {
            sequence: 1,
            page: 3,
        })
        .map_err(|e| format!("{e:?}"))?;
    if pending
        .advance_after_durable_apply(|| Ok::<(), &str>(()))
        .map_err(|_| "durable apply unexpectedly failed".to_owned())?
        != (Checkpoint {
            sequence: 1,
            page: 3,
        })
    {
        return Err("successful apply lost checkpoint".into());
    }
    Ok(())
}

#[test]
fn same_image_metadata_drift_rebinds() -> Result<(), String> {
    let (view, image) = image()?;
    let declaration = view
        .canonical_entities()
        .next()
        .ok_or("no entity")?
        .version
        .identity();
    let verified = CanonicalEntityLocator::new(image, 0, declaration)
        .verify_reopened(view)
        .map_err(|e| format!("{e:?}"))?;
    let local = row("metadata", view, image)?;
    let remote_origin = IngestedVersion::new(
        IngestionOrigin::RemotePull,
        local.coordinate(),
        publication(view, image, facts())?,
        entities(view, image)?,
    )
    .map_err(|e| format!("{e:?}"))?;
    let local_rows = [local];
    let remote_rows = [remote_origin];
    let mut output = [ReconciliationOperation::Remove(&local); 1];
    reconcile_into(&local_rows, &remote_rows, &mut output).map_err(|e| format!("{e:?}"))?;
    match output[0] {
        ReconciliationOperation::Rebind { local, desired }
            if local.origin() == IngestionOrigin::RegistryUpdate
                && desired.origin() == IngestionOrigin::RemotePull
                && local.entities() == desired.entities() => {}
        _ => return Err("origin-only drift was not rebound".into()),
    }

    let _ = verified;
    Ok(())
}

#[test]
fn shuffled_sets_are_deterministic_and_replace_images() -> Result<(), String> {
    let (view, first) = image()?;
    let (second_view, second) = image_with_seed(9)?;
    let a = row("a", view, first)?;
    let b = row("b", view, first)?;
    let c = row("c", view, first)?;
    let steady = row("steady", view, first)?;
    let replacement = row("a", second_view, second)?;
    let d = row("d", view, first)?;
    let rebind = IngestedVersion::new(
        IngestionOrigin::Scrape,
        b.coordinate(),
        publication(
            view,
            first,
            IndexLocatorFacts::new(
                GenerationId::from_canonical_bytes(b"new-generation"),
                IndexSnapshotId::from_canonical_bytes(b"snapshot"),
                ContentId::<CompilePublicationDomain>::from_canonical_bytes(b"publication"),
            ),
        )?,
        entities(view, first)?,
    )
    .map_err(|e| format!("{e:?}"))?;
    let local = [c, a, b, steady];
    let desired = [rebind, replacement, d, steady];
    let mut output = [ReconciliationOperation::Remove(&a); 6];
    let count = reconcile_into(&local, &desired, &mut output).map_err(|e| format!("{e:?}"))?;
    if count != 5 {
        return Err("wrong operation count".into());
    }
    if !matches!(output[0], ReconciliationOperation::Replace { .. })
        || !matches!(output[1], ReconciliationOperation::Rebind { .. })
        || !matches!(output[2], ReconciliationOperation::Insert(_))
        || !matches!(output[3], ReconciliationOperation::Unchanged(_))
        || !matches!(output[4], ReconciliationOperation::Remove(_))
    {
        return Err("wrong operation order".into());
    }
    let local_shuffled = [steady, a, b, c];
    let desired_shuffled = [d, replacement, steady, rebind];
    let mut second_output = [ReconciliationOperation::Remove(&a); 6];
    let second_count = reconcile_into(&local_shuffled, &desired_shuffled, &mut second_output)
        .map_err(|e| format!("{e:?}"))?;
    if second_count != count {
        return Err("permutation changed operation count".into());
    }
    for (left, right) in output[..count].iter().zip(second_output[..count].iter()) {
        if operation_payload(left) != operation_payload(right) {
            return Err("permutation changed operation payload or order".into());
        }
    }
    Ok(())
}

#[test]
fn verified_locator_admission_rejects_cross_image_and_duplicates() -> Result<(), String> {
    let (view, image) = image()?;
    let (wrong_view, wrong) = image_with_seed(9)?;
    let declaration = view
        .canonical_entities()
        .next()
        .ok_or("no entity")?
        .version
        .identity();
    let verified = CanonicalEntityLocator::new(image, 0, declaration)
        .verify_reopened(view)
        .map_err(|e| format!("{e:?}"))?;
    let coordinate = row("x", view, image)?.coordinate();
    if !matches!(
        IngestedVersion::new(
            IngestionOrigin::Scrape,
            coordinate,
            publication(wrong_view, wrong, facts())?,
            entities(view, image)?
        ),
        Err(IngestedVersionFault::EntityImageMismatch)
    ) {
        return Err("cross image accepted".into());
    }
    if !matches!(
        IngestedVersion::new(
            IngestionOrigin::RemotePull,
            coordinate,
            publication(view, image, facts())?,
            &[verified, verified]
        ),
        Err(IngestedVersionFault::DuplicateDeclaration(_))
    ) {
        return Err("duplicate accepted".into());
    }
    let duplicate = row("x", view, image)?;
    let mut output = [ReconciliationOperation::Remove(&duplicate); 1];
    let before = output;
    let duplicated_rows = [duplicate, duplicate];
    if !matches!(
        reconcile_into(&duplicated_rows, &[], &mut output),
        Err(ReconciliationFault::DuplicateCoordinate(_))
    ) || output != before
    {
        return Err("duplicate coordinate mutated or accepted".into());
    }
    if !matches!(
        reconcile_into(&[duplicate], &[], &mut []),
        Err(ReconciliationFault::OutputTooShort { .. })
    ) {
        return Err("short scratch accepted".into());
    }
    let too_many = [duplicate; 65];
    if !matches!(
        reconcile_into(&too_many, &[], &mut output),
        Err(ReconciliationFault::InputTooLong)
    ) || output != before
    {
        return Err("row bound mutated output".into());
    }
    Ok(())
}
