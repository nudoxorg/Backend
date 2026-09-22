//! Real Tantivy-backed lexical source with canonical ranking at the adapter boundary.

use crate::{
    Binding, Cursor, DocumentState, Error, FieldSelection, LexicalPage, LexicalSource, Limits,
    MatchMode, Query, QueryRequest, RankedHit, Relevance, SchemaVersion,
};
use backend_semantic::EntityId;
use backend_version::CoverageWitness;
use std::{collections::BTreeMap, path::Path};
use tantivy::{
    DocAddress, Index, IndexReader, Searcher, TantivyDocument, Term, doc,
    query::{BooleanQuery, FuzzyTermQuery, Occur, Query as TantivyQuery, TermQuery},
    schema::{Field, IndexRecordOption, STORED, STRING, Schema, Value},
};

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

impl TantivySource {
    /// Builds the standard in-memory local-service adapter in one admitted step.
    ///
    /// # Errors
    ///
    /// Returns a typed contract or Tantivy construction failure.
    pub fn local_adapter(
        state: &DocumentState,
        limits: Limits,
    ) -> Result<TantivyAdapter, TantivySourceError> {
        Ok(TantivyAdapter::new(Self::build(state, limits)?, limits)?)
    }

    /// Builds and commits a concrete Tantivy projection from complete reconciled state.
    ///
    /// # Errors
    ///
    /// Returns a typed contract or Tantivy construction failure.
    pub fn build(state: &DocumentState, limits: Limits) -> Result<Self, TantivySourceError> {
        let limits = limits.validate()?;
        if !matches!(state.coverage(), CoverageWitness::Complete(_)) {
            return Err(Error::IncompleteCoverage.into());
        }
        let (schema, raw_token, folded_token, ranking_token, field_name, ordinal) =
            projection_schema();
        let index = Index::create_in_ram(schema);
        Self::populate(
            state,
            limits,
            index,
            raw_token,
            folded_token,
            ranking_token,
            field_name,
            ordinal,
        )
    }

    /// Reopens a committed directory only when its schema and complete state
    /// binding match the supplied authoritative snapshot.
    ///
    /// # Errors
    ///
    /// Returns a typed I/O, schema, binding, coverage, or Tantivy failure.
    pub fn open_in_dir(
        state: &DocumentState,
        limits: Limits,
        directory: impl AsRef<Path>,
    ) -> Result<Self, TantivySourceError> {
        let limits = limits.validate()?;
        if !matches!(state.coverage(), CoverageWitness::Complete(_)) {
            return Err(Error::IncompleteCoverage.into());
        }
        let persisted = std::fs::read(directory.as_ref().join(BINDING_FILE))?;
        if persisted.as_slice() != projection_fingerprint(state.binding()) {
            return Err(Error::StaleRoot.into());
        }
        let index = Index::open_in_dir(directory)?;
        let (schema, raw_token, folded_token, ranking_token, field_name, ordinal) =
            projection_schema();
        if index.schema() != schema {
            return Err(Error::SchemaDrift.into());
        }
        let reader = index.reader()?;
        let expected_tokens = state
            .iter()
            .flat_map(|(_, fields)| fields.iter())
            .flat_map(|(_, text)| searchable_tokens(&text))
            .count();
        let indexed_tokens =
            usize::try_from(reader.searcher().num_docs()).map_err(|_| Error::SizeLimit)?;
        if indexed_tokens != expected_tokens {
            return Err(Self::corrupt(
                "indexed token count does not match bound state",
            ));
        }
        Ok(Self {
            binding: state.binding(),
            coverage: state.coverage(),
            limits,
            _index: index,
            reader,
            raw_token,
            folded_token,
            ranking_token,
            field_name,
            ordinal,
            documents: state.iter().map(|(document, _)| document).collect(),
        })
    }

    fn corrupt(detail: &'static str) -> TantivySourceError {
        TantivySourceError::Corrupt(detail)
    }

    /// Builds and commits the projection in a durable Tantivy directory.
    ///
    /// The supplied state remains the authority for semantic identities and the
    /// complete root binding; Tantivy stores only the replaceable search projection.
    ///
    /// # Errors
    ///
    /// Returns a typed I/O, contract, or Tantivy construction failure.
    pub fn build_in_dir(
        state: &DocumentState,
        limits: Limits,
        directory: impl AsRef<Path>,
    ) -> Result<Self, TantivySourceError> {
        let limits = limits.validate()?;
        if !matches!(state.coverage(), CoverageWitness::Complete(_)) {
            return Err(Error::IncompleteCoverage.into());
        }
        let (schema, raw_token, folded_token, ranking_token, field_name, ordinal) =
            projection_schema();
        let index = Index::create_in_dir(directory.as_ref(), schema)?;
        let source = Self::populate(
            state,
            limits,
            index,
            raw_token,
            folded_token,
            ranking_token,
            field_name,
            ordinal,
        )?;
        std::fs::write(
            directory.as_ref().join(BINDING_FILE),
            projection_fingerprint(state.binding()),
        )?;
        Ok(source)
    }

