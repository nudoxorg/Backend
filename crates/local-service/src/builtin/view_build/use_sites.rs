//! Which row stands for a declaration an image holds only as a copy.
//!
//! A Go image holds every declaration of its package: those of its own file,
//! and copies of the siblings' that carry no source. The copies are not
//! projected as rows, yet a copy can still be the caller of something (a test
//! function has no image of its own, so every image meets it as a copy). The
//! row that names such a caller is the declaration's structural row.

use super::identity::{declaration_family, declaration_kind, semantic_symbol};
use backend_engine::{PackageKey, RowId, SymbolKey, ViewRoot};
use backend_semantic::ir::{DeclarationIdentity, ItemKind};
use std::cell::OnceCell;
use std::collections::BTreeMap;

/// Resolves the declaration an image names to the one row that stands for it.
pub(crate) struct UseSites<'view> {
    view: &'view ViewRoot,
    package: PackageKey,
    /// The structural declarations of the package by name, with their kind
    /// family, built the first time a copy has to be named.
    structural: OnceCell<BTreeMap<String, Vec<(SymbolKey, u8)>>>,
}

impl<'view> UseSites<'view> {
    /// Names rows of `package` in `view`.
    pub(crate) fn new(view: &'view ViewRoot, package: PackageKey) -> Self {
        Self {
            view,
            package,
            structural: OnceCell::new(),
        }
    }

    /// The row of the declaration `identity` names in an image: its own row
    /// when the image published one, else the only structural declaration of
    /// its name and kind in the package. A declaration that no row stands
    /// for, or that several do, has none.
    pub(crate) fn symbol(
        &self,
        identity: DeclarationIdentity,
        name: Option<&[u8]>,
        kind: ItemKind,
    ) -> Option<SymbolKey> {
        let own = semantic_symbol(self.package, identity);
        if self.view.row_ref(RowId::Symbol(own)).is_some() {
            return Some(own);
        }
        let name = std::str::from_utf8(name?).ok()?;
        let family = declaration_family(declaration_kind(kind));
        let declarations = self
            .structural
            .get_or_init(|| self.structural_declarations());
        let mut named = declarations
            .get(name)?
            .iter()
            .filter(|(_, candidate)| *candidate == family);
        let (symbol, _) = named.next()?;
        named.next().is_none().then_some(*symbol)
    }

    /// [`Self::symbol`], as the one row that stands for the declaration when
    /// both publication lanes published it: the structural row.
    pub(crate) fn canonical_symbol(
        &self,
        identity: DeclarationIdentity,
        name: Option<&[u8]>,
        kind: ItemKind,
    ) -> Option<SymbolKey> {
        let symbol = self.symbol(identity, name, kind)?;
        match self.view.canonical_row_id(RowId::Symbol(symbol)) {
            RowId::Symbol(canonical) => Some(canonical),
            _ => None,
        }
    }

    fn structural_declarations(&self) -> BTreeMap<String, Vec<(SymbolKey, u8)>> {
        let mut declarations = BTreeMap::<String, Vec<(SymbolKey, u8)>>::new();
        for row in self.view.row_refs() {
            let (RowId::Symbol(symbol), Some(kind)) = (row.id, row.kind) else {
                continue;
            };
            if row.package != Some(self.package)
                || row.source.captured().is_none()
                || row.label.contains("::semantic::")
                || row.label.contains("::external::")
            {
                continue;
            }
            let Some(name) = row
                .label
                .rsplit("::")
                .next()
                .filter(|name| !name.is_empty())
            else {
                continue;
            };
            declarations
                .entry(name.to_owned())
                .or_default()
                .push((symbol, declaration_family(kind)));
        }
        declarations
    }
}
