//! Adversarial tests for the address parser, written against the grammar in
//! `docs/MCP-SURFACE-PLAN.md` §4 rather than against the implementation.
//!
//! # What this file is hunting
//!
//! Not "does it parse a happy path" — `src/mcp/address.rs`'s own unit tests
//! cover that. This file hunts the three outcomes that are actually dangerous:
//!
//! 1. **Parses, but means something else.** An npm scope begins with `@`, and
//!    a Maven coordinate contains `:` — both collide with the grammar's own
//!    delimiters. A parser that splits on the *first* `@` or the *last* `:`
//!    yields a well-formed address naming the wrong package, which no error
//!    will ever reveal. This is the only class here that can produce a silent
//!    wrong answer, so it gets the most cases.
//! 2. **Panics or hangs.** Address text arrives from a model, so it is
//!    adversarial by default: unbalanced brackets, 50,000 segments, 10,000
//!    nested generics. A panic in the MCP layer takes down the tool call; an
//!    unbounded recursion takes down the process.
//! 3. **Renders what it cannot re-parse.** If `Display` and `parse` disagree,
//!    every address the server emits is a key the server itself will reject —
//!    the defect would surface as mass `not_found`s far from its cause.
//!
//! Everything here is pure parsing. No corpus, no producer, no `#[ignore]`.

use nudox_engine::mcp::address::{Address, AddressSegment, PackageCoord, Qualifier};

// ---------------------------------------------------------------------------
// 1. Shapes that must parse
// ---------------------------------------------------------------------------

/// Real address shapes from every ecosystem the corpus supports.
#[test]
fn real_addresses_from_every_ecosystem_parse() {
    for input in [
        // The legacy form. This MUST stay valid: it is what makes migration
        // free, and every existing key in every existing test is one.
        "cargo:serde#3f1a9c8d1e2b4a7f0c5583e9d21b6470ac8f1d3e5b90724681fca35d0e9b8c72",
        "cargo:serde@1.0.196::serde::de::Deserializer::deserialize_map[method]",
        "cargo:memchr@2.8.3::memchr::memchr::memchr::Memchr[struct]",
        "npm:@types/node@20.11.0::express::Request[interface]",
        "maven:com.google.code.gson:gson@2.11.0::com.google.gson.Gson.toJson[method]",
        "go:std@1.22.0::encoding/json::Encoder::Encode[method]",
        "pypi:requests@2.32.3::requests.sessions.Session.get[method]",
        "cargo:serde::serde::de::Deserializer[trait]",
        "cargo:serde",
        // An anonymous segment: every Rust impl block's name is its rendered
        // `impl Trait for Self`, so the bracket carries the whole segment.
        "cargo:serde_json@1.0.117::value::de::[impl Deserializer for Value]::deserialize_str[method]",
        // Nested generics inside a bracket qualifier.
        "maven:g:a@1::A.f[method(Map<String,List<Integer>>)]",
    ] {
        assert!(
            Address::parse(input).is_ok(),
            "must parse: {input}\n  got: {:?}",
            Address::parse(input).err()
        );
    }
}

/// All three separators are interchangeable on input, so these three spellings
/// of one path must produce identical segment lists.
#[test]
fn the_three_separators_normalize_to_one_segment_list() {
    let colons = Address::parse("cargo:serde@1.0.196::a::b::C").expect("colons");
    let dots = Address::parse("cargo:serde@1.0.196::a.b.C").expect("dots");
    let slashes = Address::parse("cargo:serde@1.0.196::a/b/C").expect("slashes");
    assert_eq!(colons.path, dots.path, "`::` and `.` must agree");
    assert_eq!(dots.path, slashes.path, "`.` and `/` must agree");
    assert_eq!(colons.path.len(), 3, "expected three segments");
}

// ---------------------------------------------------------------------------
// 2. The dangerous class: parses, but means something else
// ---------------------------------------------------------------------------

