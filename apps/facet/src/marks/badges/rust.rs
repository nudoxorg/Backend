//! Rust's signature grammar: `fn`, `trait`, `type`, `struct`/`enum`/`union`,
//! `const`/`static`, `macro_rules!`.

use super::{Badge, Glyph, Ink, Item, Reading, Shape, between, find_word, has_word, ident, last_name, names_word, split_top, type_params};
use crate::icons::Lang;

/// Reads a normalised Rust declaration.
pub(super) fn read(item: &Item<'_>, text: &str) -> Reading {
    let s = strip_visibility(text);
    let name = item.name;
    let mut badges = Vec::new();
    let shape;
    if s.starts_with("macro_rules!") || s.starts_with("macro ") {
        shape = Shape::Macro;
        badges.push(Badge::new(
            Glyph::Macro,
            format!("{name}!"),
            "A macro: you call it with a bang and it writes code for you.",
            Ink::Plain,
        ));
    } else if let Some(at) = fn_at(s) {
        shape = Shape::Function;
        function(&s[..at], &s[at + 2..], name, &mut badges);
    } else if let Some(rest) = keyword(s, &["unsafe trait", "auto trait", "trait"]) {
        shape = Shape::Contract;
        contract(s.starts_with("unsafe"), rest, &mut badges);
    } else if let Some(rest) = s.strip_prefix("type ") {
        shape = Shape::Alias;
        alias(rest, &mut badges);
    } else if let Some((kind, rest)) = nominal(s) {
        shape = kind;
        record(kind, name, rest, &mut badges);
    } else if let Some((is_static, rest)) = value(s) {
        shape = if is_static { Shape::Static } else { Shape::Constant };
        constant(is_static, rest, &mut badges);
    } else {
        shape = item.kind.map_or(Shape::Item, Shape::of_kind);
    }
    Reading {
        shape,
        word: shape.word(Lang::Rust),
        badges,
    }
}

/// `pub`, `pub(crate)`, `pub(in path)` and `default` peeled off.
fn strip_visibility(text: &str) -> &str {
    let mut s = text.trim();
    if let Some(rest) = s.strip_prefix("pub") {
        let rest = rest.trim_start();
        s = if rest.starts_with('(') {
            rest.find(')').map_or(rest, |close| rest[close + 1..].trim_start())
        } else {
            rest
        };
    }
    s.strip_prefix("default ").unwrap_or(s).trim()
}

/// The start of the `fn` keyword when it is followed by a name.
fn fn_at(s: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(at) = find_word(&s[from..], "fn") {
        let at = from + at;
        let after = s[at + 2..].trim_start();
        // `fn(u8) -> u8` is a function-pointer type, not a declaration.
        if !ident(after).is_empty() && s[at + 2..].starts_with(char::is_whitespace) {
            return Some(at);
        }
        from = at + 2;
    }
    None
}

/// The text after the first of `words` when `s` starts with it as a word.
fn keyword<'a>(s: &'a str, words: &[&str]) -> Option<&'a str> {
    words.iter().find_map(|word| {
        let rest = s.strip_prefix(word)?;
        rest.starts_with(char::is_whitespace).then(|| rest.trim_start())
    })
}

enum Receiver {
    Reads,
    Changes,
    Consumes,
}

fn receiver(param: &str) -> Option<Receiver> {
    let param = param.trim();
    let (borrowed, rest) = param.strip_prefix('&').map_or((false, param), |rest| (true, rest.trim_start()));
    let rest = if rest.starts_with('\'') {
        rest.split_once(char::is_whitespace).map_or("", |(_, tail)| tail.trim_start())
    } else {
        rest
    };
    let (changes, rest) = rest.strip_prefix("mut ").map_or((false, rest), |tail| (true, tail.trim_start()));
    let bare = rest.strip_prefix("self")?;
    if !(bare.is_empty() || bare.starts_with(':') || bare.starts_with(char::is_whitespace)) {
        return None;
    }
    Some(match (borrowed, changes) {
        (true, true) => Receiver::Changes,
        (true, false) => Receiver::Reads,
        // `mut self` and `self` take it by value.
        (false, _) => Receiver::Consumes,
    })
}

