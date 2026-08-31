use super::*;
use crate::publication::credit::{CreditPool, PendingLease};
use crate::{ArtifactName, DurablePublisher, PublicationLimits};
use allocation_counter::{AllocationInfo, measure};
use nudox_id::{ContentId, DependencySetDomain, GenerationId};
use std::{
    error::Error,
    fmt, fs, io,
    num::NonZeroUsize,
    path::PathBuf,
    sync::{
        OnceLock,
        atomic::AtomicBool,
        mpsc::{Receiver, RecvError, sync_channel},
    },
};

static NEXT_FIXTURE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

struct Fixture {
    directory: PathBuf,
}

impl Fixture {
    fn new(label: &str) -> io::Result<Self> {
        let ordinal = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir().join(format!(
            "nudox-publication-owner-{label}-{}-{ordinal}",
            std::process::id()
        ));
        fs::create_dir_all(&directory)?;
        Ok(Self { directory })
    }

    fn paths(&self) -> PublicationPaths {
        PublicationPaths::in_directory(&self.directory)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

type CommandParts = (Command, Receiver<OwnerOutcome>, Arc<CreditLease>);

fn command(pool: &Arc<CreditPool>, input: PublicationInput) -> io::Result<CommandParts> {
    let lease = CreditPool::reserve(pool)
        .ok_or_else(|| io::Error::other("bounded command lease unavailable"))?;
    let (response, receiver) = sync_channel(1);
    Ok((
        Command {
            input,
            lease: Arc::clone(&lease),
            response,
        },
        receiver,
        lease,
    ))
}

#[derive(Debug)]
enum TerminalError {
    Receive(RecvError),
    Failed {
        label: &'static str,
        source: PublicationFailure,
    },
    Cancelled {
        label: &'static str,
    },
    Published {
        label: &'static str,
    },
}

impl fmt::Display for TerminalError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Receive(source) => write!(formatter, "terminal receive failed: {source}"),
            Self::Failed { label, source } => write!(formatter, "{label} failed: {source}"),
            Self::Cancelled { label } => write!(formatter, "{label} was cancelled"),
            Self::Published { label } => write!(formatter, "{label} unexpectedly published"),
        }
    }
}

impl Error for TerminalError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Receive(source) => Some(source),
            Self::Failed { source, .. } => Some(source),
            Self::Cancelled { .. } | Self::Published { .. } => None,
        }
    }
}

fn published(
    response: Receiver<OwnerOutcome>,
    label: &'static str,
) -> Result<PublicationFacts, TerminalError> {
    match response.recv().map_err(TerminalError::Receive)? {
        OwnerOutcome::Published(facts) => Ok(facts),
        OwnerOutcome::Failed(source) => Err(TerminalError::Failed { label, source }),
        OwnerOutcome::Cancelled => Err(TerminalError::Cancelled { label }),
    }
}

fn nonzero(value: usize) -> io::Result<NonZeroUsize> {
    NonZeroUsize::new(value).ok_or_else(|| io::Error::other("test capacity must be nonzero"))
}

fn verified_input(root: u8, dep_set: u8) -> PublicationInput {
    let root = GenerationId::from_canonical_bytes(&[root]);
    let dep_set = ContentId::<DependencySetDomain>::from_canonical_bytes(&[dep_set]);
    PublicationInput::from_parts(*root, *dep_set)
}

fn write_fact(
    paths: &PublicationPaths,
    input: PublicationInput,
) -> Result<ReceiptFacts, Box<dyn Error>> {
    let mut journal = FileJournal::create(paths.journal())?;
    let receipt = journal.append(WorkflowEvent {
        version: WorkflowVersion::WAVE1,
        key: input.key,
        kind: EventKind::Requested,
    })?;
    let mut fact_bytes = [0; FACT_BYTES];
    persist_fact(paths, input, *receipt, &mut fact_bytes)?;
    Ok(*receipt)
}

fn write_fact_and_head(
    paths: &PublicationPaths,
    input: PublicationInput,
) -> Result<(), Box<dyn Error>> {
    let receipt = write_fact(paths, input)?;
    let mut fact_bytes = [0; FACT_BYTES];
    let fact =
        read_fact(&paths.fact())?.ok_or_else(|| io::Error::other("fact fixture disappeared"))?;
    fact_bytes.copy_from_slice(&fact.bytes);
    let mut head_bytes = [0; HEAD_BYTES];
    persist_head(paths, fact.identity, receipt, &mut head_bytes)?;
    Ok(())
}

fn state(pool: &Arc<CreditPool>) -> PublisherState {
    PublisherState {
        closed: AtomicBool::new(false),
        credits: Arc::clone(pool),
        published: OnceLock::new(),
    }
}

