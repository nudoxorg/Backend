//! Owner-sealed keyset frontiers for bounded related-edge reads.
//!
//! A parsed cursor is only a claim. The owner must verify it against the
//! current complete semantic publication and source-policy revision before
//! seeking its exclusive edge boundary.
use crate::{GraphRelation, NativeOwnerStatement, RowId, SemanticLinkKind};
use serde::{Deserialize, Serialize};

/// Greatest number of edge identities admitted in one delivery.
pub const MAX_RELATED_PAGE_EDGES: u16 = 200;

/// A row identity claim used only as an ordering boundary, not as authority to
/// construct a selected row. Variant order matches `RowId`.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RelatedRowKey {
    /// Package identity bytes.
    Package([u8; 32]),
    /// Symbol identity bytes.
    Symbol([u8; 32]),
    /// Object identity bytes.
    Object([u8; 32]),
}
impl From<RowId> for RelatedRowKey {
    fn from(id: RowId) -> Self {
        match id {
            RowId::Package(id) => Self::Package(*id.as_bytes()),
            RowId::Symbol(id) => Self::Symbol(*id.as_bytes()),
            RowId::Object(id) => Self::Object(*id.as_bytes()),
        }
    }
}

/// Edge identity; confidence and origin are evidence, not identity.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelatedEdgeKey {
    /// Directed source.
    pub from: RelatedRowKey,
    /// Directed destination.
    pub to: RelatedRowKey,
    /// Semantic relation kind.
    pub relation: SemanticLinkKind,
}
impl RelatedEdgeKey {
    /// Derives the identity used to group all origins of an edge.
    #[must_use]
    pub fn new(relation: GraphRelation) -> Self {
        Self {
            from: relation.from.into(),
            to: relation.to.into(),
            relation: relation.relation,
        }
    }
}

/// Direction of a related edge read.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RelatedDirection {
    /// Edges leaving the center.
    Outgoing,
    /// Edges entering or leaving the center.
    Both,
}

/// The exact untrusted query claims whose continuation is sealed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelatedPageScope {
    /// Exact selected view root.
    pub root: [u8; 32],
    /// Claim for the selected center, resolved against the view by the owner.
    pub source: [u8; 32],
    /// Whether source is a selected-view claim rather than a canonical key.
    pub source_selected: bool,
    /// Edge direction.
    pub direction: RelatedDirection,
    /// Original query credit.
    pub issued: u16,
    /// This delivery's maximum edge count.
    pub delivery: u16,
    /// Distinguishes fixed from windowed delivery. A windowed successor may
    /// change delivery without changing the sealed predecessor or issued credit.
    pub windowed: bool,
}
impl RelatedPageScope {
    /// Checks the bounded window before a request crosses a process boundary.
    pub fn admit(&self) -> Result<(), RelatedPageError> {
        if self.issued == 0
            || self.issued > MAX_RELATED_PAGE_EDGES
            || self.delivery == 0
            || self.delivery > self.issued
            || (!self.windowed && self.delivery != self.issued)
        {
            return Err(RelatedPageError::PageBound);
        }
        Ok(())
    }
}

/// Untrusted continuation returned by a prior owner page.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelatedPageCursor {
    scope: RelatedPageScope,
    semantic_revision: [u8; 32],
    after: RelatedEdgeKey,
    seal: [u8; 32],
}
#[derive(Serialize)]
struct CursorStatement<'a> {
    scope: &'a RelatedPageScope,
    semantic_revision: &'a [u8; 32],
    after: &'a RelatedEdgeKey,
}
impl NativeOwnerStatement for CursorStatement<'_> {
    const DOMAIN: &'static [u8] = b"backend.related-edge-page.cursor.v1\0";
}
impl RelatedPageCursor {
    /// Untrusted scope claim used to reconstruct an exact continuation request.
    #[must_use]
    pub fn scope(&self) -> &RelatedPageScope {
        &self.scope
    }
    /// Whether this untrusted predecessor scope permits the requested successor.
    /// Authentication of the predecessor remains the owner's responsibility.
    #[must_use]
    pub fn permits_successor(&self, scope: &RelatedPageScope) -> bool {
        self.scope.root == scope.root
            && self.scope.source == scope.source
            && self.scope.source_selected == scope.source_selected
            && self.scope.direction == scope.direction
            && self.scope.issued == scope.issued
            && self.scope.windowed == scope.windowed
            && (scope.windowed || self.scope.delivery == scope.delivery)
            && scope.admit().is_ok()
    }
    /// Untrusted boundary claim for reply correlation; the owner verifies its seal.
    #[must_use]
    pub fn after(&self) -> RelatedEdgeKey {
        self.after
    }
    /// Checks the predecessor's claimed semantic revision without authenticating it.
    #[must_use]
    pub fn matches_revision(&self, revision: [u8; 32]) -> bool {
        self.semantic_revision == revision
    }
    /// Checks the untrusted cursor claims against a page's requested scope and revision.
    #[must_use]
    pub fn matches_page(&self, scope: &RelatedPageScope, revision: [u8; 32]) -> bool {
        self.scope == *scope && self.semantic_revision == revision
    }
    fn statement(&self) -> CursorStatement<'_> {
        CursorStatement {
            scope: &self.scope,
            semantic_revision: &self.semantic_revision,
            after: &self.after,
        }
    }
}

