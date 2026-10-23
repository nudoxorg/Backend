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

    /// Exercises the native cross-file default-alias path seen in query-core:
    /// `DefaultError` is a conditional over `Register`, and `QueryState`
    /// carries it as its second defaulted type parameter. The minimal library
    /// deliberately owns a nongeneric global `Error`. This is a regression
    /// fixture for the affected authority path; it is not yet known to fail
    /// without the pending TSZ identity fix.
    #[test]
    fn cross_file_default_error_keeps_library_error_and_query_state_params_separate()
    -> Result<(), Box<dyn Error>> {
        const TYPES: &str = "export interface Register {}\n\
            export type DefaultError = Register extends { defaultError: infer TError }\n\
              ? TError\n\
              : Error;\n";
        const QUERY: &str = "import type { DefaultError } from './types';\n\
            export interface QueryState<TData = unknown, TError = DefaultError> {\n\
              data: TData | undefined;\n\
              error: TError | null;\n\
            }\n";
        const CONSUMER: &str = "import type { QueryState } from './query';\n\
            export declare const state: QueryState;\n\
            export const error: QueryState['error'] = state.error;\n";

        let mut checker = TszCheckerOptions::default();
        checker.no_lib = false;
        let recipe = TszProjectOptions {
            checker,
            semantic_options: TszProjectSemanticOptions::declaration_scoped(),
            program_diagnostics: Vec::new(),
            module_resolutions: vec![
                TszProjectModuleResolution {
                    importer_path: "src/query.ts".into(),
                    specifier: "./types".into(),
                    request_kind: TszProjectModuleRequestKind::EsmImport,
                    resolution_mode: None,
                    target: TszProjectModuleResolutionTarget::File {
                        path: "src/types.ts".into(),
                    },
                },
                TszProjectModuleResolution {
                    importer_path: "src/consumer.ts".into(),
                    specifier: "./query".into(),
                    request_kind: TszProjectModuleRequestKind::EsmImport,
                    resolution_mode: None,
                    target: TszProjectModuleResolutionTarget::File {
                        path: "src/query.ts".into(),
                    },
                },
            ],
            file_formats: Vec::new(),
            environment: TszEnvironmentFingerprint::from_sha256([0x5b; 32]),
        };
        let library = TszLibraryInput::from_utf8(
            "lib.es5.d.ts",
            b"interface Error { name: string; message: string; }".to_vec(),
        )?
        .into_lib_file();
        let mut authority = TszProjectAuthority::new();
        authority.update(
            vec![
                input("src/types.ts", TYPES),
                input("src/query.ts", QUERY),
                input("src/consumer.ts", CONSUMER),
            ],
            recipe,
            &[library],
        )?;
        let project = checked_project(&authority)?;
        let consumer = project
            .program()
            .files
            .iter()
            .position(|file| file.file_name == "src/consumer.ts")
            .ok_or_else(|| {
                io::Error::other("consumer module is absent from the checked project")
            })?;
        let query_file = project
            .program()
            .files
            .iter()
            .position(|file| file.file_name == "src/query.ts")
            .ok_or_else(|| io::Error::other("query module is absent from the checked project"))?
            as u32;
        project.with_file_checker_and_types(consumer, |checker, binder, _, types| {
            let state = binder
                .file_locals
                .get("state")
                .ok_or_else(|| io::Error::other("defaulted QueryState value is not bound"))?;
            let state_type = checker.get_type_of_symbol(state);
            let _state_type = checker.resolve_lazy_type(state_type);

            let error = binder
                .file_locals
                .get("error")
                .ok_or_else(|| io::Error::other("projected QueryState error is not bound"))?;
            let error_type = checker.get_type_of_symbol(error);
            let error_type = checker.resolve_lazy_type(error_type);
            // Inspect retained declarations rather than relying on whichever
            // structural spelling the formatter chooses. The builtin Error
            // must remain the non-program, zero-parameter declaration; the
            // authored QueryState must retain both defaulted parameters.
            let definitions = &project.program().definition_store;
            let error_defs = definitions
                .find_defs_by_name(types.intern_string("Error"))
                .unwrap_or_default();
            let library_errors = error_defs
                .iter()
                .filter_map(|def_id| definitions.get(*def_id))
                .filter(|def| def.file_id == Some(u32::MAX))
                .collect::<Vec<_>>();
            assert!(
                !library_errors.is_empty()
                    && library_errors.iter().all(|def| def.type_params.is_empty()),
                "all retained standard-library Error definitions remain present with arity zero"
            );
            let query_defs = definitions
                .find_defs_by_name(types.intern_string("QueryState"))
                .unwrap_or_default();
            let query_state = query_defs
                .iter()
                .filter_map(|def_id| definitions.get(*def_id))
                .find(|def| def.file_id == Some(query_file))
                .ok_or_else(|| io::Error::other("authored QueryState definition is absent"))?;
            assert_eq!(query_state.type_params.len(), 2);
            assert_eq!(
                query_state.type_params[0].name,
                types.intern_string("TData")
            );
            assert_eq!(
                query_state.type_params[1].name,
                types.intern_string("TError")
            );
            assert!(
                query_state
                    .type_params
                    .iter()
                    .all(|param| param.default.is_some())
            );

            // Force the default branch and retain a supplemental readable
            // result, without requiring a nominal display format.
            let error_display = checker.format_type(error_type);
            assert!(
                error_display.contains("Error"),
                "projected default error still refers to Error: {error_display}"
            );
            Ok::<_, io::Error>(())
        })?;
        Ok(())
    }
}
