//! The source-facts reader: small crates written into a temp directory
//! (each rule on its own), and the design board's own numbers for real
//! crates on this machine as the oracle (skipped where the registry or the
//! board data is absent).

#![allow(clippy::expect_used, clippy::panic)]

use super::docs::{first_paragraph, first_sentence, items_cancellable};
use super::manifest::{self, Enabled};
use super::scan::{Literals, mask, scan, scan_cancellable, sloc};
use super::{Entry, Reading, SOURCE_FACTS_CAPACITY, Service, SourceAuthority, hint_identity};
use crate::model::pages::PackageRef;
use crate::host::registry::CompositionGeneration;
use facet::folio::state::{Build, Library, Unsafe};
use std::fs;
use std::path::{Path, PathBuf};

fn krate(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../.local/scratch")
        .join(format!("nudox-source-facts-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    for (path, body) in files {
        let at = dir.join(path);
        fs::create_dir_all(at.parent().expect("parent")).expect("dir");
        fs::write(at, body).expect("file");
    }
    dir
}

#[test]
fn off_thread_source_walks_stop_when_their_owner_cancels() {
    let dir = krate(
        "cancelled",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"cancelled\"\nversion = \"0.1.0\"\n",
            ),
            ("src/lib.rs", "pub mod nested;\n"),
            ("src/nested.rs", "//! nested docs\npub fn run() {}\n"),
        ],
    );
    assert!(
        scan_cancellable(&dir, "src/lib.rs", || true).is_none(),
        "the scanner must not return facts after cancellation"
    );
    assert!(
        items_cancellable(&dir.join("src"), &dir, || true).is_none(),
        "the module reader must not return facts after cancellation"
    );
}

#[test]
fn source_reader_bounds_flights_and_discards_obsolete_completions() {
    let mut service = Service::default();
    let mut first = None;
    for index in 0..SOURCE_FACTS_CAPACITY {
        let package =
            PackageRef::parse(&format!("pkg:cargo/pressure-{index}@1.0.0")).expect("exact package");
        let (flight, cancellation) = service
            .begin(package.clone(), None, None, Vec::new())
            .expect("within the flight bound");
        if index == 0 {
            first = Some((package, flight, cancellation));
        }
    }
    assert_eq!(service.entries.len(), SOURCE_FACTS_CAPACITY);

    let waiting = PackageRef::parse("pkg:cargo/pressure-overflow@1.0.0").expect("package");
    assert!(service
        .begin(waiting.clone(), None, None, Vec::new())
        .is_none());
    assert_eq!(service.entries.len(), SOURCE_FACTS_CAPACITY);
    let (obsolete, obsolete_flight, obsolete_cancel) = first.expect("first flight");
    assert!(obsolete_cancel.is_cancelled());

    // A canceled worker returns after a new request for that exact package
    // has already started. Its old flight ticket must not overwrite the new
    // in-flight entry.
    service.remove(&obsolete);
    let (replacement_flight, _) = service
        .begin(obsolete.clone(), None, None, Vec::new())
        .expect("a canceled slot is reusable");
    assert_ne!(replacement_flight, obsolete_flight);
    service.complete(
        &obsolete,
        obsolete_flight,
        Reading::Absent("obsolete result".into()),
    );
    assert!(matches!(
        service.entries.get(&obsolete).map(|slot| &slot.entry),
        Some(Entry::Reading(_))
    ));
    assert_eq!(service.entries.len(), SOURCE_FACTS_CAPACITY);
}

