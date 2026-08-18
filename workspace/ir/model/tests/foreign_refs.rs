//! A cross-package reference must survive sealing as a `Ref::Foreign` that
//! names its target.
//!
//! # The defect these tests pin
//!
//! `Lowering::refer_import` returned `Ref::Local(import_index)` — an index into
//! a per-package import arena that `IrPackage::seal` **drops** — with a comment
//! saying it would be "resolved later". Nothing ever resolved it. `seal`
//! rewrote `Ref::Local` to `Ref::Intro` only for indices minted by
//! `create_export`, so every import ref fell through a bare `if let` with no
//! `else`, no assertion and no error channel, and arrived in the sealed table
//! as a dangling arena index. `Ref::Foreign` was constructed nowhere in
//! `workspace/ir/model/src` at all.
//!
//! Downstream, three separate files carried a comment asserting the case was
//! impossible and then silently dropped it: the renderer produced a bare `?`,
//! the `mentions` reverse index produced no posting, and the graph adapter
//! produced `null`.

use nudox_ir::{
    build::*,
    change::{EcosystemId, IntroId, PackageName, StableRef},
    entry::EntryInner,
    foreign::{ForeignKey, ForeignOrigin, ForeignResolver, Resolution, TableResolver, Unlinked},
    index::Ref,
    kind::Kind,
    kinds::ty::Type,
    lower::Lowering,
};

fn lineage() -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("cargo"), PackageName::new("memchr"))
}

fn core_lineage() -> PackageLineageId {
    PackageLineageId::new(EcosystemId::new("rust-sysroot"), PackageName::new("core"))
}

fn sym(name: &str) -> Symbol {
    Symbol {
        name: name.to_owned(),
        visibility: Visibility::Public,
        documentation: String::new(),
        source: std::path::PathBuf::new(),
        span: 0..0,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    }
}

/// Build `struct Memchr;` plus `impl Clone for Memchr`, `impl Debug for
/// Memchr`, `impl Iterator for Memchr` — the shape of the shipped screenshot,
/// with the traits living in another package.
fn memchr_like() -> Lowering<&'static str> {
    let mut low: Lowering<&'static str> = Lowering::new(PackageId::path("memchr"), sym("memchr"));
    let self_ref = low.refer::<Record>("Memchr");
    low.declare("Memchr", None, sym("Memchr"), Record::builder().build());

    for (id, trait_path, display) in [
        ("impl#clone", "core::clone::Clone", "Clone"),
        ("impl#debug", "core::fmt::Debug", "Debug"),
        ("impl#iterator", "core::iter::Iterator", "Iterator"),
    ] {
        let of = low.nominal_import(ForeignKey::in_package(core_lineage(), trait_path, display));
        low.declare(
            id,
            None,
            sym(&format!("impl {display} for Memchr")),
            Impl::builder()
                .of(of)
                .self_ty(Type::Nominal(self_ref.clone().into_raw()))
                .build(),
        );
    }
    low
}

/// Every trait in the Implementations list must arrive in the sealed table as a
/// `Ref::Foreign` carrying the trait's real name — not as a `Ref::Local` that
/// the renderer turns into `?`.
#[test]
fn foreign_trait_impls_seal_as_named_foreign_refs() {
    let table = memchr_like()
        .finish()
        .expect("no `refer`'d id is left undeclared — refer_import creates no slot")
        .seal(&lineage(), &Unlinked)
        .table;

    let mut traits: Vec<String> = Vec::new();
    for (_, entry) in table.iter() {
        let EntryInner::Owned(Kind::Impl(i)) = entry.kind() else {
            continue;
        };
        let Some(Type::Nominal(raw)) = i.of.as_ref() else {
            panic!("every impl in this fixture implements a trait");
        };
        match raw {
            Ref::Foreign { key, target } => {
                assert_eq!(
                    key.origin,
                    ForeignOrigin::Package(core_lineage()),
                    "the producer named the owning package exactly"
                );
                assert!(
                    target.is_none(),
                    "sealed against `Unlinked`, so nothing may claim to be linked"
                );
                traits.push(key.display.to_string());
            }
            other => panic!(
                "a cross-package trait must seal as Ref::Foreign, not {other:?} — \
                 a Ref::Local here is the dangling import index that renders as `?`"
            ),
        }
    }

    traits.sort();
    assert_eq!(
        traits,
        vec![
            "Clone".to_owned(),
            "Debug".to_owned(),
            "Iterator".to_owned()
        ],
        "each impl must name its own trait; identical labels mean the \
         reference lost its identity"
    );
}

/// `Ref::Foreign` is actually constructed on the shipped path, and a resolver
/// can turn a named reference into a linked one.
#[test]
fn a_resolver_links_a_named_reference_to_a_stable_ref() {
    let clone_intro = IntroId::from_raw([0x11; 32]);
    let mut resolver = TableResolver::new();
    resolver.link(
        ForeignKey::in_package(core_lineage(), "core::clone::Clone", "Clone"),
        StableRef::new(core_lineage(), clone_intro),
    );

    let table = memchr_like()
        .finish()
        .expect("lowering must succeed")
        .seal(&lineage(), &resolver)
        .table;

    let mut linked = 0usize;
    let mut named_only = 0usize;
    for (_, entry) in table.iter() {
        let EntryInner::Owned(Kind::Impl(i)) = entry.kind() else {
            continue;
        };
        let Some(Type::Nominal(Ref::Foreign { key, target })) = i.of.as_ref() else {
            continue;
        };
        match target {
            Some(sr) => {
                assert_eq!(key.display.as_ref(), "Clone");
                assert_eq!(
                    sr,
                    &StableRef::new(core_lineage(), clone_intro),
                    "the linked target must be exactly what the resolver returned"
                );
                linked += 1;
            }
            None => named_only += 1,
        }
    }
    assert_eq!(
        linked, 1,
        "exactly the trait the resolver knew about is linked"
    );
    assert_eq!(
        named_only, 2,
        "the two the resolver did not know stay named-but-unlinked, \
         never linked to a fabricated target"
    );
}

