//! The one presentation model every Backend v2 product surface renders.
//!
//! # Why this is its own crate
//!
//! The presentation model could have lived as a module tree inside
//! `backend-library`. It does not, and the decision is deliberate: nothing in
//! this crate can perform I/O. Every value here is derived from an
//! already-admitted reply and can be constructed, compared, and rendered in a
//! unit test without a daemon, a socket, a clock, or a filesystem. Keeping it
//! separate is what makes that property checkable rather than merely intended
//! — a `backend-present` function cannot reach a socket because the crate
//! cannot name one.
//!
//! It sits at DAG order 10, in the `core` layer, above `backend-library` and
//! `backend-client`. It names exactly one client type — [`backend_client::ClientError`]
//! — because the lowering of a transport failure into a [`Fault`] is shared. If
//! each surface mapped transport failures itself, the two surfaces would
//! disagree about what a dropped endpoint is called the first time someone
//! edited one of them; here they cannot. It does *not* name a session, a
//! socket, or a path: [`Engine`] is the abstract seam a surface implements.
//!
//! A desktop surface that wants the same model adds
//! `backend-present = { path = "../../crates/present" }` to its manifest and
//! `"backend-present"` to its `dependencies` array in
//! `docs/architecture/package-dag.json`. No other change is required, because
//! the model already sits below every application package in the order.
//!
//! # What the model guarantees
//!
//! * **Identity is the centre of gravity.** [`Identity`] parses the engine's
//!   coordinate spelling into typed parts, renders a readable trail
//!   (`polyglot › src/lib.rs:2 › ferris`), and round-trips
//!   [`Identity::coordinate`] back to the exact bytes the engine accepts.
//!   Abbreviated keys ([`KeyTag`]) are display-only and are never parsed back.
//! * **Faults are content.** Every failure a surface can observe lowers into
//!   one [`Fault`] carrying a typed [`Operand`], a [`Cause`], and an
//!   [`Affordance`]. The three-line grammar is shared, so CLI, MCP, and
//!   desktop print the same slug, the same operand, and the same next step.
//! * **Parity is structural.** The Markdown renderer used by MCP and by the
//!   CLI's `--format markdown` is the *same* function; the human renderer
//!   differs only by colour and width. So is the *request sequence*: [`answer`]
//!   decides which probes a page or an outline needs, expressed against the
//!   [`Engine`] trait rather than against a socket, so a surface contributes
//!   only a mechanical adapter. Divergence would have to be written on purpose.
//! * **No fabricated authority.** A type token resolved by name is marked
//!   [`Resolved::ByName`] and never claims a proven semantic link; a lane with
//!   no result renders `✗` with its reason rather than an empty success.

#![deny(unsafe_code)]

mod assemble;
mod budget;
mod call;
mod coverage;
mod drive;
mod dto;
mod fault;
mod glyph;
mod grammar;
mod identity;
mod language;
mod outline;
mod page;
mod product;
mod record;
mod render;
mod shelf;
mod signature;
mod status;

pub use assemble::{
    outline_tree, page_from_document, page_from_document_with_graph_relations, project_of,
    record_list, record_list_from_rows, shelf_from_root, shelf_from_snapshot,
};
pub use budget::{
    BudgetExceeded, DEFAULT_RESPONSE_BUDGET_BYTES, Detail, ESTIMATED_BYTES_PER_TOKEN,
    EncodedPayload, MAX_PREVIEW_TEXT_BYTES, MAX_RESPONSE_BUDGET_BYTES, PayloadBudget, bounded_text,
    encode_answer, encode_serializable, encode_value, estimate_tokens, oversized_fault,
};
pub use call::{
    DEFAULT_LIMIT, Invocation, Request, SURFACE_VERB, lower, lower_surface_json, row_for,
};
pub use coverage::{
    CoverageLine, LaneCoverage, LaneShards, LaneState, RowCount as CoverageRows, lane_name,
    reason_name,
};
pub use drive::{Answer, Engine, Probe, answer, answer_paged};
pub use dto::{
    CapabilitiesDto, CoverageDto, FaultDto, IdentityDto, LanguageCountDto, MemberGroupDto,
    OutlineDto, OutlineNodeDto, PageDto, ProductDto, ProductRecordDto, ReasonDto, RecordDto,
    RecordListDto, RelationGroupDto, ShelfDto, ShelfEntryDto, SignatureTokenDto, SourceDto,
    StatusDto, answer_value, fault_value,
};
pub use fault::{Affordance, Cause, CauseSlug, Fault, FaultSlug, Operand};
pub use glyph::{KindGlyph, LanguageGlyph, RelationDirection, RelationLabel, relation_label};
pub use grammar::{
    ArgumentKind, ArgumentSpec, CommandGrammar, GRAMMARS, domain_name, domains, grammar_for,
    grammar_for_tool, grammars_in, registry_size,
};
pub use identity::{
    Coordinate, Identity, IdentityKey, IdentityShape, KeyTag, LineNumber, PackagePath, ProjectRef,
    SymbolSegment, SymbolTrail,
};
pub use language::Language;
pub use outline::{OutlineEntry, OutlineTree, row_resolver};
pub use page::{
    Member, MemberGroup, Page, Prose, Relation, RelationGroup, Source, SourceLine, SourceSite,
    Truncation,
};
pub use product::{ProductRecord, ProductView, product_view};
pub use record::{Record, RecordList, RecordState, Score};
pub use render::{Colour, Style, Theme, Width, display_width, markdown, text};
pub use shelf::{LanguageCount, Readiness, RowCount, Shelf, ShelfEntry};
pub use signature::{Resolved, Signature, Target, Token, TokenKind};
pub use status::{
    CapabilitySummary, EmbeddingState, FamilyRollup, PublishedRows, ReasonRollup, Sequence,
    SlotCount, Status, unavailable_name,
};

/// Widest human rendering used when no terminal width is known.
pub const DEFAULT_WIDTH: usize = 100;

/// Narrowest human rendering the layout still reads correctly at.
pub const MINIMUM_WIDTH: usize = 40;

#[cfg(test)]
#[allow(clippy::expect_used, clippy::panic)]
mod tests;
