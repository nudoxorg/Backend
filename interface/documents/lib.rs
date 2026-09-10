//! The `interface-documents` crate exists to project semantic images into one presentation-neutral document model every surface renders.
//! Its public types are the complete boundary; implementation details remain private.
//! Callers compose capabilities through explicit authority, ownership, and failure values.
//!
//! The model is the contract between the engine and the three surfaces. A [`Page`] is what the GUI
//! draws, what the MCP server renders as Markdown, and what the CLI prints; none of them reads a
//! semantic image directly. Every field is typed, every link target is either proven or spelled
//! out as unresolved, and no renderer may invent text the image did not retain.

mod address;
mod census;
mod model;
mod outline;
mod project;
mod projector;
mod prose;
pub mod render;
mod relations;
mod signature;
mod text;

pub use address::{AddressWalk, MAX_ADDRESS_WALK};
pub use census::{Census, Count, KindCount};
pub use model::{
    Block, Direction, ExternalRef, ForeignOrigin, Inline, MemberGroup, MemberRow, Page, Prose,
    RelationGroup, RelationRole, RelationRow, Signature, SourceLocation, Symbol, Target, Token,
    TokenKind,
};
pub use outline::{Outline, OutlineNode};
pub use projector::{Projector, image_language};
pub use relations::{IncomingLink, ReverseLinks};
pub use signature::SignatureDialect;
pub use project::{
    MAX_PAGE_MEMBERS, MAX_PAGE_RELATIONS, MAX_PROSE_BYTES, MAX_SIGNATURE_TOKENS, MissingPool,
    PageTruncation, ProjectionError, ProjectionLimits,
};
pub use render::{PageVisitor, walk_page};
pub use text::{ByteBudget, ByteOffset, ByteSpan, Name, NameError, NameFidelity, Text};