/// npm scoped packages begin with `@`. Splitting the coordinate on the FIRST
/// `@` would silently yield package `""` and version `types/node@20.11.0` — a
/// wrong address that still parses, which is worse than a rejection.
#[test]
fn an_npm_scope_does_not_get_eaten_by_the_version_separator() {
    let a = Address::parse("npm:@types/node@20.11.0::Foo").expect("scoped npm package");
    assert_eq!(a.package.ecosystem, "npm");
    assert_eq!(
        a.package.name, "@types/node",
        "the scope must survive intact"
    );
    assert_eq!(a.package.version.as_deref(), Some("20.11.0"));
}

/// A Maven coordinate is `group:artifact` — it contains the same `:` that
/// separates the ecosystem. Splitting on the LAST `:` would make the ecosystem
/// `maven:com.google.code.gson`.
#[test]
fn a_maven_coordinate_colon_does_not_get_eaten_by_the_ecosystem_separator() {
    let a = Address::parse("maven:com.google.code.gson:gson@2.11.0::Foo").expect("maven");
    assert_eq!(a.package.ecosystem, "maven");
    assert_eq!(a.package.name, "com.google.code.gson:gson");
    assert_eq!(a.package.version.as_deref(), Some("2.11.0"));
}

/// A Go module path contains `/`, which is also a path separator inside the
/// symbol half. The `::` boundary is what keeps them apart.
#[test]
fn a_go_module_path_is_not_split_into_symbol_segments() {
    let a = Address::parse("go:github.com/pkg/errors@v0.9.1::errors::New[fn]").expect("go");
    assert_eq!(a.package.name, "github.com/pkg/errors");
    assert_eq!(a.package.version.as_deref(), Some("v0.9.1"));
    assert_eq!(a.path.len(), 2, "only the symbol half contributes segments");
}

/// purl spells Go `golang` and C/C++ `clang`; the ecosystem tags that go into
/// the hash preimage are `go` and `cpp`. If input normalization were dropped,
/// the address would stop being the preimage and every Stage-1 hash lookup
/// would silently miss.
#[test]
fn purl_ecosystem_spellings_normalize_to_the_preimage_tags() {
    assert_eq!(
        Address::parse("golang:x@1::Y")
            .expect("golang")
            .package
            .ecosystem,
        "go"
    );
    assert_eq!(
        Address::parse("clang:x@1::Y")
            .expect("clang")
            .package
            .ecosystem,
        "cpp"
    );
}

/// Anything the renderer emits, the parser must accept, and the round trip must
/// be a fixed point. A renderer that emits text its own parser rejects turns
/// every emitted address into a key the server will refuse.
#[test]
fn render_and_parse_are_inverse() {
    for input in [
        "cargo:serde@1.0.196::serde::de::Deserializer[trait]",
        "npm:@types/node@20.11.0::express::Request[interface]",
        "maven:com.google.code.gson:gson@2.11.0::com.google.gson.Gson.toJson[method]",
        "go:github.com/pkg/errors@v0.9.1::errors::New[fn]",
        "cargo:serde#3f1a9c8d1e2b4a7f0c5583e9d21b6470ac8f1d3e5b90724681fca35d0e9b8c72",
    ] {
        let first = Address::parse(input).expect("parse");
        let rendered = first.to_string();
        let second = Address::parse(&rendered)
            .unwrap_or_else(|e| panic!("re-parse of rendered {rendered:?} failed: {e:?}"));
        assert_eq!(
            first, second,
            "round trip is not a fixed point: {input} -> {rendered}"
        );
    }
}

// ---------------------------------------------------------------------------
// 3. Malformed input: rejected cleanly, never panicking
// ---------------------------------------------------------------------------

#[test]
fn malformed_addresses_are_rejected_rather_than_panicking() {
    for input in [
        "",
        "::",
        "#",
        "cargo:",
        ":serde",
        "cargo:serde#",
        "cargo:serde#zzzznothex",
        // A truncated hash must never resolve to a *different* symbol, so a
        // short hex run is a rejection, not a prefix match (prefixes are a
        // later, sealer-verified stage).
        "cargo:serde#3f1a",
        "cargo:serde@1.0.196::a::[unclosed",
        "cargo:serde@1.0.196::a[method(]",
        "cargo:serde@1.0.196::a[method(Map<K,V)]",
        "cargo:serde@1.0.196::a::'unterminated",
        "cargo:serde@1.0.196::a]",
    ] {
        let outcome = Address::parse(input);
        assert!(
            outcome.is_err(),
            "must reject {input:?}, but parsed to {outcome:?}"
        );
    }
}

