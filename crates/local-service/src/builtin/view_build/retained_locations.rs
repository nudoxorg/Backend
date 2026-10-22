//! Source placement from immutable, authenticated syntax captures.
//!
//! A declaration's retained excerpt has no separately stored file offset.
//! Recover that offset only when reparsing its bytes yields a unique exact
//! translation of all four captured owner/name endpoints. No filesystem read
//! or name/order pairing can establish that translation.
use backend_compile::{
    SourceBindingIdentity, SourceDeclaration, SourceExcerpt, SourceExcerptExtent, SourceLocation,
};
use backend_semantic::ir::{FactAvailability, SemanticEntity, SemanticReader};
use backend_semantic::vocabulary::LanguageProfile;
use std::collections::BTreeSet;

struct Window<'a> {
    path: &'a str,
    text: &'a str,
    origin: u32,
    start_line: u32,
}

pub(super) struct RetainedLocations<'a> {
    windows: Vec<Window<'a>>,
}

pub(super) struct Placement {
    pub(super) location: SourceLocation,
    pub(super) excerpt: SourceExcerpt,
}

impl<'a> RetainedLocations<'a> {
    pub(super) fn for_entities<Reader: SemanticReader + ?Sized>(
        profile: LanguageProfile,
        reader: &Reader,
        session: &backend_engine::application::DocumentationSession<'_, Reader>,
        declarations: &[&'a SourceDeclaration],
        exact: &std::collections::BTreeMap<
            backend_semantic::ir::DeclarationIdentity,
            backend_compile::SourceBinding,
        >,
    ) -> Result<Self, String> {
        Self::for_entities_controlled(profile, reader, session, declarations, exact, None)
    }

    pub(super) fn for_entities_controlled<Reader: SemanticReader + ?Sized>(
        profile: LanguageProfile,
        reader: &Reader,
        session: &backend_engine::application::DocumentationSession<'_, Reader>,
        declarations: &[&'a SourceDeclaration],
        exact: &std::collections::BTreeMap<
            backend_semantic::ir::DeclarationIdentity,
            backend_compile::SourceBinding,
        >,
        cancelled: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<Self, String> {
        let checkpoint = |point| {
            super::super::check_search_preparation(cancelled, point)
                .map_err(|error| error.to_string())
        };
        checkpoint("retained-start")?;
        if !matches!(profile, LanguageProfile::TypeScript(_)) {
            return Ok(Self {
                windows: Vec::new(),
            });
        }
        let mut required = Vec::new();
        for entity in session.publication_entities() {
            checkpoint("retained-entity")?;
            let entity = entity.map_err(|error| error.to_string())?;
            if exact.contains_key(&entity.entity.version.identity()) {
                continue;
            }
            if entity.entity.authority.source != FactAvailability::Captured
                || entity.entity.authority.source_file != FactAvailability::Captured
            {
                continue;
            }
            if let Some(span) = entity.entity.source.filter(|span| !span.is_empty()) {
                // File equality remains an admission condition in place().
                if reader.atom(span.file()).is_some() {
                    required.push((span.start(), span.end()));
                }
            }
        }
        Self::build(profile, declarations, Some(&required), cancelled)
    }

    #[cfg(test)]
    pub(super) fn of(
        profile: LanguageProfile,
        declarations: &[&'a SourceDeclaration],
    ) -> Result<Self, String> {
        Self::build(profile, declarations, None, None)
    }

    fn build(
        profile: LanguageProfile,
        declarations: &[&'a SourceDeclaration],
        required: Option<&[(u32, u32)]>,
        cancelled: Option<&std::sync::atomic::AtomicBool>,
    ) -> Result<Self, String> {
        let checkpoint = |point| {
            super::super::check_search_preparation(cancelled, point)
                .map_err(|error| error.to_string())
        };
        checkpoint("retained-build")?;
        let mut windows = Vec::new();
        if !matches!(profile, LanguageProfile::TypeScript(_)) {
            return Ok(Self { windows });
        }
        if required.is_some_and(<[(u32, u32)]>::is_empty) {
            return Ok(Self { windows });
        }
        let Ok(frontend) = backend_frontend_typescript::syntax_frontend() else {
            return Ok(Self { windows });
        };
        for site in declarations {
            checkpoint("retained-site")?;
            let SourceBindingIdentity::Exact(capture) = site.binding_identity() else {
                continue;
            };
            if let Some(spans) = required {
                let mut covered = false;
                for (start, end) in spans {
                    checkpoint("retained-required-span")?;
                    if capture.owner().0 <= *start && *end <= capture.owner().1 {
                        covered = true;
                        break;
                    }
                }
                if !covered {
                    continue;
                }
            }
            if site.source_excerpt().extent() != Some(SourceExcerptExtent::Complete) {
                continue;
            }
            let Some(text) = site.source_excerpt().text() else {
                continue;
            };
            let mut origins = BTreeSet::new();
            for (input, prefix) in [
                (text.to_owned(), 0),
                (
                    format!("class __NudoxRetainedSourceContext__ {{\n{text}\n}}"),
                    "class __NudoxRetainedSourceContext__ {\n".len(),
                ),
            ] {
                checkpoint("retained-parse")?;
                let Ok(analysis) = frontend.analyze(
                    std::path::Path::new(site.location().path()),
                    input.as_bytes(),
                ) else {
                    continue;
                };
                checkpoint("retained-parsed")?;
                for candidate in analysis.declarations().iter() {
                    checkpoint("retained-candidate")?;
                    if candidate.name() != site.name() || candidate.kind() != site.kind() {
                        continue;
                    }
                    let SourceBindingIdentity::Exact(relative) = candidate.binding_identity()
                    else {
                        continue;
                    };
                    let (owner_start, owner_end) = relative.owner();
                    let (name_start, name_end) = relative.name();
                    let Some(start) = (name_start as usize).checked_sub(prefix) else {
                        continue;
                    };
                    let Some(origin) = capture.name().0.checked_sub(start as u32) else {
                        continue;
                    };
                    let translated = [owner_start, owner_end, name_start, name_end].map(|offset| {
                        (offset as usize)
                            .checked_sub(prefix)
                            .filter(|offset| *offset <= text.len())
                            .and_then(|offset| origin.checked_add(offset as u32))
                    });
                    if translated
                        == [
                            Some(capture.owner().0),
                            Some(capture.owner().1),
                            Some(capture.name().0),
                            Some(capture.name().1),
                        ]
                    {
                        // site.line is the grammar definition/statement line,
                        // which can precede both the owner and name captures.
                        let prefix_lines =
                            input[..prefix].bytes().filter(|&b| b == b'\n').count() as u32;
                        let Some(relative_line) = candidate
                            .line()
                            .checked_sub(1)
                            .and_then(|line| line.checked_sub(prefix_lines))
                        else {
                            continue;
                        };
                        let Some(start_line) = site
                            .line()
                            .checked_sub(relative_line)
                            .filter(|line| *line != 0)
                        else {
                            continue;
                        };
                        origins.insert((origin, start_line));
                    }
                }
            }
            if let [(origin, start_line)] = origins.into_iter().collect::<Vec<_>>().as_slice() {
                windows.push(Window {
                    path: site.location().path(),
                    text,
                    origin: *origin,
                    start_line: *start_line,
                });
            }
        }
        checkpoint("retained-complete")?;
        Ok(Self { windows })
    }

    pub(super) fn place<Reader: SemanticReader + ?Sized>(
        &self,
        reader: &Reader,
        entity: &SemanticEntity,
    ) -> Option<Placement> {
        if entity.authority.source != FactAvailability::Captured
            || entity.authority.source_file != FactAvailability::Captured
        {
            return None;
        }
        let span = entity.source.filter(|span| !span.is_empty())?;
        let path = std::str::from_utf8(reader.atom(span.file())?).ok()?;
        self.place_span(path, span.start(), span.end())
    }

    fn place_span(&self, path: &str, start: u32, end: u32) -> Option<Placement> {
        if start >= end {
            return None;
        }
        let mut placed: Option<Placement> = None;
        for window in &self.windows {
            if window.path != path {
                continue;
            }
            let Some(start) = start.checked_sub(window.origin).map(|v| v as usize) else {
                continue;
            };
            let Some(end) = end.checked_sub(window.origin).map(|v| v as usize) else {
                continue;
            };
            let Some(text) = window.text.get(start..end) else {
                continue;
            };
            let start_lines = window.text[..start].bytes().filter(|&b| b == b'\n').count() as u32;
            let line = window.start_line.checked_add(start_lines)?;
            let placement = Placement {
                location: SourceLocation::new(path, line).ok()?,
                excerpt: SourceExcerpt::capture_bounded(text),
            };
            if let Some(previous) = &placed {
                if previous.location != placement.location || previous.excerpt != placement.excerpt
                {
                    return None;
                }
            } else {
                placed = Some(placement);
            }
        }
        placed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use backend_compile::{SourceBindingCapture, SourceExcerptExtent};
    use backend_semantic::vocabulary::TypeScriptSource;
    const PROFILE: LanguageProfile = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);

    #[test]
    fn angular04_constructor_parameters_and_fields_recover_from_retained_frozen_source()
    -> Result<(), String> {
        let cases = [
            (
                "article-list.component.ts",
                include_str!("fixtures/angular04-retained/article-list.component.ts"),
                "private articlesService: ArticlesService",
                "articlesService",
                101,
            ),
            (
                "favorite-button.component.ts",
                include_str!("fixtures/angular04-retained/favorite-button.component.ts"),
                "private readonly articleService: ArticlesService",
                "articleService",
                45,
            ),
            (
                "editor.component.ts",
                include_str!("fixtures/angular04-retained/editor.component.ts"),
                "private readonly articleService: ArticlesService",
                "articleService",
                37,
            ),
            (
                "article.component.ts",
                include_str!("fixtures/angular04-retained/article.component.ts"),
                "private readonly articleService: ArticlesService",
                "articleService",
                62,
            ),
        ];
        let frontend = backend_frontend_typescript::syntax_frontend().map_err(|e| e.to_string())?;
        for (path, source, field, parameter, line) in cases {
            let analysis = frontend
                .analyze(std::path::Path::new(path), source.as_bytes())
                .map_err(|e| e.to_string())?;
            let declarations = analysis.declarations().iter().collect::<Vec<_>>();
            let locations = RetainedLocations::of(PROFILE, &declarations)?;
            let field_start = source.find(field).ok_or("frozen field")?;
            let parameter_start = field_start + field.find(parameter).ok_or("binding")?;
            for (start, end, text) in [
                (field_start, field_start + field.len(), field),
                (
                    parameter_start,
                    parameter_start + parameter.len(),
                    parameter,
                ),
            ] {
                let placement = locations
                    .place_span(path, start as u32, end as u32)
                    .ok_or_else(|| format!("no retained proof for {path}:{line} {text}"))?;
                assert_eq!(placement.location.path(), path);
                assert_eq!(placement.location.start_line(), line);
                assert_eq!(placement.excerpt.text(), Some(text));
            }
            assert!(
                locations
                    .place_span(
                        "other.ts",
                        field_start as u32,
                        (field_start + field.len()) as u32
                    )
                    .is_none()
            );
            assert!(
                locations
                    .place_span(path, source.len() as u32 + 1, source.len() as u32 + 2)
                    .is_none()
            );
        }
        Ok(())
    }

    #[test]
    fn exact_window_geometry_handles_utf8_crlf_and_refuses_legacy_truncated_or_shifted_capture()
    -> Result<(), String> {
        let path = "retained.ts";
        let source = "// λ 🐍 before absolute byte offsets\r\nexport class Left {\r\n constructor(private service: LeftType) {}\r\n}\r\nclass Right {\r\n constructor(private service: RightType) {}\r\n}\r\n";
        let frontend = backend_frontend_typescript::syntax_frontend().map_err(|e| e.to_string())?;
        let analysis = frontend
            .analyze(std::path::Path::new(path), source.as_bytes())
            .map_err(|e| e.to_string())?;
        let sites = analysis.declarations().iter().collect::<Vec<_>>();
        let locations = RetainedLocations::of(PROFILE, &sites)?;
        for (needle, line) in [("service: LeftType", 3), ("service: RightType", 6)] {
            let start = source.find(needle).ok_or("binding")?;
            let placement = locations
                .place_span(path, start as u32, (start + "service".len()) as u32)
                .ok_or("exact span")?;
            assert_eq!(placement.location.start_line(), line);
            assert_eq!(placement.excerpt.text(), Some("service"));
        }
        let left = sites
            .iter()
            .copied()
            .find(|site| site.name() == "Left")
            .ok_or("class capture")?;
        let SourceBindingIdentity::Exact(capture) = left.binding_identity() else {
            return Err("exact capture".into());
        };
        let shifted = left
            .clone()
            .with_binding_identity(SourceBindingIdentity::Exact(
                SourceBindingCapture::new(
                    capture.owner().0,
                    capture.owner().1 + 1,
                    capture.name().0,
                    capture.name().1,
                )
                .ok_or("shifted capture")?,
            ));
        let legacy = left
            .clone()
            .with_binding_identity(SourceBindingIdentity::Legacy);
        let truncated = left.clone().with_source_excerpt(SourceExcerpt::captured(
            left.source_excerpt().text().ok_or("excerpt")?,
            SourceExcerptExtent::Truncated,
        )?);
        let start = source.find("service: LeftType").ok_or("binding")? as u32;
        for unavailable in [&shifted, &legacy, &truncated] {
            assert!(
                RetainedLocations::of(PROFILE, &[unavailable])?
                    .place_span(path, start, start + 7)
                    .is_none()
            );
        }
        // A byte inside a multi-byte codepoint can never become a source site.
        let invalid = source.find('λ').ok_or("UTF-8")? as u32 + 1;
        assert!(locations.place_span(path, invalid, invalid + 1).is_none());
        Ok(())
    }
}

#[cfg(test)]
mod multiline_anchor_tests {
    use super::*;
    use backend_semantic::vocabulary::TypeScriptSource;

    #[test]
    fn statement_lines_anchor_multiline_imports_and_const_declarators() -> Result<(), String> {
        let source = "// λ 🐍\r\nimport {\r\n  Foo,\r\n  Bar as Renamed\r\n} from './module';\r\nconst\r\n  holder = () => {\r\n    const child = 1;\r\n    return child;\r\n  };\r\n";
        let path = "multiline.ts";
        let analysis = backend_frontend_typescript::syntax_frontend()
            .map_err(|e| e.to_string())?
            .analyze(std::path::Path::new(path), source.as_bytes())
            .map_err(|e| e.to_string())?;
        let sites = analysis.declarations().iter().collect::<Vec<_>>();
        let locations = RetainedLocations::of(
            LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
            &sites,
        )?;
        for (needle, line) in [("Foo", 3), ("Renamed", 4), ("holder", 7), ("child = 1", 8)] {
            let start = source.find(needle).ok_or("source needle")?;
            let placement = locations
                .place_span(path, start as u32, (start + needle.len()) as u32)
                .ok_or_else(|| format!("missing multiline placement {needle}"))?;
            assert_eq!(placement.location.start_line(), line, "{needle}");
            assert_eq!(placement.excerpt.text(), Some(needle));
        }
        assert!(
            RetainedLocations::build(
                LanguageProfile::TypeScript(TypeScriptSource::TypeScript),
                &sites,
                Some(&[]),
                None,
            )?
            .windows
            .is_empty()
        );
        Ok(())
    }
}

#[cfg(test)]
mod cold_projection_tests {
    use super::*;
    use backend_engine::{RowId, product_source_file_key};
    use backend_semantic::ir::{
        BorrowedTree, CorePayloadHash, DeclarationFamilyId, DeclarationPublicationRole,
        EntityAuthorityFacts, EntityVersion, IrBuilder, ItemKind, SemanticImageView,
        SourceIdentity, SourceSpan, TreeItemInput, VariantFingerprint, Visibility,
        encode_full_semantic_image, full_semantic_image_len,
    };
    use backend_semantic::vocabulary::{
        CompileRecipeFact, NativeTool, PackageUrl, Stage, TypeScriptSource,
    };
    use backend_version::{ContentId, SourceFactDomain, ToolchainDomain};
    use std::{
        collections::{BTreeMap, BTreeSet},
        sync::Arc,
    };

    #[test]
    fn cold_angular_parameter_field_and_import_rows_keep_semantic_ids_and_proven_coordinates()
    -> Result<(), String> {
        let source = include_str!("fixtures/angular04-retained/favorite-button.component.ts");
        let path = "src/app/features/article/components/favorite-button.component.ts";
        let profile = LanguageProfile::TypeScript(TypeScriptSource::TypeScript);
        let analysis = backend_frontend_typescript::syntax_frontend()
            .map_err(|e| e.to_string())?
            .analyze(std::path::Path::new(path), source.as_bytes())
            .map_err(|e| e.to_string())?;
        let source_id = ContentId::<SourceFactDomain>::from_canonical_bytes(source.as_bytes());
        let package = backend_engine::package_key("fixture");
        let file_key = product_source_file_key(package.to_bytes(), path);
        let record = super::super::super::ProductSourceRecord::file(
            package.to_bytes(),
            path,
            backend_engine::SourceLanguage::TypeScript,
            [1; 32],
            [2; 32],
            analysis.declarations().clone(),
        )?
        .with_source_identity(source_id)?;
        let sources = super::super::super::IndexedSources {
            projects: BTreeMap::from([(
                package.to_bytes(),
                super::super::super::IndexedProject {
                    package,
                    label: "fixture".to_owned(),
                    files: Arc::from([file_key]),
                },
            )]),
            files: vec![(file_key, record)],
            cargo_aliases: BTreeMap::new(),
            source_snapshot: None,
        };
        let owned = super::super::semantic::StructuralSites::of(&sources)
            .map_err(|e| e.to_string())?
            .declarations_of_source(package.as_bytes(), path, source_id)
            .map_err(|e| e.to_string())?;
        let sites = owned.iter().collect::<Vec<_>>();
        let mut builder = IrBuilder::new();
        builder
            .set_image_provenance_for_package(
                SourceIdentity {
                    identity: source_id,
                    byte_len: source.len() as u32,
                },
                CompileRecipeFact::derive(
                    profile,
                    Stage::LowerIr,
                    NativeTool::TypeScriptCompiler,
                    source_id,
                    ContentId::<ToolchainDomain>::from_canonical_bytes(b"fixture"),
                ),
                &PackageUrl::parse("pkg:npm/fixture@1.0.0".to_owned())
                    .map_err(|e| e.to_string())?,
                path,
            )
            .map_err(|e| e.to_string())?;
        let file = builder
            .intern_atom(path.as_bytes())
            .map_err(|e| e.to_string())?;
        let field = "private readonly articleService: ArticlesService";
        let field_start = source.find(field).ok_or("actual field")?;
        let name_start = field_start + field.find("articleService").ok_or("binding")?;
        let import = sites
            .iter()
            .copied()
            .find(|site| {
                site.kind() == backend_compile::DeclarationKind::Import
                    && site.name() == "ArticlesService"
            })
            .ok_or("actual imported binding")?;
        let SourceBindingIdentity::Exact(import_capture) = import.binding_identity() else {
            return Err("exact import capture".into());
        };
        let definitions = [
            (
                b"articleService".as_slice(),
                ItemKind::Parameter,
                (name_start as u32, (name_start + 14) as u32),
            ),
            (
                b"articleService".as_slice(),
                ItemKind::Field,
                (field_start as u32, (field_start + field.len()) as u32),
            ),
            (
                b"ArticlesService".as_slice(),
                ItemKind::Reexport,
                import_capture.owner(),
            ),
        ];
        let mut items = definitions
            .iter()
            .map(|(name, kind, extent)| TreeItemInput {
                name,
                anonymous_callable_anchor: None,
                kind: *kind,
                visibility: Visibility::Unknown,
                authority: EntityAuthorityFacts {
                    publication: DeclarationPublicationRole::PrimaryOwned,
                    source: FactAvailability::Captured,
                    source_file: FactAvailability::Captured,
                    ..EntityAuthorityFacts::default()
                },
                parent: None,
                semantic_type: None,
                members: &[],
                docs: &[],
                attributes: &[],
                source: SourceSpan::new(file, extent.0, extent.1),
                extension: None,
            })
            .collect::<Vec<_>>();
        items.push(TreeItemInput {
            name: b"synthetic_type_carrier",
            kind: ItemKind::Parameter,
            authority: EntityAuthorityFacts {
                publication: DeclarationPublicationRole::ReferenceOnly,
                ..EntityAuthorityFacts::default()
            },
            source: None,
            ..items[0]
        });
        let versions = (1..=4)
            .map(|byte| EntityVersion {
                family: DeclarationFamilyId::from_raw([byte; 16]),
                variant: VariantFingerprint::from_raw([byte; 16]),
                core_payload: CorePayloadHash::from_raw([byte; 16]),
            })
            .collect::<Vec<_>>();
        builder
            .add_borrowed_tree(BorrowedTree {
                versions: &versions,
                items: &items,
                links: &[],
            })
            .map_err(|e| e.to_string())?;
        let ir = builder.finish().map_err(|e| e.to_string())?;
        let mut bytes = vec![0; full_semantic_image_len(&ir).map_err(|e| e.to_string())?];
        encode_full_semantic_image(&ir, &mut bytes).map_err(|e| e.to_string())?;
        drop(ir);
        let image = SemanticImageView::reopen(&bytes).map_err(|e| e.to_string())?;
        let (initial, _) = super::super::super::initial_view().map_err(|e| e.to_string())?;
        let project = sources.projects.get(package.as_bytes()).ok_or("project")?;
        let projected = super::super::semantic::project_image_rows(
            &image,
            *blake3::hash(&bytes).as_bytes(),
            project,
            profile,
            initial.basis(),
            false,
            path,
            &sites,
            None,
            None,
        )
        .map_err(|e| e.to_string())?;
        assert_eq!(
            projected.rows.len(),
            3,
            "canonical support does not own a project search row"
        );
        let mut exact = super::super::exact_sites::ExactDeclarationSites::default();
        for (index, version) in versions.iter().take(3).enumerate() {
            let id = RowId::Symbol(super::super::identity::semantic_symbol(
                package,
                version.identity(),
            ));
            let projected_row = projected
                .rows
                .iter()
                .find(|row| row.row.id == id)
                .ok_or("retained semantic identity")?;
            let row = &projected_row.row;
            let expected_line = if index == 2 { 14 } else { 45 };
            assert_eq!(
                row.label,
                format!(
                    "fixture::{path}:{expected_line}::{}",
                    std::str::from_utf8(definitions[index].0).map_err(|e| e.to_string())?
                )
            );
            assert_eq!(
                row.source.captured().ok_or("source location")?.start_line(),
                expected_line
            );
            if let Some(binding) = projected_row.exact_binding {
                exact.record(package, path, binding, id);
            }
        }
        let mut plan =
            super::super::structural::StructuralProjectionPlan::of(&sources, &BTreeSet::new())
                .map_err(|e| e.to_string())?;
        plan.unify_exact_sites(&sources, &exact)
            .map_err(|e| e.to_string())?;
        let mut facts = Vec::new();
        super::super::query::append_structural_query_facts(&sources, &plan, &mut facts)
            .map_err(|e| e.to_string())?;
        assert!(
            !facts
                .iter()
                .any(|fact| fact.presentation().name == "ArticlesService"),
            "the structural import twin contributes no duplicate search identity"
        );
        Ok(())
    }
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };

    #[test]
    fn cancelled_retained_excerpt_walk_cannot_return_partial_locations() -> Result<(), String> {
        let profile =
            LanguageProfile::TypeScript(backend_semantic::vocabulary::TypeScriptSource::TypeScript);
        let source = (0..128)
            .map(|index| format!("export function run{index}() {{ return {index}; }}\n"))
            .collect::<String>();
        let analysis = backend_frontend_typescript::syntax_frontend()
            .map_err(|error| error.to_string())?
            .analyze(std::path::Path::new("retained.ts"), source.as_bytes())
            .map_err(|error| error.to_string())?;
        let sites = analysis.declarations().iter().collect::<Vec<_>>();
        let (sources, _) = super::super::tests::cross_file_sources(&[(
            "retained.ts",
            backend_engine::SourceLanguage::TypeScript,
            analysis.declarations().clone(),
        )])?;
        let record = &sources.files.first().ok_or("captured source file")?.1;
        let expected = super::super::structural::complete_declarations_for_file(&sources, record)
            .map_err(|error| error.to_string())?;
        let declaration_token = Arc::new(AtomicBool::new(false));
        {
            let _cancel = crate::builtin::cancel_search_preparation_after(
                "materialize-declaration",
                64,
                Arc::clone(&declaration_token),
            );
            let error = super::super::structural::complete_declarations_for_file_controlled(
                &sources,
                record,
                Some(&declaration_token),
            )
            .err()
            .ok_or("cancelled declaration collection returned a partial file")?;
            assert!(error.to_string().contains("search preparation cancelled"));
            assert!(declaration_token.load(Ordering::Acquire));
        }
        declaration_token.store(false, Ordering::Release);
        assert_eq!(
            super::super::structural::complete_declarations_for_file_controlled(
                &sources,
                record,
                Some(&declaration_token)
            )
            .map_err(|error| error.to_string())?,
            expected
        );
        let baseline = RetainedLocations::build(profile, &sites, None, None)?;
        assert!(!baseline.windows.is_empty());
        let token = Arc::new(AtomicBool::new(true));
        assert!(RetainedLocations::build(profile, &sites, None, Some(&token)).is_err());
        for (point, visits) in [
            ("retained-site", 64),
            ("retained-parse", 4),
            ("retained-candidate", 64),
        ] {
            token.store(false, Ordering::Release);
            let _cancel =
                crate::builtin::cancel_search_preparation_after(point, visits, Arc::clone(&token));
            let error = RetainedLocations::build(profile, &sites, None, Some(&token))
                .err()
                .ok_or("cancelled excerpt walk returned partial locations")?;
            assert!(
                error.contains("search preparation cancelled"),
                "{point}: {error}"
            );
            assert!(
                token.load(Ordering::Acquire),
                "real excerpt walk reached {point}"
            );
        }
        token.store(false, Ordering::Release);
        let resumed = RetainedLocations::build(profile, &sites, None, Some(&token))?;
        assert_eq!(resumed.windows.len(), baseline.windows.len());
        for (actual, expected) in resumed.windows.iter().zip(&baseline.windows) {
            assert_eq!(
                (actual.path, actual.text, actual.origin, actual.start_line),
                (
                    expected.path,
                    expected.text,
                    expected.origin,
                    expected.start_line
                )
            );
        }
        Ok(())
    }
}
