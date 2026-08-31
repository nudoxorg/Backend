use std::{
    error::Error,
    fs, io,
    num::NonZeroUsize,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
};

use allocation_counter::{AllocationInfo, measure};
use nudox_durable_journal::{DurablePublisher, PublicationLimits, PublicationPaths};
use nudox_hydration::PlanScratch;
use nudox_hydration::{Projection, VerifiedGeneration, VerifiedGenerationFacts, demand, plan};
use nudox_id::{ContentId, ObjectDomain};
use nudox_object::ObjectRef;
use nudox_root::{
    ClosureScratch, GenerationRoot, GenerationView, ParsedLocality, PreparedLocality, RootEntry,
};
use nudox_schema::SchemaId;
use nudox_store_memory::{InsertOutcome, MemoryStore, StoreCapacity};

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

fn fixture_path() -> PathBuf {
    let ordinal = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "nudox-durable-publisher-public-{}-{ordinal}",
        std::process::id()
    ))
}

struct VerifiedFixture {
    root: GenerationRoot<ObjectDomain>,
    store: MemoryStore<ObjectDomain>,
}

type VerifiedFixtureGeneration<'store> = VerifiedGeneration<'store, ObjectDomain, Box<[u8]>>;

impl VerifiedFixture {
    fn new() -> Result<Self, Box<dyn Error>> {
        let root = GenerationRoot::new(Vec::from([
            RootEntry {
                key: 1_u64.into(),
                parent: None,
                object: object(1),
            },
            RootEntry {
                key: 2_u64.into(),
                parent: Some(1_u64.into()),
                object: object(2),
            },
            RootEntry {
                key: 3_u64.into(),
                parent: Some(1_u64.into()),
                object: object(3),
            },
        ]))?;
        let mut store = MemoryStore::new(StoreCapacity {
            bytes: 12_u64.into(),
            slots: 3_u32.into(),
        })?;
        for byte in [1_u8, 2, 3] {
            match store.insert_owned(object(byte), Box::from([byte; 4])) {
                Ok(InsertOutcome::Inserted) => {}
                Ok(InsertOutcome::AlreadyPresent) => {
                    return Err(io::Error::other("fixture object unexpectedly replayed").into());
                }
                Err(rejected) => {
                    return Err(
                        io::Error::other(format!("fixture object rejected: {rejected:?}")).into(),
                    );
                }
            }
        }
        Ok(Self { root, store })
    }

    fn verified(&self) -> Result<VerifiedFixtureGeneration<'_>, Box<dyn Error>> {
        let prepared = PreparedLocality::prepare(&self.root, &[])?;
        let mut locality_bytes = Vec::new();
        locality_bytes.try_reserve_exact(usize::from(prepared.required_bytes))?;
        locality_bytes.resize(usize::from(prepared.required_bytes), 0);
        prepared.write(&mut locality_bytes)?;
        let locality = ParsedLocality::try_from(locality_bytes.as_slice())?;
        let view = GenerationView::new(&self.root, &locality)?;
        let mut closure = ClosureScratch::new(self.root.len())?;
        let mut planning = PlanScratch::new(self.root.entry_count.into())?;
        let complete = plan(
            demand(&view, Projection::CompleteGeneration),
            &mut closure,
            &mut planning,
            |_| true,
        )?;
        Ok(complete.stage().verify_store(&self.store)?)
    }
}

fn object(byte: u8) -> ObjectRef<ObjectDomain> {
    ObjectRef {
        content: ContentId::from_canonical_bytes(&[byte; 4]),
        length: 4_u64.into(),
        schema: SchemaId::Object,
        kind: u16::from(byte).into(),
    }
}

#[test]
fn create_reopen_and_shutdown_are_independently_empty_before_submission()
-> Result<(), Box<dyn Error>> {
    let directory = fixture_path();
    fs::create_dir_all(&directory)?;
    let paths = PublicationPaths::in_directory(&directory);
    let limits = PublicationLimits::new(
        NonZeroUsize::new(2).ok_or_else(|| io::Error::other("nonzero queue capacity"))?,
        NonZeroUsize::new(2).ok_or_else(|| io::Error::other("nonzero group capacity"))?,
    )?;

    let publisher = DurablePublisher::create(&paths, limits)?;
    assert_eq!(publisher.published()?, None);
    publisher.shutdown()?;

    let reopened = DurablePublisher::reopen(&paths, limits)?;
    assert_eq!(reopened.published()?, None);
    reopened.shutdown()?;

    assert!(directory.join("journal").is_file());
    assert!(!directory.join("publication.fact").exists());
    assert!(!directory.join("publication.head").exists());
    fs::remove_dir_all(directory)?;
    Ok(())
}