#[test]
fn source_facts_cache_keys_bind_exact_hints_and_registry_composition_generation() {
    let mut service = Service::default();
    let package = PackageRef::parse("pkg:cargo/present@0.4.2").expect("package");
    let mut hints = std::collections::HashMap::new();
    hints.insert("serde".to_owned(), "1.0.220".to_owned());
    hints.insert("syn".to_owned(), "2.0.99".to_owned());
    let exact_hints = hint_identity(&hints);
    let mut reordered_hints = std::collections::HashMap::new();
    reordered_hints.insert("syn".to_owned(), "2.0.99".to_owned());
    reordered_hints.insert("serde".to_owned(), "1.0.220".to_owned());
    assert_eq!(
        exact_hints,
        hint_identity(&reordered_hints),
        "input map order does not change resolver-hint identity"
    );
    let authority = Some(SourceAuthority {
        endpoint: PathBuf::from("/owner.sock"),
        key: "cargo-home|registry-a".into(),
        generation: CompositionGeneration(7),
    });
    let (flight, _) = service
        .begin(
            package.clone(),
            None,
            authority.clone(),
            exact_hints.clone(),
        )
        .expect("the source read starts");
    service.complete(
        &package,
        flight,
        Reading::Absent("fixture result".into()),
    );
    assert!(matches!(
        service.get(&package, &None, &authority, &exact_hints),
        Some(Reading::Absent(_))
    ));

    let mut changed_hints = hints.clone();
    changed_hints.insert("serde".to_owned(), "1.0.221".to_owned());
    assert!(
        service
            .get(&package, &None, &authority, &hint_identity(&changed_hints))
            .is_none(),
        "changed resolver choices cannot reuse the previous dependency facts"
    );

    let other_authority = Some(SourceAuthority {
        key: "cargo-home|registry-b".into(),
        ..authority.clone().expect("authority")
    });
    assert!(
        service
            .get(&package, &None, &other_authority, &exact_hints)
            .is_none(),
        "changed Cargo authority cannot reuse the previous source facts"
    );

    // A new RegistrySource instance may share the same endpoint and stable
    // Cargo path authority; installation gives it a new generation so the
    // old facts are not attributed to its provider.
    let replacement = Some(SourceAuthority {
        generation: CompositionGeneration(8),
        ..authority.clone().expect("authority")
    });
    assert!(
        service
            .get(&package, &None, &replacement, &exact_hints)
            .is_none(),
        "replacing a source provider invalidates source facts without pointer identity"
    );
}

#[test]
fn masking_blanks_comments_and_keeps_offsets() {
    let src = "let a = 1; // unsafe { }\n/* std::fs /* nested */ still */ let b = \"x // y\";\n";
    let masked = mask(src, Literals::Keep);
    assert_eq!(masked.len(), src.len());
    assert!(
        !masked.contains("unsafe") && !masked.contains("std::fs") && !masked.contains("nested")
    );
    assert!(masked.contains("\"x // y\""), "strings survive when kept");
    assert_eq!(masked.matches('\n').count(), 2);
    let both = mask(src, Literals::Blank);
    assert!(!both.contains("x // y"), "strings are blanked when asked");
    // A lifetime is not a char literal; a char literal is.
    let quote = mask(
        "fn f<'a>(x: &'a str) { let c = 'u'; let d = '\\n'; }",
        Literals::Blank,
    );
    assert!(
        quote.contains("<'a>")
            && quote.contains("&'a str")
            && !quote.contains("'u'")
            && !quote.contains("'\\n'"),
        "{quote}"
    );
}

#[test]
fn unsafe_is_counted_in_blocks_functions_impls_and_traits_only() {
    let dir = krate(
        "unsafe",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"u\"\nversion = \"0.1.0\"\n",
            ),
            (
                "src/lib.rs",
                "unsafe fn a() {}\npub unsafe extern \"C\" fn b() {}\nunsafe impl Send for X {}\nunsafe trait T {}\nfn c() { unsafe { a() } }\n// unsafe { in a comment }\nlet unsafe_thing = 1;\n",
            ),
        ],
    );
    let scanned = scan(&dir, "src/lib.rs");
    assert_eq!(
        scanned.unsafe_count, 5,
        "a, b, the impl, the trait and the block are five; `unsafe_thing` and the comment are not"
    );
    assert_eq!(scanned.unsafe_code, Unsafe::Allowed);
}

#[test]
fn a_crate_that_forbids_unsafe_says_so_and_a_deny_counts_too() {
    for (name, attr) in [
        ("forbid", "#![forbid(unsafe_code)]"),
        ("deny", "#![deny(missing_docs, unsafe_code)]"),
    ] {
        let dir = krate(
            name,
            &[
                (
                    "Cargo.toml",
                    "[package]\nname = \"f\"\nversion = \"0.1.0\"\n",
                ),
                ("src/lib.rs", &format!("{attr}\npub fn x() {{}}\n")),
            ],
        );
        assert_eq!(
            scan(&dir, "src/lib.rs").unsafe_code,
            Unsafe::Forbidden,
            "{attr}"
        );
    }
    let plain = krate(
        "allow",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"f\"\nversion = \"0.1.0\"\n",
            ),
            ("src/lib.rs", "#![allow(unsafe_code)]\n"),
        ],
    );
    assert_eq!(scan(&plain, "src/lib.rs").unsafe_code, Unsafe::Allowed);
}

