//! Exact native endpoints are references to owned declarations, not additional
//! pathless declarations. Keep image-local cached projections unchanged and
//! resolve their endpoint keys against the selected immutable publication.

use super::{call_join, identity, source_availability::SelectedSourceArtifacts};
use super::super::BuiltinModelError;
use backend_engine::{Fragment, PackageKey, Row, RowId, SymbolKey};
use backend_engine::application::DocumentationSession;
use backend_semantic::ir::SemanticCoreReader;
use backend_semantic::ir::{ExternalTarget, ExternalTargetIdentity, ForeignTargetOrigin, SemanticReader, TYPESCRIPT_TSZ_SOURCE_ECOSYSTEM};
use std::collections::{BTreeMap, BTreeSet};

/// One package's TS/TSX image leases, never the whole workspace's images.
/// ProductSemanticPublicationKey::Ord and its canonical order codec both put
/// the typed PackageReference first, so a relation page boundary cannot split
/// the lifetime incorrectly. Profiles/coordinates may interleave inside it.
pub(super) struct NativePackageSelections<T> {
    package: Option<(backend_engine::PackageReference, PackageKey)>,
    selected: Vec<T>,
}

impl<T> Default for NativePackageSelections<T> {
    fn default() -> Self { Self { package: None, selected: Vec::new() } }
}

impl<T> NativePackageSelections<T> {
    pub(super) fn advance(
        &mut self,
        key: &backend_engine::ProductSemanticPublicationKey,
        mut finish: impl FnMut(PackageKey, &[T]) -> Result<(), BuiltinModelError>,
    ) -> Result<(), BuiltinModelError> {
        if let Some((package, _)) = &self.package {
            if package > key.package() {
                return Err(BuiltinModelError("semantic package projection order decreased".to_owned()));
            }
            if package != key.package() { self.finish(&mut finish)?; }
        }
        if self.package.is_none() {
            self.package = Some((key.package().clone(), key.package_key()));
        }
        Ok(())
    }

    pub(super) fn retain(
        &mut self,
        key: &backend_engine::ProductSemanticPublicationKey,
        selected: T,
    ) -> Result<(), BuiltinModelError> {
        if self.package.as_ref().map(|(package, _)| package) != Some(key.package()) {
            return Err(BuiltinModelError("native image is outside the current package projection".to_owned()));
        }
        if matches!(key.profile(), backend_semantic::vocabulary::LanguageProfile::TypeScript(_)) {
            self.selected.push(selected);
        }
        Ok(())
    }

    pub(super) fn finish(
        &mut self,
        mut finish: impl FnMut(PackageKey, &[T]) -> Result<(), BuiltinModelError>,
    ) -> Result<(), BuiltinModelError> {
        if let Some((_, package)) = &self.package
            && !self.selected.is_empty()
        {
            finish(*package, &self.selected)?;
        }
        self.selected.clear();
        self.package = None;
        Ok(())
    }
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use backend_semantic::vocabulary::{LanguageProfile, PackageUrl, RustEdition, TypeScriptSource};
    use std::sync::Arc;

    fn key(package: &str, source: Option<TypeScriptSource>) -> backend_engine::ProductSemanticPublicationKey {
        let (profile, coordinate) = match source {
            Some(source) => (LanguageProfile::TypeScript(source), "pkg:npm/fixture@1.0.0"),
            None => (LanguageProfile::Rust(RustEdition::Rust2024), "pkg:cargo/fixture@1.0.0"),
        };
        backend_engine::ProductSemanticPublicationKey::new(
            backend_engine::PackageReference::parse(package.to_owned()).expect("package"),
            PackageUrl::parse(coordinate.to_owned()).expect("coordinate"), profile,
        ).expect("profile")
    }

    #[test]
    fn native_package_image_leases_end_at_the_boundary_but_keep_both_typescript_profiles() {
        let ts = key("/workspace/a-long-name", Some(TypeScriptSource::TypeScript));
        let tsx = key("/workspace/a-long-name", Some(TypeScriptSource::Tsx));
        let rust = key("/workspace/a-long-name", None);
        let next = key("/workspace/z", Some(TypeScriptSource::TypeScript));
        assert!(ts < next && tsx < next && rust < next,
            "typed relation order groups package before coordinate/profile");
        let mut selected = NativePackageSelections::<Arc<u8>>::default();
        let mut compact = Vec::new();
        let mut finish = |package, items: &[Arc<u8>]| {
            compact.push((package, items.iter().map(|item| **item).collect::<Vec<_>>()));
            Ok(())
        };
        selected.advance(&ts, &mut finish).expect("first package");
        let image_ts = Arc::new(1u8);
        let weak_ts = Arc::downgrade(&image_ts);
        selected.retain(&ts, image_ts).expect("TS lease");
        // Non-TSZ profiles neither flush the package nor retain their payload.
        selected.advance(&rust, &mut finish).expect("same package");
        let unrelated = Arc::new(9u8);
        let weak_unrelated = Arc::downgrade(&unrelated);
        selected.retain(&rust, unrelated).expect("other profile");
        assert!(weak_unrelated.upgrade().is_none());
        selected.advance(&tsx, &mut finish).expect("same package TSX");
        let image_tsx = Arc::new(2u8);
        let weak_tsx = Arc::downgrade(&image_tsx);
        selected.retain(&tsx, image_tsx).expect("TSX lease");
        assert!(weak_ts.upgrade().is_some() && weak_tsx.upgrade().is_some());
        selected.advance(&next, &mut finish).expect("package boundary");
        assert!(weak_ts.upgrade().is_none() && weak_tsx.upgrade().is_none(),
            "the actual production grouping owner releases image leases before the next package loads");
        let image_next = Arc::new(3u8);
        let weak_next = Arc::downgrade(&image_next);
        selected.retain(&next, image_next).expect("next package lease");
        selected.finish(&mut finish).expect("last package");
        assert!(weak_next.upgrade().is_none());
        // Explicit per-package seeks use hashed project-map order. A completed
        // seek releases its group, so a later seek may start earlier in Ord.
        selected.advance(&ts, &mut finish).expect("separate package seek");
        selected.finish(&mut finish).expect("empty seek");
        assert_eq!(compact, vec![(ts.package_key(), vec![1, 2]), (next.package_key(), vec![3])]);
    }
}
