//! A choice read in its own language, into one shape: the fork.
//!
//! - Rust: an `enum`'s variants, each with what it carries.
//! - TypeScript: a type alias that is a union of literals (`"email" | "url"`)
//!   or of types (`string | number`); `(string & {})` keeps it open.
//! - Go: a named type and the `const` block of its values (`iota`). Go can't
//!   close it: any value of the underlying type converts.
//! - Python: a class that extends `Enum`.
//! - Java, C# and C++: an `enum`'s constants, with their values.
//!
//! Accessors that read one case (`as_str`, `is_str`, `as_table_mut`) move
//! onto that case's tine.

use super::{
    Accessor, Case, CaseKind, Choice, DeclKind, Effect, Lang, Rung, Source, SourceMember, go_prim, split_union,
    strip_comment, words,
};

/// The fork for `source`, when it is a choice.
pub(super) fn choice(source: &Source) -> Option<Choice> {
    let mut choice = match (source.kind, source.lang) {
        (DeclKind::Enum, _) => variants(source)?,
        (DeclKind::Alias | DeclKind::Other, Lang::TypeScript) => union(source)?,
        (DeclKind::Alias | DeclKind::Other, Lang::Go) => iota(source)?,
        (DeclKind::Class, Lang::Python) if python_enum(source) => variants(source)?,
        _ => return None,
    };
    accessors(&mut choice, source);
    Some(choice)
}

fn python_enum(source: &Source) -> bool {
    source.extends.iter().any(|base| {
        let last = base.rsplit('.').next().unwrap_or(base);
        matches!(last, "Enum" | "IntEnum" | "StrEnum" | "Flag" | "IntFlag")
    })
}

fn case(name: &str, kind: CaseKind, member: Option<&SourceMember>) -> Case {
    Case {
        name: name.to_owned(),
        kind,
        carries: Vec::new(),
        fields: Vec::new(),
        value: None,
        doc: member.and_then(|member| member.summary.clone()).filter(|doc| !doc.trim().is_empty()),
        accessors: Vec::new(),
        deprecated: member.is_some_and(|member| member.deprecated.is_some()),
        link: member.and_then(|member| member.link.clone()),
    }
}

/// An enum's members: `Typed(A, B)`, `Neighbourhood`, `Foo { a: u8 }`,
/// `Red = 1`, `RED("r")`.
fn variants(source: &Source) -> Option<Choice> {
    let members = source
        .made_of
        .iter()
        .filter(|member| matches!(member.kind, DeclKind::Variant | DeclKind::Constant | DeclKind::Field))
        .collect::<Vec<_>>();
    if members.is_empty() {
        return None;
    }
    let mut cases = Vec::new();
    for member in members {
        let mut out = case(&member.name, CaseKind::Name, Some(member));
        let text = strip_comment(member.signature.as_deref().unwrap_or(""), source.lang).trim().trim_end_matches(',').trim();
        let rest = text.strip_prefix(member.name.as_str()).unwrap_or("").trim();
        if let Some(inner) = rest.strip_prefix('(').and_then(|rest| rest.strip_suffix(')')) {
            if source.lang == Lang::Rust {
                out.carries = crate::semantics::types::split_top(inner, ',')
                    .into_iter()
                    .map(|part| part.trim().to_owned())
                    .filter(|part| !part.is_empty())
                    .map(|part| words(&part, source.lang))
                    .collect();
            } else {
                // A constructor's arguments (Java): what it is made with.
                out.value = Some(inner.trim().to_owned()).filter(|value| !value.is_empty());
            }
        } else if let Some(inner) = rest.strip_prefix('{').and_then(|rest| rest.strip_suffix('}')) {
            out.fields = crate::semantics::types::split_top(inner, ',')
                .into_iter()
                .filter_map(|part| {
                    let (name, ty) = part.split_once(':')?;
                    let ty = ty.trim();
                    Some(Rung {
                        name: name.trim().trim_start_matches("pub ").trim().to_owned(),
                        ty: words(ty, source.lang),
                        optional: ty.starts_with("Option<"),
                        readonly: false,
                        doc: None,
                        deprecated: false,
                    })
                })
                .collect();
        } else if let Some(value) = rest.strip_prefix('=') {
            out.value = Some(value.trim().to_owned()).filter(|value| !value.is_empty());
        }
        cases.push(out);
    }
    Some(Choice { cases, open: None, shared: Vec::new(), told_by: None, each: None })
}

/// The right-hand side of `type X<T = U> = …;`: after the first `=` outside
/// angle brackets.
fn alias_body(signature: &str) -> Option<&str> {
    let mut depth = 0_i32;
    for (at, ch) in signature.char_indices() {
        match ch {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => depth -= 1,
            '=' if depth == 0 && !signature[at + 1..].starts_with('>') => {
                return Some(signature[at + 1..].trim().trim_end_matches(';').trim());
            }
            _ => {}
        }
    }
    None
}

