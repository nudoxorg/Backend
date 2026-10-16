//! Captured mixed-project controls with the exact Docs8b JS-config / TSX-test bytes.
//! These run the linked resolver, native authority and checker; no external TS.
//! The JSON type probe and small dependency/config environment are synthetic.

use super::*;

const JS_PATH: &str = "src/frontend/.prettierrc.js";
const TS_PATH: &str = "src/frontend/main.ts";
const TSX_PATH: &str = "src/frontend/apps/impress/src/api/__tests__/fetchApi.test.tsx";
const JSON_TYPES_PATH: &str = "src/frontend/apps/impress/src/api/__tests__/json-types.test.tsx";
const PACKAGE_JSON_PATH: &str = "src/frontend/apps/impress/package.json";
const JSON_PATH: &str = "src/frontend/apps/impress/src/api/__tests__/settings.json";
const JSON: &str = r#"{"enabled":true,"title":"Docs","text":"require('not-an-import')"}"#;
// Read-only Docs8b capture: SHA256 0fc4b979be7c1bf432e39d701569a6d750f5b09e58e53f8e04df2fcfc08bb08c (114 B).
const JS: &str = include_str!("fixtures/docs-prettierrc.js");
const TS: &str = "export const profile: string = 'typescript';";
// SHA256 c1b278a13b813d2a92ea041ae28e8a24298110a6697247cb81d12b43a7c18ed3 (1524 B).
const TSX: &str = include_str!("fixtures/docs-fetchApi.test.tsx");
const JSON_TYPES: &str = r#"import manifest from '../../../package.json';
import settings from './settings.json';
export const version: string = manifest.version;
export const title: string = settings.title;
export const wrong: string = settings.enabled;
export const view = <section>{settings.title}</section>;
"#;

