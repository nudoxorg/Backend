//! Public scalar/SIMD equivalence and sparse-scan work laws.

use core::mem::size_of;

use nudox_id::{
    CONTENT_PAYLOAD_BYTES, ContentPayload, DependencySetDomain, Domain, GenerationId, ObjectDomain,
};
use nudox_object::{
    ObjectKind, ObjectLength, ObjectRef, ProviderId, ProviderIdError, ProviderSet, RemoteBase,
};
use nudox_root::{
    EntryKey, GenerationEntry, GenerationRoot, GenerationView, Locality, LocalityError,
    LocalityException, LocalityScanWork, LocalityValidator, NonResident, PreparedLocality,
    RootBuildError, RootEntry, ValidatedLocality,
};
use nudox_schema::SchemaId;
use thiserror::Error;

const LONG_LANE_ROWS: u8 = 64;
const CONTENT_DOMAIN_OFFSET: usize = size_of::<GenerationId>();
const LOCALITY_HEADER_BYTES: usize =
    size_of::<GenerationId>() + size_of::<u8>() + size_of::<[u32; 4]>();
const LOCALITY_SCHEMA_OFFSET: usize = CONTENT_PAYLOAD_BYTES + size_of::<u64>();
const ROW_BYTES: usize = size_of::<u32>();

