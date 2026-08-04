//! Phase 0 toolchain proof for the OXC crate set (OXC-PLAN.md §4 Phase 0).
//!
//! This is a THROWAWAY link/API smoke test. It exists only to prove the
//! vendored oxc 0.139.0 + oxc_resolver 11.23.0 crates build and link together
//! under Buck2. It does NOT touch the existing deno_doc TypeScript producer.
//!
//! It:
//!   1. parses a fixture `.d.ts` string,
//!   2. builds semantic analysis,
//!   3. asserts on `module_record.local_export_entries` (non-empty),
//!   4. link-checks oxc_resolver / oxc_isolated_declarations / oxc_jsdoc are
//!      usable so the whole set links.

use oxc_allocator::Allocator;
use oxc_isolated_declarations::{IsolatedDeclarations, IsolatedDeclarationsOptions};
use oxc_parser::Parser;
use oxc_resolver::{ResolveOptions, Resolver};
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;

const FIXTURE: &str = r#"
export interface Foo {
  bar: string;
  baz(n: number): boolean;
}

export type Alias = Foo | number;

export declare function frob(x: Alias): Promise<void>;

export default class Widget {
  constructor(public readonly id: number) {}
  get name(): string { return ""; }
}
"#;

#[test]
fn parse_semantic_and_module_record() {
    let allocator = Allocator::new();
    let source_type = SourceType::d_ts();

    // 1. Parse the fixture `.d.ts`.
    let ret = Parser::new(&allocator, FIXTURE, source_type).parse();
    assert!(!ret.panicked, "parser panicked on fixture .d.ts");

    // 3. `export` declarations must surface as local export entries.
    let exports = &ret.module_record.local_export_entries;
    assert!(
        !exports.is_empty(),
        "expected non-empty local_export_entries for a fixture full of `export`s"
    );

    // 2. Build semantic analysis (jsdoc feature enabled in the vendored crate).
    let sem = SemanticBuilder::new()
        .with_check_syntax_error(false)
        .build(&ret.program);
    // The JSDoc finder is only present with the `jsdoc` cargo feature; touching
    // it link-checks oxc_jsdoc.
    let _jsdoc = sem.semantic.jsdoc();
    assert!(
        sem.semantic.scoping().symbol_ids().count() > 0,
        "semantic analysis produced no symbols"
    );
}

#[test]
fn link_check_isolated_declarations() {
    // Link-check oxc_isolated_declarations: construct + build over a source `.ts`.
    let allocator = Allocator::new();
    let source_type = SourceType::ts();
    let src = "export function id<T>(x: T): T { return x; }\n";
    let ret = Parser::new(&allocator, src, source_type).parse();

    let id = IsolatedDeclarations::new(
        &allocator,
        IsolatedDeclarationsOptions {
            strip_internal: false,
        },
    );
    let out = id.build(&ret.program);
    // We only care that it links + runs; the emitted program is a valid handle.
    let _ = out.program;
}

#[test]
fn link_check_resolver() {
    // Link-check oxc_resolver: default options + resolve_dts is callable.
    let resolver = Resolver::new(ResolveOptions::default());
    // Resolving against a nonexistent path is expected to error; we only prove
    // the symbol links and the call compiles.
    let cwd = std::env::current_dir().unwrap();
    let _ = resolver.resolve(&cwd, "./definitely-not-a-real-module-xyz");
    let _ = resolver.resolve_dts(&cwd, "./definitely-not-a-real-module-xyz");
}