/// A segment name can itself contain the separator.
///
/// `impl_display_name` (`rust/ra/item.rs:1633`) renders an impl block's name
/// from `self_ty.display()`, and its own doc comment warns that a qualified
/// associated-type path such as `<T as IntoParallelIterator>::Item` survives
/// into that string. So a real Rust impl segment can contain `::` — the very
/// delimiter that separates segments for `cargo`. If the renderer emits such a
/// segment unquoted, re-parsing splits one segment into three and the address
/// silently names a different declaration.
///
/// This is the round-trip hazard with the worst blast radius, because it only
/// triggers on the impls that are already hardest to address.
#[test]
fn a_segment_containing_the_separator_survives_a_round_trip() {
    let hostile = [
        "cargo:rayon@1.9.0::iter::[impl Display for std::vec::Vec<T>]::fmt[method]",
        "cargo:rayon@1.9.0::iter::[impl Iterator for <T as IntoParallelIterator>::Item]::next[method]",
        "pypi:pkg@1::mod.'name.with.dots'.attr",
    ];
    for input in hostile {
        let Ok(first) = Address::parse(input) else {
            // Rejecting outright is a defensible answer; silently mis-splitting
            // is not. Only the round trip below is load-bearing.
            continue;
        };
        let rendered = first.to_string();
        let second = Address::parse(&rendered).unwrap_or_else(|e| {
            panic!(
                "rendered address does not re-parse: {input}\n  rendered: {rendered}\n  err: {e:?}"
            )
        });
        assert_eq!(
            first, second,
            "segment containing a separator broke the round trip:\n  input:    {input}\n  rendered: {rendered}"
        );
    }
}

/// A quoted segment whose text contains the quote character must round-trip.
///
/// This is not hypothetical: the renderer quotes impl segments, and a real
/// Rust impl name contains lifetimes, which are spelled with an apostrophe —
/// the same character used as the quote delimiter. Observed verbatim in a live
/// `search_symbols` response over serde 1.0.196:
///
/// ```text
/// cargo:serde::…::'impl Deserialize<'de> for Mutex<T>'::deserialize[function]
/// ```
///
/// A naive "find the next `'`" scan closes the quote at `<'de`, so the segment
/// becomes `impl Deserialize<` and the remainder is reinterpreted as path
/// structure. That is the silent-wrong-answer class: it parses, and it names
/// something else.
#[test]
fn a_quoted_segment_containing_an_apostrophe_round_trips() {
    // The exact impl name the real Rust producer mints for serde, apostrophes
    // and all. Built structurally rather than as text, because the point is
    // that *whatever* the renderer chooses to emit must parse back — not that
    // any particular spelling is accepted.
    let impl_name = "impl Deserialize<'de> for Mutex<T>";
    let original = Address {
        package: PackageCoord {
            ecosystem: "cargo".to_owned(),
            name: "serde".to_owned(),
            version: Some("1.0.196".to_owned()),
        },
        path: vec![
            AddressSegment {
                name: Some("de".to_owned()),
                qualifiers: Vec::new(),
            },
            AddressSegment {
                name: Some(impl_name.to_owned()),
                qualifiers: Vec::new(),
            },
            AddressSegment {
                name: Some("deserialize".to_owned()),
                qualifiers: vec![Qualifier::Kind("function".to_owned())],
            },
        ],
        key: None,
    };

    let rendered = original.to_string();
    let reparsed = Address::parse(&rendered).unwrap_or_else(|e| {
        panic!(
            "the renderer emitted an address its own parser rejects:\n  {rendered}\n  err: {e:?}"
        )
    });
    assert_eq!(
        original, reparsed,
        "lifetime apostrophes broke the round trip:\n  rendered: {rendered}"
    );
    assert_eq!(
        reparsed.path[1].effective_name(),
        impl_name,
        "the impl segment was truncated at the lifetime apostrophe"
    );

    // The ambiguous *unescaped* spelling must not silently parse into
    // something else. Rejecting it is correct; truncating it at `<'` and
    // reinterpreting the remainder as path structure is the failure mode.
    let ambiguous =
        "cargo:serde@1.0.196::de::'impl Deserialize<'de> for Mutex<T>'::deserialize[function]";
    if let Ok(bad) = Address::parse(ambiguous) {
        assert!(
            bad.path.iter().any(|s| s.effective_name() == impl_name),
            "unescaped form parsed but mangled the impl segment into {:?}",
            bad.path
                .iter()
                .map(AddressSegment::effective_name)
                .collect::<Vec<_>>()
        );
    }
}

