//! The page model read the way a person reads the page: the four target
//! pages' anatomy, `can` line, Does list and In use, as plain text, from
//! the fixture world.

use super::world::{find, names, read, world};
use crate::semantics::page::{Payload, Row, Shape, caps_of, facts, in_use, page, shape};
use crate::semantics::types::{Piece, Spelled, Target};

fn words(s: &Spelled) -> String {
    s.plain()
}

fn pieces(p: &[Piece]) -> String {
    p.iter().map(Piece::text).collect()
}

fn row_text(row: &Row) -> String {
    match row {
        Row::One(m) => {
            let params: Vec<String> = m.sig.params.iter().map(words).collect();
            let ret = m.sig.ret.as_ref().map(|r| format!(" → {}", words(r))).unwrap_or_default();
            let args = if params.is_empty() { String::new() } else { format!("({})", params.join(", ")) };
            format!("{}{args}{ret}", m.name)
        }
        Row::Fold(f) => {
            let ins: Vec<String> = f.inputs.iter().take(4).map(words).collect();
            let more = f.inputs.len().saturating_sub(4);
            let ret = f.ret.as_ref().map(|r| format!(" → {}", words(r))).unwrap_or_default();
            format!("{}… (one of {} and {more} more){ret} · {} of them", f.prefix, ins.join(", "), f.members.len())
        }
    }
}

#[test]
fn relation_label_is_a_fork_of_three() {
    let w = world();
    let label = find("present::glyph::RelationLabel");
    let Shape::Fork(fork) = shape(w, names(), label) else { panic!("a fork") };
    assert_eq!(fork.heading(), "one of");
    let branches: Vec<String> = fork
        .branches
        .iter()
        .map(|b| match &b.payload {
            Payload::Unit => b.name.to_string(),
            Payload::Tuple(parts) => format!("{} {}", b.name, parts.iter().map(words).collect::<Vec<_>>().join(" × ")),
            Payload::Record(parts) => format!("{} {{..{}}}", b.name, parts.len()),
        })
        .collect();
    assert_eq!(branches, ["Typed SemanticLinkKind × RelationDirection", "Neighbourhood", "Related"]);
    assert_eq!(fork.branches[0].doc.as_deref(), Some("A relation whose compiler kind and direction are both known."));
    // Both payload types are links into the world.
    let Payload::Tuple(parts) = &fork.branches[0].payload else { panic!() };
    for part in parts {
        let [Target::Node(n)] = part.targets()[..] else { panic!("{part:?}") };
        assert_eq!(w.node(*n).name, part.plain());
    }
}

#[test]
fn relation_label_can_and_does() {
    let w = world();
    let label = find("present::glyph::RelationLabel");
    let caps: Vec<String> = caps_of(w, label).iter().map(|c| c.word.to_string()).collect();
    assert_eq!(caps, ["copies freely", "debug-prints", "hashes", "sorts", "prints", "to text"]);
    let p = page(w, names(), label);
    // `as_str(self)` on a Copy type only reads it.
    assert_eq!(p.does.groups.len(), 1);
    assert_eq!(p.does.groups[0].receiver.heading(), "reads it");
    assert_eq!(p.does.groups[0].rows.iter().map(row_text).collect::<Vec<_>>(), ["as_str → text"]);
    assert_eq!(p.does.through.len(), 1);
    assert_eq!(p.does.through[0].trait_name.as_ref(), "Display");
    assert_eq!(p.does.through[0].members.iter().map(|(_, n)| n.to_string()).collect::<Vec<_>>(), ["fmt"]);
    let f = facts(w, label);
    assert_eq!((f.kind, f.place.as_ref(), f.used_in), ("enum", "present::glyph", 3));
}

#[test]
fn relation_group_holds_two_private_fields() {
    let w = world();
    let group = find("present::page::RelationGroup");
    let Shape::Holds(holds) = shape(w, names(), group) else { panic!("holds") };
    assert_eq!(holds.heading(), "holds, all private");
    let fields: Vec<String> =
        holds.fields.iter().map(|f| format!("{} {}", f.name.as_deref().unwrap_or(""), words(&f.ty))).collect();
    assert_eq!(fields, ["label RelationLabel", "relations list of Relation"]);
    // `RelationLabel` links to the enum, `Relation` to present's Relation.
    assert_eq!(holds.fields[0].ty.targets(), [&Target::Node(find("present::glyph::RelationLabel"))]);
    assert_eq!(holds.fields[1].ty.targets(), [&Target::Node(find("present::page::Relation"))]);
    assert_eq!(holds.fields[1].ty.source.as_ref(), "Box<[Relation]>");
    let p = page(w, names(), group);
    let does: Vec<(String, Vec<String>)> = p
        .does
        .groups
        .iter()
        .map(|g| (g.receiver.heading().to_owned(), g.rows.iter().map(row_text).collect()))
        .collect();
    assert_eq!(
        does,
        [
            ("reads it".to_owned(), vec!["label → RelationLabel".to_owned(), "relations → list of Relation".to_owned()]),
            (
                "makes one, or stands alone".to_owned(),
                vec!["new(RelationLabel, any Into‹list of Relation›) → RelationGroup".to_owned()]
            ),
        ]
    );
    let caps: Vec<String> = p.caps.iter().map(|c| c.word.to_string()).collect();
    assert_eq!(caps, ["clones", "debug-prints", "compares"]);
}

