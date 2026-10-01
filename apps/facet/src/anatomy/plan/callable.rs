//! A callable read in its own language, into one shape: the pipe. Its
//! inputs are ports, its receiver enters from above with what the call does
//! to it, its output leaves to the right, and each way it can fail drops
//! below in coral.
//!
//! Rust, TypeScript, Python, Java and C# read through the shared signature
//! projection (`semantics::recorded::callable`); Go reads here: a receiver
//! `(f *FlagSet)`, grouped parameters (`a, b string`), and results whose
//! trailing `error` is the drop ("or returns error").

use super::{Callable, Drop, DropVerb, Effect, Lang, Port, Recv, Source, Tok, TokKind, Ty, spelled, words};
use crate::semantics::members::Receiver;
use crate::semantics::recorded::{Language, callable as recorded};
use crate::semantics::types::{Piece, split_top};

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

/// The pipe for `source`, when its signature reads as a callable.
pub(super) fn callable(source: &Source) -> Option<Callable> {
    let signature = source.signature.as_deref()?;
    let mut out = match source.lang {
        Lang::Go => go(signature, &source.name, source.owner.as_deref())?,
        lang => shared(signature, &source.name, lang, source.owner.as_deref(), source.kind == super::DeclKind::Method)?,
    };
    // A documented panic is a drop too.
    let panics = source.failures.iter().any(|failure| failure.member.is_none() && failure.verb == DropVerb::Panics);
    if panics && !out.drops.iter().any(|drop| drop.verb == DropVerb::Panics) {
        out.drops.push(Drop { verb: DropVerb::Panics, ty: None });
    }
    Some(out)
}

fn word(text: &str) -> Tok {
    Tok { kind: TokKind::Word, text: text.to_owned(), fam: super::Fam::Type }
}

/// A port's type in words; a bare generic with a bound reads as the bound
/// (`T` where `T: Deserialize` reads "any Deserialize").
fn port_ty(ty: &crate::semantics::types::Spelled, wheres: &[crate::semantics::model::Where]) -> Ty {
    let solid = ty.pieces.iter().filter(|piece| !matches!(piece, Piece::Space)).collect::<Vec<_>>();
    if let [Piece::Var(name)] = solid.as_slice()
        && let Some(clause) = wheres.iter().find(|clause| clause.name == *name)
    {
        let mut pieces = clause.sentence.iter().skip_while(|piece| matches!(piece, Piece::Space)).cloned().collect::<Vec<_>>();
        if let Some(Piece::Word(first)) = pieces.first().cloned() {
            match first.as_ref().strip_prefix("is") {
                Some("") => {
                    pieces.remove(0);
                }
                Some(rest) if rest.starts_with(' ') => pieces[0] = Piece::Word(rest.trim_start().to_owned().into()),
                _ => {}
            }
        }
        let pieces = pieces.into_iter().skip_while(|piece| matches!(piece, Piece::Space)).collect::<Vec<_>>();
        return spelled(&pieces, &ty.source);
    }
    spelled(&ty.pieces, &ty.source)
}

fn shared(signature: &str, name: &str, lang: Lang, owner: Option<&str>, method: bool) -> Option<Callable> {
    let pipe = recorded(signature, name, language(lang))?;
    // TypeScript's types read in its own words (`unknown` is "anything",
    // `$ZodType` keeps its `$`); the shared projection spells Rust's.
    let own = |ty: &crate::semantics::types::Spelled| (lang == Lang::TypeScript).then(|| words(&ty.source, lang));
    let mut receiver = None;
    let mut ports = Vec::new();
    for input in &pipe.inputs {
        match (&input.receiver, &input.ty) {
            (Some(kind), _) => {
                let effect = match kind {
                    Receiver::Reads => Effect::Reads,
                    Receiver::Changes => Effect::Changes,
                    Receiver::UsesUp => Effect::UsesUp,
                    Receiver::Makes => Effect::Makes,
                };
                let ty = owner.map_or_else(|| Ty { toks: vec![word("it")], exact: String::new() }, |owner| words(owner, lang));
                receiver = Some(Recv { name: "self".to_owned(), ty, effect });
            }
            (None, Some(ty)) => ports.push(Port { name: input.name.to_string(), ty: own(ty).unwrap_or_else(|| port_ty(ty, &pipe.wheres)) }),
            (None, None) => {}
        }
    }
    // A method whose signature names no receiver (TypeScript, Java, C#)
    // is still called on its owner; what the call does to it is unsaid.
    if receiver.is_none()
        && method
        && let Some(owner) = owner
    {
        receiver = Some(Recv { name: String::new(), ty: words(owner, lang), effect: Effect::None });
    }
    let drops = match &pipe.fails {
        Some(error) => vec![Drop { verb: DropVerb::FailsWith, ty: error.as_ref().map(|ty| own(ty).unwrap_or_else(|| spelled(&ty.pieces, &ty.source))) }],
        None => Vec::new(),
    };
    Some(Callable {
        receiver,
        ports,
        gives: pipe.output.as_ref().map(|out| own(out).unwrap_or_else(|| spelled(&out.pieces, &out.source))).filter(|ty| !ty.toks.is_empty()),
        drops,
        flags: pipe.flags.iter().map(|flag| (*flag).to_owned()).collect(),
    })
}

