use crate::id::UniqueId;

pub use crate::id::{PackageId, PackageIdView};

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct PackageInfo<Id> {
    pub(crate) id: PackageId,
    pub(crate) exports: Vec<Id>,
    pub(crate) imports: Vec<UniqueId<Id>>,
}
