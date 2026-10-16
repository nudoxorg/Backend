//! Actual captured Angular entry/config/service bytes with a bounded synthetic
//! local graph. External Angular/RxJS dependencies deliberately remain absent.

use super::*;

const MAIN: &str = include_str!("fixtures/angular-main.ts");
const SERVICE: &str = include_str!("fixtures/angular-articles.service.ts");

#[test]
fn captured_angular_entry_keeps_dotted_static_dynamic_and_model_imports() {
    let fixture = Fixture::new();
    fixture.write("angular.json", include_str!("fixtures/angular.json"));
    fixture.write(
        "tsconfig.json",
        include_str!("fixtures/angular-tsconfig.json"),
    );
    fixture.write(
        "tsconfig.app.json",
        include_str!("fixtures/angular-tsconfig.app.json"),
    );
    let files = [
        ("src/main.ts", MAIN),
        (
            "src/app/app.component.ts",
            "import { ArticlesService } from './features/article/services/articles.service'; export class AppComponent { readonly service?: ArticlesService; }",
        ),
        (
            "src/app/app.config.ts",
            "import { routes } from './app.routes'; export const appConfig = { providers: [], routes };",
        ),
        (
            "src/app/app.routes.ts",
            "export const routes = [{ path: 'article', loadComponent: () => import('./features/article/article.component').then(m => m.ArticleComponent) }];",
        ),
        (
            "src/app/features/article/article.component.ts",
            "import { ArticlesService } from './services/articles.service'; export class ArticleComponent { constructor(readonly service: ArticlesService) {} }",
        ),
        (
            "src/app/features/article/services/articles.service.ts",
            SERVICE,
        ),
        (
            "src/app/features/article/models/article-list-config.model.ts",
            "export interface ArticleListConfig { type: string; filters: Record<string, string>; }",
        ),
        (
            "src/app/features/article/models/article.model.ts",
            "export interface Article { slug: string; title: string; }",
        ),
        ("src/unreferenced.ts", "export const unrelated = true;"),
    ];
    for (path, source) in files {
        fixture.write(path, source);
    }
    let admitted = fixture.admit();
    let inputs = admitted.inputs();
    let mut resolver = inputs.resolver();
    let sources = files
        .iter()
        .map(|(path, source)| PackageSource::new(path, source).unwrap())
        .collect::<Vec<_>>();
    let native = build_native_inputs(
        &inputs,
        &mut resolver,
        &sources,
        Instant::now() + Duration::from_secs(30),
        &AtomicBool::new(false),
    )
    .expect("captured native program");
    native
        .closure_witness
        .validate_current(admitted.witness())
        .expect("exact captured closure");
    assert!(!fixture.0.join("node_modules").exists());
    assert_eq!(
        native.sources.len(),
        8,
        "configured main plus reachable local files, not just the one root or all captured files"
    );
    for (path, source) in files {
        if path == "src/unreferenced.ts" {
            assert_eq!(
                native.package_membership[path],
                TypeScriptPackageSourceMembership::ExcludedFromConfiguredProgram
            );
        } else {
            assert!(
                matches!(&native.package_membership[path], TypeScriptPackageSourceMembership::Selected { program_path } if &**program_path == path),
                "selected native source: {path}"
            );
            assert!(
                native
                    .sources
                    .iter()
                    .any(|file| file.path == path && file.source == source),
                "retained exact source: {path}"
            );
        }
    }
    for (importer, specifier, target, kind) in [
        (
            "src/main.ts",
            "./app/app.component",
            "src/app/app.component.ts",
            TszProjectModuleRequestKind::EsmImport,
        ),
        (
            "src/main.ts",
            "./app/app.config",
            "src/app/app.config.ts",
            TszProjectModuleRequestKind::EsmImport,
        ),
        (
            "src/app/app.config.ts",
            "./app.routes",
            "src/app/app.routes.ts",
            TszProjectModuleRequestKind::EsmImport,
        ),
        (
            "src/app/app.routes.ts",
            "./features/article/article.component",
            "src/app/features/article/article.component.ts",
            TszProjectModuleRequestKind::DynamicImport,
        ),
        (
            "src/app/features/article/services/articles.service.ts",
            "../models/article-list-config.model",
            "src/app/features/article/models/article-list-config.model.ts",
            TszProjectModuleRequestKind::EsmImport,
        ),
        (
            "src/app/features/article/services/articles.service.ts",
            "../models/article.model",
            "src/app/features/article/models/article.model.ts",
            TszProjectModuleRequestKind::EsmImport,
        ),
    ] {
        assert!(native.options.module_resolutions.iter().any(|request|
            request.importer_path == importer && request.specifier == specifier && request.request_kind == kind
                && matches!(&request.target, TszProjectModuleResolutionTarget::File { path } if path == target)),
            "native closure edge {importer} -> {specifier}");
    }
    for specifier in [
        "@angular/platform-browser",
        "@angular/core",
        "@angular/common/http",
        "rxjs",
        "rxjs/operators",
    ] {
        assert!(
            native
                .options
                .module_resolutions
                .iter()
                .any(|request| request.specifier == specifier
                    && matches!(
                        &request.target,
                        TszProjectModuleResolutionTarget::Unresolved
                    )),
            "missing external declarations remain unresolved: {specifier}"
        );
    }
}