/// Bounded query and optional untrusted continuation.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RelatedPageRequest {
    /// Exact scope of this page.
    pub scope: RelatedPageScope,
    /// Previously issued continuation, absent on the first page.
    pub cursor: Option<RelatedPageCursor>,
}

/// A request admitted against the exact current semantic publication and owner.
pub struct VerifiedRelatedPageRequest<'a> {
    request: &'a RelatedPageRequest,
    revision: [u8; 32],
    secret: &'a [u8; 32],
}
impl<'a> VerifiedRelatedPageRequest<'a> {
    /// Verify before accessing a continuation boundary. `revision` must commit
    /// to every selected publication binding, image and source-policy input.
    pub fn verify(
        request: &'a RelatedPageRequest,
        revision: [u8; 32],
        secret: &'a [u8; 32],
    ) -> Result<Self, RelatedPageError> {
        request.scope.admit()?;
        if let Some(cursor) = &request.cursor {
            if !cursor.permits_successor(&request.scope) || cursor.semantic_revision != revision {
                return Err(RelatedPageError::CursorMismatch);
            }
            crate::verify_native_owner_statement(secret, &cursor.statement(), &cursor.seal)
                .map_err(|_| RelatedPageError::CursorMismatch)?;
        }
        Ok(Self {
            request,
            revision,
            secret,
        })
    }
    /// Exclusive lower bound, if resuming.
    #[must_use]
    pub fn after(&self) -> Option<RelatedEdgeKey> {
        self.request.cursor.as_ref().map(|cursor| cursor.after)
    }
    /// Exact admitted query scope.
    #[must_use]
    pub fn scope(&self) -> &RelatedPageScope {
        &self.request.scope
    }
    /// Seal the last consumed key, including a key whose evidence conflicted.
    pub fn issue_cursor(
        &self,
        after: RelatedEdgeKey,
    ) -> Result<RelatedPageCursor, RelatedPageError> {
        if self.after().is_some_and(|previous| after <= previous) {
            return Err(RelatedPageError::CursorMismatch);
        }
        let mut cursor = RelatedPageCursor {
            scope: self.request.scope.clone(),
            semantic_revision: self.revision,
            after,
            seal: [0; 32],
        };
        cursor.seal = crate::seal_native_owner_statement(self.secret, &cursor.statement())
            .map_err(|_| RelatedPageError::CursorMismatch)?;
        Ok(cursor)
    }
}

/// One edge-oriented result page. Each key corresponds to the relation at the
/// same index in the snapshot; the center and endpoints are selected rows.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RelatedEdgePage {
    /// Selected center and edge endpoint rows, with graph evidence.
    pub snapshot: crate::ViewSnapshot,
    /// Strictly ordered identities of the emitted edges.
    pub keys: Box<[RelatedEdgeKey]>,
    /// Exact owner-selected semantic publication and source-policy revision.
    pub semantic_revision: [u8; 32],
    /// Completion or continuation after the last consumed identity.
    pub terminal: RelatedEdgeTerminal,
    /// Some selected evidence was unavailable or conflicted.
    pub withheld: bool,
}

/// Completion state for a bounded related-edge page.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RelatedEdgeTerminal {
    /// No further edge identities remain.
    Complete,
    /// Continue strictly after the last consumed identity.
    More(RelatedPageCursor),
}

