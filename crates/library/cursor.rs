//! Version-bound branch/log/view cursors and bounded subscriptions.

use crate::canonical::{
    BranchKey, Frontier, LogKey, ViewRecipeId, ViewStateRoot, ViewVersion, branch_key, log_key,
    view_key, view_state_root, view_version,
};
use crate::{CommittedViewDelta, ViewRoot};

mod codec;

/// Product schema version carried by cursors created by this crate.
pub const CURSOR_SCHEMA: u16 = crate::canonical::PROTOCOL_SCHEMA;

/// Byte length of the authenticated local control cursor encoding.
///
/// This is intentionally a fixed-width identity envelope.  A receiver never
/// constructs IDs from these bytes; it compares the complete envelope with a
/// caller-owned typed cursor and returns that already admitted value.
pub const CURSOR_CONTROL_BYTES: usize = 2 + 32 * 5 + 2 + 8;

/// Byte length of a self-describing bounded-query cursor envelope.
pub const CURSOR_QUERY_BYTES: usize = CURSOR_CONTROL_BYTES + 8;

/// Bounded position in one branch/log/view/schema stream.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Cursor {
    recipe: ViewRecipeId,
    version: ViewVersion,
    branch: BranchKey,
    log: LogKey,
    schema: u16,
    root: ViewStateRoot,
    sequence: u64,
    query_offset: u64,
}

impl Cursor {
    /// Creates the empty cursor for the default branch/log and empty view.
    #[must_use]
    pub fn new() -> Self {
        let root = view_state_root(&[]);
        Self::for_view(
            view_key(b"default-view"),
            view_version(root.as_bytes()),
            Frontier::new(
                branch_key("main"),
                log_key("library"),
                CURSOR_SCHEMA,
                root,
                0,
            ),
        )
    }

    /// Creates a compatibility cursor at one root using the default view.
    #[must_use]
    pub fn at(root: ViewStateRoot, sequence: u64) -> Self {
        Self::for_view(
            view_key(b"default-view"),
            view_version(root.as_bytes()),
            Frontier::new(
                branch_key("main"),
                log_key("library"),
                CURSOR_SCHEMA,
                root,
                sequence,
            ),
        )
    }

    /// Creates a cursor bound to one recipe, immutable view version, and
    /// complete branch/log/schema/root/sequence frontier.
    #[must_use]
    pub const fn for_view(recipe: ViewRecipeId, version: ViewVersion, frontier: Frontier) -> Self {
        Self {
            recipe,
            version,
            branch: frontier.branch,
            log: frontier.log,
            schema: frontier.schema,
            root: frontier.root,
            sequence: frontier.sequence,
            query_offset: 0,
        }
    }

    /// Creates the cursor at a checked view's visible relation root.
    ///
    /// A [`ViewRoot`] frontier names the source relation, while a subscription
    /// cursor names the visible relation produced by that view.  This helper
    /// keeps that distinction explicit and prevents callers from accidentally
    /// binding a cursor to `view.frontier().root` instead of `view.root()`.
    #[must_use]
    pub fn for_view_root(view: &ViewRoot) -> Self {
        Self::for_view_root_at(view, view.frontier().sequence)
    }

    /// Creates the cursor at a view's visible relation root and an explicit
    /// stream sequence.
    #[must_use]
    pub fn for_view_root_at(view: &ViewRoot, sequence: u64) -> Self {
        Self::for_view(
            view.recipe(),
            view.version(),
            Frontier::new(
                view.frontier().branch,
                view.frontier().log,
                view.frontier().schema,
                view.root(),
                sequence,
            ),
        )
    }

    /// Creates a compatibility cursor from a frontier using a deterministic
    /// default recipe/version binding.
    #[must_use]
    pub fn from_frontier(frontier: Frontier) -> Self {
        Self::for_view(
            view_key(b"default-view"),
            view_version(frontier.root.as_bytes()),
            frontier,
        )
    }

