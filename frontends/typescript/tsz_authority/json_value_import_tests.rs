// Real linked project/checker controls for the caller-selected JSON target.
mod json_value_import_tests {
    use super::*;
    use std::{error::Error, io};

    const MAIN: &str = "src/main.ts";
    const SELECTED: &str = "captured/data/settings.json";
    const DECOY: &str = "src/settings.json";
    const VALUES: &str = r#"{"enabled":true,"count":7,"title":"Docs"}"#;
    const STRINGS: &str = r#"{"enabled":"yes","count":"seven","title":"Docs"}"#;
    const SOURCE: &str = "import settings from './settings.json';\n\
        import * as namespace from './settings.json';\n\
        export const wrong_boolean: string = settings.enabled;\n\
        export const wrong_number: string = settings.count;\n\
        export const wrong_namespace: string = namespace.enabled;\n\
        export const valid_title: string = settings.title;\n";

    fn sources(selected: &str) -> Vec<TszFileInput> {
        vec![
            input(MAIN, SOURCE),
            input(SELECTED, selected),
            // The conventional relative path is present, but it is not the
            // admitted request target. Looking it up by spelling hides errors.
            input(DECOY, STRINGS),
        ]
    }

    fn recipe(
        target: TszProjectModuleResolutionTarget,
    ) -> Result<TszProjectOptions, TszCompilerOptionsError> {
        let mut recipe = options();
        recipe.checker = checker_options_from_compiler_api_json(
            r#"{"compilerOptions":{"noLib":true,"strict":true,"resolveJsonModule":true,"module":"ESNext","moduleResolution":"bundler","esModuleInterop":true}}"#,
        )?;
        recipe.module_resolutions = vec![TszProjectModuleResolution {
            importer_path: MAIN.into(),
            specifier: "./settings.json".into(),
            request_kind: TszProjectModuleRequestKind::EsmImport,
            resolution_mode: None,
            target,
        }];
        Ok(recipe)
    }

    fn selected_recipe() -> Result<TszProjectOptions, TszCompilerOptionsError> {
        recipe(TszProjectModuleResolutionTarget::File {
            path: SELECTED.into(),
        })
    }

    fn checked_project(authority: &TszProjectAuthority) -> Result<&TszProject, io::Error> {
        authority
            .project()
            .ok_or_else(|| io::Error::other("project update completed without a checked project"))
    }

    fn assignments(project: &TszProject) -> Vec<(u32, String)> {
        project
            .diagnostics()
            .filter(|diagnostic| diagnostic.file == MAIN && diagnostic.code == 2322)
            .map(|diagnostic| {
                assert!(project.diagnostic_utf8_span(diagnostic).is_some());
                (diagnostic.start, diagnostic.message_text.clone())
            })
            .collect()
    }

    #[test]
    fn configured_json_default_imports_keep_exact_value_types_in_fresh_and_partitioned_checks()
    -> Result<(), Box<dyn Error>> {
        let mut fresh = TszProjectAuthority::new();
        fresh.update(sources(VALUES), selected_recipe()?, &[])?;
        let expected = assignments(checked_project(&fresh)?);
        assert_eq!(
            expected.len(),
            3,
            "default and namespace imports retain JSON types: {expected:?}"
        );
        assert_eq!(
            expected
                .iter()
                .filter(|(_, message)| message.contains("boolean") && message.contains("string"))
                .count(),
            2
        );
        assert_eq!(
            expected
                .iter()
                .filter(|(_, message)| message.contains("number") && message.contains("string"))
                .count(),
            1
        );
        for workers in [1, 2] {
            let cancelled = AtomicBool::new(false);
            let budget = execution_budget(&cancelled, 1_000_000);
            let mut partitioned = TszProjectAuthority::new();
            partitioned.update_with_checker_execution(
                sources(VALUES),
                selected_recipe()?,
                &[],
                TszProjectCheckerExecution {
                    checkpoint: &budget,
                    workers: std::num::NonZeroUsize::new(workers)
                        .ok_or_else(|| io::Error::other("checker worker count must be nonzero"))?,
                },
            )?;
            assert_eq!(
                assignments(checked_project(&partitioned)?),
                expected,
                "the exact JSON target owns diagnostics with {workers} checker workers"
            );
        }
        Ok(())
    }