/// Linking a reference must not move any identity or content hash.
///
/// This is the invariant that keeps a package's `IntroId`s and its
/// `manifest::generation_stamp` properties *of the package* rather than of
/// whatever else happened to be loaded. Without it the change plane sees a
/// phantom generation every time the corpus warms up, in a direction that
/// depends on load order.
#[test]
fn linking_changes_no_identity_and_no_content_hash() {
    let unlinked = memchr_like()
        .finish()
        .expect("lowering must succeed")
        .seal(&lineage(), &Unlinked)
        .table;

    let clone_intro = IntroId::from_raw([0x11; 32]);
    let mut resolver = TableResolver::new();
    for (path, display) in [
        ("core::clone::Clone", "Clone"),
        ("core::fmt::Debug", "Debug"),
        ("core::iter::Iterator", "Iterator"),
    ] {
        resolver.link(
            ForeignKey::in_package(core_lineage(), path, display),
            StableRef::new(core_lineage(), clone_intro),
        );
    }
    let linked = memchr_like()
        .finish()
        .expect("lowering must succeed")
        .seal(&lineage(), &resolver)
        .table;

    let mut a: Vec<IntroId> = unlinked.iter().map(|(i, _)| i).collect();
    let mut b: Vec<IntroId> = linked.iter().map(|(i, _)| i).collect();
    a.sort();
    b.sort();
    assert_eq!(
        a, b,
        "an IntroId must not depend on whether a dependency was loaded"
    );

    for (intro, entry) in unlinked.iter() {
        let other = linked.get(intro).expect("same identity set");
        assert_eq!(
            nudox_ir::content::entry_content_hash(entry),
            nudox_ir::content::entry_content_hash(other),
            "content hash of `{}` moved when its foreign refs were linked; \
             generation_stamp folds this over every entry, so a package's \
             stamp would become a function of corpus load order",
            entry.sym().name
        );
    }
}

/// `Resolution` distinguishes the boring case from the interesting one.
///
/// "the dependency is not loaded" is normal in a local-first corpus. "the
/// package is loaded and the path names nothing in it" means the referring
/// producer's path grammar and the target's disagree — a producer bug, and the
/// only variant worth chasing. A flat `Option<StableRef>` cannot tell them
/// apart, which is why the report carries the reason.
#[test]
fn seal_reports_why_each_reference_went_unlinked() {
    let outcome = memchr_like()
        .finish()
        .expect("lowering must succeed")
        .seal(&lineage(), &Unlinked);

    assert_eq!(
        outcome.report.unlinked.len(),
        3,
        "three distinct foreign keys, each reported once"
    );
    for (key, resolution) in &outcome.report.unlinked {
        assert_eq!(
            *resolution,
            Resolution::PackageNotLoaded,
            "`{}` names a real package that simply is not loaded",
            key.path
        );
    }
    assert!(outcome.report.linked.is_empty());
    assert!(
        outcome.report.unmapped_local.is_empty(),
        "an arena-local ref surviving seal is always a bug; got {:?}",
        outcome.report.unmapped_local
    );
}

/// A producer that cannot name the owning package says so, and the resolver
/// refuses to guess.
///
/// Java cannot derive a Maven coordinate from `java.util.List`; Go cannot
/// derive a module path from an import path. Both emit `Namespace`. A wrong
/// link renders as a *working hyperlink to the wrong symbol*, which is strictly
/// worse than no link.
#[test]
fn an_unplaceable_origin_is_never_guessed_into_a_package() {
    let namespace = ForeignKey::in_namespace(
        EcosystemId::new("maven"),
        "java.util",
        "java.util.List",
        "List",
    );
    let universe = ForeignKey::in_universe(EcosystemId::new("go"), "error", "error");

    assert_eq!(Unlinked.resolve(&namespace), Resolution::Unplaceable);
    assert_eq!(Unlinked.resolve(&universe), Resolution::Unplaceable);
    assert_eq!(namespace.origin.lineage(), None);
    assert_eq!(universe.origin.lineage(), None);
}

/// The interning contract: N references to one target share one key.
#[test]
fn repeated_references_to_one_target_share_a_key() {
    let mut low: Lowering<&'static str> = Lowering::new(PackageId::path("p"), sym("p"));
    let a = low.nominal_import(ForeignKey::in_package(
        core_lineage(),
        "core::clone::Clone",
        "Clone",
    ));
    let b = low.nominal_import(ForeignKey::in_package(
        core_lineage(),
        "core::clone::Clone",
        "Clone",
    ));
    let (Type::Nominal(Ref::Foreign { key: ka, .. }), Type::Nominal(Ref::Foreign { key: kb, .. })) =
        (&a, &b)
    else {
        panic!("both must be foreign nominals");
    };
    assert!(
        triomphe::Arc::ptr_eq(ka, kb),
        "identical keys must be interned to one allocation"
    );
}
