//! Proves real rust-analyzer declaration extraction over the demo fixture.
//! Covers identities, shapes, source spans, docs, and mutation.
//! Keeps the sysroot failure contract executable.

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use compiler_languages_rust::{
    DeclarationDetails, DeclarationKind, RustToolchain, StructForm, extract_declarations,
    extract_declarations_with_forced_panic,
};

fn fixture_root() -> PathBuf {
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "nudox-rust-fixture-{stamp}-{}",
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir_all(root.join("src")).expect("create fixture");
    let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/demo");
    for file in ["lib.rs", "util.rs"] {
        fs::copy(fixture.join(file), root.join("src").join(file)).expect("copy fixture source");
    }
    fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("write manifest");
    root
}

fn facts() -> (PathBuf, Box<[compiler_languages_rust::DeclarationFact]>) {
    let root = fixture_root();
    let rustc = std::env::var_os("RUSTC")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("rustc"));
    let toolchain = RustToolchain::discover(rustc).expect("host rustc has a sysroot");
    let facts = extract_declarations(&root, &toolchain).expect("fixture loads and lowers");
    (root, facts)
}

fn by_name<'a>(
    facts: &'a [compiler_languages_rust::DeclarationFact],
    name: &str,
    kind: DeclarationKind,
) -> &'a compiler_languages_rust::DeclarationFact {
    facts
        .iter()
        .find(|fact| fact.path() == name && fact.kind() == kind)
        .unwrap_or_else(|| panic!("missing {kind:?} {name}"))
}

