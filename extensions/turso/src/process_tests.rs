//! Two-process contracts for shared access.
//!
//! Each test re-executes this test binary as one or more peer processes that
//! open the same database file, so the contracts are exercised through the
//! operating system's real cross-process locks, shared memory and process
//! death rather than through threads that share one address space.

#![allow(clippy::expect_used, clippy::panic)]

use crate::process_harness::{self, Database, Peer};
use crate::{
    AuthorityNamespace, ProjectionError, ProjectionGraphSeed, ProjectionSeed, SourceObservation,
    SourceObservationValue, TursoAuthority, TursoProjection,
};
use std::time::Instant;

const TEST_PREFIX: &str = "process_tests::";

fn namespace(package: &str) -> AuthorityNamespace {
    AuthorityNamespace::package_metadata(package, "registry:crates-io", "main", "stable")
        .expect("namespace")
}

fn observation(package: &str, count: u64) -> SourceObservation {
    SourceObservation::new(
        namespace(package),
        Some([11; 32]),
        1_000 + count,
        SourceObservationValue::KnownCount(count),
    )
    .expect("observation")
}

/// One command a parent sends to a peer, as a line of standard input.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Command {
    /// Record an observation of `count` for `package`.
    Write { package: String, count: u64 },
    /// Read the latest observation for `package`.
    Read { package: String },
    /// Begin a writer transaction and keep it open without committing.
    HoldWriter,
}

impl Command {
    fn encode(&self) -> String {
        match self {
            Self::Write { package, count } => format!("WRITE {package} {count}"),
            Self::Read { package } => format!("READ {package}"),
            Self::HoldWriter => "HOLD".to_owned(),
        }
    }

    fn decode(line: &str) -> Option<Self> {
        let mut words = line.split_whitespace();
        match words.next()? {
            "WRITE" => Some(Self::Write {
                package: words.next()?.to_owned(),
                count: words.next()?.parse().ok()?,
            }),
            "READ" => Some(Self::Read {
                package: words.next()?.to_owned(),
            }),
            "HOLD" => Some(Self::HoldWriter),
            _ => None,
        }
    }
}

/// A peer's answer to one command.
#[derive(Clone, Debug, Eq, PartialEq)]
enum Report {
    Opened,
    Wrote { sequence: u64 },
    Seen { sequence: u64, count: u64 },
    SeenNothing,
    Holding,
    Failed(String),
}

impl Report {
    fn encode(&self) -> String {
        match self {
            Self::Opened => "OPENED".to_owned(),
            Self::Wrote { sequence } => format!("WROTE {sequence}"),
            Self::Seen { sequence, count } => format!("SEEN {sequence} {count}"),
            Self::SeenNothing => "SEEN-NOTHING".to_owned(),
            Self::Holding => "HOLDING".to_owned(),
            Self::Failed(reason) => format!("FAILED {reason}"),
        }
    }

    fn decode(line: &str) -> Self {
        let mut words = line.split_whitespace();
        let number = |word: Option<&str>| word.and_then(|word| word.parse().ok()).expect("number");
        match words.next() {
            Some("OPENED") => Self::Opened,
            Some("WROTE") => Self::Wrote {
                sequence: number(words.next()),
            },
            Some("SEEN") => Self::Seen {
                sequence: number(words.next()),
                count: number(words.next()),
            },
            Some("SEEN-NOTHING") => Self::SeenNothing,
            Some("HOLDING") => Self::Holding,
            Some("FAILED") => Self::Failed(line.trim_start_matches("FAILED ").to_owned()),
            _ => panic!("unrecognised peer report {line:?}"),
        }
    }
}

fn ask(peer: &mut Peer, command: &Command) -> Report {
    Report::decode(&peer.ask(&command.encode()))
}

fn write(peer: &mut Peer, package: &str, count: u64) -> Report {
    ask(
        peer,
        &Command::Write {
            package: package.to_owned(),
            count,
        },
    )
}

fn spawn(test: &str, database: &Database) -> Peer {
    let (peer, opened) = Peer::spawn(&format!("{TEST_PREFIX}{test}"), &database.0);
    assert_eq!(
        Report::decode(&opened),
        Report::Opened,
        "both processes hold the database open together"
    );
    peer
}