#[test]
fn capabilities_name_the_line_and_the_file_and_skip_tests_and_examples() {
    let dir = krate(
        "caps",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"c\"\nversion = \"0.1.0\"\n",
            ),
            (
                "build.rs",
                "use std::process::Command;\nfn main() { Command::new(\"cc\").status().unwrap(); }\n",
            ),
            (
                "src/lib.rs",
                "use std::net::TcpStream;\nuse std::fs::File;\nfn e() { let _ = std::env::var(\"X\"); }\nextern \"C\" { fn puts(); }\n",
            ),
            ("src/net.rs", "fn n() { TcpStream::connect(a); }\n"),
            ("tests/t.rs", "use std::net::TcpStream;\n"),
            (
                "examples/e.rs",
                "use std::process::Command;\nfn main() { Command::new(\"x\"); }\n",
            ),
        ],
    );
    let scanned = scan(&dir, "src/lib.rs");
    assert_eq!(
        scanned.net.count, 2,
        "the import (its two patterns share a line, so it counts once) and the use in net.rs; tests are skipped"
    );
    assert_eq!(scanned.net.examples[0].file, "src/lib.rs");
    assert_eq!(scanned.net.examples[0].line, 1);
    assert_eq!(scanned.net.examples[0].text, "use std::net::TcpStream;");
    assert!(
        scanned.net.examples.iter().any(|e| e.file == "src/net.rs"),
        "one example per file first"
    );
    assert_eq!(scanned.fs.count, 1);
    assert_eq!(scanned.env.count, 1);
    assert_eq!(scanned.ffi.count, 1);
    assert_eq!(scanned.build, Build::Script);
    assert_eq!(
        scanned.process.count, 2,
        "build.rs names Command twice, on two lines; examples do not count"
    );
    assert_eq!(scanned.process.examples[0].file, "build.rs");
    assert_eq!(scanned.examples, 1);
    assert_eq!(
        scanned.suite,
        super::scan::Suite::Tested,
        "a tests directory"
    );
}

#[test]
fn a_bare_command_new_is_not_std_process_unless_it_was_imported() {
    let clap = krate(
        "clap",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"c\"\nversion = \"0.1.0\"\n",
            ),
            (
                "src/lib.rs",
                "fn cli() { let c = Command::new(\"app\"); }\n",
            ),
        ],
    );
    assert_eq!(scan(&clap, "src/lib.rs").process.count, 0);
    let real = krate(
        "std",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"c\"\nversion = \"0.1.0\"\n",
            ),
            (
                "src/lib.rs",
                "use std::process::{Command, Stdio};\nfn go() { Command::new(\"ls\"); }\n",
            ),
        ],
    );
    // The board's rule, which the oracle tests below hold this reader to: a
    // grouped import (`std::process::{Command, Stdio}`) is not itself a hit,
    // so only the `Command::new(` line counts (it is `std`'s, being imported).
    assert_eq!(scan(&real, "src/lib.rs").process.count, 1);
    let plain = krate(
        "plain",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"c\"\nversion = \"0.1.0\"\n",
            ),
            (
                "src/lib.rs",
                "use std::process::Command;\nfn go() { Command::new(\"ls\"); }\n",
            ),
        ],
    );
    assert_eq!(
        scan(&plain, "src/lib.rs").process.count,
        2,
        "the plain import and the call are two lines"
    );
}

#[test]
fn lines_of_code_skip_blanks_and_line_comments_and_stay_in_src() {
    let dir = krate(
        "sloc",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"s\"\nversion = \"0.1.0\"\n",
            ),
            (
                "src/lib.rs",
                "// a comment\n\nfn a() {}\n   // indented comment\nfn b() {}\n",
            ),
            ("src/deep/mod.rs", "fn c() {}\n"),
            ("benches/b.rs", "fn not_counted() {}\n"),
        ],
    );
    assert_eq!(scan(&dir, "src/lib.rs").sloc, 3);
    assert_eq!(sloc(&dir, "src/lib.rs"), 3);
}