    /// Returns the stable view recipe identity bound to this cursor.
    #[must_use]
    pub const fn recipe(self) -> ViewRecipeId {
        self.recipe
    }

    /// Returns the immutable view version observed by this cursor.
    #[must_use]
    pub const fn version(self) -> ViewVersion {
        self.version
    }

    /// Returns the branch identity bound to this cursor.
    #[must_use]
    pub const fn branch(self) -> BranchKey {
        self.branch
    }

    /// Returns the log identity bound to this cursor.
    #[must_use]
    pub const fn log(self) -> LogKey {
        self.log
    }

    /// Returns the protocol/schema version bound to this cursor.
    #[must_use]
    pub const fn schema(self) -> u16 {
        self.schema
    }

    /// Returns the observed row relation root at the cursor position.
    #[must_use]
    pub const fn root(self) -> ViewStateRoot {
        self.root
    }

    /// Returns the next sequence expected by this cursor.
    #[must_use]
    pub const fn sequence(self) -> u64 {
        self.sequence
    }

    /// Returns the opaque row offset carried by a bounded query continuation.
    ///
    /// Subscription cursors leave this value at zero. It is intentionally not
    /// part of the branch/log stream identity or wire frontier.
    #[must_use]
    pub const fn query_offset(self) -> u64 {
        self.query_offset
    }

    /// Binds a bounded query continuation to its next row offset.
    #[must_use]
    pub(crate) const fn with_query_offset(mut self, offset: u64) -> Self {
        self.query_offset = offset;
        self
    }

    /// Returns all cursor bindings as a frontier descriptor.
    #[must_use]
    pub const fn frontier(self) -> Frontier {
        Frontier::new(self.branch, self.log, self.schema, self.root, self.sequence)
    }

    /// Returns whether this cursor belongs to the supplied owner frontier.
    ///
    /// Recipe and query offset are intentionally excluded: callers that own a
    /// query must compare its recipe separately, while the offset is the
    /// bounded position being admitted. This predicate centralizes the
    /// version, branch, log, schema, visible-root, and sequence checks shared
    /// by client, graph-query, and wire admission.
    #[must_use]
    pub fn matches_owner(self, owner: Self) -> bool {
        self.version == owner.version
            && self.branch == owner.branch
            && self.log == owner.log
            && self.schema == owner.schema
            && self.root == owner.root
            && self.sequence <= owner.sequence
    }
}

impl Default for Cursor {
    fn default() -> Self {
        Self::new()
    }
}

/// One committed event observed after a cursor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CursorEvent {
    /// A durable user intent was appended.
    Intent {
        /// Idempotency identity of the intent.
        id: crate::IntentId,
    },
    /// A view transition was committed against an exact base root/version.
    View {
        /// Checked base/target transition.
        delta: Box<CommittedViewDelta>,
    },
}

impl CursorEvent {
    /// Returns the target root if this event advances a view root.
    #[must_use]
    pub const fn target_root(&self) -> Option<ViewStateRoot> {
        match self {
            Self::Intent { .. } => None,
            Self::View { delta } => Some(delta.target()),
        }
    }

    /// Returns the target view version if this event advances a view root.
    #[must_use]
    pub const fn target_version(&self) -> Option<ViewVersion> {
        match self {
            Self::Intent { .. } => None,
            Self::View { delta } => Some(delta.target_version()),
        }
    }
}

/// Why a subscription must replace its cursor with a complete snapshot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CursorResetReason {
    /// One or more events were pruned or missing.
    Gap,
    /// The selected branch/log was discarded or rebased.
    BranchDiscarded,
    /// The source root is no longer retained.
    Pruned,
    /// The event stream schema is incompatible.
    SchemaMismatch,
    /// An event did not chain from the subscriber's observed root/version.
    RootMismatch,
}

