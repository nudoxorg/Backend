//! Lossless windows of an already admitted declaration's members and relations.
//!
//! These cursors confer no producer authority. Each request first repeats the
//! normal exact document, member and relation admissions, then checks a content
//! commitment including the complete source basis. They cannot turn stale page
//! bytes into a current read or hide a failed required subread.
//! Each section request repeats exact owner/direct-member and relation admissions.
//! Member selection seeks the retained parent arrangement, never the package outline.

use backend_library::{Document, DocumentVersion, encode_id};
use serde::{Deserialize, Serialize};

use crate::{Answer, Detail, Fault, Page};

const ROWS: usize = 16;
// Leave room for the MCP wrapper and duplicate readable preview. Final CLI/MCP
// admission still measures the complete actual envelope under its own ceiling.
const PAGE_BYTES: usize = 32 * 1024;
const PAGE_TOKENS: usize = 8_000;

/// Copy this complete arguments object to backend.document. Selected bytes
/// remain a locator; the ordinary owner read must prove its current membership.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentContinuation {
    /// Exact label asserted for the selected row.
    pub coordinate: String,
    /// Full public selected-symbol-id object, never its display abbreviation.
    pub selected_symbol: crate::IdentitySemanticData,
    /// Exact source/content-bound section position.
    pub cursor: String,
}

impl DocumentContinuation {
    /// Executable CLI arguments equivalent to this MCP arguments object.
    #[must_use]
    pub fn cli_arguments(&self) -> Vec<String> {
        let crate::IdentitySemanticData::SelectedSymbolId(symbol) = &self.selected_symbol;
        vec![
            "show".to_owned(),
            self.coordinate.clone(),
            "--selected-id".to_owned(),
            encode_id(symbol.bytes()),
            "--cursor".to_owned(),
            self.cursor.clone(),
        ]
    }
}

/// Extent of member/relation rows in this declaration response. Documentation,
/// signature, exact selected identity, source availability and faults repeat on
/// every page. Member rows precede relation rows, in their original group order.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentSectionPage {
    /// First included section row (zero based).
    pub start: usize,
    /// First row after this page.
    pub end: usize,
    /// Exact total member rows in the admitted projection.
    pub total_members: usize,
    /// Exact total relation rows in the admitted projection.
    pub total_relations: usize,
    /// Opaque content/source-bound continuation; absent only at the terminal page.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    /// Complete next-call arguments, preserving the exact selected ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub continuation: Option<DocumentContinuation>,
}

impl DocumentSectionPage {
    pub(crate) fn description(&self, _coordinate: &str) -> String {
        let extent = format!(
            "Declaration sections {}..{} of {} ({} members, {} relations).",
            self.start,
            self.end,
            self.total_members + self.total_relations,
            self.total_members,
            self.total_relations
        );
        match &self.next_cursor {
            Some(_) => {
                let command = self
                    .continuation
                    .as_ref()
                    .map(|next| {
                        let arguments = next.cli_arguments();
                        crate::shell::command("show", &arguments[1..]).unwrap_or_default()
                    })
                    .unwrap_or_default();
                format!(
                    "{extent} More: {command}. MCP: copy section_page.continuation unchanged to backend.document; retain selected_symbol."
                )
            }
            None => format!("{extent} Complete."),
        }
    }
}

fn fit(page: &Page) -> Result<(), Fault> {
    let answer = Answer::Page(Box::new(page.clone()));
    let encoded = crate::encode_answer(&answer, Detail::Full, None, PAGE_BYTES)
        .map_err(crate::oversized_fault)?;
    let text = std::str::from_utf8(&encoded.bytes)
        .map_err(|error| Fault::usage("document", error.to_string()))?;
    let tokens =
        crate::cl100k_tokens(text).map_err(|error| Fault::usage("document", error.to_string()))?;
    if tokens > PAGE_TOKENS {
        return Err(Fault::new(
            crate::FaultSlug::Transport,
            crate::Operand::Whole,
            crate::Cause::new(
                crate::CauseSlug::Oversized,
                format!(
                    "declaration section page has {tokens} cl100k_base/ordinary-v1 tokens, above its {PAGE_TOKENS} token budget"
                ),
            ),
            crate::Affordance::None,
        ));
    }
    Ok(())
}