    #[test]
    fn configured_json_edits_retargets_and_unresolved_requests_cannot_reuse_or_guess_types()
    -> Result<(), Box<dyn Error>> {
        let mut authority = TszProjectAuthority::new();
        authority.update(sources(VALUES), selected_recipe()?, &[])?;
        assert_eq!(assignments(checked_project(&authority)?).len(), 3);

        let changed = authority.update(sources(STRINGS), selected_recipe()?, &[])?;
        assert_eq!(
            changed.parsed_and_bound, 1,
            "only the selected JSON bytes changed"
        );
        assert_eq!(changed.reused_binds, 2);
        assert!(!changed.reused_project_result);
        assert!(
            assignments(checked_project(&authority)?).is_empty(),
            "the actual selected JSON now supplies strings"
        );

        authority.update(sources(VALUES), selected_recipe()?, &[])?;
        assert_eq!(assignments(checked_project(&authority)?).len(), 3);
        let retargeted = authority.update(
            sources(VALUES),
            recipe(TszProjectModuleResolutionTarget::File { path: DECOY.into() })?,
            &[],
        )?;
        assert_eq!(retargeted.parsed_and_bound, 0);
        assert!(!retargeted.reused_project_result);
        assert!(
            assignments(checked_project(&authority)?).is_empty(),
            "only a new exact selected target can use the conventional-path strings"
        );

        authority.update(
            sources(VALUES),
            recipe(TszProjectModuleResolutionTarget::Unresolved)?,
            &[],
        )?;
        let project = checked_project(&authority)?;
        assert!(project.diagnostics().any(|diagnostic| diagnostic.file == MAIN
            && matches!(diagnostic.code, 2307 | 2732)),
            "neither available JSON file can replace an unresolved request");
        assert!(
            assignments(project).is_empty(),
            "unresolved-import recovery stays separate from JSON authority"
        );
        Ok(())
    }
    #[test]
    fn configured_json_default_alias_keeps_captured_modes_without_losing_type_only_refusal()
    -> Result<(), Box<dyn Error>> {
        let mut recipe = selected_recipe()?;
        // Match the captured engine program's semantic policy, explicit
        // resolver mode and per-file format observations, not only the
        // frontend fixture's legacy mode-inapplicable key.
        recipe.semantic_options = TszProjectSemanticOptions::declaration_scoped();
        recipe.module_resolutions[0].resolution_mode = Some(TszResolutionModeOverride::Import);
        recipe.file_formats = [MAIN, SELECTED, DECOY]
            .into_iter()
            .map(|path| TszProjectFileFormatInput {
                path: path.into(),
                format: TszProjectFileFormat::EsModule,
            })
            .collect();
        let mut authority = TszProjectAuthority::new();
        authority.update(sources(VALUES), recipe.clone(), &[])?;
        assert_eq!(assignments(checked_project(&authority)?).len(), 3);

        let mut type_only = sources(VALUES);
        type_only[0].source = "import type settings from './settings.json';\n\
            export const wrong: string = settings.enabled;\n"
            .into();
        let report = authority.update(type_only, recipe, &[])?;
        assert!(!report.reused_project_result);
        let project = checked_project(&authority)?;
        assert!(
            project.diagnostics().any(|diagnostic| {
                diagnostic.file == MAIN
                    && diagnostic.code == 1361
                    && project.diagnostic_utf8_span(diagnostic).is_some()
            }),
            "preserving a JSON value shape never authorizes an import-type value read: {:?}",
            project.diagnostics().collect::<Vec<_>>()
        );
        Ok(())
    }
}
