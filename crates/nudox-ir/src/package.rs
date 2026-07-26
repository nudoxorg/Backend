mod builder;
mod info;
mod seal;

#[cfg(test)]
mod tests;

use std::hash::Hash;

use crate::{
    entry::{Entry, Symbol},
    index::UntypedEntryIndex,
    kinds::Module,
};

pub use crate::id::{PackageId, PackageIdView};

pub use self::{builder::EntryBuilder, info::PackageInfo};

/// A built, serializable package IR.
///
/// `IrPackage` holds the complete IR for a single package: a tree of entries
/// rooted at a [`Module`], along with the package's export and import tables.
///
/// # Construction
///
/// Packages are built using the closure-based builder pattern:
///
/// ```rust
/// use nudox_ir::build::*;
/// # let sym = |n: &str| Symbol {
/// #     name: n.to_owned(),
/// #     visibility: Visibility::Public,
/// #     documentation: String::new(),
/// #     source: std::path::PathBuf::new(),
/// #     span: 0..0,
/// #     aliases: Box::new([]),
/// #     deprecation: None,
/// #     doc_links: Box::new([]),
/// #     attrs: Box::new([]),
/// #     cfg: None,
/// # };
///
/// let pkg = IrPackage::build(PackageId::path("my-pkg"), sym("root"), |mut root| {
///     root.create("point_id", sym("Point"), |mut rec| {
///         // ... add fields ...
///         Record::builder().build()
///     });
/// });
/// ```
///
/// The closure receives an [`EntryBuilder`] that tracks parent-child
/// relationships automatically.
///
/// # Iteration
///
/// [`IrPackage::iter`] yields every entry. The `Option<&Id>`
/// is `None` for the root module and `Some(id)` for every other entry,
/// providing a mapping from the builder-level id to the entry.
pub struct IrPackage<Id: Eq + Hash> {
    info: PackageInfo<Id>,
    entries: Vec<(UntypedEntryIndex, Entry)>,
}

impl<Id: Eq + Hash> IrPackage<Id> {
    /// Low-level constructor used by [`crate::lower::Lowering::finish`].
    ///
    /// Callers are responsible for ensuring the entries and info are
    /// consistent. `finish` performs all validation before calling this.
    pub(crate) fn from_parts(
        info: PackageInfo<Id>,
        entries: Vec<(UntypedEntryIndex, Entry)>,
    ) -> Self {
        IrPackage { info, entries }
    }

    pub fn build(id: PackageId, sym: Symbol, build: impl FnOnce(EntryBuilder<Id>)) -> Self {
        let mut pkg = IrPackage {
            info: PackageInfo::new(id),
            entries: Vec::new(),
        };

        EntryBuilder::build(&mut pkg, sym, None, |b| {
            build(b);
            Module
        });

        debug_assert!(pkg.is_valid(), "built invalid package");

        pkg
    }

    pub fn info(&self) -> &PackageInfo<Id> {
        &self.info
    }

    pub fn iter(&self) -> impl Iterator<Item = (Option<&Id>, &Entry)> {
        self.entries
            .iter()
            .map(|(idx, entry)| (self.info.export_idx_to_id(*idx), entry))
    }

    fn is_valid(&self) -> bool {
        // TODO: do some validation passes to make sure that the generated IR
        // doesn't have any issues we can catch
        true
    }
}