#[test]
fn a_manifest_reads_its_authors_edition_and_build_script() {
    let dir = krate(
        "manifest",
        &[(
            "Cargo.toml",
            "[package]\nname = \"m\"\nversion = \"1.2.3\"\nedition = \"2021\"\nrust-version = \"1.71\"\nauthors = [\"Ada Lovelace <ada@x.org>\", \"Bob <bob@x.org\", \"plain@mail.com\"]\nrepository = \"https://github.com/x/m\"\ncategories = [\"parsing\"]\nbuild = \"build.rs\"\n[lib]\nproc-macro = true\n",
        )],
    );
    let m = manifest::read(&dir).expect("manifest");
    assert_eq!(
        m.authors,
        ["Ada Lovelace", "Bob"],
        "addresses never survive; a bare address is no name"
    );
    assert_eq!(m.edition.as_deref(), Some("2021"));
    assert_eq!(m.rust_version.as_deref(), Some("1.71"));
    assert_eq!(m.repository.as_deref(), Some("https://github.com/x/m"));
    assert!(m.library == Library::ProcMacro && m.build == Build::Script);
    assert_eq!(m.lib, "src/lib.rs");
    let none = krate(
        "edition",
        &[(
            "Cargo.toml",
            "[package]\nname = \"m\"\nversion = \"0.1.0\"\n",
        )],
    );
    assert_eq!(
        manifest::read(&none).expect("manifest").edition.as_deref(),
        Some("2015"),
        "Cargo's own default"
    );
}

#[test]
fn a_feature_graph_says_what_each_switch_turns_on() {
    let dir = krate(
        "features",
        &[(
            "Cargo.toml",
            "[package]\nname = \"f\"\nversion = \"0.1.0\"\n[features]\ndefault = [\"std\"]\nstd = [\"alloc\", \"dep:serde\"]\nalloc = []\nfull = [\"std\", \"net\", \"tokio/rt\", \"weak?/x\"]\nnet = [\"mio\"]\n[dependencies]\nserde = { version = \"1\", optional = true }\nmio = { version = \"0.8\", optional = true }\ntokio = { version = \"1\", optional = true }\nweak = { version = \"1\", optional = true }\nplain = \"1\"\n[dev-dependencies]\ncriterion = \"0.5\"\n",
        )],
    );
    let m = manifest::read(&dir).expect("manifest");
    let node = |name: &str| {
        m.graph
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, node)| node.clone())
            .unwrap_or_else(|| panic!("feature {name}"))
    };
    assert_eq!(node("std").enables, ["alloc"]);
    assert_eq!(node("std").deps, ["serde"]);
    assert!(
        node("std").enabled == Enabled::ByDefault && node("alloc").enabled == Enabled::ByDefault,
        "reachable from default through enables"
    );
    assert_eq!(node("full").enabled, Enabled::OnRequest);
    assert_eq!(node("full").enables, ["std", "net"]);
    assert_eq!(
        node("full").deps,
        ["tokio"],
        "`tokio/rt` turns the optional dep on; the weak `weak?/x` turns nothing on"
    );
    assert_eq!(node("net").deps, ["mio"]);
    assert_eq!(m.default, ["std"]);
    // `serde` is named by `dep:`, so it is no implicit feature; `mio` (named bare by `net`) is a feature-less dep too.
    assert!(
        m.features.contains(&"tokio".to_owned()) || m.graph.iter().any(|(n, _)| n == "tokio"),
        "tokio is never named by dep:, so it is an implicit feature"
    );
    assert!(
        !m.dependencies.iter().any(|d| d.key == "criterion"),
        "dev dependencies never count"
    );
    assert!(
        m.dependencies
            .iter()
            .any(|d| d.key == "plain" && d.need == manifest::Need::Required)
    );
}