/// Invalid bounded edge query or continuation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelatedPageError {
    /// Delivery exceeds the admitted bounded window.
    PageBound,
    /// Scope, publication, owner or frontier differs from the issued cursor.
    CursorMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> RelatedPageScope {
        RelatedPageScope {
            root: [1; 32],
            source: [2; 32],
            source_selected: true,
            direction: RelatedDirection::Both,
            issued: 10,
            delivery: 10,
            windowed: false,
        }
    }
    fn key(n: u8) -> RelatedEdgeKey {
        RelatedEdgeKey {
            from: RelatedRowKey::Symbol([2; 32]),
            to: RelatedRowKey::Symbol([n; 32]),
            relation: SemanticLinkKind::Calls,
        }
    }
    #[test]
    fn continuation_is_bound_to_scope_revision_owner_and_exclusive_frontier() {
        let secret = [3; 32];
        let revision = [4; 32];
        let request = RelatedPageRequest {
            scope: scope(),
            cursor: None,
        };
        let verified = VerifiedRelatedPageRequest::verify(&request, revision, &secret).unwrap();
        let cursor = verified.issue_cursor(key(5)).unwrap();
        let resumed = RelatedPageRequest {
            cursor: Some(cursor.clone()),
            ..request.clone()
        };
        let verified = VerifiedRelatedPageRequest::verify(&resumed, revision, &secret).unwrap();
        assert_eq!(verified.after(), Some(key(5)));
        assert!(verified.issue_cursor(key(5)).is_err());
        assert!(verified.issue_cursor(key(4)).is_err());
        assert!(verified.issue_cursor(key(6)).is_ok());
        assert!(VerifiedRelatedPageRequest::verify(&resumed, [9; 32], &secret).is_err());
        assert!(VerifiedRelatedPageRequest::verify(&resumed, revision, &[9; 32]).is_err());
        let mut tampered = resumed.clone();
        tampered.cursor.as_mut().unwrap().after = key(6);
        assert!(VerifiedRelatedPageRequest::verify(&tampered, revision, &secret).is_err());
        let mut windowed_first = request.clone();
        windowed_first.scope.windowed = true;
        let first = VerifiedRelatedPageRequest::verify(&windowed_first, revision, &secret).unwrap();
        let windowed_cursor = first.issue_cursor(key(5)).unwrap();
        let mut narrower = windowed_first.clone();
        narrower.scope.delivery = 5;
        narrower.cursor = Some(windowed_cursor.clone());
        assert!(VerifiedRelatedPageRequest::verify(&narrower, revision, &secret).is_ok());
        narrower.scope.issued = 9;
        assert!(VerifiedRelatedPageRequest::verify(&narrower, revision, &secret).is_err());
        let mut tampered_window = windowed_cursor;
        tampered_window.scope.delivery = 5;
        narrower.scope.issued = 10;
        narrower.cursor = Some(tampered_window);
        assert!(VerifiedRelatedPageRequest::verify(&narrower, revision, &secret).is_err());
        for changed in [
            RelatedPageScope {
                root: [9; 32],
                ..scope()
            },
            RelatedPageScope {
                source: [9; 32],
                ..scope()
            },
            RelatedPageScope {
                source_selected: false,
                ..scope()
            },
            RelatedPageScope {
                direction: RelatedDirection::Outgoing,
                ..scope()
            },
            RelatedPageScope {
                issued: 9,
                delivery: 9,
                ..scope()
            },
        ] {
            let foreign = RelatedPageRequest {
                scope: changed,
                cursor: Some(cursor.clone()),
            };
            assert!(VerifiedRelatedPageRequest::verify(&foreign, revision, &secret).is_err());
        }
    }
    #[test]
    fn edge_order_keeps_distinct_relations_and_ignores_confidence() {
        let from = crate::RowId::Symbol(crate::symbol_key("a"));
        let to = crate::RowId::Symbol(crate::symbol_key("b"));
        let calls = crate::GraphRelation::new(from, to, SemanticLinkKind::Calls);
        let weak = calls.with_confidence(crate::SemanticConfidence::Syntactic);
        let reads = crate::GraphRelation::new(from, to, SemanticLinkKind::Reads);
        assert_eq!(RelatedEdgeKey::new(calls), RelatedEdgeKey::new(weak));
        assert_ne!(RelatedEdgeKey::new(calls), RelatedEdgeKey::new(reads));
        let mut keys = [RelatedEdgeKey::new(reads), RelatedEdgeKey::new(calls)];
        keys.sort();
        assert_eq!(keys[0], RelatedEdgeKey::new(calls));
        assert_eq!(keys[1], RelatedEdgeKey::new(reads));
    }
    #[test]
    fn invalid_windows_fail_before_traversal() {
        for (issued, delivery, windowed) in [
            (0, 0, false),
            (201, 201, false),
            (10, 0, true),
            (10, 11, true),
            (10, 9, false),
        ] {
            let request = RelatedPageRequest {
                scope: RelatedPageScope {
                    issued,
                    delivery,
                    windowed,
                    ..scope()
                },
                cursor: None,
            };
            assert!(matches!(
                VerifiedRelatedPageRequest::verify(&request, [4; 32], &[3; 32]),
                Err(RelatedPageError::PageBound)
            ));
        }
    }
}
