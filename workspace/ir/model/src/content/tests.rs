use std::path::PathBuf;

use super::entry_content_hash;
use crate::{
    entry::{AttrTok, CfgExpr, Deprecation, DocLink, Entry, Node, Symbol, Visibility},
    index::{RawRef, Ref},
    kind::EntryKind,
    kinds::{
        Field, FieldKey, FnModifier, Function, GenericParam, Module, Trait, function::Receiver,
        generics::WherePred, ty::Type,
    },
    test_helpers::sym,
};

// -------------------------------------------------------------------------
// Helper: build a minimal post-seal entry (Owned(Module)) with no Node.
// -------------------------------------------------------------------------

fn module_entry_with_sym(s: Symbol) -> Entry {
    Entry::new(s, Node::build(None::<RawRef>, []), Module.into_kind())
}

// -------------------------------------------------------------------------
// 1. Determinism
// -------------------------------------------------------------------------

/// The same entry hashes identically when called twice.
#[test]
fn deterministic() {
    let e = module_entry_with_sym(sym("foo"));
    let h1 = entry_content_hash(&e);
    let h2 = entry_content_hash(&e);
    assert_eq!(h1, h2, "content hash must be deterministic");
}

// -------------------------------------------------------------------------
// 2. Sensitivity — one axis per assertion
// -------------------------------------------------------------------------

fn base_sym() -> Symbol {
    Symbol {
        name: "my_fn".to_owned(),
        visibility: Visibility::Public,
        documentation: "Does a thing.".to_owned(),
        source: PathBuf::from("src/lib.rs"),
        span: 0..10,
        aliases: Box::new([]),
        deprecation: None,
        doc_links: Box::new([]),
        attrs: Box::new([]),
        cfg: None,
    }
}

fn base_entry() -> Entry {
    module_entry_with_sym(base_sym())
}

#[test]
fn sensitive_to_name() {
    let a = base_entry();
    let mut s2 = base_sym();
    s2.name = "other_fn".to_owned();
    let b = module_entry_with_sym(s2);
    assert_ne!(
        entry_content_hash(&a),
        entry_content_hash(&b),
        "changing the name must change the hash"
    );
}

#[test]
fn sensitive_to_documentation() {
    let a = base_entry();
    let mut s2 = base_sym();
    s2.documentation = "Does something else.".to_owned();
    let b = module_entry_with_sym(s2);
    assert_ne!(
        entry_content_hash(&a),
        entry_content_hash(&b),
        "changing documentation must change the hash"
    );
}

#[test]
fn sensitive_to_visibility() {
    let a = base_entry();
    let mut s2 = base_sym();
    s2.visibility = Visibility::Private;
    let b = module_entry_with_sym(s2);
    assert_ne!(
        entry_content_hash(&a),
        entry_content_hash(&b),
        "changing visibility must change the hash"
    );
}

#[test]
fn sensitive_to_attrs() {
    let a = base_entry();
    let mut s2 = base_sym();
    s2.attrs = Box::new([AttrTok {
        token: "must_use".to_owned(),
        arg: None,
    }]);
    let b = module_entry_with_sym(s2);
    assert_ne!(
        entry_content_hash(&a),
        entry_content_hash(&b),
        "adding an attr must change the hash"
    );
}

#[test]
fn sensitive_to_cfg_predicate() {
    let a = base_entry();
    let mut s2 = base_sym();
    s2.cfg = Some(CfgExpr::Feature("serde".to_owned()));
    let b = module_entry_with_sym(s2);
    assert_ne!(
        entry_content_hash(&a),
        entry_content_hash(&b),
        "adding a cfg predicate must change the hash"
    );
}

/// Changing a field type changes the hash.
#[test]
fn sensitive_to_field_type() {
    let intro_a = crate::change::IntroId::from_raw([0xaa; 32]);
    let intro_b = crate::change::IntroId::from_raw([0xbb; 32]);

    let make_field_entry = |intro: crate::change::IntroId| {
        let f = Field::builder()
            .key(FieldKey::Named)
            .ty(Type::Nominal(Ref::Intro(intro)))
            .build();
        Entry::new(base_sym(), Node::build(None::<RawRef>, []), f.into_kind())
    };

    let ea = make_field_entry(intro_a);
    let eb = make_field_entry(intro_b);
    assert_ne!(
        entry_content_hash(&ea),
        entry_content_hash(&eb),
        "different field types must yield different hashes"
    );
}

