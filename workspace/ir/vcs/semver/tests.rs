use super::*;
use crate::wire::PayloadTable;
use crate::wire::{
    EntryPayloadFlags, FnSigFlags, FunctionWire, KindWire, ModuleWire, OwnedEntryPayload,
    SymbolWire,
};
use ir::change::IntroId;
use ir::entry::Visibility;
use ir::kind::KindDiscriminant;

fn make_sym(name: &str, vis: Visibility, doc: Option<&str>) -> SymbolWire {
    SymbolWire {
        name: name.into(),
        visibility: vis,
        documentation: doc.map(|s| s.to_string()),
        source_path: "src/lib.rs".into(),
        span_start: 0,
        span_end: 10,
        aliases: Vec::new(),
        deprecation: None,
        doc_links: Vec::new(),
        attrs: Vec::new(),
        cfg: None,
    }
}

fn fn_payload(name: &str) -> OwnedEntryPayload {
    let sym = make_sym(name, Visibility::Public, None);
    OwnedEntryPayload::sealed(
        sym,
        KindDiscriminant::Function,
        KindWire::Function(FunctionWire {
            input_params: Box::new([]),
            output_params: Box::new([]),
            sig: FnSigFlags::default(),
            generics: Box::new([]),
            wheres: Box::new([]),
        }),
        EntryPayloadFlags::default(),
    )
}

fn fn_payload_with_doc(name: &str, doc: &str) -> OwnedEntryPayload {
    let sym = make_sym(name, Visibility::Public, Some(doc));
    OwnedEntryPayload::sealed(
        sym,
        KindDiscriminant::Function,
        KindWire::Function(FunctionWire {
            input_params: Box::new([]),
            output_params: Box::new([]),
            sig: FnSigFlags::default(),
            generics: Box::new([]),
            wheres: Box::new([]),
        }),
        EntryPayloadFlags::default(),
    )
}

fn intro(n: u8) -> IntroId {
    IntroId::from_raw([n; 32])
}

/// V-1 acceptance: doc-only edit moves embed_hash + payload_hash but NOT api_surface_hash.
#[test]
fn v1_doc_only_edit_gates() {
    let id = intro(1);
    let p_without_doc = fn_payload("foo");
    let p_with_doc = fn_payload_with_doc("foo", "This function does something.");

    let path = MonikerPath::new(vec![SmolStr::new("foo")]);

    let ash_before = compute_api_surface_hash(&p_without_doc, id);
    let ash_after = compute_api_surface_hash(&p_with_doc, id);
    assert_eq!(
        ash_before, ash_after,
        "api_surface_hash must not change on doc-only edit"
    );

    let eh_before = compute_embed_hash(&p_without_doc, &path, std::slice::from_ref(&path));
    let eh_after = compute_embed_hash(&p_with_doc, &path, std::slice::from_ref(&path));
    assert_ne!(
        eh_before, eh_after,
        "embed_hash must change on doc-only edit"
    );

    assert_ne!(
        p_without_doc.payload_hash, p_with_doc.payload_hash,
        "payload_hash must change on doc-only edit"
    );
}

#[test]
fn surface_exports_pub_items() {
    let mut table = PayloadTable::new();
    let id = intro(1);
    table.insert_live(id, fn_payload("foo"), None);

    let policy = ExportPolicy::default();
    let surf = surface(&table, &policy);

    assert!(surf.items.contains_key(&id));
    let path = MonikerPath::new(vec![SmolStr::new("foo")]);
    assert_eq!(surf.monikers.get(&path), Some(&id));
}

#[test]
fn surface_excludes_private_items() {
    let mut table = PayloadTable::new();
    let id = intro(2);
    let sym = make_sym("bar", Visibility::Private, None);
    let payload = OwnedEntryPayload::sealed(
        sym,
        KindDiscriminant::Function,
        KindWire::Function(FunctionWire {
            input_params: Box::new([]),
            output_params: Box::new([]),
            sig: FnSigFlags::default(),
            generics: Box::new([]),
            wheres: Box::new([]),
        }),
        EntryPayloadFlags::default(),
    );
    table.insert_live(id, payload, None);

    let surf = surface(&table, &ExportPolicy::default());
    assert!(
        !surf.items.contains_key(&id),
        "private item should not be exported"
    );
}

#[test]
fn surface_excludes_doc_hidden_by_default() {
    let mut table = PayloadTable::new();
    let id = intro(3);
    let mut sym = make_sym("hidden_fn", Visibility::Public, None);
    sym.attrs.push(AttrTok {
        token: "doc_hidden".into(),
        arg: None,
    });
    let payload = OwnedEntryPayload::sealed(
        sym,
        KindDiscriminant::Function,
        KindWire::Function(FunctionWire {
            input_params: Box::new([]),
            output_params: Box::new([]),
            sig: FnSigFlags::default(),
            generics: Box::new([]),
            wheres: Box::new([]),
        }),
        EntryPayloadFlags::default(),
    );
    table.insert_live(id, payload, None);

    let surf = surface(&table, &ExportPolicy::default());
    assert!(
        !surf.items.contains_key(&id),
        "doc_hidden item should not be exported by default"
    );

    let permissive = ExportPolicy {
        include_doc_hidden: true,
        ..Default::default()
    };
    let surf2 = surface(&table, &permissive);
    assert!(
        surf2.items.contains_key(&id),
        "doc_hidden item should be exported with include_doc_hidden=true"
    );
}

#[test]
fn surface_child_items_exported_under_parent() {
    let mut table = PayloadTable::new();
    let mod_id = intro(10);
    let fn_id = intro(11);

    let mod_sym = make_sym("mymod", Visibility::Public, None);
    let mod_payload = OwnedEntryPayload::sealed(
        mod_sym,
        KindDiscriminant::Module,
        KindWire::Module(ModuleWire {}),
        EntryPayloadFlags::default(),
    );
    table.insert_live(mod_id, mod_payload, None);
    table.insert_live(fn_id, fn_payload("inner_fn"), Some(mod_id));

    let surf = surface(&table, &ExportPolicy::default());
    assert!(surf.items.contains_key(&fn_id));
    let path = MonikerPath::new(vec![SmolStr::new("mymod"), SmolStr::new("inner_fn")]);
    assert_eq!(surf.monikers.get(&path), Some(&fn_id));
}
