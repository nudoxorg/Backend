mod java_support;

use backend_semantic::ir::semantic_render::EmbeddingProfile;
use backend_semantic::ir::{EntityId, Ir, ItemKind};
use java_support::ImageBuilder;
use std::{error::Error, io};

const SOURCE: &[u8] =
    b"class Widget { int value; int run(int input, String label) { return 1; } }\n";

fn fixture() -> Result<Ir, Box<dyn Error>> {
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
    Ok(java_support::compile(SOURCE, b.finish(SOURCE))?)
}

fn missing(name: &[u8], kind: ItemKind) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotFound,
        format!("missing Java fixture item {:?} ({kind:?})", String::from_utf8_lossy(name)),
    )
}

fn item(ir: &Ir, name: &[u8], kind: ItemKind) -> Result<EntityId, io::Error> {
    ir.items()
        .find(|item| item.name() == name && item.kind() == kind)
        .map(|item| item.id())
        .ok_or_else(|| missing(name, kind))
}
fn signature(ir: &Ir, name: &[u8], kind: ItemKind) -> Result<String, io::Error> {
    ir.signature(item(ir, name, kind)?)
        .map(ToString::to_string)
        .ok_or_else(|| io::Error::other(format!("missing Java fixture signature for {:?}", name)))
}
fn signatures(ir: &Ir, name: &[u8]) -> Result<Vec<String>, io::Error> {
    ir.items()
        .filter(|item| item.name() == name && item.kind() == ItemKind::Function)
        .map(|item| {
            ir.signature(item.id())
                .map(ToString::to_string)
                .ok_or_else(|| {
                    io::Error::other(format!("missing Java fixture signature for {:?}", name))
                })
        })
        .collect()
}
fn primitive(ir: &Ir) -> Result<String, io::Error> {
    let ty = ir
        .items()
        .find_map(|item| {
            (item.kind() == ItemKind::Parameter)
                .then(|| item.semantic_type())
                .flatten()
        })
        .ok_or_else(|| io::Error::other("Java fixture has no parameter semantic type"))?;
    ir.display_type(ty)
        .map(ToString::to_string)
        .ok_or_else(|| io::Error::other("Java fixture parameter type has no display form"))
}
fn field_type(ir: &Ir, name: &[u8]) -> Result<String, io::Error> {
    let ty = ir
        .item(item(ir, name, ItemKind::Field)?)
        .ok_or_else(|| missing(name, ItemKind::Field))?
        .semantic_type()
        .ok_or_else(|| io::Error::other(format!("Java fixture field {:?} has no type", name)))?;
    ir.display_type(ty)
        .map(ToString::to_string)
        .ok_or_else(|| io::Error::other(format!("Java fixture field {:?} has no display type", name)))
}

#[test]
fn java_declarations_render_exactly() -> Result<(), Box<dyn Error>> {
    let ir = fixture()?;
    // Trunk 4d1cceba9: unknown visibility renders prefix-free, and a record
    // ignores its self-nominal row.
    assert_eq!(signature(&ir, b"Widget", ItemKind::Record)?, "struct Widget");
    // Trunk 4d1cceba9 exposes the symbol's FunctionPointer row as a live
    // function tail; the fixture's carriers keep their javac type-spelling
    // names (`int`, `int`), typed i32, with the declared result i32.
    assert_eq!(
        signature(&ir, b"run", ItemKind::Function)?,
        "fn run(int: i32, int: i32) -> i32"
    );
    // `value` declares the primitive `int` row, which lowers to i32.
    assert_eq!(signature(&ir, b"value", ItemKind::Field)?, "value: i32");
    Ok(())
}

#[test]
fn java_kind_prefixes_and_overloads_are_exact() -> Result<(), Box<dyn Error>> {
    let ir = fixture()?;
    // Trunk 4d1cceba9: unknown visibility renders prefix-free for every kind.
    assert_eq!(signature(&ir, b"Shape", ItemKind::Trait)?, "trait Shape");
    assert_eq!(signature(&ir, b"Color", ItemKind::Enum)?, "enum Color");
    // Trunk 4d1cceba9 renders each overload's own FunctionPointer row: the
    // fixtures declare int -> int and boolean -> boolean; carriers keep their
    // javac spelling as the label while types lower to i32 and bool.
    assert_eq!(
        signatures(&ir, b"overloaded")?,
        vec![
            "fn overloaded(int: i32) -> i32",
            "fn overloaded(boolean: bool) -> bool"
        ]
    );
    Ok(())
}

#[test]
fn java_type_rows_render_exactly() -> Result<(), Box<dyn Error>> {
    let ir = fixture()?;
    assert_eq!(primitive(&ir)?, "i32");
    // ba97930fc closed the structural forms these rows use: `input`'s Array
    // row is one structural sequence over its `Widget` component, `label`'s
    // wildcard is `? extends Widget`, and `Outer.Inner` is a qualified path.
    assert_eq!(field_type(&ir, b"input")?, "[]Widget");
    assert_eq!(field_type(&ir, b"label")?, "? extends Widget");
    assert_eq!(field_type(&ir, b"Outer.Inner")?, "Outer.Inner");
    Ok(())
}

#[test]
fn java_docs_and_embedding_render_exactly() -> Result<(), Box<dyn Error>> {
    let ir = fixture()?;
    let id = item(&ir, b"Outer.Inner", ItemKind::Field)?;
    // Trunk 4d1cceba9 attaches admitted Javadoc: the fixture's inline
    // {@link Target target} links locally to the admitted Target class,
    // rendered as a docs.rs markdown link.
    assert_eq!(
        ir.display_docs(id)
            .ok_or_else(|| io::Error::other("Java fixture docs are missing"))?
            .to_string(),
        "See [target](struct.Target.html)."
    );
    let run = item(&ir, b"run", ItemKind::Function)?;
    // `run` carries no documentation facts, so the DOCUMENTED profile stream
    // equals the signature-only stream under the new docs plane.
    assert_eq!(
        ir.embedding_text(run, EmbeddingProfile::SYMBOL)
            .ok_or_else(|| io::Error::other("Java fixture symbol embedding is missing"))?
            .to_string(),
        "fn run(int: i32, int: i32) -> i32"
    );
    assert_eq!(
        ir.embedding_text(run, EmbeddingProfile::DOCUMENTED)
            .ok_or_else(|| io::Error::other("Java fixture documented embedding is missing"))?
            .to_string(),
        "fn run(int: i32, int: i32) -> i32"
    );
    Ok(())
}