#[test]
fn genuine_verified_generation_publishes_binds_and_reopens() -> Result<(), Box<dyn Error>> {
    let directory = fixture_path();
    fs::create_dir_all(&directory)?;
    let paths = PublicationPaths::in_directory(&directory);
    let limits = PublicationLimits::new(
        NonZeroUsize::new(2).ok_or_else(|| io::Error::other("nonzero queue capacity"))?,
        NonZeroUsize::new(2).ok_or_else(|| io::Error::other("nonzero group capacity"))?,
    )?;
    // The typed wait error retains its borrowed witness; leak this tiny fixture so a failing
    // test can return the complete `PublicationError` through its `Error` source chain.
    let fixture: &'static VerifiedFixture = Box::leak(Box::new(VerifiedFixture::new()?));
    let publisher = DurablePublisher::create(&paths, limits)?;
    let verified = fixture.verified()?;
    let pending = match publisher.try_publish(verified) {
        Ok(pending) => pending,
        Err(_) => return Err(io::Error::other("fixture submission was rejected").into()),
    };
    let published = pending.wait()?;
    let expected_facts = VerifiedGenerationFacts {
        pinned_root: published.pinned_root,
        dep_set: published.dep_set,
    };
    assert_eq!(*published, expected_facts);
    let publication = published.publication;
    assert_eq!(publisher.published()?, Some(publication));
    publisher.shutdown()?;

    let reopened = DurablePublisher::reopen(&paths, limits)?;
    assert_eq!(reopened.published()?, Some(publication));
    let verified = fixture.verified()?;
    let pending = match reopened.try_publish(verified) {
        Ok(pending) => pending,
        Err(_) => return Err(io::Error::other("duplicate submission was rejected").into()),
    };
    match pending.cancel() {
        Ok(generation) => assert_eq!(*generation, expected_facts),
        Err(nudox_durable_journal::CancelError::Completed {
            generation,
            publication: completed,
        }) => {
            assert_eq!(*generation, expected_facts);
            assert_eq!(*completed, publication);
        }
        Err(nudox_durable_journal::CancelError::Failed { .. })
        | Err(nudox_durable_journal::CancelError::OwnerLost { .. }) => {
            return Err(io::Error::other("duplicate cancellation did not reach a terminal").into());
        }
        Err(_) => {
            return Err(io::Error::other("duplicate cancellation had an unknown terminal").into());
        }
    }
    reopened.shutdown()?;
    fs::remove_dir_all(directory)?;
    Ok(())
}

#[test]
fn dropping_a_real_pending_witness_is_drained_before_shutdown() -> Result<(), Box<dyn Error>> {
    let directory = fixture_path();
    fs::create_dir_all(&directory)?;
    let paths = PublicationPaths::in_directory(&directory);
    let limits = PublicationLimits::new(
        NonZeroUsize::new(1).ok_or_else(|| io::Error::other("nonzero queue capacity"))?,
        NonZeroUsize::new(1).ok_or_else(|| io::Error::other("nonzero group capacity"))?,
    )?;
    let fixture: &'static VerifiedFixture = Box::leak(Box::new(VerifiedFixture::new()?));
    let publisher = DurablePublisher::create(&paths, limits)?;
    let verified = fixture.verified()?;
    let pending = match publisher.try_publish(verified) {
        Ok(pending) => pending,
        Err(_) => return Err(io::Error::other("fixture submission was rejected").into()),
    };
    drop(pending);
    publisher.shutdown()?;
    let reopened = DurablePublisher::reopen(&paths, limits)?;
    assert_eq!(reopened.published()?, None);
    reopened.shutdown()?;
    fs::remove_dir_all(directory)?;
    Ok(())
}

#[test]
fn warmed_public_admission_and_terminal_have_bounded_thread_local_allocations()
-> Result<(), Box<dyn Error>> {
    let directory = fixture_path();
    fs::create_dir_all(&directory)?;
    let paths = PublicationPaths::in_directory(&directory);
    let limits = PublicationLimits::new(
        NonZeroUsize::new(2).ok_or_else(|| io::Error::other("nonzero queue capacity"))?,
        NonZeroUsize::new(2).ok_or_else(|| io::Error::other("nonzero group capacity"))?,
    )?;
    let fixture: &'static VerifiedFixture = Box::leak(Box::new(VerifiedFixture::new()?));
    let publisher = DurablePublisher::create(&paths, limits)?;
    let verified = fixture.verified()?;
    let first = match publisher.try_publish(verified) {
        Ok(pending) => pending,
        Err(_) => return Err(io::Error::other("warm-up submission was rejected").into()),
    };
    let _published = first.wait()?;

    let verified = fixture.verified()?;
    let mut pending = None;
    let admission = measure(|| {
        pending = publisher.try_publish(verified).ok();
    });
    println!("warmed public admission allocation ledger: {admission:?}");
    let Some(pending) = pending else {
        return Err(io::Error::other("warmed submission was rejected").into());
    };
    assert_eq!(
        admission,
        AllocationInfo {
            count_total: 3,
            count_current: 3,
            count_max: 3,
            bytes_total: 752,
            bytes_current: 752,
            bytes_max: 752,
        }
    );

    // The allocation counter is thread-local. The owner emits the response on its own thread, so
    // waiting is intentionally outside this main-thread admission measurement. The warmed owner
    // unit test records its one 64-byte terminal node, while this assertion records the exact
    // caller-side lease and response-channel lifetime retained until wait/drop.
    let _terminal = pending.wait()?;
    let verified = fixture.verified()?;
    let dropped = match publisher.try_publish(verified) {
        Ok(pending) => pending,
        Err(_) => return Err(io::Error::other("drop-schedule submission was rejected").into()),
    };
    drop(dropped);
    publisher.shutdown()?;
    fs::remove_dir_all(directory)?;
    Ok(())
}