#[derive(Debug, Error)]
enum LocalityTestError {
    #[error("root fixture construction failed")]
    Root(#[from] RootBuildError),
    #[error("provider fixture construction failed")]
    Provider(#[from] ProviderIdError),
    #[error("locality fixture construction or validation failed")]
    Locality(#[from] LocalityError),
    #[error("locality fixture output failed")]
    Write(#[from] nudox_root::LocalityWriteError),
    #[error("root did not issue a locality row for {key:?}")]
    MissingRow { key: EntryKey },
    #[error("canonical fixture did not contain its second row lane")]
    MissingSecondRow,
    #[error("{path:?} validation accepted duplicate canonical rows")]
    InvalidAccepted { path: ValidationPath },
    #[error("fixture did not contain its exact {cell:?} wire cell")]
    MissingWireCell { cell: FixtureCell },
    #[error("{cell:?} mutation produced the wrong rejection: {observed}")]
    WrongMutationError {
        cell: FixtureCell,
        observed: LocalityError,
    },
    #[error("direct-writer witness and public-parser witness projected different entries")]
    WriterWitnessMismatch,
}

#[derive(Debug)]
enum ValidationPath {
    Scalar,
    DetectedSimd,
}

#[derive(Clone, Copy, Debug)]
enum FixtureCell {
    Provider,
    Descriptor,
    ContentAuthority,
    Schema,
}

struct Fixture {
    root: GenerationRoot<ObjectDomain>,
    bytes: Vec<u8>,
    providers: ProviderSet,
}

struct ScanEvidence {
    entries: Vec<GenerationEntry<ObjectDomain>>,
    work: LocalityScanWork,
}

fn domain_object<DomainTag: Domain>(row: u8) -> ObjectRef<DomainTag> {
    ObjectRef {
        content: nudox_id::ContentId::from_digest([row; 32]),
        length: ObjectLength::from(u64::from(row) + 1),
        schema: SchemaId::Object,
        kind: ObjectKind::from(1),
    }
}

fn object(row: u8) -> ObjectRef<ObjectDomain> {
    domain_object(row)
}

fn fixture() -> Result<Fixture, LocalityTestError> {
    let entries = (0..LONG_LANE_ROWS)
        .map(|row| RootEntry {
            key: EntryKey::from(u64::from(row)),
            parent: None,
            object: object(row),
        })
        .collect();
    let root = GenerationRoot::new(entries)?;
    let providers = ProviderSet::only(ProviderId::try_from(7)?);
    let facts = (0..LONG_LANE_ROWS)
        .map(|row| {
            let key = EntryKey::from(u64::from(row));
            root.locality_row(key)
                .map(|row| LocalityException::new(row, NonResident::Promised(providers)))
                .ok_or(LocalityTestError::MissingRow { key })
        })
        .collect::<Result<Vec<_>, _>>()?;
    let prepared = PreparedLocality::prepare(&root, &facts)?;
    let mut bytes = vec![0; usize::from(prepared.required_bytes)];
    let _validated = prepared.write(&mut bytes)?;
    Ok(Fixture {
        root,
        bytes,
        providers,
    })
}

fn scan(view: &GenerationView<'_, '_, ObjectDomain>) -> ScanEvidence {
    let mut scan = view.measured_closure();
    let entries = scan.by_ref().collect();
    ScanEvidence {
        entries,
        work: *scan,
    }
}

#[test]
fn detected_simd_preserves_scalar_semantics_and_linear_scan_work() -> Result<(), LocalityTestError>
{
    let fixture = fixture()?;
    let scalar = ValidatedLocality::<ObjectDomain>::try_from(fixture.bytes.as_slice())?;
    let accelerated = LocalityValidator::new().validate::<ObjectDomain>(&fixture.bytes)?;

    let scalar = scan(&GenerationView::new(&fixture.root, &scalar)?);
    let accelerated = scan(&GenerationView::new(&fixture.root, &accelerated)?);
    let expected_work = LocalityScanWork {
        rows: usize::from(LONG_LANE_ROWS),
        sparse_comparisons: usize::from(LONG_LANE_ROWS),
    };

    assert_eq!(accelerated.entries, scalar.entries);
    assert_eq!(accelerated.work, expected_work);
    assert_eq!(scalar.work, expected_work);
    assert!(accelerated.entries.iter().all(|entry| {
        matches!(entry.locality, Locality::Promised(providers) if providers == fixture.providers)
    }));
    Ok(())
}

#[test]
fn detected_simd_reports_the_same_first_non_strict_row_as_scalar() -> Result<(), LocalityTestError>
{
    let mut fixture = fixture()?;
    let second_row = fixture
        .bytes
        .get_mut(LOCALITY_HEADER_BYTES + ROW_BYTES..LOCALITY_HEADER_BYTES + 2 * ROW_BYTES)
        .ok_or(LocalityTestError::MissingSecondRow)?;
    second_row.fill(0);

    let scalar = ValidatedLocality::<ObjectDomain>::try_from(fixture.bytes.as_slice())
        .err()
        .ok_or(LocalityTestError::InvalidAccepted {
            path: ValidationPath::Scalar,
        })?;
    let accelerated = LocalityValidator::new()
        .validate::<ObjectDomain>(&fixture.bytes)
        .err()
        .ok_or(LocalityTestError::InvalidAccepted {
            path: ValidationPath::DetectedSimd,
        })?;
    let expected = LocalityError::ExceptionRowsNotStrict {
        ordinal: 1,
        previous: 0,
        current: 0,
    };

    assert_eq!(scalar, expected);
    assert_eq!(accelerated, expected);
    Ok(())
}

#[test]
fn typed_witness_rejects_each_payload_invariant_once_then_projects_infallibly()
-> Result<(), LocalityTestError> {
    let fixture = mutation_fixture()?;
    assert_typed_projection(&fixture)?;
    reject_empty_provider(&fixture)?;
    reject_wrong_authority(&fixture)?;
    reject_unknown_schema(fixture)
}

#[test]
fn second_domain_round_trips_exact_authority_through_writer_and_parser()
-> Result<(), LocalityTestError> {
    let root = GenerationRoot::new(vec![RootEntry {
        key: EntryKey::from(1),
        parent: None,
        object: domain_object::<DependencySetDomain>(1),
    }])?;
    let basis = GenerationId::from_digest([2; 32]);
    let remote = domain_object::<DependencySetDomain>(3);
    let facts = [LocalityException::new(
        root.locality_row(EntryKey::from(1))
            .ok_or(LocalityTestError::MissingRow {
                key: EntryKey::from(1),
            })?,
        NonResident::Overlaid(RemoteBase::Present {
            generation: basis,
            object: remote,
        }),
    )];
    let prepared = PreparedLocality::prepare(&root, &facts)?;
    let mut bytes = vec![0; usize::from(prepared.required_bytes)];
    write_and_compare(&root, prepared, &mut bytes)?;
    match ValidatedLocality::<ObjectDomain>::try_from(bytes.as_slice()) {
        Err(LocalityError::ContentDomain { expected, observed })
            if expected == ObjectDomain::CODE
                && observed == u8::from(DependencySetDomain::CODE) =>
        {
            Ok(())
        }
        Err(observed) => Err(wrong_rejection(FixtureCell::ContentAuthority, observed)),
        Ok(_) => Err(LocalityTestError::MissingWireCell {
            cell: FixtureCell::ContentAuthority,
        }),
    }
}

struct MutationFixture {
    root: GenerationRoot<ObjectDomain>,
    bytes: Vec<u8>,
    providers: ProviderSet,
    remote: ObjectRef<ObjectDomain>,
    basis: GenerationId,
    domain_offset: usize,
    provider_offset: usize,
    descriptor_offset: usize,
}

fn mutation_fixture() -> Result<MutationFixture, LocalityTestError> {
    let root = GenerationRoot::new(vec![
        RootEntry {
            key: EntryKey::from(1),
            parent: None,
            object: object(1),
        },
        RootEntry {
            key: EntryKey::from(2),
            parent: None,
            object: object(2),
        },
    ])?;
    let providers = ProviderSet::only(ProviderId::try_from(63)?);
    let remote = object(9);
    let basis = GenerationId::from_digest([7; 32]);
    let facts = [
        LocalityException::new(
            root.locality_row(EntryKey::from(1))
                .ok_or(LocalityTestError::MissingRow {
                    key: EntryKey::from(1),
                })?,
            NonResident::Promised(providers),
        ),
        LocalityException::new(
            root.locality_row(EntryKey::from(2))
                .ok_or(LocalityTestError::MissingRow {
                    key: EntryKey::from(2),
                })?,
            NonResident::Overlaid(RemoteBase::Present {
                generation: basis,
                object: remote,
            }),
        ),
    ];
    let prepared = PreparedLocality::prepare(&root, &facts)?;
    let mut bytes = vec![0; usize::from(prepared.required_bytes)];
    write_and_compare(&root, prepared, &mut bytes)?;
    let provider_offset = wire_offset(&bytes, &providers.to_be_bytes(), FixtureCell::Provider)?;
    let descriptor = ContentPayload::from(remote.content);
    let descriptor_offset = wire_offset(&bytes, descriptor.as_ref(), FixtureCell::Descriptor)?;
    Ok(MutationFixture {
        root,
        bytes,
        providers,
        remote,
        basis,
        domain_offset: CONTENT_DOMAIN_OFFSET,
        provider_offset,
        descriptor_offset,
    })
}

fn write_and_compare<DomainTag: Domain + PartialEq>(
    root: &GenerationRoot<DomainTag>,
    prepared: PreparedLocality<'_, DomainTag>,
    bytes: &mut [u8],
) -> Result<(), LocalityTestError> {
    let writer_entries = {
        let writer = prepared.write(bytes)?;
        GenerationView::new(root, &writer)?
            .closure()
            .collect::<Vec<_>>()
    };
    let parsed = ValidatedLocality::try_from(&*bytes)?;
    let parsed_entries = GenerationView::new(root, &parsed)?
        .closure()
        .collect::<Vec<_>>();
    if writer_entries == parsed_entries {
        Ok(())
    } else {
        Err(LocalityTestError::WriterWitnessMismatch)
    }
}

fn assert_typed_projection(fixture: &MutationFixture) -> Result<(), LocalityTestError> {
    let witness = ValidatedLocality::try_from(fixture.bytes.as_slice())?;
    let view = GenerationView::new(&fixture.root, &witness)?;
    assert_eq!(
        view.get(EntryKey::from(1)).map(|entry| entry.locality),
        Some(Locality::Promised(fixture.providers))
    );
    assert_eq!(
        view.get(EntryKey::from(2)).map(|entry| entry.locality),
        Some(Locality::Overlaid(RemoteBase::Present {
            generation: fixture.basis,
            object: fixture.remote,
        }))
    );
    Ok(())
}

fn reject_empty_provider(fixture: &MutationFixture) -> Result<(), LocalityTestError> {
    let mut bytes = fixture.bytes.clone();
    mutate(
        &mut bytes,
        fixture.provider_offset,
        size_of::<u64>(),
        FixtureCell::Provider,
    )?
    .fill(0);
    match rejection(&bytes, FixtureCell::Provider)? {
        LocalityError::EmptyProvider {
            ordinal: 0,
            observed: 0,
            ..
        } => Ok(()),
        observed => Err(wrong_rejection(FixtureCell::Provider, observed)),
    }
}

fn reject_wrong_authority(fixture: &MutationFixture) -> Result<(), LocalityTestError> {
    let mut bytes = fixture.bytes.clone();
    let authority = mutate(
        &mut bytes,
        fixture.domain_offset,
        1,
        FixtureCell::ContentAuthority,
    )?;
    let Some(authority) = authority.first_mut() else {
        return Err(LocalityTestError::MissingWireCell {
            cell: FixtureCell::ContentAuthority,
        });
    };
    *authority ^= u8::MAX;
    let observed_authority = *authority;
    match rejection(&bytes, FixtureCell::ContentAuthority)? {
        LocalityError::ContentDomain { expected, observed }
            if expected == ObjectDomain::CODE && observed == observed_authority =>
        {
            Ok(())
        }
        observed => Err(wrong_rejection(FixtureCell::ContentAuthority, observed)),
    }
}

fn reject_unknown_schema(mut fixture: MutationFixture) -> Result<(), LocalityTestError> {
    mutate(
        &mut fixture.bytes,
        fixture.descriptor_offset + LOCALITY_SCHEMA_OFFSET,
        size_of::<SchemaId>(),
        FixtureCell::Schema,
    )?
    .fill(u8::MAX);
    match rejection(&fixture.bytes, FixtureCell::Schema)? {
        LocalityError::PresentOverlaySchema { ordinal: 0, .. } => Ok(()),
        observed => Err(wrong_rejection(FixtureCell::Schema, observed)),
    }
}

fn rejection(bytes: &[u8], cell: FixtureCell) -> Result<LocalityError, LocalityTestError> {
    ValidatedLocality::<ObjectDomain>::try_from(bytes)
        .err()
        .ok_or(LocalityTestError::MissingWireCell { cell })
}

const fn wrong_rejection(cell: FixtureCell, observed: LocalityError) -> LocalityTestError {
    LocalityTestError::WrongMutationError { cell, observed }
}

fn wire_offset(bytes: &[u8], needle: &[u8], cell: FixtureCell) -> Result<usize, LocalityTestError> {
    bytes
        .windows(needle.len())
        .position(|candidate| candidate == needle)
        .ok_or(LocalityTestError::MissingWireCell { cell })
}

fn mutate(
    bytes: &mut [u8],
    offset: usize,
    width: usize,
    cell: FixtureCell,
) -> Result<&mut [u8], LocalityTestError> {
    bytes
        .get_mut(offset..offset + width)
        .ok_or(LocalityTestError::MissingWireCell { cell })
}