/// The grammar says lowercase hex. Uppercase must not silently resolve to the
/// same key, because `IntroId::to_hex` emits lowercase and a case-insensitive
/// parser would make two spellings of one key that compare unequal as strings.
#[test]
fn hex_case_is_not_silently_accepted_both_ways() {
    let lower = "cargo:serde#3f1a9c8d1e2b4a7f0c5583e9d21b6470ac8f1d3e5b90724681fca35d0e9b8c72";
    let upper = "cargo:serde#3F1A9C8D1E2B4A7F0C5583E9D21B6470AC8F1D3E5B90724681FCA35D0E9B8C72";
    let lo = Address::parse(lower).expect("lowercase hex is the canonical form");
    match Address::parse(upper) {
        // Rejecting is fine.
        Err(_) => {}
        // Accepting is fine ONLY if it decodes to the same key — otherwise two
        // spellings of one identity disagree.
        Ok(up) => assert_eq!(
            lo.key, up.key,
            "uppercase hex parsed but decoded to a different key"
        ),
    }
}

/// Degenerate coordinate and path shapes that are easy to accept by accident.
#[test]
fn degenerate_shapes_do_not_parse_into_something_plausible() {
    for input in [
        "cargo:serde@",                // empty version
        "cargo:serde@1.0.196::a::::b", // empty interior segment
        "cargo:serde@1.0.196::a::",    // trailing separator
        "cargo:serde@1.0.196:: ",      // whitespace-only segment
        "cargo: serde@1.0.196::a",     // space in the coordinate
        "cargo:serde@1.0.196::a[]",    // empty qualifier block
    ] {
        if let Ok(parsed) = Address::parse(input) {
            // If it parses at all, no segment may be empty or blank — an empty
            // segment would hash into the preimage as `""` and match nothing,
            // presenting as "symbol does not exist".
            for seg in &parsed.path {
                assert!(
                    !seg.effective_name().trim().is_empty(),
                    "{input:?} parsed with an empty/blank segment: {parsed:?}"
                );
            }
            assert!(
                parsed.package.version.as_deref() != Some(""),
                "{input:?} parsed with an empty version"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 4. Resource exhaustion
// ---------------------------------------------------------------------------

/// Address text comes from a model, so it is adversarial by default. None of
/// these need to *parse* — they need to terminate without panicking or
/// exhausting the stack. An unbounded recursion here is a process kill, not a
/// failed tool call.
#[test]
fn pathological_inputs_terminate_without_panicking() {
    let cases = [
        format!("cargo:a@1::f[method({})]", "<".repeat(10_000)),
        format!(
            "cargo:a@1::f[method({}{})]",
            "Map<".repeat(2_000),
            ">".repeat(2_000)
        ),
        format!("cargo:a@1::{}", vec!["seg"; 50_000].join("::")),
        format!("cargo:a@1::{}", "::".repeat(50_000)),
        format!("cargo:a@1::{}", "[".repeat(10_000)),
        format!("cargo:{}@1::X", "n".repeat(100_000)),
    ];
    for input in &cases {
        // Deliberately ignoring the outcome: the assertion is that control
        // returns at all.
        let _ = Address::parse(input);
    }
}
