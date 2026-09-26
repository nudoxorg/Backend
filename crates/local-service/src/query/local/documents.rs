//! Selected-view documents for one local search coordinator.
//!
//! The coordinator admits the snapshot. This module turns that snapshot into
//! lexical fields, semantic text, and stable cross-index identities.

use super::{QueryError, SelectedDocuments, SemanticDocument};
use backend_engine::{RowId, ViewRoot, WorkspaceRoot};
use backend_extension_tantivy as lexical;
use backend_extension_trustfall::{SemanticQueryCorpus, SemanticQueryPresentation};
use backend_semantic::{Entity, EntityId, Source};
use std::collections::BTreeMap;

pub(super) fn collect_selected_documents(
    workspace: WorkspaceRoot,
    view: &ViewRoot,
    semantic_evidence: &SemanticQueryCorpus,
) -> Result<SelectedDocuments, QueryError> {
    let capacity = usize::try_from(view.row_count()).map_err(|_| QueryError::CorpusLimit)?;
    let mut documents = Vec::with_capacity(capacity);
    let mut entities = BTreeMap::new();
    let mut candidates = BTreeMap::new();
    let mut document_bytes = 0usize;
    let mut selected_rows = BTreeMap::new();
    let mut cursor = backend_engine::ViewPageCursor::first(view);
    loop {
        let page = view
            .page(cursor, backend_engine::MAX_SNAPSHOT_PAGE_ROWS)
            .map_err(|_| QueryError::InvalidView)?;
        for row in page.rows() {
            if selected_rows.insert(row.id.stable_key(), row.id).is_some() {
                return Err(QueryError::IdentityCollision);
            }
        }
        let Some(next) = page.next() else { break };
        cursor = next;
    }
    // A producer can mint a synthetic child fact that carries its own
    // owning function's name for identity purposes only: a function's
    // return-type "result slot", admitted as a `Parameter`-kind fact
    // literally named like the function (see `driver/lower/python.rs` and
    // `driver/lower/rust.rs`). That name was never chosen for user-facing
    // lookup; it exists so the slot's content-addressed coordinate has one.
    // Indexing it under the lexical "name" field anyway turns every name
    // search for the function into two hits.
    //
    // A real, distinct declaration can coincidentally share its exact
    // spelling with its parent -- a constructor (`class Foo { Foo() {} }`),
    // a Rust `mod foo { pub fn foo() }`, a method named like its class -- so
    // name-sharing alone is not a safe signal; those must stay searchable.
    // What is unique to the synthetic slot is its *kind*: it is the only
    // "variable"-presented fact whose immediate parent is itself callable
    // (a function or method). A real field, constructor, or nested function
    // never presents as `variable`, so requiring `variable` kind and a
    // callable parent, on top of the name match, narrows this to exactly
    // the synthetic result slot without a new IR/wire bit.
    let facts_by_id: BTreeMap<&str, &SemanticQueryPresentation> = semantic_evidence
        .facts()
        .iter()
        .map(|fact| {
            let presentation = fact.presentation();
            (presentation.id.as_str(), presentation)
        })
        .collect();
    let mut semantic_documents = Vec::with_capacity(semantic_evidence.facts().len());
    for fact in semantic_evidence.facts() {
        let presentation = fact.presentation();
        let row = selected_rows
            .remove(&presentation.id)
            .ok_or(QueryError::InvalidSemanticEvidence)?;
        {
            let entity = entity_id(workspace, row)?;
            let candidate = candidate_id(entity, semantic_evidence.evidence_digest())?;
            if entities.insert(entity, row).is_some()
                || candidates.insert(candidate, entity).is_some()
            {
                return Err(QueryError::IdentityCollision);
            }
            let is_synthetic_result_slot = presentation.kind == "variable"
                && presentation
                    .parent
                    .as_deref()
                    .and_then(|parent| facts_by_id.get(parent))
                    .is_some_and(|parent_fact| {
                        parent_fact.name == presentation.name
                            && matches!(parent_fact.kind.as_str(), "function" | "method")
                    });
            // The coordinate itself ends in `::<name>`, so merely dropping
            // the "name" field would still leave this fact lexically
            // matchable through its own "coordinate" field. Leaving the row
            // out of the lexical corpus entirely is the only way a name
            // search for the parent stops finding it too; it stays a normal
            // member of `entities`/`candidates`/`semantic_documents`; so
            // direct coordinate lookups (`show`) and the semantic/vector
            // lane are unaffected.
            if !is_synthetic_result_slot {
                let row_fields = fields(presentation);
                document_bytes = checked_document_bytes(document_bytes, &row_fields)?;
                documents.push((entity, row_fields));
            }
            semantic_documents.push(SemanticDocument {
                row,
                text: semantic_text(presentation),
            });
        }
    }
    if !selected_rows.is_empty() {
        return Err(QueryError::InvalidSemanticEvidence);
    }
    Ok((
        documents,
        entities,
        candidates,
        semantic_documents.into_boxed_slice(),
    ))
}

