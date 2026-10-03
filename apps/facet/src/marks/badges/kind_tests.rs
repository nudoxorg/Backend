use super::{Item, Lang, Reading, Shape, read};
use crate::folio::cards::CardFacts;
use crate::icons::Kind;
use crate::tokens::Family;

const LANGUAGES: [Lang; 9] = [
    Lang::Rust,
    Lang::Typescript,
    Lang::Python,
    Lang::Go,
    Lang::Java,
    Lang::Csharp,
    Lang::C,
    Lang::Cpp,
    Lang::Unknown,
];

#[test]
fn typed_values_keep_distinct_words_without_borrowing_source_in_every_language() {
    for lang in LANGUAGES {
        for signature in [None, Some("nominal(entity=signal, package=p)")] {
            let variable = Item::new("signal", lang)
                .kind(Some(Kind::Variable))
                .signature(signature);
            let constant = Item::new("signal", lang)
                .kind(Some(Kind::Constant))
                .signature(signature);
            let variable_reading = read(&variable);
            let constant_reading = read(&constant);
            assert_eq!(
                (variable_reading.shape, variable_reading.word),
                (Shape::Variable, "variable"),
                "{lang:?}"
            );
            assert_eq!(
                (constant_reading.shape, constant_reading.word),
                (Shape::Constant, "const"),
                "{lang:?}"
            );
            assert!(variable_reading.badges.is_empty());
            assert!(constant_reading.badges.is_empty());
            let variable_card = CardFacts::of(&variable, None);
            let constant_card = CardFacts::of(&constant, None);
            assert_eq!(variable_card.kind, Kind::Variable);
            assert_eq!(variable_card.word, "variable");
            assert_eq!(constant_card.kind, Kind::Constant);
            assert_eq!(constant_card.word, "const");
            assert!(variable_card.badges.is_empty());
        }
    }
    assert_eq!(Shape::Variable.family(), Family::Value);
    assert_eq!(Shape::Variable.icon_kind(), Kind::Variable);
}

#[test]
fn captured_kind_controls_words_while_source_grammar_keeps_its_own_badges() {
    let callable_value = Item::new("signal", Lang::Typescript)
        .kind(Some(Kind::Variable))
        .signature(Some("export const signal = (input: number) => input"));
    let reading = read(&callable_value);
    assert_eq!(
        reading.shape,
        Shape::Function,
        "the captured signature is a callable value"
    );
    assert_eq!(reading.word, "variable");
    assert!(
        reading.says("takes 1"),
        "the actual signature still describes its input"
    );
    let card = CardFacts::of(&callable_value, None);
    assert_eq!(
        (card.kind, card.word.as_ref()),
        (Kind::Variable, "variable")
    );

    // A prepared source reading cannot rename an independently supplied kind.
    let prepared =
        read(&Item::new("signal", Lang::Rust).signature(Some("pub const signal: usize = 1;")));
    assert_eq!(prepared.word, "const");
    let rebound =
        CardFacts::from_reading("signal", Some(Kind::Variable), Lang::Rust, &prepared, None);
    assert_eq!(
        (rebound.kind, rebound.word.as_ref()),
        (Kind::Variable, "variable")
    );
    assert_eq!(rebound.badges, prepared.badges);
}

#[test]
fn the_closed_captured_kind_grammar_is_shared_by_cards_and_readings() {
    let cases = [
        (Kind::Module, "module"),
        (Kind::Package, "package"),
        (Kind::Import, "import"),
        (Kind::Unknown, "item"),
        (Kind::Struct, "struct"),
        (Kind::Class, "class"),
        (Kind::Enum, "enum"),
        (Kind::Union, "union"),
        (Kind::Type, "alias"),
        (Kind::Trait, "trait"),
        (Kind::Interface, "interface"),
        (Kind::Function, "fn"),
        (Kind::Method, "method"),
        (Kind::Constructor, "constructor"),
        (Kind::Macro, "macro"),
        (Kind::Constant, "const"),
        (Kind::Field, "field"),
        (Kind::Property, "property"),
        (Kind::Variable, "variable"),
        (Kind::Variant, "variant"),
    ];
    assert_eq!(cases.len(), Kind::ALL.len());
    for ((kind, expected), declared_kind) in cases.into_iter().zip(Kind::ALL) {
        assert_eq!(kind, declared_kind);
        let item = Item::new("signal", Lang::Rust).kind(Some(kind));
        let reading = read(&item);
        let projection = reading.kind_presentation(Some(kind), Lang::Rust);
        assert_eq!(projection.kind(), kind);
        assert_eq!(projection.word(), expected);
        assert_eq!(reading.word, expected);
        let card = CardFacts::of(&item, None);
        assert_eq!((card.kind, card.word.as_ref()), (kind, expected));
        assert!(card.badges.is_empty());
    }
}

#[test]
fn an_absent_kind_retains_authored_static_and_language_words() {
    let static_item =
        Item::new("signal", Lang::Rust).signature(Some("pub static signal: u32 = 0;"));
    let static_reading = read(&static_item);
    assert_eq!(
        (static_reading.shape, static_reading.word),
        (Shape::Static, "static")
    );
    let static_card = CardFacts::of(&static_item, None);
    assert_eq!(
        (static_card.kind, static_card.word.as_ref()),
        (Kind::Constant, "static")
    );
    assert_eq!(read(&static_item.kind(Some(Kind::Constant))).word, "static");
    assert_eq!(
        read(&static_item.kind(Some(Kind::Variable))).word,
        "variable"
    );

    for (lang, word) in [
        (Lang::Rust, "fn"),
        (Lang::Go, "func"),
        (Lang::Python, "def"),
        (Lang::C, "function"),
        (Lang::Unknown, "function"),
    ] {
        let reading = Reading {
            shape: Shape::Function,
            word,
            badges: vec![],
        };
        let card = CardFacts::from_reading("signal", None, lang, &reading, None);
        assert_eq!((card.kind, card.word.as_ref()), (Kind::Function, word));
    }
}
