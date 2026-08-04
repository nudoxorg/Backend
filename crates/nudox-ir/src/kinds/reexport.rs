use crate::visitor::Visitor;

// FIXME: flags for glob vs named re-exports are not yet represented; they
// await the attribute subsystem.

/// A marker kind for a re-export (public alias) entry.
///
/// The re-export target is captured by
/// [`EntryInner::Reference`](crate::entry::EntryInner::Reference) on the
/// owning [`Entry`](crate::entry::Entry) — no additional data is needed here.
/// The re-export's name and visibility live on the entry's
/// [`Symbol`](crate::entry::Symbol).
#[derive(Debug, Clone, PartialEq, Eq, Visitor, serde::Serialize, serde::Deserialize)]
pub struct Reexport;