#[test]
fn every_fixture_item_has_a_typed_identity_and_shape() {
    let (root, facts) = facts();
    let expected = [
        ("util", DeclarationKind::Module),
        ("Reexported", DeclarationKind::Reexport),
        ("Named", DeclarationKind::Reexport),
        ("util::documented", DeclarationKind::Function),
        ("util::Named", DeclarationKind::Struct),
        ("util::Tuple", DeclarationKind::Struct),
        ("util::UnitStruct", DeclarationKind::Struct),
        ("util::Unit", DeclarationKind::Enum),
        ("util::Choice", DeclarationKind::Enum),
        ("util::Raw", DeclarationKind::Union),
        ("util::Parent", DeclarationKind::Trait),
        ("util::LocalTrait", DeclarationKind::Trait),
        ("util::Demo", DeclarationKind::Enum),
        ("util::impl LocalTrait for Demo", DeclarationKind::Impl),
        ("util::impl Demo", DeclarationKind::Impl),
        ("util::Number", DeclarationKind::TypeAlias),
        ("util::ANSWER", DeclarationKind::Const),
        ("util::COUNTER", DeclarationKind::Static),
        ("util::asynchronous", DeclarationKind::Function),
        ("util::dangerous", DeclarationKind::Function),
        ("util::constant", DeclarationKind::Function),
        ("util::tuple_parameter", DeclarationKind::Function),
        ("util::bounded", DeclarationKind::Function),
        ("util::c_abi", DeclarationKind::Function),
        ("util::inner", DeclarationKind::Module),
        ("util::inner::f", DeclarationKind::Function),
        ("Demo::inherent", DeclarationKind::Function),
        ("util::Reexported", DeclarationKind::Struct),
    ];
    for (name, kind) in expected {
        let fact = by_name(&facts, name, kind);
        assert!(fact.source().starts_with(&root));
    }
    match &by_name(&facts, "util::Named", DeclarationKind::Struct).details() {
        DeclarationDetails::Struct(details) => {
            assert_eq!(details.form(), StructForm::Named);
            assert_eq!(details.fields().len(), 2);
        }
        other => panic!("wrong details {other:?}"),
    }
    match by_name(&facts, "util::Tuple", DeclarationKind::Struct).details() {
        DeclarationDetails::Struct(details) => assert_eq!(
            details
                .fields()
                .iter()
                .map(|field| field.ty())
                .collect::<Vec<_>>(),
            vec!["u8", "u16"]
        ),
        other => panic!("wrong details {other:?}"),
    }
    match by_name(&facts, "util::UnitStruct", DeclarationKind::Struct).details() {
        DeclarationDetails::Struct(details) => {
            assert_eq!(details.form(), StructForm::Unit);
            assert!(details.fields().is_empty());
        }
        other => panic!("wrong details {other:?}"),
    }
    match by_name(&facts, "util::tuple_parameter", DeclarationKind::Function).details() {
        DeclarationDetails::Function(details) => {
            assert_eq!(details.parameters()[0].ty(), "(u8, u16)")
        }
        other => panic!("wrong details {other:?}"),
    }
    match by_name(&facts, "util::bounded", DeclarationKind::Function).details() {
        DeclarationDetails::Function(details) => {
            assert_eq!(details.generics(), Some("<T: Into<Vec<u8>>>"))
        }
        other => panic!("wrong details {other:?}"),
    }
    match by_name(&facts, "util::c_abi", DeclarationKind::Function).details() {
        DeclarationDetails::Function(details) => assert_eq!(details.abi(), Some("C")),
        other => panic!("wrong details {other:?}"),
    }
    match by_name(&facts, "util::UnsafeTrait", DeclarationKind::Trait).details() {
        DeclarationDetails::Trait { unsafe_trait, .. } => assert!(*unsafe_trait),
        other => panic!("wrong details {other:?}"),
    }
    match by_name(&facts, "util::AutoTrait", DeclarationKind::Trait).details() {
        DeclarationDetails::Trait { auto_trait, .. } => assert!(*auto_trait),
        other => panic!("wrong details {other:?}"),
    }
    match &by_name(&facts, "util::Choice", DeclarationKind::Enum).details() {
        DeclarationDetails::Enum { variants } => assert_eq!(
            variants
                .iter()
                .find(|v| v.name() == "Second")
                .and_then(|v| v.discriminant()),
            Some("7")
        ),
        other => panic!("wrong details {other:?}"),
    }
    match &by_name(&facts, "util::COUNTER", DeclarationKind::Static).details() {
        DeclarationDetails::Static { mutable } => assert!(*mutable),
        other => panic!("wrong details {other:?}"),
    }
    match by_name(&facts, "util::ANSWER", DeclarationKind::Const).details() {
        DeclarationDetails::Const { ty, value } => {
            assert_eq!(ty.as_deref(), Some("u32"));
            assert_eq!(value.as_deref(), Some("42"));
        }
        other => panic!("wrong details {other:?}"),
    }
    match by_name(&facts, "util::LocalTrait", DeclarationKind::Trait).details() {
        DeclarationDetails::Trait { supers, .. } => assert_eq!(
            supers.iter().map(String::as_str).collect::<Vec<_>>(),
            ["Parent"]
        ),
        other => panic!("wrong details {other:?}"),
    }
    let inherent = facts
        .iter()
        .find(|fact| {
            fact.kind() == DeclarationKind::Impl
                && matches!(
                    fact.details(),
                    DeclarationDetails::Impl { inherent: true, .. }
                )
        })
        .expect("inherent impl");
    assert_eq!(inherent.path(), "util::impl Demo");
    fs::remove_dir_all(root).expect("remove fixture");
}