    fn populate(
        state: &DocumentState,
        limits: Limits,
        index: Index,
        raw_token: Field,
        folded_token: Field,
        ranking_token: Field,
        field_name: Field,
        ordinal: Field,
    ) -> Result<Self, TantivySourceError> {
        let mut writer = index.writer(WRITER_MEMORY_BYTES)?;
        let mut documents = Vec::new();
        for (document, fields) in state.iter() {
            let document_ordinal = u64::try_from(documents.len()).map_err(|_| Error::SizeLimit)?;
            documents.push(document);
            for (field, text) in fields {
                for token in searchable_tokens(&text) {
                    writer.add_document(doc!(
                        raw_token => token.searchable.as_str(),
                        folded_token => token.searchable.to_ascii_lowercase(),
                        ranking_token => token.ranking.as_str(),
                        field_name => field.as_str(),
                        ordinal => document_ordinal,
                    ))?;
                }
            }
        }
        writer.commit()?;
        let reader = index.reader()?;
        Ok(Self {
            binding: state.binding(),
            coverage: state.coverage(),
            limits,
            _index: index,
            reader,
            raw_token,
            folded_token,
            ranking_token,
            field_name,
            ordinal,
            documents,
        })
    }

    /// Executes every clause against Tantivy, reconciles identities globally, then ranks.
    ///
    /// # Errors
    ///
    /// Returns a typed query-admission, index-read, or projection-integrity failure.
    pub fn search(&self, query: &Query) -> Result<Vec<RankedHit>, TantivySourceError> {
        query.validate(self.limits)?;
        if query.terms.is_empty() {
            return Ok(self
                .documents
                .iter()
                .copied()
                .map(|document| RankedHit {
                    document,
                    relevance: Relevance::all_documents(),
                })
                .collect());
        }
        let mut candidates = self.query_clause_candidates(&query.terms[0], query)?;
        for term in query.terms.iter().skip(1) {
            let posting = self.query_clause_candidates(term, query)?;
            let mut intersection = BTreeMap::new();
            for (document, relevance) in candidates {
                if let Some(next) = posting.get(&document) {
                    intersection.insert(document, relevance.combine(*next)?);
                }
            }
            candidates = intersection;
        }
        let mut hits = candidates
            .into_iter()
            .map(|(document, relevance)| RankedHit {
                document,
                relevance,
            })
            .collect::<Vec<_>>();
        hits.sort_unstable_by(|left, right| {
            right
                .relevance
                .cmp(&left.relevance)
                .then_with(|| left.document.cmp(&right.document))
        });
        Ok(hits)
    }

    fn query_clause_candidates(
        &self,
        term: &str,
        query: &Query,
    ) -> Result<BTreeMap<EntityId, Relevance>, TantivySourceError> {
        let engine_query = self.compile_clause(term, query);
        let searcher = self.reader.searcher();
        let count = usize::try_from(searcher.num_docs()).map_err(|_| Error::SizeLimit)?;
        if count == 0 {
            return Ok(BTreeMap::new());
        }
        let matches = searcher.search(
            engine_query.as_ref(),
            &tantivy::collector::TopDocs::with_limit(count).order_by_score(),
        )?;
        let mut ranked = BTreeMap::new();
        for (_, address) in matches {
            let (document, relevance) = self.admit_engine_match(&searcher, address, term)?;
            ranked
                .entry(document)
                .and_modify(|current: &mut Relevance| *current = (*current).max(relevance))
                .or_insert(relevance);
        }
        Ok(ranked)
    }

    fn compile_clause(&self, term: &str, query: &Query) -> Box<dyn TantivyQuery> {
        let token_field = match query.case {
            crate::CaseSensitivity::Sensitive => self.raw_token,
            crate::CaseSensitivity::FoldAscii => self.folded_token,
        };
        let token = Term::from_field_text(token_field, term);
        let token_query: Box<dyn TantivyQuery> = match query.match_mode {
            MatchMode::Exact => Box::new(TermQuery::new(token, IndexRecordOption::Basic)),
            MatchMode::Prefix => Box::new(FuzzyTermQuery::new_prefix(token, 0, true)),
        };
        match &query.fields {
            FieldSelection::All => token_query,
            FieldSelection::Only(field) => Box::new(BooleanQuery::new(vec![
                (Occur::Must, token_query),
                (
                    Occur::Must,
                    Box::new(TermQuery::new(
                        Term::from_field_text(self.field_name, field),
                        IndexRecordOption::Basic,
                    )),
                ),
            ])),
        }
    }

    fn admit_engine_match(
        &self,
        searcher: &Searcher,
        address: DocAddress,
        term: &str,
    ) -> Result<(EntityId, Relevance), TantivySourceError> {
        let stored: TantivyDocument = searcher.doc(address)?;
        let ordinal = stored
            .get_first(self.ordinal)
            .and_then(|value| value.as_u64())
            .and_then(|value| usize::try_from(value).ok())
            .ok_or(TantivySourceError::Corrupt("document ordinal is missing"))?;
        let document = self
            .documents
            .get(ordinal)
            .copied()
            .ok_or(TantivySourceError::Corrupt(
                "document ordinal is outside the binding",
            ))?;
        let matched = stored
            .get_first(self.ranking_token)
            .and_then(|value| value.as_str())
            .ok_or(TantivySourceError::Corrupt("matched token is missing"))?;
        let field = stored
            .get_first(self.field_name)
            .and_then(|value| value.as_str())
            .ok_or(TantivySourceError::Corrupt("field name is missing"))?;
        Ok((
            document,
            Relevance::new(term.len(), matched.len(), field_weight(field), 1)?,
        ))
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
