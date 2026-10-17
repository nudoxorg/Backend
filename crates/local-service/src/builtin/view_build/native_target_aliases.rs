//! Exact native endpoints are references to owned declarations, not additional
//! pathless declarations. Keep image-local cached projections unchanged and
//! resolve their endpoint keys against the selected immutable publication.

use super::{call_join, identity, source_availability::SelectedSourceArtifacts};
use super::super::BuiltinModelError;
use backend_engine::{Fragment, PackageKey, Row, RowId, SymbolKey};
use backend_engine::application::DocumentationSession;
use backend_semantic::ir::{ExternalTarget, ExternalTargetIdentity, ForeignTargetOrigin, SemanticReader, TYPESCRIPT_TSZ_SOURCE_ECOSYSTEM};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Default)]
pub(super) struct NativeTargetAliases {
    aliases: BTreeMap<SymbolKey, Option<SymbolKey>>,
}

impl NativeTargetAliases {
    pub(super) fn record_selected(
        &mut self,
        package: PackageKey,
        selected: &[SelectedSourceArtifacts],
    ) -> Result<(), BuiltinModelError> {
        let snapshots = selected.iter().flat_map(|publication| publication.images()).collect::<Vec<_>>();
        let images = snapshots.iter().map(|image| image.reopen().map_err(|error|
            BuiltinModelError(format!("reopen selected native endpoint image: {error}"))))
            .collect::<Result<Vec<_>, _>>()?;
        if !images.iter().any(|image| image.canonical_externals().any(|(_, target)| {
            matches!(target, ExternalTarget::Foreign(foreign)
                if matches!(foreign.origin, ForeignTargetOrigin::Universe { ecosystem }
                    if image.atom(ecosystem) == Some(TYPESCRIPT_TSZ_SOURCE_ECOSYSTEM.as_bytes())))
        })) {
            return Ok(());
        }
        let views = images.iter().collect::<Vec<_>>();
        let programs = selected.iter().filter_map(|publication| publication.program()).collect::<Vec<_>>();
        let index = call_join::ProjectCallableIndex::build_from_views_with_programs(
            &views, &BTreeMap::new(), &programs, !selected.iter().any(|publication| publication.has_program_manifest()),
        )?;
        let mut published = BTreeSet::new();
        for image in &images {
            for entity in DocumentationSession::new(image).publication_entities() {
                published.insert(entity.map_err(|error|
                    BuiltinModelError(format!("read selected native endpoint owner: {error}")))?
                    .entity.version.identity());
            }
        }
        for (snapshot, image) in snapshots.iter().zip(&images) {
            self.record_image(package, snapshot.content_digest(), image, &index, &published)?;
        }
        Ok(())
    }

    pub(super) fn record_image(
        &mut self,
        package: PackageKey,
        digest: [u8; 32],
        image: &backend_semantic::ir::SemanticImageView<'_>,
        index: &call_join::ProjectCallableIndex,
        published: &BTreeSet<backend_semantic::ir::DeclarationIdentity>,
    ) -> Result<(), BuiltinModelError> {
        for (external, _) in image.canonical_externals() {
            let Some(target) = call_join::join_native_typescript_target(image, external, index, published)? else {
                continue;
            };
            let endpoint = ExternalTargetIdentity::capture(image, external).map_err(|error|
                BuiltinModelError(format!("identify selected native endpoint: {error}")))?;
            let alias = identity::external_semantic_symbol(package, digest, endpoint);
            let target = identity::semantic_symbol(package, target);
            self.aliases.entry(alias).and_modify(|known| {
                if *known != Some(target) { *known = None; }
            }).or_insert(Some(target));
        }
        Ok(())
    }

    /// Apply one exact key arrangement to rows and their document links. An
    /// unselected, ambiguous or missing target keeps its external row; the
    /// target must already be a real declaration in this same projected view.
    pub(super) fn apply(self, rows: &mut Vec<Row>) {
        let present = rows.iter().map(|row| row.id).collect::<BTreeSet<_>>();
        let aliases = self.aliases.into_iter().filter_map(|(alias, target)| {
            let target = target?;
            present.contains(&RowId::Symbol(target)).then_some((alias, target))
        }).collect::<BTreeMap<_, _>>();
        rows.retain(|row| !matches!(row.id, RowId::Symbol(symbol) if aliases.contains_key(&symbol)));
        for row in rows {
            for fragment in row.document.iter_mut() {
                if let Fragment::Link { target, .. } = fragment
                    && let Some(resolved) = aliases.get(target)
                {
                    *target = *resolved;
                }
            }
        }
    }
}