#[test]
fn a_first_paragraph_skips_headings_badges_lists_and_code() {
    let readme = "# tokio\n\n[![crate](https://img/x.svg)](https://crates.io/x) [![docs](https://img/y.svg)](https://docs.rs/x)\n\n```rust\nfn ignored() {}\n```\n\nAn event-driven, **non-blocking** I/O platform for [writing](https://x) asynchronous applications. It has more.\n\n- a list\n";
    assert_eq!(
        first_paragraph(readme).as_deref(),
        Some(
            "An event-driven, non-blocking I/O platform for writing asynchronous applications. It has more."
        )
    );
    assert_eq!(
        first_sentence("An event-driven platform. It has more."),
        Some("An event-driven platform".to_owned())
    );
    assert_eq!(
        first_sentence("Use e.g. a channel. Then wait."),
        Some("Use e.g. a channel".to_owned())
    );
    assert_eq!(
        first_paragraph("# Examples\n\nnot this"),
        None,
        "a doc that opens with Examples has no summary"
    );
    assert_eq!(
        super::docs::clean_inline(
            "See ![logo](https://x/y.png) the [guide](https://x) and `code`."
        ),
        "See the guide and `code`.",
        "an image is gone, a link is its text"
    );
    assert!(
        super::docs::nav_row(
            "[![Latest Version](https://img/a.svg)](https://crates.io/a) [![Documentation](https://img/b.svg)](https://docs.rs/a)"
        ),
        "a row of nested badges has no words of its own"
    );
    assert!(
        !super::docs::nav_row("Read [the guide](https://x) before you start."),
        "a sentence that links is prose"
    );
}

#[test]
fn module_docs_come_from_inner_docs_then_the_mod_declaration() {
    let dir = krate(
        "docs",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"d\"\nversion = \"0.1.0\"\n",
            ),
            (
                "src/lib.rs",
                "//! The crate's own words. More of them.\n\n/// Channels between tasks.\npub mod mpsc;\npub mod undocumented;\n\npub fn root() {}\n",
            ),
            ("src/mpsc.rs", "pub struct Sender;\n"),
            (
                "src/undocumented.rs",
                "//! The module speaks for itself.\n\npub fn x() {}\n",
            ),
        ],
    );
    let facts = super::read(&dir, &std::collections::HashMap::new(), None).expect("facts");
    assert_eq!(
        facts.docs.get("lib").map(|d| d.sentence.as_str()),
        Some("The crate's own words")
    );
    assert_eq!(
        facts.docs.get("mpsc").map(|d| d.sentence.as_str()),
        Some("Channels between tasks"),
        "the `///` on `pub mod mpsc;`"
    );
    assert_eq!(
        facts.docs.get("undocumented").map(|d| d.sentence.as_str()),
        Some("The module speaks for itself")
    );
    let mpsc = facts.module("mpsc").expect("mpsc");
    assert_eq!(mpsc.items.len(), 1);
    assert_eq!(mpsc.items[0].signature, "pub struct Sender;");
}

#[test]
fn a_name_kept_in_a_private_module_is_public_where_it_is_re_exported() {
    let dir = krate(
        "reexports",
        &[
            (
                "Cargo.toml",
                "[package]\nname = \"r\"\nversion = \"0.1.0\"\n",
            ),
            (
                "src/lib.rs",
                "//! Re-exports.\nmod inner;\nmod wide;\npub mod open;\npub use inner::{Alpha, Beta as Gamma, helper};\npub use wide::*;\npub use open::Delta;\npub use crate::inner::Missing;\npub use serde::Serialize;\npub fn direct() {}\n",
            ),
            (
                "src/inner.rs",
                "/// The first.\npub struct Alpha;\npub struct Beta;\npub fn helper() {}\npub struct Unexported;\n",
            ),
            ("src/wide.rs", "pub struct One;\npub enum Two { A }\n"),
            ("src/open.rs", "pub struct Delta;\n"),
        ],
    );
    let modules = super::docs::items(&dir.join("src"), &dir);
    let names = |path: &str| -> Vec<String> {
        modules
            .iter()
            .find(|m| m.path == path)
            .map(|m| m.items.iter().map(|i| i.name.clone()).collect())
            .unwrap_or_default()
    };
    let lib = names("lib");
    for want in ["direct", "Alpha", "Gamma", "helper", "One", "Two"] {
        assert!(
            lib.iter().any(|n| n == want),
            "`{want}` is public at the crate root: {lib:?}"
        );
    }
    assert!(
        !lib.iter()
            .any(|n| n == "Beta" || n == "Unexported" || n == "Serialize" || n == "Missing"),
        "the renamed original, what nobody re-exports, other crates' names and names that do not exist are not: {lib:?}"
    );
    assert_eq!(
        lib.iter().filter(|n| *n == "Delta").count(),
        0,
        "a name defined in a public module is counted where it is defined, not twice"
    );
    assert!(names("open").iter().any(|n| n == "Delta"));
    let root = modules.iter().find(|m| m.path == "lib").expect("lib");
    assert_eq!(
        root.access,
        super::docs::Access::Public,
        "the crate root is reachable"
    );
    assert!(
        modules
            .iter()
            .find(|m| m.path == "inner")
            .is_some_and(|m| m.access == super::docs::Access::Private),
        "`mod inner;` is private"
    );
    let alpha = root
        .items
        .iter()
        .find(|i| i.name == "Alpha")
        .expect("Alpha");
    assert_eq!(
        alpha.from.as_deref(),
        Some("inner"),
        "it says where it is defined"
    );
    assert_eq!(
        alpha.doc.as_deref(),
        Some("The first"),
        "and keeps the author's words"
    );
}