fn commitment(page: &Page, document: &Document) -> Result<String, Fault> {
    let mut bytes = b"nudox-document-sections-v1\0".to_vec();
    bytes.extend_from_slice(document.symbol.as_bytes());
    bytes.extend_from_slice(document.basis.as_bytes());
    match document.source_basis() {
        Some(source) => {
            bytes.push(1);
            bytes.extend_from_slice(source.root.as_bytes());
            bytes.extend_from_slice(source.object.as_bytes());
            bytes.extend_from_slice(source.branch.as_bytes());
            bytes.extend_from_slice(source.log.as_bytes());
            bytes.extend_from_slice(&source.schema.to_be_bytes());
        }
        None => bytes.push(0),
    }
    // Stream a chain of bounded per-row commitments rather than allocate a
    // second complete document DTO. Field order and group boundaries are part
    // of the preimage; neither a display name nor a short key is an identity.
    let invariant = page.select_sections(0, 0);
    serde_json::to_writer(&mut bytes, &crate::dto::PageDto::new(&invariant))
        .map_err(|error| Fault::usage("document", error.to_string()))?;
    for block in page.prose() {
        let row = match block {
            crate::Prose::Text(text) => serde_json::to_vec(&(0_u8, text, None::<String>)),
            crate::Prose::Code(text) => serde_json::to_vec(&(1_u8, text, None::<String>)),
            crate::Prose::Link { label, target } => {
                serde_json::to_vec(&(2_u8, label, Some(encode_id(target.as_bytes()))))
            }
        }
        .map_err(|error| Fault::usage("document", error.to_string()))?;
        bytes.extend_from_slice(&row);
    }
    let mut digest = DocumentVersion::from_value(&bytes);
    for group in page.members() {
        for member in group.members() {
            let row = (
                "member",
                group.kind().name(),
                crate::dto::IdentityDto::new(member.identity()),
                member.kind().map(|kind| kind.name()),
                member.signature().map(crate::Signature::text),
                member.summary(),
            );
            let row = serde_json::to_vec(&row)
                .map_err(|error| Fault::usage("document", error.to_string()))?;
            let row = DocumentVersion::from_value(&row);
            let mut pair = [0; 64];
            pair[..32].copy_from_slice(digest.as_bytes());
            pair[32..].copy_from_slice(row.as_bytes());
            digest = DocumentVersion::from_value(&pair);
        }
    }
    for group in page.relations() {
        for relation in group.relations() {
            let row = (
                "relation",
                group.label().as_str(),
                crate::dto::IdentityDto::new(relation.identity()),
                relation.kind().map(|kind| kind.name()),
            );
            let row = serde_json::to_vec(&row)
                .map_err(|error| Fault::usage("document", error.to_string()))?;
            let row = DocumentVersion::from_value(&row);
            let mut pair = [0; 64];
            pair[..32].copy_from_slice(digest.as_bytes());
            pair[32..].copy_from_slice(row.as_bytes());
            digest = DocumentVersion::from_value(&pair);
        }
    }
    Ok(encode_id(digest.as_bytes()))
}