/// The first balanced `(…)` in `text` from `from`, and what follows it.
fn parens(text: &str) -> Option<(&str, &str)> {
    let text = text.trim_start();
    let inner = text.strip_prefix('(')?;
    let mut depth = 1_i32;
    for (at, ch) in inner.char_indices() {
        match ch {
            '(' | '[' | '{' => depth += 1,
            ')' | ']' | '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some((&inner[..at], &inner[at + 1..]));
                }
            }
            _ => {}
        }
    }
    None
}

/// `a, b string, c int` → `[(a, string), (b, string), (c, int)]`; unnamed
/// results (`int, error`) have empty names.
fn go_list(list: &str, named: bool) -> Vec<(String, String)> {
    let parts = split_top(list, ',').into_iter().map(|part| part.trim().to_owned()).filter(|part| !part.is_empty()).collect::<Vec<_>>();
    // A list is named when any part has a name and a type.
    let any_named = named && parts.iter().any(|part| part.split_once(char::is_whitespace).is_some_and(|(_, ty)| !ty.trim().is_empty()));
    if !any_named {
        return parts.into_iter().map(|ty| (String::new(), ty)).collect();
    }
    let mut out = Vec::new();
    let mut pending = Vec::new();
    for part in parts {
        match part.split_once(char::is_whitespace) {
            Some((name, ty)) => {
                let ty = ty.trim().to_owned();
                for name in pending.drain(..) {
                    out.push((name, ty.clone()));
                }
                out.push((name.to_owned(), ty));
            }
            None => pending.push(part),
        }
    }
    out.extend(pending.into_iter().map(|name| (name, String::new())));
    out
}

/// `func (f *FlagSet) Parse(arguments []string) error`, with or without
/// `func`.
fn go(signature: &str, name: &str, owner: Option<&str>) -> Option<Callable> {
    let text = signature.trim();
    let text = text.strip_prefix("func").map_or(text, str::trim_start);
    let mut receiver = None;
    let rest = if text.starts_with('(') {
        let (recv, rest) = parens(text)?;
        let (recv_name, recv_ty) = recv.trim().split_once(char::is_whitespace).map_or(("", recv.trim()), |(n, t)| (n, t.trim()));
        let effect = if recv_ty.starts_with('*') { Effect::Changes } else { Effect::Reads };
        let ty = recv_ty.trim_start_matches('*');
        let ty = owner.filter(|owner| !owner.is_empty()).unwrap_or(ty);
        receiver = Some(Recv { name: recv_name.to_owned(), ty: words(ty, Lang::Go), effect });
        rest.trim_start()
    } else {
        text
    };
    let rest = rest.strip_prefix(name)?.trim_start();
    // Type parameters: `[T any]`.
    let rest = if let Some(inner) = rest.strip_prefix('[') { inner.split_once(']').map_or(rest, |(_, after)| after) } else { rest };
    let (params, results) = parens(rest)?;
    let ports = go_list(params, true)
        .into_iter()
        .map(|(name, ty)| {
            let variadic = ty.strip_prefix("...");
            let ty = variadic.map_or_else(|| words(&ty, Lang::Go), |inner| {
                let mut ty = words(inner, Lang::Go);
                ty.toks.insert(0, word("any number of"));
                ty.exact = format!("...{inner}");
                ty
            });
            Port { name, ty }
        })
        .collect();
    let results = results.trim().trim_end_matches('{').trim();
    let mut outs = if results.starts_with('(') { parens(results).map(|(list, _)| go_list(list, true)).unwrap_or_default() } else if results.is_empty() { Vec::new() } else { vec![(String::new(), results.to_owned())] };
    let mut drops = Vec::new();
    if outs.last().is_some_and(|(_, ty)| ty.trim() == "error") {
        outs.pop();
        drops.push(Drop { verb: DropVerb::Returns, ty: Some(words("error", Lang::Go)) });
    }
    let gives = match outs.as_slice() {
        [] => None,
        [(_, ty)] => Some(words(ty, Lang::Go)),
        many => {
            let mut toks = Vec::new();
            let mut exact = Vec::new();
            for (n, (_, ty)) in many.iter().enumerate() {
                if n > 0 {
                    toks.push(word("and"));
                }
                toks.extend(words(ty, Lang::Go).toks);
                exact.push(ty.clone());
            }
            Some(Ty { toks, exact: format!("({})", exact.join(", ")) })
        }
    };
    Some(Callable { receiver, ports, gives, drops, flags: Vec::new() })
}