#[test]
fn docs_spans_and_utf8_identity_are_source_accurate() {
    let (root, facts) = facts();
    let fact = by_name(&facts, "util::documented", DeclarationKind::Function);
    assert_eq!(
        fact.documentation(),
        Some("Adds a [`Reexported`] value while preserving `Value`.")
    );
    assert_eq!(fact.doc_links()[0].target(), "Reexported");
    let source = fs::read_to_string(fact.source()).expect("read source");
    assert_eq!(
        &source[fact.span()],
        "/// Adds a [`Reexported`] value while preserving `Value`.\npub fn documented<Value>(value: Value, величина: usize) -> Value\nwhere\n    Value: Clone,\n{\n    if величина == 0 {\n        return value;\n    }\n    let record = Reexported { value: 1 };\n    if record.value == 0 {\n        value\n    } else {\n        helper(value)\n    }\n}"
    );
    let parameter = match fact.details() {
        DeclarationDetails::Function(details) => details
            .parameters()
            .iter()
            .find(|p| p.name().contains('в'))
            .expect("unicode parameter"),
        _ => panic!("not function"),
    };
    assert_eq!(
        &source[source.find('в').expect("unicode name")
            ..source.find('в').expect("unicode name") + 'в'.len_utf8()],
        "в"
    );
    assert_eq!(parameter.name(), "величина");
    fs::remove_dir_all(root).expect("remove fixture");
}

#[test]
fn mutation_changes_the_extracted_field_identity() {
    let root = fixture_root();
    let util = root.join("src/util.rs");
    let mut source = fs::read_to_string(&util).expect("read util");
    source = source.replace("value: usize,", "replacement: usize,");
    fs::write(&util, source).expect("mutate util");
    let rustc = std::env::var_os("RUSTC")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("rustc"));
    let toolchain = RustToolchain::discover(rustc).expect("host rustc has a sysroot");
    let facts = extract_declarations(&root, &toolchain).expect("mutated fixture loads");
    let field = facts
        .iter()
        .find(|fact| fact.path() == "util::Reexported")
        .expect("declaration remains");
    match field.details() {
        DeclarationDetails::Struct(details) => {
            assert_eq!(details.fields()[0].name(), Some("replacement"))
        }
        other => panic!("wrong details {other:?}"),
    }
    fs::remove_dir_all(root).expect("remove fixture");
}

#[test]
fn sysroot_failure_remains_typed() {
    let result = RustToolchain::discover("/definitely/not-a-rustc");
    assert!(matches!(
        result,
        Err(compiler_languages_rust::LoadError::SysrootUnavailable { .. })
    ));
}

#[test]
fn one_item_panic_becomes_a_fact_without_losing_siblings() {
    let root = fixture_root();
    let rustc = std::env::var_os("RUSTC")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("rustc"));
    let toolchain = RustToolchain::discover(rustc).expect("host rustc has a sysroot");
    let facts = extract_declarations_with_forced_panic(&root, &toolchain, "documented")
        .expect("fault injection remains isolated");
    let panic = by_name(&facts, "util::documented", DeclarationKind::PerItemPanic);
    assert!(matches!(
        panic.details(),
        DeclarationDetails::PerItemPanic {
            original_kind: DeclarationKind::Function,
            message,
        } if message == "forced item lowering panic"
    ));
    assert_eq!(
        by_name(&facts, "util::helper", DeclarationKind::Function).kind(),
        DeclarationKind::Function
    );
    fs::remove_dir_all(root).expect("remove fixture");
}

#[test]
fn nested_file_modules_follow_the_module_chain() {
    let root = fixture_root();
    let module_dir = root.join("src/util");
    fs::create_dir(&module_dir).expect("create util module directory");
    fs::write(
        module_dir.join("leaf.rs"),
        "pub mod deep { pub fn f() {} }\n",
    )
    .expect("write leaf module");
    fs::write(
        root.join("src/util.rs"),
        "pub mod leaf;\npub struct Named;\npub struct Reexported;\n",
    )
    .expect("write nested module declaration");
    let rustc = std::env::var_os("RUSTC")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("rustc"));
    let toolchain = RustToolchain::discover(rustc).expect("host rustc has a sysroot");
    let facts = extract_declarations(&root, &toolchain).expect("nested fixture loads");
    by_name(&facts, "util::leaf", DeclarationKind::Module);
    by_name(&facts, "util::leaf::deep", DeclarationKind::Module);
    by_name(&facts, "util::leaf::deep::f", DeclarationKind::Function);
    fs::remove_dir_all(root).expect("remove fixture");
}