pub(crate) fn window(page: Page, document: &Document, cursor: Option<&str>) -> Result<Page, Fault> {
    let total_members = page
        .members()
        .iter()
        .map(|group| group.members().len())
        .sum::<usize>();
    let total_relations = page
        .relations()
        .iter()
        .map(|group| group.relations().len())
        .sum::<usize>();
    let total = total_members
        .checked_add(total_relations)
        .ok_or_else(|| Fault::usage("document", "section count overflow"))?;
    // With no sections to page, retain the existing scalar-only interface:
    // final adapter admission still either emits every byte or refuses it.
    if cursor.is_none() && (total == 0 || (total <= ROWS && fit(&page).is_ok())) {
        return Ok(page);
    }
    let committed = commitment(&page, document)?;
    let start = match cursor {
        None => 0,
        Some(token) => {
            let invalid = || {
                Fault::usage(
                    "cursor",
                    "declaration sections changed or cursor does not match this exact source and coordinate; restart show explicitly",
                )
            };
            if token.len() > 100 {
                return Err(invalid());
            }
            let mut parts = token.split(':');
            if parts.next() != Some("ds1") || parts.next() != Some(committed.as_str()) {
                return Err(invalid());
            }
            let offset = parts
                .next()
                .ok_or_else(invalid)?
                .parse::<usize>()
                .map_err(|_| invalid())?;
            if parts.next().is_some() || offset == 0 || offset >= total {
                return Err(invalid());
            }
            offset
        }
    };
    let mut end = start.saturating_add(ROWS).min(total);
    loop {
        let mut selected = page.select_sections(start, end);
        let next_cursor = (end < total).then(|| format!("ds1:{committed}:{end}"));
        let continuation = next_cursor
            .as_ref()
            .map(|cursor| {
                let selected_id =
                    backend_library::SelectedSymbolId::new(*document.symbol.as_bytes())
                        .ok_or_else(|| {
                            Fault::usage(
                                "document",
                                "cannot continue a declaration without a full selected row ID",
                            )
                        })?;
                Ok::<_, Fault>(DocumentContinuation {
                    coordinate: page.identity().coordinate().as_str().to_owned(),
                    selected_symbol: crate::IdentitySemanticData::SelectedSymbolId(selected_id),
                    cursor: cursor.clone(),
                })
            })
            .transpose()?;
        selected.section_page = Some(DocumentSectionPage {
            start,
            end,
            total_members,
            total_relations,
            next_cursor,
            continuation,
        });
        let failure = match fit(&selected) {
            Ok(()) => return Ok(selected),
            Err(failure) => failure,
        };
        if end <= start + 1 {
            // No scalar is clipped or replaced by synthetic availability.
            // Surface admission retains its normal typed overflow result.
            return Err(failure);
        }
        end = start + (end - start) / 2;
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::{
        Identity, IdentityKey, Language, LineNumber, Member, MemberGroup, PackagePath, Prose,
        Relation, RelationGroup, RelationLabel, Signature, Source, SourceSite, Truncation,
    };
    use backend_library::{Basis, DeclarationKind, object_version, symbol_key, view_state_root};

    const COORDINATE: &str = "/srv/ci/ndr25a2/source/docs::semantic::46af6473ddcaf6963c3c8d63315b9bc2b34b37a744f514c2d57fce60cf8f8d54::Document";
    const PHYSICAL: &str = include_str!("fixtures/docs-document/models.py");

    #[derive(Deserialize)]
    struct PhysicalMember {
        name: String,
        line: u32,
        signature: String,
        summary: String,
        kind: String,
    }

    fn fixture() -> (Page, Document) {
        // The physical 86,300-byte source is actual evidence. These member
        // rows are source-derived presentation inputs, not compiler/native
        // identity proof; the fixture symbol is deliberately locally named.
        assert_eq!(PHYSICAL.len(), 86_300);
        assert_eq!(
            PHYSICAL.lines().nth(993),
            Some("class Document(MP_Node, BaseModel):")
        );
        let rows: Vec<PhysicalMember> =
            serde_json::from_str(include_str!("fixtures/docs-document/members.json"))
                .expect("source rows");
        assert_eq!(rows.len(), 47);
        let root = view_state_root(&[]);
        let basis = Basis::new(root, object_version(b"document source fixture"));
        let symbol = symbol_key("docs document presentation fixture");
        let document = Document::new(symbol, root, vec![]).with_source_basis(basis);
        let line = LineNumber::new(994).expect("source line");
        let excerpt = backend_library::SourceExcerpt::capture_bounded(
            &PHYSICAL.lines().skip(993).collect::<Vec<_>>().join("\n"),
        );
        let source = Source::Captured {
            site: SourceSite::new(PackagePath::new("src/backend/core/models.py"), line),
            lines: Source::number_lines(excerpt.text().expect("captured"), line),
            truncation: Truncation::Truncated,
        };
        let members: Vec<_> = rows
            .iter()
            .map(|row| {
                let coordinate = format!(
                    "/srv/ci/ndr25a2/source/docs::src/backend/core/models.py:{}::Document::{}",
                    row.line, row.name
                );
                let kind = if row.kind == "function" {
                    DeclarationKind::Function
                } else {
                    DeclarationKind::Variable
                };
                assert!(
                    PHYSICAL
                        .lines()
                        .nth(row.line as usize - 1)
                        .expect("member line")
                        .contains(&row.name)
                );
                Member::new(
                    Identity::parse_with_key(
                        &coordinate,
                        IdentityKey::Symbol(symbol_key(&coordinate)),
                    ),
                    Some(kind),
                    Some(Signature::tokenize(&row.signature, Language::Python)),
                )
                .with_summary(&row.summary)
            })
            .collect();
        let relations = members
            .iter()
            .map(|row| Relation::new(row.identity().clone(), row.kind()))
            .collect::<Vec<_>>();
        let page = Page::new(
            Identity::parse_with_key(COORDINATE, IdentityKey::Symbol(symbol)),
            Some(DeclarationKind::Class),
            source,
        )
        .with_signature(Signature::tokenize(
            "class Document(MP_Node, BaseModel):",
            Language::Python,
        ))
        .with_prose(vec![Prose::Text(
            "Pad document, the content of which lives in the collaboration server.".to_owned(),
        )])
        .with_members(MemberGroup::group(members))
        .with_relations(vec![RelationGroup::new(RelationLabel::Related, relations)]);
        (page, document)
    }

    #[test]
    fn actual_docs_source_sections_roundtrip_losslessly_inside_ordinary_budget() {
        let (full, document) = fixture();
        let expected_members = full.members().to_vec();
        let expected_relations = full.relations().to_vec();
        let original = full.clone();
        let mut cursor = None;
        let mut members = Vec::new();
        let mut relations = Vec::new();
        let mut end = 0;
        let mut pages = 0;
        loop {
            let selected = window(full.clone(), &document, cursor.as_deref())
                .expect("bounded document sections");
            let extent = selected
                .section_page()
                .expect("large declaration is paginated");
            assert_eq!(extent.start, end);
            assert!(extent.end > extent.start);
            assert_eq!((extent.total_members, extent.total_relations), (47, 47));
            for detail in [Detail::Standard, Detail::Full] {
                let answer = Answer::Page(Box::new(selected.clone()));
                let encoded = crate::encode_answer(
                    &answer,
                    detail,
                    None,
                    crate::DEFAULT_RESPONSE_BUDGET_BYTES,
                )
                .expect("ordinary budget");
                let dto: crate::dto::PageDto =
                    serde_json::from_slice(&encoded.bytes).expect("wire roundtrip");
                assert_eq!(
                    dto,
                    crate::dto::PageDto::from_projection(&crate::PageProjection::new(
                        &selected, detail
                    ))
                );
                assert_eq!(dto.identity, crate::dto::IdentityDto::new(full.identity()));
                assert_eq!(dto.section_page.as_ref(), Some(extent));
                assert!(dto.source.is_some());
                assert_eq!(dto.source.as_ref().expect("source").extent, "truncated");
            }
            let preview = crate::markdown::answer(&Answer::Page(Box::new(selected.clone())));
            assert!(
                preview.len() <= crate::MAX_PREVIEW_TEXT_BYTES,
                "continuation must not be clipped from preview"
            );
            assert!(preview.contains(COORDINATE));
            if let Some(next) = &extent.next_cursor {
                assert!(preview.contains(next));
            }
            assert_eq!(selected.signature(), full.signature());
            assert_eq!(selected.prose(), full.prose());
            assert_eq!(selected.source(), full.source());
            members.extend(
                selected
                    .members()
                    .iter()
                    .flat_map(|group| group.members())
                    .cloned(),
            );
            relations.extend(
                selected
                    .relations()
                    .iter()
                    .flat_map(|group| group.relations())
                    .cloned(),
            );
            end = extent.end;
            cursor.clone_from(&extent.next_cursor);
            pages += 1;
            if cursor.is_none() {
                break;
            }
            assert!(pages < 100);
        }
        assert!(pages > 1);
        assert_eq!(
            MemberGroup::group(members).as_ref(),
            expected_members.as_slice()
        );
        assert_eq!(relations.as_slice(), expected_relations[0].relations());
        assert_eq!(full, original, "pagination cannot alter admitted facts");
        assert_eq!(end, 94);
    }

    #[test]
    fn section_cursor_rejects_changed_source_content_and_invalid_offsets() {
        let (page, document) = fixture();
        let first = window(page.clone(), &document, None).expect("first");
        let token = first
            .section_page()
            .expect("extent")
            .next_cursor
            .as_deref()
            .expect("next");
        let different = document.clone().with_source_basis(Basis::new(
            document.basis,
            object_version(b"another source"),
        ));
        assert!(window(page.clone(), &different, Some(token)).is_err());
        let changed = page
            .clone()
            .with_prose(vec![Prose::Text("changed producer prose".to_owned())]);
        assert!(window(changed, &document, Some(token)).is_err());
        let changed = page.clone().with_members(Vec::<MemberGroup>::new());
        assert!(window(changed, &document, Some(token)).is_err());
        for suffix in ["0", "94", "99999999999999999999999999", "16:tail", "-1"] {
            let forged = format!(
                "{}:{suffix}",
                token.rsplit_once(':').expect("cursor format").0
            );
            assert!(
                window(page.clone(), &document, Some(&forged)).is_err(),
                "{suffix}"
            );
        }
        assert!(window(page, &document, Some(&"x".repeat(101))).is_err());
    }

    #[test]
    fn pages_preserve_required_faults_and_oversized_scalars_report_real_measurements() {
        let (page, document) = fixture();
        let fault = Fault::usage("members", "actual required subread fault");
        let page = page.with_notes(vec![fault.clone()]);
        let first = window(page.clone(), &document, None).expect("useful failed page");
        assert_eq!(first.fault(), Some(&fault));
        let next = first
            .section_page()
            .expect("extent")
            .next_cursor
            .as_deref()
            .expect("next");
        assert_eq!(
            window(page.clone(), &document, Some(next))
                .expect("next useful failed page")
                .fault(),
            Some(&fault)
        );
        let huge = page
            .clone()
            .with_prose(vec![Prose::Text("x".repeat(80_000))]);
        let mut singleton = huge.select_sections(0, 1);
        singleton.section_page = Some(DocumentSectionPage {
            start: 0,
            end: 1,
            total_members: 47,
            total_relations: 47,
            next_cursor: Some(format!(
                "ds1:{}:1",
                commitment(&huge, &document).expect("commitment")
            )),
            continuation: Some(DocumentContinuation {
                coordinate: COORDINATE.to_owned(),
                selected_symbol: crate::IdentitySemanticData::SelectedSymbolId(
                    backend_library::SelectedSymbolId::new(*document.symbol.as_bytes())
                        .expect("fixture selected id"),
                ),
                cursor: format!(
                    "ds1:{}:1",
                    commitment(&huge, &document).expect("commitment")
                ),
            }),
        });
        let expected = crate::encode_answer(
            &Answer::Page(Box::new(singleton)),
            Detail::Full,
            None,
            PAGE_BYTES,
        )
        .expect_err("actual encoded byte count");
        let observed = window(huge, &document, None).expect_err("never clip a scalar");
        assert_eq!(observed, crate::oversized_fault(expected));
        assert!(observed.cause().sentence().contains("above"));
        let tokens = page.with_prose(vec![Prose::Text("∉".repeat(5_000))]);
        let observed =
            window(tokens, &document, None).expect_err("never disguise token refusal as bytes");
        assert!(
            observed.cause().sentence().contains("tokens"),
            "{}",
            observed.cause().sentence()
        );
    }

    #[test]
    fn invariant_only_overflow_stays_an_explicit_adapter_fault_without_false_continuation() {
        let (page, document) = fixture();
        let page = page
            .select_sections(0, 0)
            .with_prose(vec![Prose::Code("scalar".repeat(20_000))]);
        let actual = window(page.clone(), &document, None).expect("unchanged scalar interface");
        assert_eq!(actual, page);
        assert!(actual.section_page().is_none());
        let error = crate::encode_answer(
            &Answer::Page(Box::new(actual)),
            Detail::Full,
            None,
            crate::DEFAULT_RESPONSE_BUDGET_BYTES,
        )
        .expect_err("existing final hard budget");
        assert!(error.bytes > error.budget);
    }

    #[test]
    fn document_cursor_grammar_is_shared_and_small_pages_keep_existing_shape() {
        let (page, document) = fixture();
        let small = page.select_sections(0, 1);
        assert_eq!(
            window(small.clone(), &document, None).expect("small"),
            small
        );
        let grammar = crate::grammar_for("show").expect("show");
        assert_eq!(
            grammar.continuation_domain(),
            Some(crate::ContinuationDomain::DocumentSections)
        );
        let mut invocation = crate::Invocation::new(grammar);
        invocation.push(COORDINATE);
        invocation.set("cursor", "ds1:test:16");
        assert!(
            crate::lower(&invocation, "/unused").is_err(),
            "coordinate cannot silently replace a selected ID on resume"
        );
        invocation.set("selected-id", encode_id(document.symbol.as_bytes()));
        assert!(
            matches!(crate::lower(&invocation, "/unused"), Ok(crate::Request::ResumePage { selector, cursor })
            if selector.selected_id().map(|id| *id.bytes()) == Some(*document.symbol.as_bytes()) && cursor == "ds1:test:16")
        );
    }
}
