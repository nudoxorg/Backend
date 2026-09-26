//! Real Tantivy-backed lexical source with canonical ranking at the adapter boundary.

use crate::{
    Binding, Cursor, Error, LexicalPage, LexicalSource, Limits, QueryRequest, SchemaVersion,
};
use backend_semantic::EntityId;
use backend_version::CoverageWitness;
use tantivy::{
    Index, IndexReader,
    schema::{Field, STORED, STRING, Schema},
};

mod source;

const WRITER_MEMORY_BYTES: usize = 15_000_000;
const BINDING_FILE: &str = "backend-binding-v2";

/// A real in-memory Tantivy projection pinned to one exact lexical binding.
pub struct TantivySource {
    binding: Binding,
    coverage: CoverageWitness,
    limits: Limits,
    _index: Index,
    reader: IndexReader,
    raw_token: Field,
    folded_token: Field,
    ranking_token: Field,
    field_name: Field,
    ordinal: Field,
    documents: Vec<EntityId>,
}

/// Fully admitted local query adapter backed by a concrete Tantivy index.
pub type TantivyAdapter = crate::Adapter<TantivySource>;

/// Typed failure from concrete Tantivy construction or execution.
#[derive(Debug)]
pub enum TantivySourceError {
    /// Portable admission rejected the request or state.
    Contract(Error),
    /// Tantivy rejected index construction or query execution.
    Backend(tantivy::TantivyError),
    /// The durable projection directory could not be read or committed.
    Io(std::io::Error),
    /// Tantivy returned a document outside the verified projection mapping.
    Corrupt(&'static str),
}

impl std::fmt::Display for TantivySourceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Contract(error) => error.fmt(formatter),
            Self::Backend(error) => write!(formatter, "Tantivy backend failed: {error}"),
            Self::Io(error) => write!(formatter, "Tantivy projection I/O failed: {error}"),
            Self::Corrupt(detail) => write!(formatter, "Tantivy projection is corrupt: {detail}"),
        }
    }
}

impl std::error::Error for TantivySourceError {}

impl From<Error> for TantivySourceError {
    fn from(error: Error) -> Self {
        Self::Contract(error)
    }
}

impl From<tantivy::TantivyError> for TantivySourceError {
    fn from(error: tantivy::TantivyError) -> Self {
        Self::Backend(error)
    }
}

impl From<std::io::Error> for TantivySourceError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Eq, Ord, PartialEq, PartialOrd)]
struct SearchableToken {
    searchable: String,
    ranking: String,
}

fn searchable_tokens(text: &str) -> Vec<SearchableToken> {
    // The projection only needs canonical order after tokenization.  A tree
    // allocates one node per token while this bounded vector can sort and
    // deduplicate in place, retaining the same `(searchable, ranking)` set
    // with fewer allocations and better locality during cold ingest.
    let mut tokens = Vec::new();
    for raw in text.split_whitespace().filter(|token| !token.is_empty()) {
        tokens.push(SearchableToken {
            searchable: raw.to_owned(),
            ranking: raw.to_owned(),
        });
        for identifier in raw.split(|character: char| !character.is_alphanumeric()) {
            if identifier.is_empty() {
                continue;
            }
            tokens.push(SearchableToken {
                searchable: identifier.to_owned(),
                ranking: raw.to_owned(),
            });
            tokens.extend(identifier_words(identifier).map(|word| SearchableToken {
                searchable: word.to_owned(),
                ranking: raw.to_owned(),
            }));
        }
    }
    tokens.sort_unstable();
    tokens.dedup();
    tokens
}

fn identifier_words(identifier: &str) -> impl Iterator<Item = &str> {
    let mut words = Vec::new();
    let mut characters = identifier.char_indices().peekable();
    let Some((_, mut previous)) = characters.next() else {
        return words.into_iter();
    };
    let mut start = 0;
    while let Some((index, current)) = characters.next() {
        let next = characters.peek().map(|(_, character)| *character);
        let case_boundary = (previous.is_lowercase() && current.is_uppercase())
            || (previous.is_uppercase()
                && current.is_uppercase()
                && next.is_some_and(char::is_lowercase));
        let class_boundary = previous.is_numeric() != current.is_numeric();
        if case_boundary || class_boundary {
            let end = index;
            if start < end {
                words.push(&identifier[start..end]);
            }
            start = end;
        }
        previous = current;
    }
    if start < identifier.len() {
        words.push(&identifier[start..]);
    }
    words.into_iter()
}

fn projection_schema() -> (Schema, Field, Field, Field, Field, Field) {
    let mut schema = Schema::builder();
    let raw_token = schema.add_text_field("raw_token", STRING | STORED);
    let folded_token = schema.add_text_field("folded_token", STRING);
    let ranking_token = schema.add_text_field("ranking_token", STORED);
    let field_name = schema.add_text_field("field_name", STRING | STORED);
    let ordinal = schema.add_u64_field("document_ordinal", STORED);
    (
        schema.build(),
        raw_token,
        folded_token,
        ranking_token,
        field_name,
        ordinal,
    )
}

fn projection_fingerprint(binding: Binding) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"backend-extension-tantivy/projection/v2");
    hasher.update(binding.workspace.as_bytes());
    hasher.update(binding.root.as_bytes());
    hasher.update(binding.recipe.as_bytes());
    hasher.update(binding.authority.as_bytes());
    hasher.update(binding.read_manifest.as_bytes());
    hasher.update(binding.frontier.as_bytes());
    *hasher.finalize().as_bytes()
}

impl LexicalSource for TantivySource {
    type Error = TantivySourceError;

    fn fetch(&self, request: &QueryRequest) -> Result<LexicalPage, Self::Error> {
        if request.binding != self.binding {
            return Err(Error::StaleRoot.into());
        }
        if request.limit == 0 || request.limit > self.limits.max_page {
            return Err(Error::SizeLimit.into());
        }
        if let Some(cursor) = request.cursor
            && (cursor.binding() != self.binding || cursor.query() != request.query.version)
        {
            return Err(Error::StaleCursor.into());
        }
        let hits = self.search(&request.query)?;
        let offset = request.cursor.map_or(0, Cursor::offset);
        if offset > hits.len() {
            return Err(Error::InvalidCursor.into());
        }
        let end = offset
            .checked_add(request.limit)
            .ok_or(Error::SizeLimit)?
            .min(hits.len());
        Ok(LexicalPage {
            schema: SchemaVersion::CURRENT,
            binding: self.binding,
            query: request.query.version,
            hits: hits[offset..end].to_vec(),
            next: (end < hits.len()).then(|| Cursor::new(self.binding, request.query.version, end)),
            coverage: self.coverage,
        })
    }
}

fn field_weight(field: &str) -> u16 {
    match field {
        "name" => 4,
        "signature" => 3,
        "documentation" => 2,
        _ => 1,
    }
}