#[allow(clippy::too_many_arguments)]
fn process(
    paths: &PublicationPaths,
    journal: &mut FileJournal,
    group: &mut Vec<Command>,
    state: &PublisherState,
    current: &mut Option<StoredPublication>,
    pending: &mut Option<PendingJournal>,
    poison: &mut Option<FailureOwner>,
    storage: &mut OwnerStorage,
) {
    process_group(
        journal,
        group,
        state,
        current,
        pending,
        poison,
        OwnerBuffers {
            paths,
            frames: storage.frames.as_mut_slice(),
            fact_bytes: &mut storage.fact_bytes,
            head_bytes: &mut storage.head_bytes,
        },
    );
}

#[test]
fn grouped_owner_fanout_is_one_journal_record_and_binds_same_facts() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new("fanout")?;
    let paths = fixture.paths();
    let mut journal = FileJournal::create(paths.journal())?;
    let pool = CreditPool::new(2);
    let state = state(&pool);
    let mut storage = OwnerStorage::new(nonzero(2)?)?;
    let input = verified_input(7, 8);
    let (first, first_response, first_lease) = command(&pool, input)?;
    let (second, second_response, second_lease) = command(&pool, input)?;
    let mut group = vec![first, second];
    let mut current = None;
    let mut pending = None;
    let mut poison = None;
    let frame_pointer = storage.frames.bytes.as_ptr();
    let frame_capacity = storage.frames.bytes.capacity();
    let group_pointer = storage.group.as_ptr();
    let group_capacity = storage.group.capacity();

    process(
        &paths,
        &mut journal,
        &mut group,
        &state,
        &mut current,
        &mut pending,
        &mut poison,
        &mut storage,
    );

    let first_facts = published(first_response, "first command")?;
    let second_facts = published(second_response, "second command")?;
    assert_eq!(first_facts, second_facts);
    assert_eq!(
        journal.last_receipt().map(|receipt| *receipt.sequence),
        Some(0)
    );
    assert!(paths.fact().is_file());
    assert!(paths.head().is_file());
    assert_eq!(storage.frames.bytes.as_ptr(), frame_pointer);
    assert_eq!(storage.frames.bytes.capacity(), frame_capacity);
    assert_eq!(storage.group.as_ptr(), group_pointer);
    assert_eq!(storage.group.capacity(), group_capacity);
    drop(journal);
    let limits = PublicationLimits::new(nonzero(2)?, nonzero(2)?)?;
    let reopened = DurablePublisher::reopen(&paths, limits)?;
    assert_eq!(reopened.published()?, Some(first_facts));
    reopened.shutdown()?;
    drop(first_lease);
    drop(second_lease);
    assert!(CreditPool::reserve(&pool).is_some());
    Ok(())
}

#[test]
fn warmed_duplicate_group_reuses_owner_storage_without_heap_allocation()
-> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new("warm-group")?;
    let paths = fixture.paths();
    let mut journal = FileJournal::create(paths.journal())?;
    let pool = CreditPool::new(2);
    let state = state(&pool);
    let mut storage = OwnerStorage::new(nonzero(2)?)?;
    let input = verified_input(17, 18);
    let (first, first_response, first_lease) = command(&pool, input)?;
    let mut group = vec![first];
    let mut current = None;
    let mut pending = None;
    let mut poison = None;
    process(
        &paths,
        &mut journal,
        &mut group,
        &state,
        &mut current,
        &mut pending,
        &mut poison,
        &mut storage,
    );
    let _first = published(first_response, "warm-up command")?;
    drop(first_lease);

    let frame_pointer = storage.frames.bytes.as_ptr();
    let group_pointer = storage.group.as_ptr();
    let (second, second_response, second_lease) = command(&pool, input)?;
    group.push(second);
    // Initialize the measurement guard outside the warmed owner operation; this first-use
    // bookkeeping is not part of publication admission or group storage.
    let _ = measure(|| {});
    let allocations = measure(|| {
        process(
            &paths,
            &mut journal,
            &mut group,
            &state,
            &mut current,
            &mut pending,
            &mut poison,
            &mut storage,
        );
    });
    assert_eq!(
        allocations,
        AllocationInfo {
            // The owner/group path itself allocates nothing. std::sync::mpsc retains one
            // 64-byte terminal node while the pending receiver has not consumed its result.
            count_total: 1,
            count_current: 1,
            count_max: 1,
            bytes_total: 64,
            bytes_current: 64,
            bytes_max: 64,
        }
    );
    let _second = published(second_response, "warmed duplicate")?;
    assert_eq!(storage.frames.bytes.as_ptr(), frame_pointer);
    assert_eq!(storage.group.as_ptr(), group_pointer);
    assert_eq!(pool.high_water(), 1);
    drop(second_lease);
    Ok(())
}

