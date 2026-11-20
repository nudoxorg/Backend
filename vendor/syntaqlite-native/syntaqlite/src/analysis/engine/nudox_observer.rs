// Copyright 2026 Nudox contributors. Apache-2.0.
//! Bounded observation of the native semantic walk. These are occurrence
//! coordinates, not resolved providers, statement access modes, or proof that
//! a complete source document was enumerated.
use super::walker::{ColumnRefEvent, SemanticVisitor, SourceRefEvent, WalkCtx};
use crate::analysis::catalog::ColumnResolution;
use syntaqlite_syntax::any::{AnyNodeId, AnyParsedStatement, FieldValue};

/// Native syntaqlite semantic event family.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeReferenceKind {
    /// A table or view name occurrence.
    Relation,
    /// A column name occurrence.
    Column,
}

/// Document-absolute UTF-8 byte coordinates supplied by the semantic walker.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct NativeReference {
    /// Event family.
    pub kind: NativeReferenceKind,
    /// Inclusive start byte of the occurrence in the document.
    pub start: u32,
    /// Exclusive end byte of the occurrence in the document.
    pub end: u32,
    /// Native resolution is only evidence relative to the provided catalog;
    /// it does not establish a deployed provider.
    pub resolved: bool,
}

/// The observer stopped retaining events when its explicit bound was reached.
pub struct NativeReferenceObservation {
    /// Retained events in walk order, at most the requested bound.
    pub references: Vec<NativeReference>,
    /// True when events beyond the bound were dropped.
    pub limit_reached: bool,
}

/// Widen a dequoted name range to the written token. The parser's name spans exclude the
/// enclosing quote/bracket/backtick; an occurrence coordinate is the written spelling.
/// `idx` is a span field of `node`, or a node field (a qualified name) whose first field is
/// the name span. Unquoted names, and anything that cannot be resolved, are returned as is.
fn written_range(
    stmt: &AnyParsedStatement<'_>,
    node: AnyNodeId,
    idx: u8,
    start: u32,
    end: u32,
) -> (u32, u32) {
    let span_at = |id: AnyNodeId, idx: usize| {
        let (_, fields) = stmt.extract_fields(id)?;
        (idx < fields.len()).then(|| fields[idx])
    };
    let quoted = match span_at(node, idx as usize) {
        Some(FieldValue::Span(span)) => span.is_quoted(),
        Some(FieldValue::NodeId(qualified)) if !qualified.is_null() => {
            let object = stmt
                .extract_fields(qualified)
                .and_then(|(_, f)| f.node_id_at(0));
            matches!(
                object.and_then(|o| span_at(o, 0)),
                Some(FieldValue::Span(span)) if span.is_quoted()
            )
        }
        _ => false,
    };
    match (quoted, start.checked_sub(1), end.checked_add(1)) {
        (true, Some(s), Some(e)) => (s, e),
        _ => (start, end),
    }
}

pub(super) struct Observer {
    pub result: NativeReferenceObservation,
    pub limit: usize,
}
impl Observer {
    fn push(&mut self, kind: NativeReferenceKind, start: u32, end: u32, resolved: bool) {
        if self.result.references.len() >= self.limit {
            self.result.limit_reached = true;
            return;
        }
        self.result.references.push(NativeReference {
            kind,
            start,
            end,
            resolved,
        });
    }
}
impl SemanticVisitor for Observer {
    const WANTS_SOURCE_REF: bool = true;
    const WANTS_COLUMN_REF: bool = true;
    fn on_source_ref(
        &mut self,
        stmt: &mut AnyParsedStatement<'_>,
        _: &mut WalkCtx<'_>,
        event: SourceRefEvent<'_>,
    ) {
        let (start, end) = written_range(
            stmt,
            event.node_id,
            event.name_idx,
            event.range.start.as_u32(),
            event.range.end.as_u32(),
        );
        self.push(NativeReferenceKind::Relation, start, end, event.resolved);
    }
    fn on_column_ref(
        &mut self,
        stmt: &mut AnyParsedStatement<'_>,
        _: &mut WalkCtx<'_>,
        event: ColumnRefEvent<'_>,
    ) {
        let (start, end) = written_range(
            stmt,
            event.node_id,
            event.column_idx,
            event.range.start.as_u32(),
            event.range.end.as_u32(),
        );
        self.push(
            NativeReferenceKind::Column,
            start,
            end,
            matches!(event.resolution, ColumnResolution::Found { .. }),
        );
    }
}