fn projection_peer(database: std::path::PathBuf) {
    let view = crate::tests::replacement_view();
    process_harness::report("READY");
    if process_harness::commands().next().as_deref() != Some("GO") {
        process_harness::report("FAILED missing GO");
        return;
    }
    let mut projection = match futures_executor::block_on(TursoProjection::open(&database)) {
        Ok(projection) => projection,
        Err(ProjectionError::NeedsSeed) => {
            match futures_executor::block_on(TursoProjection::seed_if_empty(
                &database,
                ProjectionSeed::new(&view, ProjectionGraphSeed::Unavailable),
            )) {
                Ok(projection) => projection,
                Err(ProjectionError::SeedConflict) => {
                    process_harness::report("SEED-CONFLICT");
                    return;
                }
                Err(ProjectionError::AlreadySeeded) => {
                    process_harness::report("ALREADY-SEEDED");
                    return;
                }
                Err(ProjectionError::Schema { found }) => {
                    process_harness::report(&format!("SCHEMA-REFUSED {found}"));
                    return;
                }
                Err(error) => {
                    process_harness::report(&format!("FAILED seed: {error}"));
                    return;
                }
            }
        }
        Err(ProjectionError::Schema { found }) => {
            process_harness::report(&format!("SCHEMA-REFUSED {found}"));
            return;
        }
        Err(error) => {
            process_harness::report(&format!("FAILED open: {error}"));
            return;
        }
    };
    match futures_executor::block_on(projection.lookup_label("generation-target", 4)) {
        Ok(rows) if rows.root == *view.root().as_bytes() && rows.ids.len() == 1 => {
            process_harness::report("OPENED");
        }
        Ok(_) => process_harness::report("FAILED selected generation had wrong seed rows"),
        Err(error) => process_harness::report(&format!("FAILED lookup: {error}")),
    }
    drop(projection);
}

#[test]
fn two_process_first_open_publishes_exactly_one_seeded_generation() {
    let Some(database_path) = process_harness::peer_database() else {
        let database = Database::new("projection-first-open");
        let (mut peer, ready) = Peer::spawn_with_env(
            &format!("{TEST_PREFIX}two_process_first_open_publishes_exactly_one_seeded_generation"),
            &database.0,
            "BACKEND_TURSO_TEST_HOLD_AFTER_STAGE",
            "1",
        );
        assert_eq!(ready, "READY");
        assert_eq!(peer.ask("GO"), "STAGED");

        let view = crate::tests::replacement_view();
        let projection = futures_executor::block_on(TursoProjection::seed_if_empty(
            &database.0,
            ProjectionSeed::new(&view, ProjectionGraphSeed::Unavailable),
        ))
        .expect("local opener publishes while the peer has a complete private stage");
        assert_eq!(
            peer.ask("GO"),
            "SEED-CONFLICT",
            "the losing stager must reacquire its source instead of adopting the winner"
        );
        let namespace =
            crate::projection_namespace::ProjectionNamespace::open_existing(&database.0)
                .expect("selected namespace");
        let selected = namespace.selected().expect("selector").expect("generation");
        assert_eq!(selected.generation, projection.generation);
        assert_eq!(
            namespace
                .generation_count_for_test()
                .expect("retained generation count"),
            1,
            "the losing initial stage must be cleaned through its exact receipt"
        );
        drop(projection);
        assert!(peer.release());
        return;
    };

    projection_peer(database_path);
}