/// Cursor read outcome, including explicit gap/reset semantics.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CursorRead {
    /// Events accepted and applied in sequence order.
    Events {
        /// Cursor after the returned events.
        cursor: Cursor,
        /// Bounded event batch.
        events: Box<[CursorEvent]>,
    },
    /// A complete coherent root must replace the subscriber's stale state.
    Reset {
        /// Cursor associated with the replacement root.
        cursor: Cursor,
        /// Complete replacement view root.
        root: Box<ViewRoot>,
        /// Reason the delta stream could not continue.
        reason: CursorResetReason,
    },
}

/// Cursor rejection before a reset root can be selected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CursorError {
    /// Source and subscriber name different view recipes.
    WrongRecipe,
    /// Source and subscriber name different branches.
    WrongBranch,
    /// Source and subscriber name different logs.
    WrongLog,
    /// Source and subscriber use incompatible schema versions.
    SchemaMismatch,
    /// Source and subscriber name different immutable view versions before
    /// an event batch can be interpreted.
    WrongVersion,
    /// Source is behind the subscriber's already consumed sequence.
    Future,
    /// Source root cannot chain from the subscriber's root.
    WrongRoot,
    /// A malformed event batch cannot be interpreted as a contiguous stream.
    Gap,
    /// The replacement root supplied for an explicit reset is not coherent.
    InvalidResetRoot,
}

/// A bounded subscription to one branch/log/view/schema stream.
#[derive(Clone, Debug)]
pub struct CursorSub {
    cursor: Cursor,
    max: usize,
}

impl CursorSub {
    /// Creates a bounded subscription at sequence zero for the default view.
    #[must_use]
    pub fn new(root: ViewStateRoot, max: usize) -> Self {
        Self::from_cursor(Cursor::at(root, 0), max)
    }

    /// Creates a bounded subscription at one complete view root.
    #[must_use]
    pub fn for_view(view: &ViewRoot, max: usize) -> Self {
        Self::from_cursor(Cursor::for_view_root(view), max)
    }

    /// Creates a bounded subscription at an explicit cursor.
    #[must_use]
    pub const fn from_cursor(cursor: Cursor, max: usize) -> Self {
        Self { cursor, max }
    }

    /// Returns the next cursor position expected by this subscriber.
    #[must_use]
    pub const fn cursor(&self) -> Cursor {
        self.cursor
    }

    /// Returns this subscription's bounded event capacity.
    #[must_use]
    pub const fn max(&self) -> usize {
        self.max
    }

    /// Reads a contiguous event suffix and emits a reset when the suffix has a
    /// sequence gap, was pruned, or cannot chain from the observed root.
    ///
    /// # Errors
    ///
    /// Returns [`CursorError`] when the reset root is incoherent, the source
    /// cursor belongs to another stream, or the source is behind the
    /// subscriber's consumed position.
    pub fn read(
        &mut self,
        source: &Cursor,
        events: &[CursorEvent],
        reset_root: ViewRoot,
    ) -> Result<CursorRead, CursorError> {
        if !reset_root.is_coherent() {
            return Err(CursorError::InvalidResetRoot);
        }
        if reset_root.capability().is_none() || reset_root.coverage().is_empty() {
            return Err(CursorError::InvalidResetRoot);
        }
        if source.recipe != self.cursor.recipe {
            return Err(CursorError::WrongRecipe);
        }
        if source.branch != self.cursor.branch {
            return Err(CursorError::WrongBranch);
        }
        if source.log != self.cursor.log {
            return Err(CursorError::WrongLog);
        }
        if source.schema != self.cursor.schema {
            return Err(CursorError::SchemaMismatch);
        }
        if source.sequence < self.cursor.sequence {
            return Err(CursorError::Future);
        }

        let available_end = self
            .cursor
            .sequence
            .checked_add(events.len() as u64)
            .ok_or(CursorError::Gap)?;
        if source.sequence > available_end {
            return Ok(self.reset(reset_root, *source, CursorResetReason::Gap));
        }

        let take = events.len().min(self.max);
        let mut next = self.cursor;
        let expected_end = next
            .sequence
            .checked_add(take as u64)
            .ok_or(CursorError::Gap)?;
        if source.sequence < expected_end {
            return Ok(self.reset(reset_root, *source, CursorResetReason::RootMismatch));
        }
        let mut accepted = Vec::with_capacity(take);
        for event in events.iter().take(take).cloned() {
            next = match next.advance_event(&event) {
                Ok(next) => next,
                Err(CursorError::Gap) => return Err(CursorError::Gap),
                Err(_) => {
                    return Ok(self.reset(reset_root, *source, CursorResetReason::RootMismatch));
                }
            };
            accepted.push(event);
        }
        if source.sequence == expected_end
            && (next.root != source.root || next.version != source.version)
        {
            return Ok(self.reset(reset_root, *source, CursorResetReason::RootMismatch));
        }
        self.cursor = next;
        Ok(CursorRead::Events {
            cursor: next,
            events: accepted.into_boxed_slice(),
        })
    }

