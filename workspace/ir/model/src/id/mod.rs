//! Package and entry identity types (`PackageId`, `UniqueId`).
mod package;
mod unique;

pub use self::{
    package::{PackageId, PackageIdView},
    unique::UniqueId,
};
