//! One canonical retained wildcard row: semantic target and exact lexical origin.
//! The semantic iterator intentionally keeps standalone binder callers source-compatible.
use crate::StableLocation;
use std::sync::Arc;
use tsz_parser::NodeIndex;

#[derive(Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RetainedReexportDeclaration {
    pub declaration: NodeIndex,
    pub location: StableLocation,
    pub owner: NodeIndex,
    pub owner_location: StableLocation,
}

impl RetainedReexportDeclaration {
    pub fn stamp_file_idx(&mut self, file_idx: u32) {
        self.location.set_file_idx_if_unassigned(file_idx);
        self.owner_location.set_file_idx_if_unassigned(file_idx);
    }
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RetainedWildcardReexport {
    pub target: (String, bool),
    // None is permitted for standalone semantic-only tables; selected request
    // admission must refuse it. No source location is inferred from row order.
    pub declaration: Option<RetainedReexportDeclaration>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(transparent)]
pub struct WildcardReexportRows(Arc<Vec<RetainedWildcardReexport>>);

pub type WildcardTargetIter<'a> = std::iter::Map<
    std::slice::Iter<'a, RetainedWildcardReexport>,
    fn(&'a RetainedWildcardReexport) -> &'a (String, bool),
>;

impl WildcardReexportRows {
    /// O(1) immutable row ownership for recursive checker calls. This copies no
    /// target string or provenance row; mutation remains binder/reducer-owned.
    pub fn shared(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
    pub fn iter(&self) -> WildcardTargetIter<'_> {
        self.0.iter().map(|row| &row.target)
    }
    pub fn to_vec(&self) -> Vec<(String, bool)> {
        self.iter().cloned().collect()
    }
    /// Storage owned by the canonical Arc<Vec<_>>, excluding target strings.
    pub fn allocation_size_bytes(&self) -> usize {
        2 * std::mem::size_of::<usize>()
            + std::mem::size_of::<Vec<RetainedWildcardReexport>>()
            + self.0.capacity() * std::mem::size_of::<RetainedWildcardReexport>()
    }
    pub fn edges(&self) -> std::slice::Iter<'_, RetainedWildcardReexport> {
        self.0.iter()
    }
    #[cfg(test)]
    fn edges_mut(&mut self) -> std::slice::IterMut<'_, RetainedWildcardReexport> {
        Arc::make_mut(&mut self.0).iter_mut()
    }
    pub fn push(&mut self, target: (String, bool)) {
        self.push_retained(RetainedWildcardReexport {
            target,
            declaration: None,
        });
    }
    pub fn push_retained(&mut self, row: RetainedWildcardReexport) {
        Arc::make_mut(&mut self.0).push(row);
    }
    pub fn len(&self) -> usize {
        self.0.len()
    }
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl From<Vec<(String, bool)>> for WildcardReexportRows {
    fn from(targets: Vec<(String, bool)>) -> Self {
        Self(
            targets
                .into_iter()
                .map(|target| RetainedWildcardReexport {
                    target,
                    declaration: None,
                })
                .collect::<Vec<_>>()
                .into(),
        )
    }
}

impl<'a> IntoIterator for &'a WildcardReexportRows {
    type Item = &'a (String, bool);
    type IntoIter = WildcardTargetIter<'a>;
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl IntoIterator for WildcardReexportRows {
    type Item = (String, bool);
    type IntoIter = std::iter::Map<
        std::vec::IntoIter<RetainedWildcardReexport>,
        fn(RetainedWildcardReexport) -> (String, bool),
    >;
    fn into_iter(self) -> Self::IntoIter {
        Arc::unwrap_or_clone(self.0)
            .into_iter()
            .map(|row| row.target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tsz_parser::parser::node::NodeAccess;
    use crate::{BinderState, ExportSurface};
    use tsz_parser::parser::ParserState;

    #[test]
    fn real_bound_rows_retain_exact_owner_span_spelling_and_semantic_surface() {
        let text = "declare module 'café' { export type * from 'd\\u0065p' with { 'resolution-mode': 'import' }; export * from 'dep' with { 'resolution-mode': 'require' }; } declare module 'other' { export * from 'dep'; }";
        let mut parser = ParserState::new("source.d.ts".into(), text.into());
        let root = parser.parse_source_file();
        assert!(parser.get_diagnostics().is_empty());
        let arena = parser.get_arena();
        let mut binder = BinderState::new();
        binder.set_file_idx(7);
        binder.bind_source_file(arena, root);
        let rows = binder.wildcard_reexports.get("café").unwrap();
        assert_eq!(
            rows.iter().cloned().collect::<Vec<_>>(),
            vec![("dep".into(), true), ("dep".into(), false)]
        );
        let shared = rows.shared();
        assert!(Arc::ptr_eq(&rows.0, &shared.0));
        assert!(std::ptr::eq(
            rows.edges().next().unwrap(),
            shared.edges().next().unwrap()
        ));
        let edges = rows.edges().collect::<Vec<_>>();
        let first = edges[0].declaration.unwrap();
        let second = edges[1].declaration.unwrap();
        assert_eq!(first.location.file_idx, 7);
        assert_eq!(first.owner, second.owner);
        assert_ne!(first.declaration, second.declaration);
        let node = arena.get(first.declaration).unwrap();
        assert_eq!(
            (node.pos, node.end),
            (first.location.pos, first.location.end)
        );
        let owner = arena.get(first.owner).unwrap();
        let module = arena.get_module(owner).unwrap();
        assert_eq!(arena.get_literal_text(module.name), Some("café"));
        assert!(
            text.get(node.pos as usize..node.end as usize)
                .unwrap()
                .contains("d\\u0065p")
        );
        let other = binder
            .wildcard_reexports
            .get("other")
            .unwrap()
            .edges()
            .next()
            .unwrap()
            .declaration
            .unwrap();
        assert_ne!(first.owner, other.owner);
        // Source provenance never changes the semantic surface/fingerprint inputs.
        let surface = ExportSurface::from_binder(&binder, arena, "café", root);
        let mut semantic_only = binder.clone();
        for row in Arc::make_mut(&mut semantic_only.wildcard_reexports)
            .get_mut("café")
            .unwrap()
            .edges_mut()
        {
            row.declaration = None;
        }
        assert_eq!(
            serde_json::to_value(surface).unwrap(),
            serde_json::to_value(ExportSurface::from_binder(
                &semantic_only,
                arena,
                "café",
                root
            ))
            .unwrap()
        );
        let json = serde_json::to_string(rows).unwrap();
        let restored: WildcardReexportRows = serde_json::from_str(&json).unwrap();
        assert_eq!(*rows, restored);
        assert!(serde_json::from_str::<WildcardReexportRows>(r#"[["dep",true]]"#).is_err());
    }

    #[test]
    fn semantic_only_rows_never_mint_a_source_proof() {
        let mut rows = WildcardReexportRows::default();
        rows.push(("dep".into(), true));
        rows.push(("dep".into(), false));
        assert!(rows.edges().all(|row| row.declaration.is_none()));
        assert_eq!(
            rows.clone().into_iter().collect::<Vec<_>>(),
            vec![("dep".into(), true), ("dep".into(), false)]
        );
    }

    use std::sync::Arc;
}