#[test]
fn every_fact_and_head_short_prefix_is_rejected_by_independent_reopen() -> Result<(), Box<dyn Error>>
{
    let limits = PublicationLimits::new(nonzero(1)?, nonzero(1)?)?;
    let fact_input = verified_input(19, 20);
    for prefix in 0..FACT_BYTES {
        let fixture = Fixture::new("fact-prefix")?;
        let paths = fixture.paths();
        write_fact(&paths, fact_input)?;
        fs::OpenOptions::new()
            .write(true)
            .open(paths.fact())?
            .set_len(prefix as u64)?;
        match DurablePublisher::reopen(&paths, limits) {
            Err(PublicationOpenError::Length {
                artifact: ArtifactName::Fact,
                expected,
                observed,
            }) => {
                assert_eq!(expected, FACT_BYTES);
                assert_eq!(observed, prefix as u64);
            }
            Err(error) => {
                return Err(io::Error::other(format!(
                    "fact prefix {prefix} returned the wrong reopen error: {error:?}"
                ))
                .into());
            }
            Ok(publisher) => {
                let _ = publisher.shutdown();
                return Err(io::Error::other(format!(
                    "fact prefix {prefix} unexpectedly reopened"
                ))
                .into());
            }
        }
    }

    let fact_fixture = Fixture::new("fact-checksum")?;
    let fact_paths = fact_fixture.paths();
    write_fact(&fact_paths, fact_input)?;
    let mut fact_bytes = fs::read(fact_paths.fact())?;
    fact_bytes[12] ^= 1;
    fs::write(fact_paths.fact(), fact_bytes)?;
    match DurablePublisher::reopen(&fact_paths, limits) {
        Err(PublicationOpenError::FactChecksum { expected, observed }) => {
            assert_ne!(expected, observed);
        }
        Err(error) => {
            return Err(io::Error::other(format!(
                "changed fact returned the wrong reopen error: {error:?}"
            ))
            .into());
        }
        Ok(publisher) => {
            let _ = publisher.shutdown();
            return Err(io::Error::other("changed fact unexpectedly reopened").into());
        }
    }

    let head_input = verified_input(21, 22);
    for prefix in 0..HEAD_BYTES {
        let fixture = Fixture::new("head-prefix")?;
        let paths = fixture.paths();
        write_fact_and_head(&paths, head_input)?;
        fs::OpenOptions::new()
            .write(true)
            .open(paths.head())?
            .set_len(prefix as u64)?;
        match DurablePublisher::reopen(&paths, limits) {
            Err(PublicationOpenError::Length {
                artifact: ArtifactName::Head,
                expected,
                observed,
            }) => {
                assert_eq!(expected, HEAD_BYTES);
                assert_eq!(observed, prefix as u64);
            }
            Err(error) => {
                return Err(io::Error::other(format!(
                    "head prefix {prefix} returned the wrong reopen error: {error:?}"
                ))
                .into());
            }
            Ok(publisher) => {
                let _ = publisher.shutdown();
                return Err(io::Error::other(format!(
                    "head prefix {prefix} unexpectedly reopened"
                ))
                .into());
            }
        }
    }

    let head_fixture = Fixture::new("head-checksum")?;
    let head_paths = head_fixture.paths();
    write_fact_and_head(&head_paths, head_input)?;
    let mut head_bytes = fs::read(head_paths.head())?;
    head_bytes[12] ^= 1;
    fs::write(head_paths.head(), head_bytes)?;
    match DurablePublisher::reopen(&head_paths, limits) {
        Err(PublicationOpenError::HeadChecksum { expected, observed }) => {
            assert_ne!(expected, observed);
        }
        Err(error) => {
            return Err(io::Error::other(format!(
                "changed head returned the wrong reopen error: {error:?}"
            ))
            .into());
        }
        Ok(publisher) => {
            let _ = publisher.shutdown();
            return Err(io::Error::other("changed head unexpectedly reopened").into());
        }
    }
    Ok(())
}