// ---------------------------------------------------------------- the board as the oracle

fn board() -> Option<serde_json::Value> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../Nudox-Design-System/v6/moments/data/trust.json");
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn unpacked(name: &str, version: &str) -> Option<PathBuf> {
    super::registry::source_of(name, version)
}

#[test]
fn toml_reads_the_way_the_board_read_it() {
    let (Some(board), Some(dir)) = (board(), unpacked("toml", "0.8.23")) else {
        return;
    };
    let record = &board["toml@0.8.23"];
    let facts = super::read(&dir, &std::collections::HashMap::new(), None).expect("toml");
    assert_eq!(
        facts.scan.sloc,
        record["sloc"].as_u64().expect("sloc") as usize
    );
    assert_eq!(
        facts.scan.unsafe_count,
        record["unsafe"].as_u64().expect("unsafe") as usize
    );
    assert_eq!(
        facts.scan.unsafe_code == Unsafe::Forbidden,
        record["forbid_unsafe"].as_bool().expect("forbid")
    );
    for (name, cap) in [
        ("net", &facts.scan.net),
        ("fs", &facts.scan.fs),
        ("process", &facts.scan.process),
        ("env", &facts.scan.env),
        ("ffi", &facts.scan.ffi),
    ] {
        assert_eq!(
            cap.count,
            record["caps"][format!("{name}_n")].as_u64().expect("count") as usize,
            "{name}"
        );
    }
    assert_eq!(
        facts.manifest.edition.as_deref(),
        record["edition"].as_str()
    );
    assert_eq!(
        facts.manifest.rust_version.as_deref(),
        record["rust_version"].as_str()
    );
    assert_eq!(
        facts.manifest.repository.as_deref(),
        record["repository"].as_str()
    );
    assert_eq!(facts.manifest.default, ["parse", "display"]);
    let all: Vec<&str> = record["features"]["all"]
        .as_array()
        .expect("all")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(facts.manifest.features, all);
    for (name, node) in &facts.manifest.graph {
        let board_node = &record["feature_graph"][name];
        assert_eq!(
            node.enabled == Enabled::ByDefault,
            board_node["default"].as_bool().expect("default"),
            "{name} default"
        );
        let deps: Vec<&str> = board_node["deps"]
            .as_array()
            .expect("deps")
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(node.deps, deps, "{name} deps");
    }
}

#[test]
fn tokio_reads_the_way_the_board_read_it() {
    let (Some(board), Some(dir)) = (board(), unpacked("tokio", "1.53.1")) else {
        return;
    };
    let record = &board["tokio@1.53.1"];
    let scanned = scan(&dir, "src/lib.rs");
    assert_eq!(
        scanned.sloc,
        record["sloc"].as_u64().expect("sloc") as usize
    );
    assert_eq!(
        scanned.unsafe_count,
        record["unsafe"].as_u64().expect("unsafe") as usize
    );
    for (name, cap) in [
        ("net", &scanned.net),
        ("fs", &scanned.fs),
        ("process", &scanned.process),
        ("env", &scanned.env),
        ("ffi", &scanned.ffi),
    ] {
        assert_eq!(
            cap.count,
            record["caps"][format!("{name}_n")].as_u64().expect("count") as usize,
            "{name}"
        );
        let first = record["caps"][name]
            .as_array()
            .and_then(|a| a.first())
            .and_then(|e| e.as_array());
        if let (Some(first), Some(mine)) = (first, cap.examples.first()) {
            assert_eq!(
                mine.file,
                first[0].as_str().expect("file"),
                "{name} first example file"
            );
            assert_eq!(
                mine.line as u64,
                first[1].as_u64().expect("line"),
                "{name} first example line"
            );
        }
    }
}
