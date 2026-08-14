//! Phase 0 toolchain proof for the tsz-core 0.1.9 crate set.
//!
//! THROWAWAY link/API smoke test. It exists only to prove the vendored
//! tsz-core 0.1.9 crate set builds and links under Buck2.
//!
//! It does NOT touch the existing OXC or deno_doc TypeScript producers.
//!
//! Coverage:
//!   1. create_scanner / ScannerState — tokenise a short `.ts` snippet,
//!      verify we can call scan() and read back a token.
//!   2. create_parser / Parser — full single-file pipeline:
//!      parse_source_file → bind_source_file → check_source_file → emit.
//!   3. create_program / WasmProgram — construct the multi-file program handle
//!      and add a source file (link-checks the whole WasmProgram surface).
//!   4. CheckerState type alias — re-exported from tsz_checker::state; touching
//!      it proves the checker sub-crate link-resolves through tsz_core.
//!
//! Key symbols checked:
//!   tsz_core::create_scanner
//!   tsz_core::create_parser
//!   tsz_core::create_program
//!   tsz_core::scanner_impl::ScannerState
//!   tsz_core::checker::CheckerState   (type alias – just name-resolved here)
//!   tsz_core::checker::CheckerOptions

// Pull in the re-exported types we intend to link-check.
use tsz_core::{create_parser, create_program, create_scanner};
// CheckerState lives in tsz_core::checker (re-export of tsz_checker::state).
// CheckerOptions is re-exported from tsz_common via tsz_checker.
use tsz_core::checker::{CheckerOptions, CheckerState};

/// A minimal TypeScript snippet used across tests.
const FIXTURE: &str = r#"
export interface Greet {
    message: string;
}

export function greet(name: string): Greet {
    return { message: "hello " + name };
}

export type MaybeGreet = Greet | null;
"#;

// ---------------------------------------------------------------------------
// Test 1 – scanner link-check
// ---------------------------------------------------------------------------

#[test]
fn scanner_link_check() {
    // create_scanner is the non-wasm entry point for ScannerState.
    let mut scanner = create_scanner(FIXTURE.to_owned(), /* skip_trivia */ true);

    // scan() advances through tokens; after at least one call the position
    // must have moved beyond 0 (whitespace-only leading trivia is skipped).
    let _tok = scanner.scan();
    // get_pos() returns the current scanner position in the source text.
    let pos = scanner.get_pos();
    assert!(
        pos > 0,
        "expected scanner to advance past position 0 after first scan(); got {pos}"
    );
}

// ---------------------------------------------------------------------------
// Test 2 – parser / single-file pipeline link-check
// ---------------------------------------------------------------------------

#[test]
fn parser_pipeline_link_check() {
    // create_parser is the non-wasm constructor for the single-file Parser.
    let mut parser = create_parser("smoke.ts".to_owned(), FIXTURE.to_owned());

    // Phase 1: parse.
    let _node_count = parser.parse_source_file();

    // Phase 2: bind (returns diagnostics JSON; we just prove it's callable).
    let bind_json = parser.bind_source_file();
    // The return is a JSON string; must be valid JSON.
    let bind_val: serde_json::Value = serde_json::from_str(&bind_json)
        .unwrap_or_else(|e| panic!("bind_source_file() returned invalid JSON: {e}\nraw: {bind_json}"));
    drop(bind_val);

    // Phase 3: check.
    let check_json = parser.check_source_file();
    let check_val: serde_json::Value = serde_json::from_str(&check_json)
        .unwrap_or_else(|e| panic!("check_source_file() returned invalid JSON: {e}\nraw: {check_json}"));
    drop(check_val);

    // Phase 4: emit (TypeScript → JavaScript transpile).
    let emitted = parser.emit();
    // The emitted output must be non-empty for a non-trivial source file.
    assert!(
        !emitted.is_empty(),
        "emit() returned an empty string for a non-trivial fixture"
    );
}

// ---------------------------------------------------------------------------
// Test 3 – WasmProgram multi-file handle link-check
// ---------------------------------------------------------------------------

#[test]
fn program_link_check() {
    // create_program() is the non-wasm constructor for WasmProgram.
    let mut prog = create_program();

    // WasmProgram::add_file is exposed via #[wasm_bindgen]; on native
    // builds it is callable directly. We add the fixture as a named file.
    prog.add_file("smoke.ts".to_owned(), FIXTURE.to_owned());

    // get_file_count() reports how many source files are registered.
    let count = prog.get_file_count();
    assert_eq!(count, 1, "expected 1 file after add_file, got {count}");
}

// ---------------------------------------------------------------------------
// Test 4 – CheckerState / CheckerOptions name resolution
// ---------------------------------------------------------------------------

#[test]
fn checker_state_name_resolution() {
    // CheckerState and CheckerOptions are re-exported through tsz_core::checker.
    // This test proves the type names link-resolve correctly.
    //
    // CheckerState<'a> requires a live binder arena to construct, so we prove
    // it resolves by naming it in a PhantomData — a compile-time-only check.
    // CheckerOptions does not implement Default (all fields are bool/enum with
    // no obvious zero), so we also prove it via PhantomData.
    fn _assert_checker_types_resolve(
        _s: std::marker::PhantomData<CheckerState<'_>>,
        _o: std::marker::PhantomData<CheckerOptions>,
    ) {
    }
    let _ = _assert_checker_types_resolve;
    // Prove the module path itself is accessible (function-pointer size is stable).
    let _sz = std::mem::size_of::<std::marker::PhantomData<CheckerOptions>>();
    assert_eq!(_sz, 0, "PhantomData must be zero-sized");
}

// Pull in serde_json so the compiler links it (used in parser_pipeline_link_check).
extern crate serde_json;