#[test]
fn another_process_refuses_an_unsupported_selected_schema_without_mutation() {
    let Some(peer_path) = process_harness::peer_database() else {
        let database = Database::new("projection-replacement");
        futures_executor::block_on(async {
            let old_view = crate::tests::fallback_seed_view();
            let old = TursoProjection::seed_if_empty(
                &database.0,
                ProjectionSeed::new(&old_view, ProjectionGraphSeed::Unavailable),
            )
            .await
            .expect("initial selected generation");
            let old_generation = old.generation;
            old.connection
                .execute(
                    "UPDATE backend_projection_meta SET schema_version=?1 WHERE singleton=1",
                    [crate::schema::SCHEMA_VERSION - 1],
                )
                .await
                .expect("mark old schema");
            crate::projection_namespace::ProjectionNamespace::open_existing(&database.0)
                .expect("namespace")
                .set_selected_schema_for_test(crate::schema::SCHEMA_VERSION - 1)
                .expect("mark selected schema old");
            let namespace =
                crate::projection_namespace::ProjectionNamespace::open_existing(&database.0)
                    .expect("namespace after marking old");
            let selected_before = namespace.selected().expect("old selector");
            let generation_count_before = namespace
                .generation_count_for_test()
                .expect("generation inventory before refusal");

            let (mut peer, ready) = Peer::spawn(
                &format!(
                    "{TEST_PREFIX}another_process_refuses_an_unsupported_selected_schema_without_mutation"
                ),
                &database.0,
            );
            assert_eq!(ready, "READY");
            assert_eq!(
                peer.ask("GO"),
                format!("SCHEMA-REFUSED {}", crate::schema::SCHEMA_VERSION - 1)
            );
            let namespace =
                crate::projection_namespace::ProjectionNamespace::open_existing(&database.0)
                    .expect("reopened namespace");
            let selected = namespace
                .selected()
                .expect("selector")
                .expect("old generation");
            assert_eq!(selected.generation, old_generation);
            assert_eq!(selected.schema_version, crate::schema::SCHEMA_VERSION - 1);
            assert_eq!(Some(selected), selected_before);
            assert_eq!(
                namespace
                    .generation_count_for_test()
                    .expect("retained generation count"),
                generation_count_before,
                "unsupported schemas are refused before a replacement stage is created"
            );
            assert!(matches!(
                old.lookup_label("generation-target", 4).await,
                Err(crate::ProjectionError::SupersededGeneration)
            ));
            drop(old);
            assert!(matches!(
                TursoProjection::open(&database.0).await,
                Err(crate::ProjectionError::Schema { found })
                    if found == crate::schema::SCHEMA_VERSION - 1
            ));
            assert!(peer.release());
        });
        return;
    };

    projection_peer(peer_path);
}

#[test]
fn killed_private_stager_never_becomes_selected_and_the_next_seed_survives() {
    let Some(peer_path) = process_harness::peer_database() else {
        let database = Database::new("projection-killed-stage");
        let (peer, staged) = Peer::spawn_with_env(
            &format!(
                "{TEST_PREFIX}killed_private_stager_never_becomes_selected_and_the_next_seed_survives"
            ),
            &database.0,
            "BACKEND_TURSO_TEST_HOLD_AFTER_STAGE",
            "1",
        );
        assert_eq!(staged, "STAGED");
        peer.kill();
        assert_eq!(
            crate::projection_namespace::ProjectionNamespace::open_existing(&database.0)
                .expect("namespace after killed stage")
                .selected()
                .expect("selector after killed stage"),
            None,
            "a fully populated private directory is still not selected"
        );

        let view = crate::tests::replacement_view();
        let projection = futures_executor::block_on(TursoProjection::seed_if_empty(
            &database.0,
            ProjectionSeed::new(&view, ProjectionGraphSeed::Unavailable),
        ))
        .expect("publish a fresh complete generation after the crash");
        assert_eq!(projection.generation.get(), 2);
        assert_eq!(
            projection
                .namespace
                .generation_count_for_test()
                .expect("count retained crash stage and selected generation"),
            2,
            "the unselected crashed directory remains reserved without cleanup authority"
        );
        let rows = futures_executor::block_on(projection.lookup_label("generation-target", 4))
            .expect("surviving generation query");
        assert_eq!(rows.root, *view.root().as_bytes());
        drop(projection);
        return;
    };

    let view = crate::tests::replacement_view();
    let result = futures_executor::block_on(TursoProjection::seed_if_empty(
        &peer_path,
        ProjectionSeed::new(&view, ProjectionGraphSeed::Unavailable),
    ));
    if let Err(error) = result {
        process_harness::report(&format!("FAILED open: {error}"));
    } else {
        process_harness::report("UNEXPECTEDLY-PUBLISHED");
    }
}