fn checked_document_bytes(
    initial: usize,
    fields: &[(String, String)],
) -> Result<usize, QueryError> {
    let bytes = fields.iter().try_fold(initial, |bytes, (field, text)| {
        bytes.checked_add(field.len())?.checked_add(text.len())
    });
    match bytes {
        Some(bytes) if bytes <= lexical::Limits::default().max_total_text_bytes => Ok(bytes),
        Some(_) | None => Err(QueryError::CorpusLimit),
    }
}

pub(in crate::builtin::query) fn entity_id(
    workspace: WorkspaceRoot,
    id: RowId,
) -> Result<EntityId, QueryError> {
    let mut namespace = [0_u8; 16];
    namespace.copy_from_slice(&workspace.as_bytes()[..16]);
    let source = Source::new(u128::from_be_bytes(namespace), "published-view")
        .map_err(|_| QueryError::InvalidView)?;
    let entity = Entity::new(source, id.stable_key(), None).map_err(|_| QueryError::InvalidView)?;
    Ok(EntityId::from_value(&entity))
}

pub(in crate::builtin::query) fn candidate_id(
    entity: EntityId,
    evidence: [u8; 32],
) -> Result<backend_extension_qdrant::CandidateId, QueryError> {
    let mut identity = blake3::Hasher::new();
    identity.update(b"backend.qdrant.semantic-evidence-candidate.v3\0");
    identity.update(&evidence);
    identity.update(entity.as_bytes());
    let digest = identity.finalize();
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest.as_bytes()[..8]);
    let value = u64::from_be_bytes(bytes);
    backend_extension_qdrant::CandidateId::new(value).map_err(|_| QueryError::IdentityCollision)
}

fn fields(row: &SemanticQueryPresentation) -> Vec<(String, String)> {
    let mut fields = vec![
        ("coordinate".to_owned(), row.coordinate.clone()),
        ("kind".to_owned(), row.kind.clone()),
        ("name".to_owned(), row.name.clone()),
    ];
    if let Some(signature) = &row.signature {
        fields.push(("signature".to_owned(), signature.clone()));
    }
    if !row.documentation.is_empty() {
        fields.push(("documentation".to_owned(), row.documentation.clone()));
    }
    fields.sort();
    fields
}

fn semantic_text(row: &SemanticQueryPresentation) -> String {
    let fields = fields(row);
    let mut text = String::new();
    for (name, value) in fields {
        text.push_str(&name);
        text.push('\n');
        text.push_str(&value);
        text.push('\n');
    }
    text
}

pub(super) fn view_identity(view: &ViewRoot) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(192);
    bytes.extend_from_slice(view.version().as_bytes());
    bytes.extend_from_slice(view.root().as_bytes());
    bytes.extend_from_slice(view.basis().root.as_bytes());
    bytes.extend_from_slice(view.basis().object.as_bytes());
    bytes.extend_from_slice(view.frontier().root.as_bytes());
    bytes
}

pub(super) fn semantic_read_identity(view: &ViewRoot, evidence: [u8; 32]) -> Vec<u8> {
    let mut bytes = view_identity(view);
    bytes.extend_from_slice(&evidence);
    bytes
}