fn function(prefix: &str, after: &str, name: &str, out: &mut Vec<Badge>) {
    let is_async = has_word(prefix, "async");
    let is_unsafe = has_word(prefix, "unsafe");
    let is_const = has_word(prefix, "const");
    let is_extern = has_word(prefix, "extern");
    // After `fn`: name, generics, `(params)`, `-> ret`, `where`.
    let after = after.trim_start();
    let after = after.get(ident(after).len()..).unwrap_or("").trim_start();
    let generics = after.starts_with('<').then(|| between(after, '<', '>')).flatten();
    let past_generics = generics.map_or(after, |body| after.get(body.len() + 2..).unwrap_or("").trim_start());
    let params = between(past_generics, '(', ')').unwrap_or("");
    let tail = past_generics
        .find('(')
        .and_then(|open| past_generics.get(open + params.len() + 2..))
        .unwrap_or("")
        .trim_start();
    let ret = tail.strip_prefix("->").map(|rest| {
        let end = find_word(rest, "where").into_iter().chain(rest.find('{')).min().unwrap_or(rest.len());
        rest[..end].trim().trim_end_matches(';').trim()
    });

    if is_async {
        out.push(Badge::new(Glyph::Async, "async", "It returns a future: you .await it.", Ink::Peri));
    }
    if is_unsafe {
        out.push(Badge::new(
            Glyph::Unsafe,
            "unsafe",
            "Calling it is unsafe: you promise what the compiler can't check.",
            Ink::Amber,
        ));
    }
    if is_const {
        out.push(Badge::new(Glyph::Const, "const", "It can run while compiling.", Ink::Plain));
    }
    if is_extern {
        out.push(Badge::new(Glyph::Ffi, "C ABI", "It uses the C calling convention.", Ink::Amber));
    }
    let parts = split_top(params);
    let recv_at = parts.iter().position(|param| receiver(param).is_some());
    let recv = recv_at.and_then(|at| receiver(parts[at]));
    let rest: Vec<&str> = parts.iter().enumerate().filter(|(at, _)| Some(*at) != recv_at).map(|(_, param)| *param).collect();
    if let Some(kind) = &recv {
        out.push(match kind {
            Receiver::Changes => Badge::new(Glyph::Changes, "changes it", "A method that changes its receiver (&mut self).", Ink::Plain),
            Receiver::Consumes => Badge::new(Glyph::Consumes, "consumes it", "A method that takes its receiver by value.", Ink::Plain),
            Receiver::Reads => Badge::new(Glyph::Reads, "reads it", "A method that only reads its receiver (&self).", Ink::Plain),
        });
    }
    if rest.is_empty() {
        if recv.is_none() {
            out.push(Badge::new(Glyph::Takes, "takes nothing", "No arguments.", Ink::Plain));
        }
    } else {
        let names = rest
            .iter()
            .map(|param| param.split(':').next().unwrap_or(param).trim().trim_start_matches("mut ").to_owned())
            .collect::<Vec<_>>()
            .join(", ");
        out.push(Badge::new(Glyph::Takes, format!("takes {}", rest.len()), names, Ink::Plain));
    }
    if rest.iter().any(|param| param.split_once(':').is_some_and(|(_, ty)| ty.trim_start().starts_with("&mut "))) {
        out.push(Badge::new(Glyph::Changes, "writes into", "One of its arguments is borrowed mutably.", Ink::Plain));
    }
    if let Some(body) = generics {
        let names = type_params(body);
        if !names.is_empty() {
            out.push(Badge::new(
                Glyph::Generic,
                names_word(&names, 3),
                format!("Generic over {}.", names.join(", ")),
                Ink::Teal,
            ));
        }
    }
    if let Some(ret) = ret {
        output(ret, is_async, name, out);
    }
}

/// The badge a return type earns.
fn output(ret: &str, is_async: bool, name: &str, out: &mut Vec<Badge>) {
    if has_word(ret, "Result") {
        out.push(Badge::new(Glyph::Fail, "can fail", "It returns a Result: an error is a normal outcome.", Ink::Coral));
    } else if ret.starts_with("Option<") {
        out.push(Badge::new(Glyph::Maybe, "maybe", "It returns an Option: sometimes there's nothing.", Ink::Plain));
    } else if ret.starts_with("impl Iterator") || ret.starts_with("impl IntoIterator") || ret.ends_with("Iter") || has_word(ret, "Iter") || ret.contains("Iter<") {
        out.push(Badge::new(Glyph::Iter, "iterates", "It hands back an iterator.", Ink::Plain));
    } else if ret.starts_with("Self") || ret == name {
        out.push(Badge::new(Glyph::Makes, "makes one", "It builds a new value of its type.", Ink::Plain));
    } else if ret == "bool" {
        out.push(Badge::new(Glyph::Maybe, "yes / no", "It answers with a bool.", Ink::Plain));
    } else if ret == "!" {
        out.push(Badge::new(Glyph::Fail, "never returns", "It does not come back: it loops or exits.", Ink::Coral));
    } else if !is_async && (ret.contains("Future") || ret.starts_with("BoxFuture")) {
        out.push(Badge::new(Glyph::Async, "future", "It returns a future: you .await it.", Ink::Peri));
    }
}

