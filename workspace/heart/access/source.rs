//! A `Source` is one configured instance of an external provider, so the system
//! can federate across many at once and pin (or fan out) a query.

use serde::{Deserialize, Serialize};
use smol_str::SmolStr;

use crate::{error::BackendKind, identity::Id};

/// Namespace for deriving deterministic [`SourceId`]s from a source's stable
/// name, so the same configured source keeps its identity across restarts.
pub const NAMESPACE: uuid::Uuid = uuid::Uuid::from_u128(0x6e75_646f_785f_7372_635f_6e73_0000_0003);

/// Marker type for the [`Id`] brand — never constructed directly.
pub struct Backend;

/// The stable identity of a configured provider instance.
pub type SourceId = Id<Backend>;

/// A configured external provider instance.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Source {
    pub id: SourceId,
    pub name: SmolStr,
    pub backend: BackendKind,
}

impl Source {
    pub fn new(name: SmolStr, backend: BackendKind) -> Self {
        let id = Id::from_name(&NAMESPACE, name.as_bytes());
        Self { id, name, backend }
    }
}