#[test]
fn from_str_is_a_pipe_that_fails_with_serde_jsons_error() {
    let w = world();
    let from_str = find("serde_json::de::from_str");
    let Shape::Pipe(pipe) = shape(w, names(), from_str) else { panic!("a pipe") };
    let inputs: Vec<String> = pipe
        .inputs
        .iter()
        .map(|i| format!("{} {}", i.name, i.ty.as_ref().map(words).unwrap_or_default()))
        .collect();
    assert_eq!(inputs, ["s text"]);
    assert_eq!(pipe.output.as_ref().map(words).as_deref(), Some("T"));
    // The prototype reads the alias `Result<T>` as "or fails with error";
    // the alias's own error is serde_json's `Error`.
    let fails = pipe.fails.clone().flatten().expect("a named error");
    assert_eq!(words(&fails), "Error");
    let [Target::Node(error)] = fails.targets()[..] else { panic!("{fails:?}") };
    assert_eq!(w.qual(*error).as_ref(), "serde_json::error");
    let wheres: Vec<String> = pipe.wheres.iter().map(|x| format!("{} {}", x.name, pieces(&x.sentence))).collect();
    assert_eq!(wheres, ["T is any Deserialize"]);
    // `de::Deserialize` is serde_core's trait, not any other `Deserialize`.
    let target = pipe.wheres[0].sentence.iter().find_map(|p| match p {
        Piece::Name { target: Target::Node(n), .. } => Some(*n),
        _ => None,
    });
    assert_eq!(target.map(|n| w.qual(n).to_string()).as_deref(), Some("serde_core::de"));
}

#[test]
fn visitor_is_a_contract_with_its_look_alikes_folded() {
    let w = world();
    let visitor = find("serde_core::de::Visitor");
    let Shape::Contract(contract) = shape(w, names(), visitor) else { panic!("a contract") };
    assert_eq!(contract.write.iter().map(row_text).collect::<Vec<_>>(), ["expecting(mutable Formatter) → nothing or fails with Error"]);
    let get: Vec<String> = contract.get.iter().map(row_text).collect();
    assert_eq!(
        get,
        [
            "visit_… (one of bool, i8, i16, i32 and 16 more) → its Value or fails with E · 22 of them",
            "visit_some(D) → its Value or fails with D’s Error",
            "visit_newtype_struct(D) → its Value or fails with D’s Error",
            "visit_seq(A) → its Value or fails with A’s Error",
            "visit_map(A) → its Value or fails with A’s Error",
            "visit_enum(A) → its Value or fails with A’s Error",
        ]
    );
    // Names starting `__` are hidden.
    assert!(get.iter().all(|r| !r.starts_with("__")));
    assert_eq!(contract.implementors, 27);
    // `fmt::Formatter` is std's, which the world does not hold: a path, not
    // serde_json's `Formatter` trait.
    let Row::One(expecting) = &contract.write[0] else { panic!() };
    assert_eq!(expecting.sig.params[0].targets(), [&Target::Path("fmt::Formatter".into())]);
}

#[test]
fn in_use_mines_real_statements_from_callers_in_other_packages_first() {
    let w = world();
    let from_str = find("serde_json::de::from_str");
    let uses = in_use(w, from_str, &mut read);
    let got: Vec<String> =
        uses.iter().map(|u| format!("{} · {}:{} · {}", u.package, u.file, u.excerpt.line, u.excerpt.lines[0])).collect();
    assert!(!got.is_empty() && got.len() <= 3, "{got:#?}");
    assert_eq!(
        got[0],
        "extension-qdrant · response.rs:30 · serde_json::from_str(body).map_err(|source| QdrantError::Decode { phase, source })"
    );
    // At most two from one package while others wait.
    let first = uses.iter().filter(|u| u.package == uses[0].package).count();
    assert!(first <= 2, "{got:#?}");
    for u in &uses {
        assert_eq!(&u.excerpt.lines[u.excerpt.hit][u.excerpt.mark.clone()], "from_str");
    }
}

#[test]
fn in_use_for_a_type_reads_the_callers_statement() {
    let w = world();
    let group = find("present::page::RelationGroup");
    let uses = in_use(w, group, &mut read);
    let got: Vec<String> = uses.iter().map(|u| format!("{}:{}", u.file, u.excerpt.line)).collect();
    assert!(got.contains(&"assemble.rs:250".to_owned()), "{got:?}");
    let assemble = uses.iter().find(|u| u.file.as_ref() == "assemble.rs").expect("assemble.rs");
    assert!(assemble.excerpt.lines[0].contains("RelationGroup::new(label, relations.into_boxed_slice())"), "{:?}", assemble.excerpt);
}
