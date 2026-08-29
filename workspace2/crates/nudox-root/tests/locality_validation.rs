//! Public scalar/SIMD equivalence and sparse-scan work laws.

use core::mem::size_of;

use nudox_id::{GenerationId, ObjectDomain};
use nudox_object::{ObjectKind, ObjectLength, ObjectRef, ProviderId, ProviderIdError, ProviderSet};
use nudox_root::{
    EntryKey, GenerationEntry, GenerationRoot, GenerationView, Locality, LocalityError,
    LocalityException, LocalityReadError, LocalityScanWork, LocalityValidator, NonResident,
    PreparedLocality, RootBuildError, RootEntry,
};
use nudox_schema::SchemaId;
use thiserror::Error;

const LONG_LANE_ROWS: u8 = 64;
const LOCALITY_HEADER_BYTES: usize = size_of::<GenerationId>() + size_of::<[u32; 4]>();
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
    #[error("validated locality changed during semantic scan")]
    Read(#[from] LocalityReadError),
    #[error("root did not issue a locality row for {key:?}")]
    MissingRow { key: EntryKey },
    #[error("canonical fixture did not contain its second row lane")]
    MissingSecondRow,
    #[error("{path:?} validation accepted duplicate canonical rows")]
    InvalidAccepted { path: ValidationPath },
}

#[derive(Debug)]
enum ValidationPath {
    Scalar,
    DetectedSimd,
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

fn object(row: u8) -> ObjectRef<ObjectDomain> {
    ObjectRef {
        content: [row; 32].into(),
        length: ObjectLength::from(u64::from(row) + 1),
        schema: SchemaId::Object,
        kind: ObjectKind::from(1),
    }
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

fn scan(view: &GenerationView<'_, '_, ObjectDomain>) -> Result<ScanEvidence, LocalityTestError> {
    let mut scan = view.measured_closure();
    let entries = scan.by_ref().collect::<Result<Vec<_>, _>>()?;
    Ok(ScanEvidence {
        entries,
        work: *scan,
    })
}

#[test]
fn detected_simd_preserves_scalar_semantics_and_linear_scan_work() -> Result<(), LocalityTestError>
{
    let fixture = fixture()?;
    let scalar = nudox_root::ValidatedLocality::<ObjectDomain>::try_from(fixture.bytes.as_slice())?;
    let accelerated = LocalityValidator::new().validate::<ObjectDomain>(&fixture.bytes)?;

    let scalar = scan(&GenerationView::new(&fixture.root, &scalar)?)?;
    let accelerated = scan(&GenerationView::new(&fixture.root, &accelerated)?)?;
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

    let scalar = nudox_root::ValidatedLocality::<ObjectDomain>::try_from(fixture.bytes.as_slice())
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