#[test]
fn current_head_conflict_retains_stored_root_and_dep_without_new_bytes()
-> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new("conflict")?;
    let paths = fixture.paths();
    let mut journal = FileJournal::create(paths.journal())?;
    let pool = CreditPool::new(2);
    let state = state(&pool);
    let mut storage = OwnerStorage::new(nonzero(2)?)?;
    let input = verified_input(9, 10);
    let (first, first_response, first_lease) = command(&pool, input)?;
    let mut group = vec![first];
    let mut current = None;
    let mut pending = None;
    let mut poison = None;
    process(
        &paths,
        &mut journal,
        &mut group,
        &state,
        &mut current,
        &mut pending,
        &mut poison,
        &mut storage,
    );
    let publication = published(first_response, "first command")?;
    drop(first_lease);
    let journal_bytes = fs::read(paths.journal())?;
    let fact_bytes = fs::read(paths.fact())?;
    let head_bytes = fs::read(paths.head())?;
    let conflicting = verified_input(11, 12);
    let (command, response, lease) = command(&pool, conflicting)?;
    group.push(command);
    process(
        &paths,
        &mut journal,
        &mut group,
        &state,
        &mut current,
        &mut pending,
        &mut poison,
        &mut storage,
    );
    match response.recv()? {
        OwnerOutcome::Failed(PublicationFailure::Conflict { facts }) => {
            assert_eq!(facts.expected_root, conflicting.root);
            assert_eq!(facts.expected_dep_set, conflicting.dep_set);
            assert_eq!(facts.observed_root, input.root);
            assert_eq!(facts.observed_dep_set, input.dep_set);
        }
        OwnerOutcome::Failed(source) => {
            return Err(Box::new(TerminalError::Failed {
                label: "conflict command",
                source,
            }));
        }
        OwnerOutcome::Published(_) => {
            return Err(Box::new(TerminalError::Published {
                label: "conflict command",
            }));
        }
        OwnerOutcome::Cancelled => {
            return Err(Box::new(TerminalError::Cancelled {
                label: "conflict command",
            }));
        }
    }
    assert_eq!(fs::read(paths.journal())?, journal_bytes);
    assert_eq!(fs::read(paths.fact())?, fact_bytes);
    assert_eq!(fs::read(paths.head())?, head_bytes);
    assert_eq!(state.published.get(), Some(&publication));
    drop(lease);
    assert!(CreditPool::reserve(&pool).is_some());
    Ok(())
}

#[test]
fn dropped_pending_guard_cancels_queued_command_without_a_journal_effect()
-> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new("cancel")?;
    let paths = fixture.paths();
    let mut journal = FileJournal::create(paths.journal())?;
    let pool = CreditPool::new(1);
    let state = state(&pool);
    let mut storage = OwnerStorage::new(nonzero(1)?)?;
    let input = verified_input(13, 14);
    let (command, response, lease) = command(&pool, input)?;
    let pending_guard = PendingLease(Arc::clone(&lease));
    drop(pending_guard);
    let mut group = vec![command];
    let mut current = None;
    let mut pending = None;
    let mut poison = None;
    process(
        &paths,
        &mut journal,
        &mut group,
        &state,
        &mut current,
        &mut pending,
        &mut poison,
        &mut storage,
    );
    match response.recv()? {
        OwnerOutcome::Cancelled => {}
        OwnerOutcome::Failed(source) => {
            return Err(Box::new(TerminalError::Failed {
                label: "cancelled command",
                source,
            }));
        }
        OwnerOutcome::Published(_) => {
            return Err(Box::new(TerminalError::Published {
                label: "cancelled command",
            }));
        }
    }
    assert!(journal.last_receipt().is_none());
    assert!(!paths.fact().exists());
    assert!(!paths.head().exists());
    drop(lease);
    assert!(CreditPool::reserve(&pool).is_some());
    Ok(())
}

#[test]
fn receiver_loss_still_completes_the_command_lease() -> Result<(), Box<dyn Error>> {
    let fixture = Fixture::new("receiver-loss")?;
    let paths = fixture.paths();
    let mut journal = FileJournal::create(paths.journal())?;
    let pool = CreditPool::new(1);
    let state = state(&pool);
    let mut storage = OwnerStorage::new(nonzero(1)?)?;
    let input = verified_input(15, 16);
    let (command, response, lease) = command(&pool, input)?;
    drop(response);
    let mut group = vec![command];
    let mut current = None;
    let mut pending = None;
    let mut poison = None;
    process(
        &paths,
        &mut journal,
        &mut group,
        &state,
        &mut current,
        &mut pending,
        &mut poison,
        &mut storage,
    );
    drop(lease);
    assert!(CreditPool::reserve(&pool).is_some());
    assert!(current.is_some());
    Ok(())
}

#[test]
fn owner_storage_preallocates_exact_reusable_frame_and_group_bounds() -> Result<(), Box<dyn Error>>
{
    let group_capacity = nonzero(3)?;
    let storage = OwnerStorage::new(group_capacity)?;
    let frame_bytes = 3 * crate::JOURNAL_FRAME_BYTES;
    assert_eq!(storage.frames.bytes.len(), frame_bytes);
    assert_eq!(storage.frames.bytes.capacity(), frame_bytes);
    assert_eq!(storage.group.capacity(), 3);
    assert!(storage.group.is_empty());
    Ok(())
}