/// Changing a generic bound changes the hash.
#[test]
fn sensitive_to_generic_bound() {
    let intro_a = crate::change::IntroId::from_raw([0xaa; 32]);
    let intro_b = crate::change::IntroId::from_raw([0xbb; 32]);

    let make_trait_entry = |bound_intro: crate::change::IntroId| {
        let t = Trait::builder()
            .supers([Type::Nominal(Ref::Intro(bound_intro))])
            .build();
        Entry::new(base_sym(), Node::build(None::<RawRef>, []), t.into_kind())
    };

    let ea = make_trait_entry(intro_a);
    let eb = make_trait_entry(intro_b);
    assert_ne!(
        entry_content_hash(&ea),
        entry_content_hash(&eb),
        "different generic bounds must yield different hashes"
    );
}

// -------------------------------------------------------------------------
// 3. Tree-independence — the central design decision
// -------------------------------------------------------------------------

/// Two entries with identical Symbol + kind but different parents hash the
/// SAME. This pins the design decision to exclude Node from the hash.
///
/// Without this property, adding one child would cascade a hash change up
/// through every ancestor — destroying the "is this declaration unchanged?"
/// fast path that this hash exists to provide.
#[test]
fn tree_independent() {
    use crate::index::UntypedEntryIndex;

    // Fabricate two different "parent" indices (export indices).
    let parent_a = UntypedEntryIndex::export(1);
    let parent_b = UntypedEntryIndex::export(2);

    // Build two entries with identical Symbol+kind but different parents.
    // Symbol does not derive Clone, so we construct two equal ones.
    let entry_a = Entry::new(
        base_sym(),
        Node::build(Ref::Local(parent_a), []),
        Module.into_kind(),
    );
    let entry_b = Entry::new(
        base_sym(),
        Node::build(Ref::Local(parent_b), []),
        Module.into_kind(),
    );

    assert_eq!(
        entry_content_hash(&entry_a),
        entry_content_hash(&entry_b),
        "entries with the same Symbol+kind but different parents must \
         hash identically — Node edges are relational structure, not content"
    );
}

// -------------------------------------------------------------------------
// 4. Golden pin
// -------------------------------------------------------------------------

/// A fully-populated entry, pinned byte-for-byte.
///
/// If this fails, the content-hash preimage layout changed. That is either
/// intentional (bump `ENTRY_CONTENT_DOMAIN`, then paste the hash from the
/// failure message here) or an accident (investigate before merging).
#[test]
fn golden_content_hash() {
    const EXPECTED: &str = "1d22e68ccc87cb32ca22ec5702ac36fe5c372977723a963dd83eaf8c187a299d";

    let sym = Symbol {
        name: "do_work".to_owned(),
        visibility: Visibility::Public,
        documentation: "Does the work.".to_owned(),
        source: PathBuf::from("src/lib.rs"),
        span: 4..42,
        aliases: Box::new(["do_it".to_owned()]),
        deprecation: Some(Deprecation {
            note: Some("use `new_do_work` instead".to_owned()),
            since: Some("2.0.0".to_owned()),
        }),
        doc_links: Box::new([DocLink {
            target: "crate::new_do_work".to_owned(),
            label: Some("new_do_work".to_owned()),
        }]),
        attrs: Box::new([AttrTok {
            token: "must_use".to_owned(),
            arg: None,
        }]),
        cfg: Some(CfgExpr::All(Box::new([
            CfgExpr::Feature("std".to_owned()),
            CfgExpr::Not(Box::new(CfgExpr::TargetOs("windows".to_owned()))),
        ]))),
    };

    let intro_target = crate::change::IntroId::from_raw([0x42; 32]);

    let kind = Function::builder()
        .receiver(Receiver::SharedRef)
        .modifiers([FnModifier::Async])
        .generics([GenericParam::Type {
            name: "T".to_owned(),
            bounds: Box::new([Type::Nominal(Ref::Intro(intro_target))]),
            default: None,
            variance: None,
        }])
        .wheres([WherePred {
            target: Type::SelfType,
            bounds: Box::new([Type::Any]),
        }])
        .abi("Rust".to_owned())
        .is_defaulted(false)
        .build();

    let e = Entry::new(sym, Node::build(None::<RawRef>, []), kind.into_kind());

    let h = entry_content_hash(&e);
    let hex = h.to_hex();

    // Deliberately no sentinel escape hatch: a pin that silently passes
    // while unset is not a guard, it is a comment.
    assert_eq!(
        hex, EXPECTED,
        "content hash golden pin changed — update EXPECTED only after \
         a deliberate format-version bump"
    );
}
