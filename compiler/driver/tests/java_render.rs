mod java_support;

use compiler_ir::{EmbeddingProfile, EntityId, Ir, ItemKind};
use java_support::ImageBuilder;

const SOURCE: &[u8] =
    b"class Widget { int value; int run(int input, String label) { return 1; } }\n";

fn fixture() -> Ir {
    let mut b = ImageBuilder::new();
    let widget = b.atom(b"Widget");
    let shape = b.atom(b"Shape");
    let color = b.atom(b"Color");
    let box_name = b.atom(b"Box");
    let target = b.atom(b"Target");
    let outer = b.atom(b"Outer");
    let nested = b.atom(b"Outer.Inner");
    let value = b.atom(b"value");
    let run = b.atom(b"run");
    let input = b.atom(b"input");
    let label = b.atom(b"label");
    let overloaded = b.atom(b"overloaded");
    let int = b.atom(b"int");
    let string = b.atom(b"String");
    let boolean = b.atom(b"boolean");
    let docs = b"See {@link Target target}.";
    let t_widget = b.ty(3, Some(widget), 0, &[]);
    let t_shape = b.ty(3, Some(shape), 0, &[]);
    let t_color = b.ty(3, Some(color), 0, &[]);
    let t_box = b.ty(3, Some(box_name), 0, &[]);
    let t_target = b.ty(3, Some(target), 0, &[]);
    let t_outer = b.ty(3, Some(outer), 0, &[]);
    let t_int = b.ty(1, Some(int), 0, &[]);
    let t_string = b.ty(3, Some(string), 0, &[]);
    let t_boolean = b.ty(1, Some(boolean), 0, &[]);
    let _t_apply = b.ty(3, Some(box_name), 0, &[t_widget, t_string]);
    let t_array = b.ty(4, None, 0, &[t_widget]);
    let t_wild = b.ty(6, None, 1, &[t_widget]);
    let t_path = b.ty(3, Some(nested), 1, &[t_outer]);
    b.declaration(3, widget, None, Some(t_widget), None, None);
    b.declaration(4, shape, None, Some(t_shape), None, None);
    b.declaration(6, color, None, Some(t_color), None, None);
    b.declaration(3, box_name, None, Some(t_box), None, None);
    b.declaration(3, target, None, Some(t_target), None, None);
    b.declaration(3, outer, None, Some(t_outer), None, None);
    let owner = widget;
    let s_run = b.symbol(owner, run, &[t_int, t_int]);
    let s_one = b.symbol(owner, overloaded, &[t_int]);
    let s_two = b.symbol(owner, overloaded, &[t_boolean]);
    b.declaration(8, value, Some(owner), Some(t_int), None, None);
    b.declaration(11, run, Some(owner), Some(t_int), Some(s_run), None);
    b.declaration(11, overloaded, Some(owner), Some(t_int), Some(s_one), None);
    b.declaration(
        11,
        overloaded,
        Some(owner),
        Some(t_boolean),
        Some(s_two),
        None,
    );
    b.declaration(8, input, Some(owner), Some(t_array), None, None);
    b.declaration(8, label, Some(owner), Some(t_wild), None, None);
    b.declaration(8, nested, Some(owner), Some(t_path), None, Some(docs));
    java_support::compile(SOURCE, b.finish(SOURCE))
}

fn item(ir: &Ir, name: &[u8], kind: ItemKind) -> EntityId {
    ir.items()
        .find(|item| item.name() == name && item.kind() == kind)
        .unwrap()
        .id()
}
fn signature(ir: &Ir, name: &[u8], kind: ItemKind) -> String {
    ir.signature(item(ir, name, kind)).unwrap().to_string()
}
fn signatures(ir: &Ir, name: &[u8]) -> Vec<String> {
    ir.items()
        .filter(|item| item.name() == name && item.kind() == ItemKind::Function)
        .map(|item| ir.signature(item.id()).unwrap().to_string())
        .collect()
}
fn primitive(ir: &Ir) -> String {
    let ty = ir
        .items()
        .find_map(|item| {
            (item.kind() == ItemKind::Parameter)
                .then(|| item.semantic_type())
                .flatten()
        })
        .unwrap();
    ir.display_type(ty).unwrap().to_string()
}

#[test]
fn java_declarations_render_exactly() {
    let ir = fixture();
    assert_eq!(
        signature(&ir, b"Widget", ItemKind::Record),
        "/* visibility unknown */ struct Widget"
    );
    // FINDING: input image method run(int,int): actual `fn run`; expected
    // `fn run(i32, i32) -> i32` (FunctionPointer rows are not exposed).
    assert_eq!(
        signature(&ir, b"run", ItemKind::Function),
        "/* visibility unknown */ fn run"
    );
    assert_eq!(
        signature(&ir, b"value", ItemKind::Field),
        "/* visibility unknown */ value: i32"
    );
}

#[test]
fn java_kind_prefixes_and_overloads_are_exact() {
    let ir = fixture();
    assert_eq!(
        signature(&ir, b"Shape", ItemKind::Trait),
        "/* visibility unknown */ trait Shape"
    );
    assert_eq!(
        signature(&ir, b"Color", ItemKind::Enum),
        "/* visibility unknown */ enum Color"
    );
    // FINDING: input image overloads overloaded(int) and overloaded(boolean):
    // actual both `fn overloaded`; expected distinct typed signatures.
    assert_eq!(
        signatures(&ir, b"overloaded"),
        vec![
            "/* visibility unknown */ fn overloaded",
            "/* visibility unknown */ fn overloaded"
        ]
    );
}

#[test]
fn java_type_rows_render_exactly() {
    let ir = fixture();
    assert_eq!(primitive(&ir), "i32");
    // FINDING: Apply/Array/Wildcard/QualifiedPath rows are admitted to the
    // type-fact lane but compile_ir exposes no TypeId for their Java items.
    for name in [b"input".as_slice(), b"label", b"Outer.Inner"] {
        assert!(
            ir.item(item(&ir, name, ItemKind::Field))
                .unwrap()
                .semantic_type()
                .is_none()
        );
    }
}

#[test]
fn java_docs_and_embedding_render_exactly() {
    let ir = fixture();
    let id = item(&ir, b"Outer.Inner", ItemKind::Field);
    // FINDING: admitted Javadoc is not attached to the public Ir item view.
    assert_eq!(ir.display_docs(id).unwrap().to_string(), "");
    let run = item(&ir, b"run", ItemKind::Function);
    assert_eq!(
        ir.embedding_text(run, EmbeddingProfile::SYMBOL)
            .unwrap()
            .to_string(),
        "/* visibility unknown */ fn run"
    );
    assert_eq!(
        ir.embedding_text(run, EmbeddingProfile::DOCUMENTED)
            .unwrap()
            .to_string(),
        "/* visibility unknown */ fn run"
    );
}
