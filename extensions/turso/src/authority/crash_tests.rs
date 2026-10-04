//! Process death at every step of a publication.
//!
//! A publication is a sequence of durable steps: the attempt is begun, the
//! candidate generation is selected (one transaction that also consumes the
//! attempt and marks every projection lagging), each projection is marked
//! current, and finally the client is acknowledged. The owner can die between
//! any two of them. These tests run the publication in a real peer process,
//! end that process at an exact step, and then ask a fresh process what the
//! authority says happened. The answer must come from the authority alone:
//! never from guessing success because a package is present, and never from
//! blindly submitting the attempt again.

#![allow(clippy::expect_used, clippy::panic)]

use super::tests::{AcceptVerifiedClosure, candidate, namespace, observation};
use super::*;
use crate::process_harness::{self, Database, Peer};

const TEST_PREFIX: &str = "authority::crash_tests::";

/// The exact step after which the publishing process is ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DeathPoint {
    /// The attempt exists; nothing was selected.
    AfterBegin,
    /// The selection transaction committed; no projection was updated.
    AfterSelect,
    /// Only the catalog projection was marked current.
    AfterCatalogProjection,
    /// Every projection was marked current; the client was not acknowledged.
    AfterAllProjections,
    /// The client was acknowledged; the owner then died.
    AfterAck,
}

impl DeathPoint {
    const ALL: [Self; 5] = [
        Self::AfterBegin,
        Self::AfterSelect,
        Self::AfterCatalogProjection,
        Self::AfterAllProjections,
        Self::AfterAck,
    ];

    fn token(self) -> &'static str {
        match self {
            Self::AfterBegin => "after-begin",
            Self::AfterSelect => "after-select",
            Self::AfterCatalogProjection => "after-catalog-projection",
            Self::AfterAllProjections => "after-all-projections",
            Self::AfterAck => "after-ack",
        }
    }

    fn parse(token: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|point| point.token() == token)
    }

    /// The independent oracle: what the authority must say once the owner has
    /// died at this point.
    const fn expected(self) -> Expected {
        match self {
            Self::AfterBegin => Expected {
                published: false,
                projections_current: [false, false, false],
            },
            Self::AfterSelect => Expected {
                published: true,
                projections_current: [false, false, false],
            },
            Self::AfterCatalogProjection => Expected {
                published: true,
                projections_current: [true, false, false],
            },
            Self::AfterAllProjections | Self::AfterAck => Expected {
                published: true,
                projections_current: [true, true, true],
            },
        }
    }
}

/// What must be true of the authority after a death at some point.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct Expected {
    published: bool,
    /// Catalog, graph and lexical, in that order.
    projections_current: [bool; 3],
}

const PROJECTIONS: [ProjectionKind; 3] = [
    ProjectionKind::Catalog,
    ProjectionKind::Graph,
    ProjectionKind::Lexical,
];

/// The tag from which the published candidate's identities derive.
const CANDIDATE_TAG: u8 = 0x50;
const INPUT: u8 = 0x4F;

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

fn unhex<const N: usize>(text: &str) -> [u8; N] {
    let mut bytes = [0; N];
    assert_eq!(text.len(), N * 2, "hex width");
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).expect("hex digit");
    }
    bytes
}

/// What the dying peer tells its parent before it is ended: the recovery
/// claim it held, and the generation it saw if it got that far.
struct Dying {
    claim: CandidateAttemptRecoveryClaim,
    seen: Option<SeenGeneration>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct SeenGeneration {
    generation: u64,
    candidate: AuthorityHash,
    root: AuthorityHash,
}

impl Dying {
    fn encode(&self) -> String {
        let claim = &self.claim;
        let base_root = claim
            .base_root
            .map_or_else(|| "-".to_owned(), |root| hex(&root));
        let seen = self.seen.as_ref().map_or_else(
            || "-".to_owned(),
            |seen| {
                format!(
                    "{}:{}:{}",
                    seen.generation,
                    hex(&seen.candidate),
                    hex(&seen.root)
                )
            },
        );
        format!(
            "AT {} {} {} {} {} {base_root} {} {seen}",
            claim.epoch,
            hex(&claim.attempt_id),
            hex(&claim.fence),
            hex(&claim.input_digest),
            claim.base_generation,
            claim.observation_sequence,
        )
    }