fn contract(is_unsafe: bool, rest: &str, out: &mut Vec<Badge>) {
    if is_unsafe {
        out.push(Badge::new(Glyph::Unsafe, "unsafe", "Implementing it is unsafe.", Ink::Amber));
    }
    let rest = rest.get(ident(rest).len()..).unwrap_or("").trim_start();
    let (generics, rest) = if rest.starts_with('<') {
        let body = between(rest, '<', '>');
        (body, body.map_or(rest, |body| rest.get(body.len() + 2..).unwrap_or("").trim_start()))
    } else {
        (None, rest)
    };
    if let Some(body) = generics {
        let names = type_params(body);
        if !names.is_empty() {
            out.push(Badge::new(Glyph::Generic, names.join(" "), "Generic parameters.", Ink::Teal));
        }
    }
    if let Some(bounds) = rest.strip_prefix(':') {
        let end = find_word(bounds, "where").into_iter().chain(bounds.find('{')).min().unwrap_or(bounds.len());
        for bound in bounds[..end].split('+').map(str::trim).filter(|bound| !bound.is_empty() && !bound.starts_with('\'')).take(3) {
            let short = last_name(bound);
            out.push(Badge::new(
                Glyph::Owner,
                format!("needs {short}"),
                format!("Anything that implements it must also be {bound}."),
                Ink::Plain,
            ));
        }
    }
}

fn alias(rest: &str, out: &mut Vec<Badge>) {
    if let Some((_, target)) = rest.split_once('=') {
        let target = target.trim().trim_end_matches(';').trim();
        out.push(Badge::new(
            Glyph::Owner,
            format!("is {}", last_name(target)),
            format!("Another name for {target}."),
            Ink::Plain,
        ));
    }
}

fn nominal(s: &str) -> Option<(Shape, &str)> {
    for (word, shape) in [("struct", Shape::Struct), ("enum", Shape::Enum), ("union", Shape::Union)] {
        if let Some(rest) = s.strip_prefix(word)
            && rest.starts_with(char::is_whitespace)
        {
            return Some((shape, rest.trim_start()));
        }
    }
    None
}

fn record(shape: Shape, name: &str, rest: &str, out: &mut Vec<Badge>) {
    let rest = rest.get(ident(rest).len()..).unwrap_or("").trim_start();
    let (generics, rest) = if rest.starts_with('<') {
        let body = between(rest, '<', '>');
        (body, body.map_or("", |body| rest.get(body.len() + 2..).unwrap_or("").trim_start()))
    } else {
        (None, rest)
    };
    if let Some(body) = generics {
        let params = split_top(body);
        if params.iter().any(|param| param.starts_with('\'')) {
            out.push(Badge::new(Glyph::Reads, "borrows", "It holds a reference: it can't outlive what it borrows.", Ink::Plain));
        }
        let names = type_params(body);
        if !names.is_empty() {
            out.push(Badge::new(
                Glyph::Generic,
                names_word(&names, 3),
                format!("Generic over {}.", names.join(", ")),
                Ink::Teal,
            ));
        }
    }
    if shape == Shape::Struct {
        match rest.chars().next() {
            Some(';') => out.push(Badge::new(Glyph::Marker, "marker", "It has no fields: its type is the point.", Ink::Plain)),
            Some('(') => {
                let inner = between(rest, '(', ')').unwrap_or("");
                let first = split_top(inner).first().copied().unwrap_or("").trim().trim_start_matches("pub ").trim();
                let first = first.split('<').next().unwrap_or(first).trim();
                if first.is_empty() || first == "()" {
                    out.push(Badge::new(Glyph::Marker, "marker", "It carries nothing: its type is the point.", Ink::Plain));
                } else {
                    out.push(Badge::new(Glyph::Owner, format!("wraps {first}"), "A tuple struct around one value.", Ink::Plain));
                }
            }
            _ => {}
        }
    }
    if name.ends_with("Error") {
        out.push(Badge::new(Glyph::Error, "error", "An error type: what goes wrong, and why.", Ink::Coral));
    } else if ["Iter", "Iterator", "Keys", "Values", "Drain", "IntoIter", "Stream"].iter().any(|suffix| name.ends_with(suffix)) {
        out.push(Badge::new(Glyph::Iter, "iterator", "You loop over it.", Ink::Plain));
    } else if name.ends_with("Guard") {
        out.push(Badge::new(Glyph::Const, "guard", "Holds something until it's dropped.", Ink::Plain));
    } else if name.ends_with("Builder") {
        out.push(Badge::new(Glyph::Makes, "builder", "Builds a value step by step.", Ink::Plain));
    }
}

fn value(s: &str) -> Option<(bool, &str)> {
    keyword(s, &["const", "static"]).map(|rest| (s.starts_with("static"), rest))
}

fn constant(is_static: bool, rest: &str, out: &mut Vec<Badge>) {
    let (mutable, rest) = rest.strip_prefix("mut ").map_or((false, rest), |tail| (true, tail));
    if is_static && mutable {
        out.push(Badge::new(Glyph::Unsafe, "mutable static", "A global you can change: unsafe to touch.", Ink::Amber));
    }
    let Some((_, ty)) = rest.split_once(':') else { return };
    let ty = ty.split(['=', ';']).next().unwrap_or(ty).trim();
    if ty.is_empty() {
        return;
    }
    let short = ty.strip_prefix("&'static ").map_or_else(|| ty.to_owned(), |rest| format!("&{rest}"));
    let short = if short.chars().count() > 18 {
        format!("{}…", short.chars().take(17).collect::<String>())
    } else {
        short
    };
    out.push(Badge::new(Glyph::Owner, short, format!("Its type: {ty}."), Ink::Plain));
}