    /// Returns a reset with the replacement root's exact recipe/version,
    /// source frontier, and observed sequence.
    fn reset(&mut self, root: ViewRoot, source: Cursor, reason: CursorResetReason) -> CursorRead {
        let cursor = Cursor::for_view_root_at(&root, source.sequence);
        self.cursor = cursor;
        CursorRead::Reset {
            cursor,
            root: Box::new(root),
            reason,
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use crate::canonical::{branch_key, object_version, symbol_key, view_key};
    use crate::{
        AuthorityScopeClaim, Basis, CoverageCapability, Row, RowId, ScopeRoot, SemanticObject,
        ViewDelta, ViewRoot,
    };

    fn capability(object: SemanticObject) -> CoverageCapability {
        let declared = AuthorityScopeClaim::from_object_version(object);
        let scope = ScopeRoot::from_bytes(object.to_bytes());
        let observation = crate::admit_producer_observation(
            crate::UntrustedProducerObservation::new(
                *scope.as_bytes(),
                scope,
                *scope.as_bytes(),
                scope.as_bytes().to_vec(),
            ),
            &TestCoverageVerifier,
        )
        .expect("producer observation");
        CoverageCapability::from_authorized_with_evidence(
            crate::admit_complete_scope(declared, observation).expect("producer coverage"),
            scope.as_bytes().to_vec(),
        )
        .expect("coverage evidence")
    }

    struct TestCoverageVerifier;

    impl crate::ProducerObservationVerifier for TestCoverageVerifier {
        type Error = &'static str;

        fn verify(
            &self,
            observation: &crate::UntrustedProducerObservation,
        ) -> Result<crate::ProducerObservationClaims, Self::Error> {
            if observation.producer_identity() == *observation.scope_root().as_bytes()
                && observation.context() == *observation.scope_root().as_bytes()
                && observation.evidence() == observation.scope_root().as_bytes()
            {
                Ok(crate::ProducerObservationClaims::new(
                    observation.producer_identity(),
                    observation.scope_root(),
                    observation.context(),
                    *blake3::hash(observation.evidence()).as_bytes(),
                ))
            } else {
                Err("invalid test producer observation")
            }
        }
    }

    fn make_root(root: ViewStateRoot) -> ViewRoot {
        let basis = Basis::new(root, object_version(b"source"));
        ViewRoot::empty_checked(
            view_key(b"view"),
            basis,
            Frontier::new(basis.branch, basis.log, basis.schema, root, 0),
            capability(basis.object),
        )
        .expect("checked view root")
    }

    #[test]
    fn branch_log_schema_are_part_of_cursor_identity() {
        let root = view_state_root(&[]);
        let mut sub = CursorSub::new(root, 4);
        let other = Cursor::for_view(
            sub.cursor().recipe(),
            sub.cursor().version(),
            Frontier::new(
                branch_key("feature"),
                log_key("other"),
                CURSOR_SCHEMA,
                root,
                1,
            ),
        );
        assert_eq!(
            sub.read(&other, &[], make_root(root)),
            Err(CursorError::WrongBranch)
        );
    }

    #[test]
    fn query_cursor_round_trips_only_against_its_owner() {
        let root = view_state_root(&[]);
        let owner = Cursor::for_view_root(&make_root(root));
        let continuation = owner.with_query_offset(17);
        let encoded = continuation.encode_query();
        assert_eq!(
            Cursor::decode_query_against(&encoded, owner),
            Ok(continuation)
        );
        let other = Cursor::for_view_root(&make_root(view_state_root(&[(
            "other".to_owned(),
            "row".to_owned(),
        )])));
        assert!(Cursor::decode_query_against(&encoded, other).is_err());
    }

    #[test]
    fn owner_matching_allows_prior_sequence_but_rejects_foreign_frontiers() {
        let root = view_state_root(&[]);
        let base = Cursor::for_view_root(&make_root(root));
        let owner = Cursor::for_view(
            base.recipe(),
            base.version(),
            Frontier::new(base.branch(), base.log(), base.schema(), base.root(), 4),
        );
        let prior = Cursor::for_view(
            owner.recipe(),
            owner.version(),
            Frontier::new(owner.branch(), owner.log(), owner.schema(), owner.root(), 3),
        )
        .with_query_offset(9);
        assert!(prior.matches_owner(owner));

        let future = Cursor::for_view(
            owner.recipe(),
            owner.version(),
            Frontier::new(owner.branch(), owner.log(), owner.schema(), owner.root(), 5),
        );
        assert!(!future.matches_owner(owner));

        let foreign = Cursor::for_view_root(&make_root(view_state_root(&[(
            "foreign".to_owned(),
            "root".to_owned(),
        )])));
        assert!(!foreign.matches_owner(owner));
    }

    #[test]
    fn control_cursor_admission_rejects_root_schema_and_replay_mutations() {
        let view = make_root(view_state_root(&[]));
        let cursor = CursorSub::for_view(&view, 4).cursor();
        let encoded = cursor.encode_control();
        assert_eq!(
            Cursor::decode_control_against(&encoded, cursor).expect("cursor"),
            cursor
        );

        let mut wrong_schema = encoded.to_vec();
        wrong_schema[0] ^= 1;
        assert!(Cursor::decode_control_against(&wrong_schema, cursor).is_err());

        let mut wrong_root = encoded.to_vec();
        wrong_root[132] ^= 1;
        assert!(Cursor::decode_control_against(&wrong_root, cursor).is_err());

        let mut replay = encoded.to_vec();
        replay[171] = 1;
        assert!(Cursor::decode_control_against(&replay, cursor).is_err());

        assert_eq!(
            Cursor::decode_control_for_root(&encoded, &view).expect("root-bound cursor"),
            cursor
        );
        let other = make_root(view_state_root(&[(
            "different".to_owned(),
            "root".to_owned(),
        )]));
        assert!(Cursor::decode_control_for_root(&encoded, &other).is_err());
    }

    #[test]
    fn a_gap_requests_a_reset_instead_of_fabricating_deltas() {
        let root_id = view_state_root(&[]);
        let mut sub = CursorSub::new(root_id, 4);
        let source = Cursor::at(root_id, 9);
        let read = sub
            .read(&source, &[], make_root(root_id))
            .expect("gap reset");
        assert!(matches!(
            read,
            CursorRead::Reset {
                reason: CursorResetReason::Gap,
                ..
            }
        ));
    }

    #[test]
    fn view_events_advance_observed_root_only_after_chain_validation() {
        let source_root = view_state_root(&[]);
        let basis = Basis::new(source_root, object_version(b"source"));
        let base = ViewRoot::empty_checked(
            view_key(b"view"),
            basis,
            Frontier::new(basis.branch, basis.log, basis.schema, source_root, 0),
            capability(basis.object),
        )
        .expect("checked view root");
        let row = Row::new(RowId::Symbol(symbol_key("s")), basis, "name");
        let prepared = base
            .prepare(ViewDelta::Upsert { row }, capability(basis.object))
            .expect("prepare");
        let mut sub = CursorSub::for_view(&base, 4);
        let (next, event) = base.commit(prepared).expect("commit");
        let source = Cursor::for_view(
            next.recipe,
            next.version,
            Frontier::new(
                next.frontier.branch,
                next.frontier.log,
                next.frontier.schema,
                next.root,
                1,
            ),
        );
        let read = sub
            .read(
                &source,
                &[CursorEvent::View {
                    delta: Box::new(event),
                }],
                next.clone(),
            )
            .expect("events");
        assert!(matches!(read, CursorRead::Events { .. }));
        assert_eq!(sub.cursor().root(), next.root);
        assert_eq!(sub.cursor().version(), next.version);
    }

    #[test]
    fn event_cursor_kernel_advances_and_rewinds_across_intents() {
        let source_root = view_state_root(&[]);
        let basis = Basis::new(source_root, object_version(b"source"));
        let base = ViewRoot::empty_checked(
            view_key(b"view"),
            basis,
            Frontier::new(basis.branch, basis.log, basis.schema, source_root, 0),
            capability(basis.object),
        )
        .expect("checked view root");
        let row = Row::new(RowId::Symbol(symbol_key("s")), basis, "name");
        let prepared = base
            .prepare(ViewDelta::Upsert { row }, capability(basis.object))
            .expect("prepare");
        let (_, event) = base.clone().commit(prepared).expect("commit");
        let start = CursorSub::for_view(&base, 4).cursor();
        let intent = CursorEvent::Intent {
            id: crate::intent_id("cursor", b"intent"),
        };
        let after_intent = start.advance_event(&intent).expect("intent advance");
        let view_event = CursorEvent::View {
            delta: Box::new(event),
        };
        let after_view = after_intent
            .advance_event(&view_event)
            .expect("view advance");
        assert_eq!(after_view.sequence(), 2);
        assert_eq!(after_view.rewind_event(&view_event), Ok(after_intent));
        assert_eq!(after_intent.rewind_event(&intent), Ok(start));
    }

    #[test]
    fn forged_view_event_ids_force_a_reset() {
        let source_root = view_state_root(&[]);
        let basis = Basis::new(source_root, object_version(b"source"));
        let base = ViewRoot::empty_checked(
            view_key(b"view"),
            basis,
            Frontier::new(basis.branch, basis.log, basis.schema, source_root, 0),
            capability(basis.object),
        )
        .expect("checked view root");
        let first = base
            .prepare(
                ViewDelta::Upsert {
                    row: Row::new(RowId::Symbol(symbol_key("first")), basis, "first"),
                },
                capability(basis.object),
            )
            .expect("first prepare");
        let (_, first_event) = base.clone().commit(first).expect("first commit");
        let second = base
            .prepare(
                ViewDelta::Upsert {
                    row: Row::new(RowId::Symbol(symbol_key("second")), basis, "second"),
                },
                capability(basis.object),
            )
            .expect("second prepare");
        let (_, second_event) = base.clone().commit(second).expect("second commit");
        let mut forged = first_event;
        forged.id = second_event.id;

        let mut sub = CursorSub::for_view(&base, 4);
        let source = Cursor::for_view(
            base.recipe,
            second_event.target_version(),
            Frontier::new(
                base.frontier.branch,
                base.frontier.log,
                base.frontier.schema,
                second_event.target(),
                1,
            ),
        );
        let read = sub
            .read(
                &source,
                &[CursorEvent::View {
                    delta: Box::new(forged),
                }],
                base.clone(),
            )
            .expect("reset");
        assert!(matches!(
            read,
            CursorRead::Reset {
                reason: CursorResetReason::RootMismatch,
                ..
            }
        ));
    }
}