#[test]
fn crash_after_selector_rename_keeps_the_committed_generation_reopenable() {
    let Some(peer_path) = process_harness::peer_database() else {
        let database = Database::new("projection-selector-rename-crash");
        let (peer, renamed) = Peer::spawn_with_env(
            &format!(
                "{TEST_PREFIX}crash_after_selector_rename_keeps_the_committed_generation_reopenable"
            ),
            &database.0,
            "BACKEND_TURSO_TEST_HOLD_AFTER_SELECTOR_RENAME",
            "1",
        );
        assert_eq!(renamed, "SELECTOR-RENAMED");
        peer.kill();

        let selected = crate::projection_namespace::ProjectionNamespace::open_existing(&database.0)
            .expect("namespace after selector rename")
            .selected()
            .expect("selector after writer crash")
            .expect("renamed selector remains present");
        assert_eq!(selected.generation.get(), 1);
        let projection = futures_executor::block_on(TursoProjection::open(&database.0))
            .expect("cold open follows the committed selector");
        let view = crate::tests::replacement_view();
        let rows = futures_executor::block_on(projection.lookup_label("generation-target", 4))
            .expect("read completely seeded generation after crash");
        assert_eq!(rows.root, *view.root().as_bytes());
        assert_eq!(rows.ids.len(), 1);
        drop(projection);
        return;
    };

    let view = crate::tests::replacement_view();
    let result = futures_executor::block_on(TursoProjection::seed_if_empty(
        &peer_path,
        ProjectionSeed::new(&view, ProjectionGraphSeed::Unavailable),
    ));
    process_harness::report(&format!("UNEXPECTEDLY-RETURNED {result:?}"));
}

/// The peer half of a test: returns `false` in the parent. In a peer it opens
/// the database, then serves commands until the parent closes its input.
fn run_as_peer() -> bool {
    let Some(database) = process_harness::peer_database() else {
        return false;
    };
    futures_executor::block_on(async {
        let mut authority = match TursoAuthority::open(&database).await {
            Ok(authority) => authority,
            Err(error) => {
                process_harness::report(&Report::Failed(format!("open: {error}")).encode());
                return;
            }
        };
        process_harness::report(&Report::Opened.encode());
        for line in process_harness::commands() {
            let report = match Command::decode(&line) {
                Some(Command::Write { package, count }) => match authority
                    .record_source_observation(observation(&package, count))
                    .await
                {
                    Ok(receipt) => Report::Wrote {
                        sequence: receipt.sequence(),
                    },
                    Err(error) => Report::Failed(format!("write: {error}")),
                },
                Some(Command::Read { package }) => {
                    match authority
                        .latest_source_observation(&namespace(&package))
                        .await
                    {
                        Ok(Some(receipt)) => match receipt.observation().value() {
                            SourceObservationValue::KnownCount(count) => Report::Seen {
                                sequence: receipt.sequence(),
                                count: *count,
                            },
                            other => Report::Failed(format!("unexpected value {other:?}")),
                        },
                        Ok(None) => Report::SeenNothing,
                        Err(error) => Report::Failed(format!("read: {error}")),
                    }
                }
                Some(Command::HoldWriter) => {
                    match authority
                        .raw_connection()
                        .execute("BEGIN IMMEDIATE", ())
                        .await
                    {
                        Ok(_) => Report::Holding,
                        Err(error) => Report::Failed(format!("hold: {error}")),
                    }
                }
                None => Report::Failed(format!("unknown command {line:?}")),
            };
            process_harness::report(&report.encode());
        }
        drop(authority);
    });
    true
}

#[test]
fn a_second_process_opens_the_database_and_commits_are_visible_both_ways() {
    if run_as_peer() {
        return;
    }
    let database = Database::new("visible");
    futures_executor::block_on(async {
        let mut parent = TursoAuthority::open(&database.0)
            .await
            .expect("parent opens");
        let mut peer = spawn(
            "a_second_process_opens_the_database_and_commits_are_visible_both_ways",
            &database,
        );

        // The peer's commit is visible to the parent while both stay open.
        assert_eq!(
            write(&mut peer, "from-peer", 7),
            Report::Wrote { sequence: 1 }
        );
        let seen = parent
            .latest_source_observation(&namespace("from-peer"))
            .await
            .expect("parent reads")
            .expect("peer's committed observation");
        assert_eq!(seen.sequence(), 1);
        assert_eq!(
            seen.observation().value(),
            &SourceObservationValue::KnownCount(7)
        );

        // And the parent's commit is visible to the peer.
        parent
            .record_source_observation(observation("from-parent", 9))
            .await
            .expect("parent writes");
        assert_eq!(
            ask(
                &mut peer,
                &Command::Read {
                    package: "from-parent".to_owned()
                }
            ),
            Report::Seen {
                sequence: 1,
                count: 9
            }
        );

        // Sequences stay per-namespace and monotonic across both writers.
        assert_eq!(
            write(&mut peer, "from-parent", 10),
            Report::Wrote { sequence: 2 }
        );
        assert_eq!(
            parent
                .record_source_observation(observation("from-parent", 11))
                .await
                .expect("parent writes again")
                .sequence(),
            3
        );
        assert!(peer.release(), "the peer exits cleanly");
        drop(parent);
    });
}

