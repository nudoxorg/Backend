//! A `Source` — one configured instance of an external provider, so the system
//! can federate across many at once and pin (or fan out) a query.

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::{error::BackendKind, identity::Id};

/// Namespace for deriving deterministic [`SourceId`]s from a source's stable
/// name, so the same configured source keeps its identity across restarts.
pub const NAMESPACE: uuid::Uuid = uuid::Uuid::from_u128(0x6e75_646f_785f_7372_635f_6e73_0000_0003);

/// The stable identity of a configured provider instance.
pub type SourceId = Id<Source>;

/// One configured provider instance the system federates over (a self-hosted
/// TerminusDB, a private Qdrant, a shared registry, ...). Records are owned by a
/// source; a query may be pinned to one or fanned across several.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
	/// This source's stable id.
	pub id: SourceId,
	/// A human-readable name for operators.
	pub name: SmolStr,
	/// Which kind of backend this source is.
	pub backend: BackendKind,
}