    fn decode(line: &str) -> Self {
        let words = line.split_whitespace().collect::<Vec<_>>();
        let [
            "AT",
            epoch,
            attempt,
            fence,
            input,
            base_generation,
            base_root,
            observation,
            seen,
        ] = words.as_slice()
        else {
            panic!("malformed death report {line:?}");
        };
        let claim = CandidateAttemptRecoveryClaim::new(
            namespace(),
            epoch.parse().expect("epoch"),
            unhex::<16>(attempt),
            unhex::<32>(fence),
            unhex::<32>(input),
            base_generation.parse().expect("base generation"),
            (*base_root != "-").then(|| unhex::<32>(base_root)),
            observation.parse().expect("observation"),
        );
        let seen = (*seen != "-").then(|| {
            let parts = seen.split(':').collect::<Vec<_>>();
            let [generation, candidate, root] = parts.as_slice() else {
                panic!("malformed generation {seen:?}");
            };
            SeenGeneration {
                generation: generation.parse().expect("generation"),
                candidate: unhex::<32>(candidate),
                root: unhex::<32>(root),
            }
        });
        Self { claim, seen }
    }
}

/// Runs the publication in this (peer) process up to `point`, reporting what a
/// parent needs to ask about it afterwards.
async fn publish_until(authority: &mut TursoAuthority, point: DeathPoint) -> Dying {
    let observed = authority
        .record_source_observation(observation(SourceObservationValue::KnownCount(3), 100))
        .await
        .expect("observe");
    let attempt = authority
        .begin_attempt(&namespace(), [INPUT; 32], &observed)
        .await
        .expect("begin");
    let claim = attempt.recovery_claim();
    if point == DeathPoint::AfterBegin {
        return Dying { claim, seen: None };
    }
    let published = candidate(
        attempt,
        CANDIDATE_TAG,
        CANDIDATE_TAG + 1,
        CANDIDATE_TAG + 2,
        CANDIDATE_TAG + 1,
    );
    let receipt = authority
        .verify_closure(&published, &AcceptVerifiedClosure)
        .expect("verify");
    let frontier = authority
        .compare_and_select(published, receipt)
        .await
        .expect("select");
    let seen = Some(SeenGeneration {
        generation: frontier.generation(),
        candidate: *frontier.candidate_id(),
        root: *frontier.target_root(),
    });
    let projected_through = match point {
        DeathPoint::AfterBegin | DeathPoint::AfterSelect => 0,
        DeathPoint::AfterCatalogProjection => 1,
        DeathPoint::AfterAllProjections | DeathPoint::AfterAck => 3,
    };
    for projector in PROJECTIONS.into_iter().take(projected_through) {
        authority
            .mark_projection_current(
                &namespace(),
                projector,
                frontier.generation(),
                *frontier.target_root(),
            )
            .await
            .expect("project");
    }
    Dying { claim, seen }
}

fn run_as_peer() -> bool {
    let Some(database) = process_harness::peer_database() else {
        return false;
    };
    futures_executor::block_on(async {
        let mut authority = TursoAuthority::open(&database).await.expect("peer opens");
        process_harness::report("OPENED");
        for line in process_harness::commands() {
            let Some(point) = line.strip_prefix("PUBLISH ").and_then(DeathPoint::parse) else {
                process_harness::report(&format!("FAILED unknown command {line:?}"));
                continue;
            };
            let dying = publish_until(&mut authority, point).await;
            // The parent ends this process as soon as it reads the line, so the
            // death happens with the authority exactly as it stands here. For
            // `AfterAck` the report itself is the client acknowledgement.
            process_harness::report(&dying.encode());
        }
    });
    true
}

fn current_flags(projections: &[ProjectionWatermark]) -> [bool; 3] {
    let mut current = [false; 3];
    for (slot, kind) in current.iter_mut().zip(PROJECTIONS) {
        *slot = projections
            .iter()
            .find(|watermark| watermark.projector() == kind)
            .expect("every projector has a watermark")
            .is_current();
    }
    current
}

#[test]
fn the_authority_names_the_outcome_after_the_owner_dies_at_every_step() {
    if run_as_peer() {
        return;
    }
    for point in DeathPoint::ALL {
        let database = Database::new(point.token());
        let (mut peer, opened) = Peer::spawn(
            &format!(
                "{TEST_PREFIX}the_authority_names_the_outcome_after_the_owner_dies_at_every_step"
            ),
            &database.0,
        );
        assert_eq!(opened, "OPENED");
        let dying = Dying::decode(&peer.ask(&format!("PUBLISH {}", point.token())));
        peer.kill();

        futures_executor::block_on(async {
            let mut authority = TursoAuthority::open(&database.0)
                .await
                .unwrap_or_else(|error| panic!("{point:?}: reopen after death: {error}"));
            let expected = point.expected();
            let claim = &dying.claim;

            // The attempt's outcome, from the authority alone.
            let disposition = authority
                .attempt_disposition(claim)
                .await
                .unwrap_or_else(|error| panic!("{point:?}: disposition: {error}"));
            let frontier = authority
                .selected_frontier(&namespace())
                .await
                .unwrap_or_else(|error| panic!("{point:?}: frontier: {error}"));

            if expected.published {
                let AttemptDisposition::Published(published) = &disposition else {
                    panic!("{point:?}: expected a published attempt, found {disposition:?}");
                };
                let frontier = frontier.expect("a published attempt has a selected head");
                let seen = dying
                    .seen
                    .as_ref()
                    .expect("the dying owner saw its generation");
                // The receipt a client may already hold equals the one a
                // restarted owner reconstructs.
                assert_eq!(published.generation(), seen.generation, "{point:?}");
                assert_eq!(published.candidate_id(), &seen.candidate, "{point:?}");
                assert_eq!(published.target_root(), &seen.root, "{point:?}");
                assert_eq!(published.generation(), frontier.generation(), "{point:?}");
                assert_eq!(published.attempt().0, &claim.attempt_id, "{point:?}");
                assert_eq!(published.attempt().1, claim.epoch, "{point:?}");
                assert_eq!(
                    current_flags(frontier.projections()),
                    expected.projections_current,
                    "{point:?}: projection watermarks"
                );

                // A restarted owner that is unsure resubmits nothing and
                // changes nothing: the same candidate is refused...
                let again = candidate(
                    attempt_for_replay(claim, &authority).await,
                    CANDIDATE_TAG,
                    CANDIDATE_TAG + 1,
                    CANDIDATE_TAG + 2,
                    CANDIDATE_TAG + 1,
                );
                let receipt = authority
                    .verify_closure(&again, &AcceptVerifiedClosure)
                    .expect("verify replay");
                assert!(
                    matches!(
                        authority.compare_and_select(again, receipt).await,
                        Err(AuthorityError::StaleAttempt | AuthorityError::StaleFrontier)
                    ),
                    "{point:?}: a replayed candidate must not select again"
                );
                // ...and finishing the interrupted projections is repair, not a
                // new publication: every mark is idempotent or completes.
                for projector in PROJECTIONS {
                    authority
                        .mark_projection_current(
                            &namespace(),
                            projector,
                            frontier.generation(),
                            *frontier.target_root(),
                        )
                        .await
                        .unwrap_or_else(|error| panic!("{point:?}: repair {projector:?}: {error}"));
                }
                let repaired = authority
                    .selected_frontier(&namespace())
                    .await
                    .expect("frontier")
                    .expect("head");
                assert_eq!(repaired.generation(), frontier.generation(), "{point:?}");
                assert_eq!(
                    current_flags(repaired.projections()),
                    [true; 3],
                    "{point:?}: repair completes every projection"
                );
                assert_eq!(
                    authority
                        .attempt_disposition(claim)
                        .await
                        .expect("disposition after repair"),
                    disposition,
                    "{point:?}: repair never changes the published receipt"
                );
            } else {
                assert_eq!(disposition, AttemptDisposition::Pending, "{point:?}");
                assert!(frontier.is_none(), "{point:?}: nothing was selected");
                // Nothing was published, so nothing may be reported as such:
                // the attempt is reopened exactly and can still complete.
                let attempt = authority
                    .recover_candidate_attempt(claim)
                    .await
                    .unwrap_or_else(|error| panic!("{point:?}: recover: {error}"));
                let published = candidate(
                    attempt,
                    CANDIDATE_TAG,
                    CANDIDATE_TAG + 1,
                    CANDIDATE_TAG + 2,
                    CANDIDATE_TAG + 1,
                );
                let receipt = authority
                    .verify_closure(&published, &AcceptVerifiedClosure)
                    .expect("verify");
                authority
                    .compare_and_select(published, receipt)
                    .await
                    .unwrap_or_else(|error| panic!("{point:?}: resume: {error}"));
                assert!(matches!(
                    authority
                        .attempt_disposition(claim)
                        .await
                        .expect("disposition after resume"),
                    AttemptDisposition::Published(_)
                ));
            }
        });
    }
}

/// Rebuilds the attempt token a restarted owner would hold from its claim, via
/// the authority's own recovery, falling back to the claim's own facts once the
/// attempt is no longer recoverable (it was consumed by a selection).
async fn attempt_for_replay(
    claim: &CandidateAttemptRecoveryClaim,
    authority: &TursoAuthority,
) -> CandidateAttempt {
    let observation = authority
        .read_observation_for_test(&claim.namespace, claim.observation_sequence)
        .await;
    CandidateAttempt::new(
        claim.namespace.clone(),
        claim.epoch,
        claim.attempt_id,
        claim.fence,
        claim.input_digest,
        claim.base_generation,
        claim.base_root,
        observation,
    )
}

impl TursoAuthority {
    async fn read_observation_for_test(
        &self,
        namespace: &AuthorityNamespace,
        sequence: u64,
    ) -> SourceObservationReceipt {
        let sequence = i64::try_from(sequence).expect("sequence");
        let tx = self
            .connection
            .unchecked_transaction()
            .await
            .expect("snapshot");
        let observation = read_observation(&tx, namespace, sequence)
            .await
            .expect("stored observation");
        tx.rollback().await.expect("release snapshot");
        observation
    }
}