#[test]
fn concurrent_writers_in_separate_processes_lose_no_update() {
    if run_as_peer() {
        return;
    }
    const PEERS: u64 = 3;
    const WRITES: u64 = 12;
    let database = Database::new("storm");
    futures_executor::block_on(async {
        let mut parent = TursoAuthority::open(&database.0)
            .await
            .expect("parent opens");
        let peers = (0..PEERS)
            .map(|_| {
                spawn(
                    "concurrent_writers_in_separate_processes_lose_no_update",
                    &database,
                )
            })
            .collect::<Vec<_>>();

        // Every process hammers one namespace at once: peers from their own
        // threads, the parent from this one.
        let workers = peers
            .into_iter()
            .map(|mut peer| {
                std::thread::spawn(move || {
                    let mut sequences = Vec::new();
                    for count in 0..WRITES {
                        match write(&mut peer, "contended", count) {
                            Report::Wrote { sequence } => sequences.push(sequence),
                            other => panic!("peer write failed: {other:?}"),
                        }
                    }
                    assert!(peer.release());
                    sequences
                })
            })
            .collect::<Vec<_>>();
        let mut sequences = Vec::new();
        for count in 0..WRITES {
            let receipt = parent
                .record_source_observation(observation("contended", count))
                .await
                .expect("parent write");
            sequences.push(receipt.sequence());
        }
        for worker in workers {
            sequences.extend(worker.join().expect("peer thread"));
        }

        // No two writers were given the same sequence and none was skipped:
        // every committed write holds exactly one place in the history.
        sequences.sort_unstable();
        let total = (PEERS + 1) * WRITES;
        assert_eq!(sequences, (1..=total).collect::<Vec<_>>());
        let latest = parent
            .latest_source_observation(&namespace("contended"))
            .await
            .expect("final read")
            .expect("observations exist");
        assert_eq!(latest.sequence(), total);
    });
}

#[test]
fn a_killed_writer_does_not_wedge_the_database_and_its_commits_survive() {
    if run_as_peer() {
        return;
    }
    let database = Database::new("killed");
    futures_executor::block_on(async {
        let mut parent = TursoAuthority::open(&database.0)
            .await
            .expect("parent opens");
        let mut peer = spawn(
            "a_killed_writer_does_not_wedge_the_database_and_its_commits_survive",
            &database,
        );
        assert_eq!(
            write(&mut peer, "committed-by-peer", 5),
            Report::Wrote { sequence: 1 }
        );
        // The peer now owns the writer lane with a transaction it will never
        // finish, then dies without releasing anything.
        assert_eq!(ask(&mut peer, &Command::HoldWriter), Report::Holding);
        peer.kill();

        // The survivor must be able to write well inside the busy timeout: the
        // dead owner is recognised by process liveness, not waited out.
        let started = Instant::now();
        let receipt = parent
            .record_source_observation(observation("after-crash", 1))
            .await
            .expect("the survivor writes after the writer died");
        let waited = started.elapsed();
        assert_eq!(receipt.sequence(), 1);
        assert!(
            waited < crate::connection::BUSY_TIMEOUT,
            "recovering from a dead writer took {waited:?}"
        );
        // The peer's committed write survived its death; its open transaction
        // left nothing behind.
        let kept = parent
            .latest_source_observation(&namespace("committed-by-peer"))
            .await
            .expect("read after crash")
            .expect("committed before death");
        assert_eq!(
            kept.observation().value(),
            &SourceObservationValue::KnownCount(5)
        );
        drop(parent);

        // A cold reopen after the crash agrees.
        let reopened = TursoAuthority::open(&database.0)
            .await
            .expect("cold reopen");
        assert!(
            reopened
                .latest_source_observation(&namespace("committed-by-peer"))
                .await
                .expect("cold read")
                .is_some()
        );
    });
}
