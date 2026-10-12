use super::*;
use tsz_common::common::ModuleKind;

fn request(importer: &str, specifier: &str, kind: ProjectModuleRequestKind, target: ProjectModuleResolutionTarget) -> ProjectModuleResolution {
    ProjectModuleResolution {
        importer_path: importer.to_owned(),
        specifier: specifier.to_owned(),
        request_kind: kind,
        resolution_mode: None,
        target,
    }
}

fn file(path: &str) -> ProjectModuleResolutionTarget {
    ProjectModuleResolutionTarget::File { path: path.to_owned() }
}

fn external() -> ProjectModuleResolutionTarget {
    ProjectModuleResolutionTarget::External { identity: "selected-ambient:pkg".to_owned() }
}

fn assert_consumer(files: Vec<(String, String)>, resolutions: Vec<ProjectModuleResolution>, codes: &[u32]) {
    let program = compile_files_with_libs(files, &[]);
    let options = CheckerOptions { module: ModuleKind::ESNext, ..CheckerOptions::default() };
    let plan = ParallelCheckPlan::build(
        &program,
        &options,
        &[],
        ProjectSemanticOptions::declaration_scoped(),
        Some(Arc::new(build_project_module_resolution_outcomes(&program, &resolutions).unwrap())),
        None,
    ).unwrap();
    // No owner check or semantic warmup precedes this first consumer session.
    let result = plan.check_file_partition(0, &program.files[..1]).unwrap();
    assert_eq!(result[0].diagnostics.iter().map(|d| d.code).collect::<Vec<_>>(), codes, "{:?}", result[0].diagnostics);
}

#[test]
fn selected_ambient_named_edge_retains_export_equals_members_and_default_precedence() {
    for (binding, expression) in [
        ("{ value as observed }", "observed"),
        ("{ default as observed }", "observed.value"),
    ] {
        for (return_type, codes) in [("number", vec![]), ("string", vec![2322])] {
            assert_consumer(
                vec![
                    ("src/main.ts".to_owned(), format!("import {binding} from 'pkg'; export function read(): {return_type} {{ return {expression}; }}")),
                    ("ambient.d.ts".to_owned(), "declare module 'pkg' { export { value, default } from './cjs'; }".to_owned()),
                    ("src/cjs.ts".to_owned(), "declare class api { static value: number; static default: string; } export = api;".to_owned()),
                    ("src/wrong.ts".to_owned(), "export const value: string = 'wrong'; export default value;".to_owned()),
                ],
                vec![
                    request("src/main.ts", "pkg", ProjectModuleRequestKind::EsmImport, external()),
                    request("ambient.d.ts", "./cjs", ProjectModuleRequestKind::EsmReExport, file("src/cjs.ts")),
                    request("ambient.d.ts", "./cjs", ProjectModuleRequestKind::EsmImport, file("src/wrong.ts")),
                ],
                &codes,
            );
        }
    }
}

#[test]
fn selected_ambient_later_declaration_hydrates_exact_cjs_import_type() {
    for (binding, expression) in [("* as observed", "observed.value"), ("{ value as observed }", "observed")] {
        for (return_type, codes) in [("number", vec![]), ("string", vec![2322])] {
            assert_consumer(
                vec![
                    ("src/main.ts".to_owned(), format!("import {binding} from 'pkg'; export function read(): {return_type} {{ return {expression}; }}")),
                    ("ambient-first.d.ts".to_owned(), "declare module 'pkg' {}".to_owned()),
                    ("ambient-later.d.ts".to_owned(), "declare module 'pkg' { export * from './cjs'; }".to_owned()),
                    ("src/cjs.ts".to_owned(), "declare const api: typeof import('./owner'); export = api;".to_owned()),
                    ("src/owner.ts".to_owned(), "export const value: number = 42;".to_owned()),
                    ("src/wrong.ts".to_owned(), "export const value: string = 'wrong';".to_owned()),
                ],
                vec![
                    request("src/main.ts", "pkg", ProjectModuleRequestKind::EsmImport, external()),
                    request("ambient-later.d.ts", "./cjs", ProjectModuleRequestKind::EsmReExport, file("src/cjs.ts")),
                    request("ambient-later.d.ts", "./cjs", ProjectModuleRequestKind::EsmImport, file("src/wrong.ts")),
                    request("src/cjs.ts", "./owner", ProjectModuleRequestKind::EsmImport, file("src/owner.ts")),
                    request("src/cjs.ts", "./owner", ProjectModuleRequestKind::EsmReExport, file("src/wrong.ts")),
                ],
                &codes,
            );
        }
    }
}

