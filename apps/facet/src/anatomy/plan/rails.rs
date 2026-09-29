//! Getting one: the routes to one, best first. The caller's own routes (the
//! world's recipes) when it has them; otherwise the makers the index records
//! (constructors, conversions into it), read from their signatures.
//!
//! Best first: routes from a plain value (text, a number) before routes from
//! a named type, before routes from anything generic; among routes from the
//! same kind, the one that can fail first, because a fallible step from text
//! is a parse, and a parse is the way in.

use super::{Effect, Lang, Rail, Source, Tok, TokKind, Ty, spelled};
use crate::semantics::recorded::{Language, callable};
use crate::semantics::types::Piece;

/// Getting one's rails for `source`.
pub(super) fn rails(source: &Source) -> Vec<Rail> {
    let mut out = if source.rails.is_empty() {
        source.does.iter().filter(|member| member.effect == Effect::Makes).filter_map(|member| maker(source, member)).collect()
    } else {
        source
            .rails
            .iter()
            .map(|rail| Rail {
                from: rail.from.clone(),
                verb: rail.verb.clone(),
                fails: rail.fails,
                maybe: rail.maybe,
                lands: rail.lands.clone(),
                code: rail.code.clone(),
                link: rail.link.clone(),
            })
            .collect::<Vec<_>>()
    };
    out.sort_by_key(|rail| (rank(&rail.from), !rail.fails));
    // Two makers that read the same in words are one route (`From<BTreeMap>`
    // and `From<HashMap>` both start from "map S → V").
    let mut seen = std::collections::BTreeSet::new();
    out.retain(|rail| seen.insert((rail.from.plain(), rail.verb.clone(), rail.fails)));
    out
}

const fn language(lang: Lang) -> Language {
    match lang {
        Lang::Rust => Language::Rust,
        Lang::TypeScript => Language::TypeScript,
        Lang::Go => Language::Go,
        Lang::Python => Language::Python,
        Lang::Java => Language::Java,
        Lang::CSharp => Language::CSharp,
        Lang::Cpp => Language::Cpp,
        Lang::Other => Language::Unknown,
    }
}

/// How plain what a route starts from is: a plain value, a named type,
/// anything else.
fn rank(from: &Ty) -> u8 {
    match from.head().map(|tok| tok.kind) {
        Some(TokKind::Prim | TokKind::Lit) => 0,
        Some(TokKind::Named) => 1,
        _ => 2,
    }
}

/// A maker's route: its first parameter is what you start from.
fn maker(source: &Source, member: &super::SourceMember) -> Option<Rail> {
    let signature = member.signature.as_deref()?;
    let pipe = callable(signature, &member.name, language(source.lang))?;
    let input = pipe.inputs.iter().find(|input| input.receiver.is_none() && input.ty.is_some());
    let from = match input.and_then(|input| input.ty.as_ref()) {
        Some(ty) => {
            let generic = match ty.pieces.iter().find(|piece| !matches!(piece, Piece::Space)) {
                Some(Piece::Var(name)) if ty.pieces.iter().filter(|piece| !matches!(piece, Piece::Space)).count() == 1 => {
                    pipe.wheres.iter().find(|clause| clause.name == *name)
                }
                _ => None,
            };
            match generic {
                // `T` where `T: Serialize`: "any Serialize".
                Some(clause) => {
                    // The sentence reads "is any Serialize": the rail says
                    // what you start from, "any Serialize".
                    let mut pieces = clause.sentence.iter().skip_while(|piece| matches!(piece, Piece::Space)).cloned().collect::<Vec<_>>();
                    if let Some(Piece::Word(word)) = pieces.first().cloned() {
                        match word.as_ref().strip_prefix("is") {
                            Some("") => { pieces.remove(0); }
                            Some(rest) if rest.starts_with(' ') => pieces[0] = Piece::Word(rest.trim_start().to_owned().into()),
                            _ => {}
                        }
                    }
                    let pieces = pieces.into_iter().skip_while(|piece| matches!(piece, Piece::Space)).collect::<Vec<_>>();
                    spelled(&pieces, &ty.source)
                }
                None => spelled(&ty.pieces, &ty.source),
            }
        }
        None => Ty { toks: vec![Tok { kind: TokKind::Word, text: "nothing".to_owned(), fam: super::Fam::Type }], exact: String::new() },
    };
    let maybe = pipe.output.as_ref().is_some_and(|output| {
        output.pieces.iter().find(|piece| !matches!(piece, Piece::Space)).is_some_and(|piece| matches!(piece, Piece::Word(word) if word.as_ref() == "maybe"))
    });
    // Rust's `FromStr::from_str` is called as `s.parse()`.
    let verb = if source.lang == Lang::Rust && member.name == "from_str" { "parse".to_owned() } else { member.name.clone() };
    Some(Rail { from, verb, fails: pipe.fails.is_some(), maybe, lands: None, code: None, link: member.link.clone() })
}