/// A TypeScript union alias: its literals and types are the cases.
fn union(source: &Source) -> Option<Choice> {
    let body = alias_body(source.signature.as_deref()?)?;
    let parts = split_union(body);
    if parts.len() < 2 {
        return None;
    }
    let mut cases = Vec::new();
    let mut open = None;
    for part in &parts {
        let part = part.trim();
        if part.replace(' ', "").starts_with("(string&") {
            open = Some("any text".to_owned());
        } else if part.starts_with(['"', '\'', '`']) || part.parse::<f64>().is_ok() || matches!(part, "true" | "false") {
            cases.push(case(&part.replace('\'', "\""), CaseKind::Literal, None));
        } else {
            let mut out = case(part, CaseKind::Type, None);
            out.carries = vec![words(part, Lang::TypeScript)];
            cases.push(out);
        }
    }
    (!cases.is_empty()).then_some(Choice { cases, open, shared: Vec::new(), told_by: None, each: None })
}

/// A Go named type (`ErrorHandling int`) and the constants declared of it,
/// in order: `ContinueOnError ErrorHandling = iota`, then bare names that
/// repeat the expression with the next `iota`.
fn iota(source: &Source) -> Option<Choice> {
    let signature = source.signature.as_deref()?.trim();
    let underlying = signature.strip_prefix("type ").unwrap_or(signature).trim().strip_prefix(source.name.as_str())?.trim();
    // A named struct, interface or func is not a choice.
    if underlying.is_empty() || go_prim(underlying).is_none() || underlying == "error" {
        return None;
    }
    let mut cases = Vec::new();
    let mut expr: Option<String> = None;
    let mut position = 0_u32;
    for member in source.made_of.iter().filter(|member| member.kind == DeclKind::Constant) {
        let text = strip_comment(member.signature.as_deref().unwrap_or(&member.name), Lang::Go).trim().to_owned();
        let rest = text.trim_start_matches("const ").trim().strip_prefix(member.name.as_str()).unwrap_or("").trim();
        let (ty, value) = rest.split_once('=').map_or((rest, None), |(ty, value)| (ty.trim(), Some(value.trim().to_owned())));
        match (ty, value) {
            // A spec of this type starts (or restarts) the run.
            (ty, Some(value)) if ty == source.name => expr = Some(value),
            // A bare name repeats the previous spec, one iota on.
            ("", None) if expr.is_some() => {}
            _ if cases.is_empty() => continue,
            _ => break,
        }
        let mut out = case(&member.name, CaseKind::Constant, Some(member));
        out.value = match expr.as_deref() {
            Some("iota") => Some(position.to_string()),
            Some(value) if !value.contains("iota") => Some(value.to_owned()),
            _ => None,
        };
        position += 1;
        cases.push(out);
    }
    if cases.is_empty() {
        return None;
    }
    Some(Choice { cases, open: Some(format!("any {underlying}")), shared: Vec::new(), told_by: None, each: Some(words(underlying, Lang::Go)) })
}

/// `DateTime` → `date_time`.
fn snake(name: &str) -> String {
    let mut out = String::new();
    for (at, ch) in name.chars().enumerate() {
        if ch.is_uppercase() {
            if at > 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

/// Moves each accessor that reads exactly one case onto that case: `as_x`,
/// `as_x_mut`, `is_x`, `into_x`, where `x` is the case's name (or the one
/// case it begins: `as_str` reads `String`).
fn accessors(choice: &mut Choice, source: &Source) {
    let names = choice.cases.iter().map(|case| snake(&case.name)).collect::<Vec<_>>();
    for member in &source.does {
        if !matches!(member.effect, Effect::Reads | Effect::Changes | Effect::UsesUp) {
            continue;
        }
        let Some((prefix, stem)) = ["as_", "is_", "into_"].iter().find_map(|prefix| member.name.strip_prefix(prefix).map(|stem| (*prefix, stem))) else {
            continue;
        };
        let (stem, mutable) = stem.strip_suffix("_mut").map_or((stem, false), |stem| (stem, true));
        let exact = names.iter().position(|name| name == stem);
        let begun = || {
            let hits = names.iter().enumerate().filter(|(_, name)| stem.len() >= 3 && name.starts_with(stem)).collect::<Vec<_>>();
            (hits.len() == 1).then(|| hits[0].0)
        };
        let Some(at) = exact.or_else(begun) else { continue };
        let order = match (prefix, mutable) {
            ("as_", false) => 0,
            ("as_", true) => 1,
            ("is_", _) => 2,
            _ => 3,
        };
        let accessor = Accessor { name: member.name.clone(), changes: mutable || member.effect == Effect::Changes, link: member.link.clone() };
        let list = &mut choice.cases[at].accessors;
        let place = list.iter().position(|other| rank(&other.name) > order).unwrap_or(list.len());
        list.insert(place, accessor);
    }
}

fn rank(name: &str) -> u8 {
    match (name.starts_with("as_"), name.ends_with("_mut"), name.starts_with("is_")) {
        (true, false, _) => 0,
        (true, true, _) => 1,
        (_, _, true) => 2,
        _ => 3,
    }
}