#[test]
fn selected_ambient_file_reexport_cycle_retains_direct_export_without_ambient_reentry() {
    for (return_type, codes) in [("number", vec![]), ("string", vec![2322])] {
        assert_consumer(
            vec![
                ("src/main.ts".to_owned(), format!("import * as observed from 'pkg'; export function read(): {return_type} {{ return observed.value; }}")),
                ("ambient.d.ts".to_owned(), "declare module 'pkg' { export * from './barrel'; }".to_owned()),
                ("src/barrel.ts".to_owned(), "export * from 'pkg'; export const value: number = 42;".to_owned()),
            ],
            vec![
                request("src/main.ts", "pkg", ProjectModuleRequestKind::EsmImport, external()),
                request("ambient.d.ts", "./barrel", ProjectModuleRequestKind::EsmReExport, file("src/barrel.ts")),
                request("src/barrel.ts", "pkg", ProjectModuleRequestKind::EsmReExport, external()),
            ],
            &codes,
        );
    }
}

#[test]
fn selected_ambient_cjs_import_type_cycle_keeps_real_direct_members() {
    for (return_type, codes) in [("number", vec![]), ("string", vec![2322])] {
        assert_consumer(
            vec![
                ("src/main.ts".to_owned(), format!("import * as observed from 'pkg'; export function read(): {return_type} {{ return observed.value; }}")),
                ("ambient.d.ts".to_owned(), "declare module 'pkg' { export const value: number; export * from './cjs'; }".to_owned()),
                ("src/cjs.ts".to_owned(), "declare const api: typeof import('pkg'); export = api;".to_owned()),
            ],
            vec![
                request("src/main.ts", "pkg", ProjectModuleRequestKind::EsmImport, external()),
                request("ambient.d.ts", "./cjs", ProjectModuleRequestKind::EsmReExport, file("src/cjs.ts")),
                request("src/cjs.ts", "pkg", ProjectModuleRequestKind::EsmImport, external()),
            ],
            &codes,
        );
    }
}

#[test]
fn selected_ambient_named_edge_precedes_another_declarations_wildcard_but_not_direct_export() {
    for direct in [false, true] {
        for (binding, expression) in [("* as observed", "observed.X"), ("{ X as observed }", "observed")] {
            for (return_type, codes) in if direct {
                [("number", vec![2322]), ("string", vec![])]
            } else {
                [("number", vec![]), ("string", vec![2322])]
            } {
                let mut files = vec![
                    ("src/main.ts".to_owned(), format!("import {binding} from 'pkg'; export function read(): {return_type} {{ return {expression}; }}")),
                    ("ambient-wildcard.d.ts".to_owned(), "declare module 'pkg' { export * from './a'; }".to_owned()),
                    ("ambient-named.d.ts".to_owned(), "declare module 'pkg' { export { Y as X } from './b'; }".to_owned()),
                    ("src/a.ts".to_owned(), "export const X: string = 'wildcard';".to_owned()),
                    ("src/b.ts".to_owned(), "export const Y: number = 42;".to_owned()),
                ];
                if direct {
                    files.push(("ambient-direct.d.ts".to_owned(), "declare module 'pkg' { export const X: string; }".to_owned()));
                }
                assert_consumer(
                    files,
                    vec![
                        request("src/main.ts", "pkg", ProjectModuleRequestKind::EsmImport, external()),
                        request("ambient-wildcard.d.ts", "./a", ProjectModuleRequestKind::EsmReExport, file("src/a.ts")),
                        request("ambient-named.d.ts", "./b", ProjectModuleRequestKind::EsmReExport, file("src/b.ts")),
                        // A different kind cannot substitute A for the named owner B.
                        request("ambient-named.d.ts", "./b", ProjectModuleRequestKind::EsmImport, file("src/a.ts")),
                    ],
                    &codes,
                );
            }
        }
    }
}