fn native(fixture: &Fixture, sources: &[PackageSource<'_>]) -> NativeTypeScriptInputs {
    let admitted = fixture.admit();
    let inputs = admitted.inputs();
    let mut resolver = inputs.resolver();
    let native = build_native_inputs(
        &inputs,
        &mut resolver,
        sources,
        Instant::now() + Duration::from_secs(30),
        &AtomicBool::new(false),
    )
    .unwrap();
    native
        .closure_witness
        .validate_current(admitted.witness())
        .unwrap();
    native
}

fn configured(resolve_json: bool) -> Fixture {
    let fixture = Fixture::new();
    fixture.write("tsconfig.json", &format!(r#"{{"compilerOptions":{{"noLib":true,"types":[],"strict":true,"allowJs":true,"checkJs":false,"resolveJsonModule":{resolve_json},"module":"ESNext","moduleResolution":"bundler","esModuleInterop":true,"jsx":"preserve"}},"files":["{JS_PATH}","{TS_PATH}","{TSX_PATH}","{JSON_TYPES_PATH}","src/frontend/jsx.d.ts","src/frontend/component.jsx","src/frontend/esm.mjs","src/frontend/cjs.cjs","src/frontend/esm.mts","src/frontend/cjs.cts","src/frontend/types.d.mts","src/frontend/types.d.cts"]}}"#));
    fixture.write(JS_PATH, JS);
    fixture.write(TS_PATH, TS);
    fixture.write(TSX_PATH, TSX);
    fixture.write(JSON_TYPES_PATH, JSON_TYPES);
    fixture.write(
        PACKAGE_JSON_PATH,
        r#"{"name":"impress","version":"1.2.3","type":"commonjs"}"#,
    );
    fixture.write(JSON_PATH, JSON);
    fixture.write(
        "src/frontend/package.json",
        r#"{"name":"docs-shape","type":"commonjs"}"#,
    );
    fixture.write("src/frontend/jsx.d.ts", "declare namespace JSX { interface IntrinsicElements { section: { children?: unknown }; } }");
    fixture.write(
        "src/frontend/component.jsx",
        "export const component = <section />;",
    );
    fixture.write("src/frontend/esm.mjs", "export const esm = 1;");
    fixture.write("src/frontend/cjs.cjs", "module.exports = { cjs: 1 };");
    fixture.write("src/frontend/esm.mts", "export const esm: number = 1;");
    fixture.write("src/frontend/cjs.cts", "export const cjs: number = 1;");
    fixture.write(
        "src/frontend/types.d.mts",
        "export declare const esmType: number;",
    );
    fixture.write(
        "src/frontend/types.d.cts",
        "export declare const cjsType: number;",
    );
    fixture
}

#[test]
fn both_captured_profiles_keep_mixed_native_kinds_and_json_value_types() {
    // The two package frontiers select the same configured program. The JS
    // config and TSX test stay present; neither is excluded to hide a refusal.
    for tsx_profile in [false, true] {
        let fixture = configured(true);
        let primary = if tsx_profile {
            vec![PackageSource::new(TSX_PATH, TSX).unwrap()]
        } else {
            vec![
                PackageSource::new(JS_PATH, JS).unwrap(),
                PackageSource::new(TS_PATH, TS).unwrap(),
            ]
        };
        let native = native(&fixture, &primary);
        assert!(native.options.checker.resolve_json_module);
        assert!(
            native
                .sources
                .iter()
                .any(|source| source.path == JSON_PATH && source.source == JSON)
        );
        assert!(
            native
                .sources
                .iter()
                .any(|source| source.path == PACKAGE_JSON_PATH),
            "package metadata becomes a semantic member only when actually imported"
        );
        assert!(
            !native
                .sources
                .iter()
                .any(|source| source.path == "src/frontend/package.json"
                    || source.path == "tsconfig.json"),
            "captured config/package metadata is not a semantic member"
        );
        assert!(native.options.module_resolutions.iter().any(|resolution|
            resolution.importer_path == JSON_TYPES_PATH && resolution.specifier == "./settings.json"
            && matches!(&resolution.target, TszProjectModuleResolutionTarget::File {path} if path == JSON_PATH)));
        assert!(
            !native
                .options
                .module_resolutions
                .iter()
                .any(|resolution| resolution.importer_path == JSON_PATH),
            "JSON strings never become TS imports"
        );
        for source in &primary {
            assert!(matches!(
                &native.package_membership[source.relative_path()],
                TypeScriptPackageSourceMembership::Selected { .. }
            ));
        }
        let mut authority = backend_frontend_typescript::TszProjectAuthority::new();
        authority
            .update(native.sources, native.options, &native.libraries)
            .unwrap();
        let project = authority.project().unwrap();
        for (path, kind, variant) in [
            (JS_PATH, 1, 1),
            (TS_PATH, 3, 0),
            (TSX_PATH, 4, 1),
            (JSON_TYPES_PATH, 4, 1),
            (PACKAGE_JSON_PATH, 6, 0),
            (JSON_PATH, 6, 0),
            ("src/frontend/component.jsx", 2, 1),
            ("src/frontend/esm.mjs", 1, 1),
            ("src/frontend/cjs.cjs", 1, 1),
            ("src/frontend/esm.mts", 3, 0),
            ("src/frontend/cjs.cts", 3, 0),
            ("src/frontend/types.d.mts", 3, 0),
            ("src/frontend/types.d.cts", 3, 0),
        ] {
            let bound = project.bind_result(path).unwrap();
            assert!(
                bound.parse_diagnostics.is_empty(),
                "native parser for {path}"
            );
            let source = bound.arena.get_source_file_at(bound.source_file).unwrap();
            assert_eq!(
                (source.script_kind, source.language_variant),
                (kind, variant),
                "{path}"
            );
        }
        assert!(
            project
                .diagnostics()
                .any(|diagnostic| diagnostic.code == 2322 && diagnostic.file == JSON_TYPES_PATH),
            "the native JSON boolean type must reject assignment to string, not become any"
        );
    }
}

#[test]
fn disabled_json_resolution_keeps_the_unresolved_import_and_real_checker_refusal() {
    let fixture = configured(false);
    let native = native(&fixture, &[PackageSource::new(TSX_PATH, TSX).unwrap()]);
    assert!(!native.sources.iter().any(|source| source.path == JSON_PATH));
    assert!(
        native
            .options
            .module_resolutions
            .iter()
            .any(|resolution| resolution.importer_path == JSON_TYPES_PATH
                && resolution.specifier == "./settings.json"
                && matches!(
                    &resolution.target,
                    TszProjectModuleResolutionTarget::Unresolved
                ))
    );
    let mut authority = backend_frontend_typescript::TszProjectAuthority::new();
    authority
        .update(native.sources, native.options, &native.libraries)
        .unwrap();
    assert!(
        authority
            .project()
            .unwrap()
            .diagnostics()
            .any(|diagnostic| matches!(diagnostic.code, 2307 | 2732)
                && diagnostic.file == JSON_TYPES_PATH),
        "disabled resolveJsonModule cannot silently produce current JSON types"
    );
}

#[test]
fn malformed_imported_json_surfaces_its_native_json_diagnostic() {
    let fixture = configured(true);
    let mut authority = backend_frontend_typescript::TszProjectAuthority::new();
    for malformed in [r#"{"title":"é🦀","enabled": }"#, r#"{"enabled":true"#] {
        fixture.write(JSON_PATH, malformed);
        let native = native(&fixture, &[PackageSource::new(TSX_PATH, TSX).unwrap()]);
        authority
            .update(native.sources, native.options, &native.libraries)
            .unwrap();
        let project = authority.project().unwrap();
        assert!(
            !project
                .bind_result(JSON_PATH)
                .unwrap()
                .parse_diagnostics
                .is_empty()
        );
        let diagnostics = project
            .diagnostics()
            .filter(|diagnostic| diagnostic.file == JSON_PATH)
            .collect::<Vec<_>>();
        assert!(
            !diagnostics.is_empty(),
            "invalid JSON is retained and diagnosed, never hidden as metadata"
        );
        assert!(
            diagnostics
                .iter()
                .all(|diagnostic| project.diagnostic_utf8_span(diagnostic).is_some()),
            "native JSON diagnostics refer to exact UTF-8 source ranges, including Unicode and EOF"
        );
    }
}

#[test]
fn captured_json_bytes_invalidate_the_recipe_and_native_value_type() {
    let fixture = configured(true);
    let sources = [PackageSource::new(TSX_PATH, TSX).unwrap()];
    let first = native(&fixture, &sources);
    let environment = first.options.environment;
    let mut authority = backend_frontend_typescript::TszProjectAuthority::new();
    authority
        .update(first.sources, first.options, &first.libraries)
        .unwrap();
    assert!(
        authority
            .project()
            .unwrap()
            .diagnostics()
            .any(|diagnostic| diagnostic.code == 2322 && diagnostic.file == JSON_TYPES_PATH)
    );
    fixture.write(JSON_PATH, r#"{"enabled":"now a string","title":"Docs"}"#);
    let next = native(&fixture, &sources);
    assert_ne!(
        environment, next.options.environment,
        "captured JSON bytes participate in the exact recipe"
    );
    let report = authority
        .update(next.sources, next.options, &next.libraries)
        .unwrap();
    assert!(!report.reused_project_result);
    assert!(
        !authority
            .project()
            .unwrap()
            .diagnostics()
            .any(|diagnostic| diagnostic.code == 2322 && diagnostic.file == JSON_TYPES_PATH),
        "the changed native JSON type makes the string assignment valid"
    );
}
