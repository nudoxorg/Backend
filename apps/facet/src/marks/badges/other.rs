//! The other six ecosystems' signatures, read as far as the text honestly
//! says: what it is, whether it is async, generic, abstract or static, and
//! what its return type promises (an error, nothing, an iterator).
//! Anything the text does not say earns no badge.

use super::{
    Badge, Glyph, Ink, Item, Reading, Shape, between, find_word, has_word, ident, last_name,
    names_word, split_top, type_params,
};
use crate::icons::Lang;

pub(super) fn read(item: &Item<'_>, text: &str, lang: Lang) -> Reading {
    let mut badges = Vec::new();
    let shape = match lang {
        Lang::Typescript => typescript(item, text, &mut badges),
        Lang::Python => python(item, text, &mut badges),
        Lang::Go => go(item, text, &mut badges),
        Lang::Java => java(item, text, &mut badges),
        Lang::Csharp => csharp(item, text, &mut badges),
        Lang::Cpp => cpp(item, text, &mut badges),
        Lang::Rust => Shape::Item,
    };
    let shape = if shape == Shape::Item {
        item.kind.map_or(Shape::Item, Shape::of_kind)
    } else {
        shape
    };
    Reading {
        shape,
        word: shape.word(lang),
        badges,
    }
}

fn badge(
    out: &mut Vec<Badge>,
    glyph: Glyph,
    word: impl Into<gpui::SharedString>,
    tip: impl Into<gpui::SharedString>,
    ink: Ink,
) {
    out.push(Badge::new(glyph, word, tip, ink));
}

fn is_async(out: &mut Vec<Badge>) {
    badge(
        out,
        Glyph::Async,
        "async",
        "It returns a future: you await it.",
        Ink::Peri,
    );
}

fn generic(out: &mut Vec<Badge>, body: Option<&str>) {
    if let Some(body) = body {
        let names = type_params(body);
        if !names.is_empty() {
            badge(
                out,
                Glyph::Generic,
                names_word(&names, 3),
                format!("Generic over {}.", names.join(", ")),
                Ink::Teal,
            );
        }
    }
}

fn takes(out: &mut Vec<Badge>, params: &[&str]) {
    if params.is_empty() {
        badge(
            out,
            Glyph::Takes,
            "takes nothing",
            "No arguments.",
            Ink::Plain,
        );
    } else {
        let names = params
            .iter()
            .map(|param| ident(param.trim_start_matches(['.', '*', '&']).trim_start()).to_owned())
            .filter(|name| !name.is_empty())
            .collect::<Vec<_>>()
            .join(", ");
        badge(
            out,
            Glyph::Takes,
            format!("takes {}", params.len()),
            names,
            Ink::Plain,
        );
    }
}

fn abstract_word(out: &mut Vec<Badge>) {
    badge(
        out,
        Glyph::Abstract,
        "abstract",
        "It has no body of its own: a subtype fills it in.",
        Ink::Plain,
    );
}

fn statics(out: &mut Vec<Badge>) {
    badge(
        out,
        Glyph::Const,
        "static",
        "It belongs to the type, not to an instance.",
        Ink::Plain,
    );
}

/// A return type spelled `ret`, read for the promises the languages share.
fn returns(out: &mut Vec<Badge>, ret: &str, futures: &[&str], iterables: &[&str]) {
    let ret = ret.trim();
    let head = ret.split(['<', '[']).next().unwrap_or(ret).trim();
    let head = head.rsplit(['.', ':']).next().unwrap_or(head);
    if futures.contains(&head) {
        if !out.iter().any(|b| b.glyph == Glyph::Async) {
            is_async(out);
        }
    } else if iterables.contains(&head) {
        badge(
            out,
            Glyph::Iter,
            "iterates",
            "It hands back something you loop over.",
            Ink::Plain,
        );
    } else if matches!(head, "Optional" | "Maybe")
        || ret.ends_with('?')
        || ret.contains("| null")
        || ret.contains("| undefined")
        || ret.contains("| None")
        || ret.starts_with("std::optional")
    {
        badge(
            out,
            Glyph::Maybe,
            "maybe",
            "Sometimes there's nothing: it may hand back null.",
            Ink::Plain,
        );
    } else if matches!(ret, "bool" | "boolean" | "Boolean") {
        badge(
            out,
            Glyph::Maybe,
            "yes / no",
            "It answers with a boolean.",
            Ink::Plain,
        );
    }
}

// ---------------------------------------------------------------- TypeScript

fn typescript(item: &Item<'_>, text: &str, out: &mut Vec<Badge>) -> Shape {
    let s = text
        .trim_start_matches("export ")
        .trim_start_matches("default ")
        .trim_start_matches("declare ")
        .trim();
    if let Some(rest) = s
        .strip_prefix("abstract class ")
        .or_else(|| s.strip_prefix("class "))
    {
        if s.starts_with("abstract") {
            abstract_word(out);
        }
        generic(
            out,
            rest.split_once('<').and_then(|_| between(rest, '<', '>')),
        );
        if let Some(base) = rest
            .split_once(" extends ")
            .map(|(_, tail)| ident(tail.trim_start()))
        {
            badge(
                out,
                Glyph::Owner,
                format!("extends {base}"),
                format!("It builds on {base}."),
                Ink::Plain,
            );
        }
        if let Some(tail) = rest.split_once(" implements ").map(|(_, tail)| tail) {
            let name = ident(tail.trim_start());
            badge(
                out,
                Glyph::Owner,
                format!("implements {name}"),
                format!("It promises everything {name} asks."),
                Ink::Plain,
            );
        }
        return Shape::Struct;
    }
    if let Some(rest) = s.strip_prefix("interface ") {
        generic(
            out,
            rest.split_once('<').and_then(|_| between(rest, '<', '>')),
        );
        if let Some(base) = rest
            .split_once(" extends ")
            .map(|(_, tail)| ident(tail.trim_start()))
        {
            badge(
                out,
                Glyph::Owner,
                format!("extends {base}"),
                format!("It also asks everything {base} asks."),
                Ink::Plain,
            );
        }
        return Shape::Contract;
    }
    if let Some(rest) = s.strip_prefix("type ") {
        generic(
            out,
            rest.split_once('<')
                .and_then(|_| between(rest, '<', '>'))
                .filter(|_| rest.find('<') < rest.find('=')),
        );
        if let Some((_, target)) = rest.split_once('=') {
            let target = target.trim().trim_end_matches(';').trim();
            badge(
                out,
                Glyph::Owner,
                format!(
                    "is {}",
                    last_name(target).chars().take(18).collect::<String>()
                ),
                format!("Another name for {target}."),
                Ink::Plain,
            );
        }
        return Shape::Alias;
    }
    if s.starts_with("enum ") || s.starts_with("const enum ") {
        return Shape::Enum;
    }
    let readonly = has_word(s, "readonly");
    let arrow = s.contains("=>")
        && (s.starts_with("const ") || s.starts_with("let ") || s.starts_with("var "));
    if let Some(at) = find_word(s, "function").filter(|_| !arrow) {
        let prefix = &s[..at];
        if has_word(prefix, "async") {
            is_async(out);
        }
        let after = s[at + "function".len()..].trim_start();
        if after.starts_with('*') {
            badge(
                out,
                Glyph::Iter,
                "iterates",
                "A generator: it yields values one at a time.",
                Ink::Plain,
            );
        }
        let after = after.trim_start_matches('*').trim_start();
        let after = after.get(ident(after).len()..).unwrap_or("").trim_start();
        let (generics, after) = if after.starts_with('<') {
            let body = between(after, '<', '>');
            (
                body,
                after
                    .get(body.map_or(0, str::len) + 2..)
                    .unwrap_or("")
                    .trim_start(),
            )
        } else {
            (None, after)
        };
        signature(
            out,
            generics,
            after,
            '(',
            ')',
            ':',
            &["Promise", "PromiseLike"],
            &[
                "Iterable",
                "Iterator",
                "IterableIterator",
                "Generator",
                "AsyncIterable",
                "AsyncGenerator",
                "AsyncIterator",
            ],
        );
        return Shape::Function;
    }
    if arrow {
        if has_word(s, "async") {
            is_async(out);
        }
        if let Some(open) = s.find('(') {
            signature(
                out,
                None,
                &s[open..],
                '(',
                ')',
                ':',
                &["Promise", "PromiseLike"],
                &["Iterable", "Iterator", "Generator"],
            );
        }
        return Shape::Function;
    }
    if s.starts_with("const ") || s.starts_with("let ") || s.starts_with("var ") {
        if readonly {
            badge(
                out,
                Glyph::Const,
                "readonly",
                "You can read it but not assign to it.",
                Ink::Plain,
            );
        }
        if let Some((_, ty)) = s.split_once(':') {
            let ty = ty.split('=').next().unwrap_or(ty).trim();
            if !ty.is_empty() {
                badge(
                    out,
                    Glyph::Owner,
                    ty.chars().take(18).collect::<String>(),
                    format!("Its type: {ty}."),
                    Ink::Plain,
                );
            }
        }
        return Shape::Constant;
    }
    // A member: `async name<T>(a: A): Promise<B>` and friends.
    if has_word(s, "static") {
        statics(out);
    }
    if has_word(s, "abstract") {
        abstract_word(out);
    }
    if has_word(s, "async") {
        is_async(out);
    }
    if let Some(open) = s.find('(') {
        signature(
            out,
            None,
            &s[open..],
            '(',
            ')',
            ':',
            &["Promise", "PromiseLike"],
            &["Iterable", "Iterator", "Generator"],
        );
        return Shape::Function;
    }
    Shape::Item
}

/// Reads `(params): ret` (or the Go/Python spellings through `arrow`).
fn signature(
    out: &mut Vec<Badge>,
    generics: Option<&str>,
    from: &str,
    open: char,
    close: char,
    colon: char,
    futures: &[&str],
    iterables: &[&str],
) {
    let params = between(from, open, close).unwrap_or("");
    let parts = split_top(params);
    takes(out, &parts);
    generic(out, generics);
    let past = from.get(params.len() + 2..).unwrap_or("").trim_start();
    if let Some(ret) = past.strip_prefix(colon).or_else(|| past.strip_prefix("=>")) {
        let ret = ret.split(['{', ';']).next().unwrap_or(ret).trim();
        returns(out, ret, futures, iterables);
    }
}

// ---------------------------------------------------------------- Python

fn python(item: &Item<'_>, text: &str, out: &mut Vec<Badge>) -> Shape {
    let s = text.trim();
    if let Some(rest) = s.strip_prefix("class ") {
        let bases = between(rest, '(', ')').unwrap_or("");
        let bases = split_top(bases);
        let shape = if bases
            .iter()
            .any(|b| b.ends_with("Protocol") || b.ends_with("ABC"))
        {
            Shape::Contract
        } else if bases
            .iter()
            .any(|b| last_name(b) == "Enum" || last_name(b).ends_with("Enum"))
        {
            Shape::Enum
        } else {
            Shape::Struct
        };
        for base in bases
            .iter()
            .filter(|b| !b.contains('=') && !matches!(last_name(b), "Protocol" | "ABC" | "object"))
            .take(2)
        {
            badge(
                out,
                Glyph::Owner,
                format!("extends {}", last_name(base)),
                format!("It builds on {base}."),
                Ink::Plain,
            );
        }
        if bases.iter().any(|b| b.starts_with("Generic[")) {
            let body = bases
                .iter()
                .find(|b| b.starts_with("Generic["))
                .and_then(|b| between(b, '[', ']'));
            generic(out, body);
        }
        return shape;
    }
    let rest = s.strip_prefix("async def ").map(|rest| {
        is_async(out);
        rest
    });
    let Some(rest) = rest.or_else(|| s.strip_prefix("def ")) else {
        // `NAME: int = 1`
        if let Some((_, ty)) = s.split_once(':') {
            let ty = ty.split('=').next().unwrap_or(ty).trim();
            if !ty.is_empty() {
                badge(
                    out,
                    Glyph::Owner,
                    ty.chars().take(18).collect::<String>(),
                    format!("Its type: {ty}."),
                    Ink::Plain,
                );
            }
        }
        return Shape::Constant;
    };
    let rest = rest.get(ident(rest).len()..).unwrap_or("").trim_start();
    let generics = if rest.starts_with('[') {
        between(rest, '[', ']')
    } else {
        None
    };
    let after_generics = if rest.starts_with('[') {
        rest.get(between(rest, '[', ']').map_or(0, str::len) + 2..)
            .unwrap_or("")
            .trim_start()
    } else {
        rest
    };
    let params = between(after_generics, '(', ')').unwrap_or("");
    let parts = split_top(params);
    let (recv, rest_params): (Vec<&str>, Vec<&str>) = parts
        .iter()
        .partition(|p| matches!(ident(p), "self" | "cls"));
    if let Some(first) = recv.first() {
        if ident(first) == "cls" {
            badge(
                out,
                Glyph::Const,
                "class method",
                "It belongs to the class: you call it on the type.",
                Ink::Plain,
            );
        } else {
            badge(
                out,
                Glyph::Reads,
                "method",
                "It works on an instance: the first argument is self.",
                Ink::Plain,
            );
        }
    }
    let rest_params: Vec<&str> = rest_params
        .into_iter()
        .filter(|p| !matches!(p.trim(), "*" | "/"))
        .collect();
    takes(out, &rest_params);
    generic(out, generics);
    let past = after_generics
        .get(params.len() + 2..)
        .unwrap_or("")
        .trim_start();
    if let Some(ret) = past.strip_prefix("->") {
        let ret = ret.split(':').next().unwrap_or(ret).trim();
        returns(
            out,
            ret,
            &["Awaitable", "Coroutine", "Future"],
            &[
                "Iterator",
                "Iterable",
                "Generator",
                "AsyncIterator",
                "AsyncGenerator",
                "AsyncIterable",
            ],
        );
    }
    let _ = item;
    Shape::Function
}

// ---------------------------------------------------------------- Go

fn go(item: &Item<'_>, text: &str, out: &mut Vec<Badge>) -> Shape {
    let s = text.trim();
    if let Some(rest) = s.strip_prefix("type ") {
        let name = ident(rest);
        let rest = rest[name.len()..].trim_start();
        if rest.starts_with('[') && !rest.starts_with("[]") {
            generic(out, between(rest, '[', ']'));
        }
        let rest = if rest.starts_with('[') && !rest.starts_with("[]") {
            rest.get(between(rest, '[', ']').map_or(0, str::len) + 2..)
                .unwrap_or("")
                .trim_start()
        } else {
            rest
        };
        if rest.starts_with("struct") {
            return Shape::Struct;
        }
        if rest.starts_with("interface") {
            return Shape::Contract;
        }
        let target = rest.trim_start_matches('=').trim();
        if !target.is_empty() {
            badge(
                out,
                Glyph::Owner,
                format!("is {}", target.chars().take(18).collect::<String>()),
                format!("Another name for {target}."),
                Ink::Plain,
            );
        }
        return Shape::Alias;
    }
    if let Some(rest) = s.strip_prefix("func ") {
        let mut rest = rest.trim_start();
        if rest.starts_with('(') {
            let recv = between(rest, '(', ')').unwrap_or("");
            if recv.contains('*') {
                badge(
                    out,
                    Glyph::Changes,
                    "changes it",
                    "A method on a pointer receiver: it can change what it is called on.",
                    Ink::Plain,
                );
            } else {
                badge(
                    out,
                    Glyph::Reads,
                    "reads it",
                    "A method on a value receiver: it works on a copy.",
                    Ink::Plain,
                );
            }
            rest = rest.get(recv.len() + 2..).unwrap_or("").trim_start();
        }
        let rest = rest.get(ident(rest).len()..).unwrap_or("").trim_start();
        let (generics, rest) = if rest.starts_with('[') {
            let body = between(rest, '[', ']');
            (
                body,
                rest.get(body.map_or(0, str::len) + 2..)
                    .unwrap_or("")
                    .trim_start(),
            )
        } else {
            (None, rest)
        };
        let params = between(rest, '(', ')').unwrap_or("");
        let parts = split_top(params);
        takes(out, &parts);
        generic(out, generics);
        let ret = rest.get(params.len() + 2..).unwrap_or("").trim();
        if ret.ends_with("error") || ret.contains(", error") || ret == "error" {
            badge(
                out,
                Glyph::Fail,
                "can fail",
                "It returns an error: failure is a normal outcome.",
                Ink::Coral,
            );
        } else if ret.ends_with(", bool)") {
            badge(
                out,
                Glyph::Maybe,
                "maybe",
                "It reports whether it found anything as a second value.",
                Ink::Plain,
            );
        } else if ret == "bool" {
            badge(
                out,
                Glyph::Maybe,
                "yes / no",
                "It answers with a bool.",
                Ink::Plain,
            );
        } else if ret.starts_with("<-chan") || ret.starts_with("chan ") {
            badge(
                out,
                Glyph::Iter,
                "channel",
                "It hands back a channel you receive from.",
                Ink::Plain,
            );
        }
        if parts.last().is_some_and(|p| p.contains("...")) {
            badge(
                out,
                Glyph::Generic,
                "variadic",
                "It takes any number of the last argument.",
                Ink::Plain,
            );
        }
        return Shape::Function;
    }
    if s.starts_with("const ") {
        return Shape::Constant;
    }
    if s.starts_with("var ") {
        let _ = item;
        return Shape::Constant;
    }
    Shape::Item
}

// ---------------------------------------------------------------- Java

fn java(item: &Item<'_>, text: &str, out: &mut Vec<Badge>) -> Shape {
    let s = text.trim();
    let head = s.split(['(', '{', '=', ';']).next().unwrap_or(s);
    let shape = if has_word(head, "interface") {
        Some(Shape::Contract)
    } else if has_word(head, "enum") {
        Some(Shape::Enum)
    } else if has_word(head, "record") || has_word(head, "class") {
        Some(Shape::Struct)
    } else {
        None
    };
    if has_word(head, "abstract") {
        abstract_word(out);
    }
    if has_word(head, "static") && shape.is_none() {
        statics(out);
    }
    if has_word(head, "synchronized") {
        badge(
            out,
            Glyph::Const,
            "synchronized",
            "One thread at a time.",
            Ink::Plain,
        );
    }
    if has_word(head, "native") {
        badge(
            out,
            Glyph::Ffi,
            "native",
            "Written outside Java, behind the JVM boundary.",
            Ink::Amber,
        );
    }
    if let Some(shape) = shape {
        if let Some(base) = s
            .split_once(" extends ")
            .map(|(_, tail)| ident(tail.trim_start()))
            .filter(|b| !b.is_empty())
        {
            badge(
                out,
                Glyph::Owner,
                format!("extends {base}"),
                format!("It builds on {base}."),
                Ink::Plain,
            );
        }
        if let Some(tail) = s
            .split_once(" implements ")
            .map(|(_, tail)| ident(tail.trim_start()))
            .filter(|b| !b.is_empty())
        {
            badge(
                out,
                Glyph::Owner,
                format!("implements {tail}"),
                format!("It promises everything {tail} asks."),
                Ink::Plain,
            );
        }
        return shape;
    }
    let Some(open) = s.find('(') else {
        return Shape::Constant;
    };
    let before = &s[..open];
    // `<T> List<T> name` — leading generics.
    let generics = before.find('<').filter(|at| {
        before[..*at].split_whitespace().all(|w| {
            matches!(
                w,
                "public"
                    | "private"
                    | "protected"
                    | "static"
                    | "final"
                    | "abstract"
                    | "default"
                    | "synchronized"
                    | "native"
            )
        })
    });
    let params = between(&s[open..], '(', ')').unwrap_or("");
    takes(out, &split_top(params));
    if generics.is_some() {
        generic(out, between(before, '<', '>'));
    }
    let after = s.get(open + params.len() + 2..).unwrap_or("");
    if after.trim_start().starts_with("throws") {
        badge(
            out,
            Glyph::Fail,
            "can fail",
            "It declares a checked exception: failure is part of its contract.",
            Ink::Coral,
        );
    }
    // The return type: the word before the name, past generics.
    let words: Vec<&str> = before.split_whitespace().collect();
    if words.len() >= 2 {
        let ret = words[words.len() - 2];
        returns(
            out,
            ret,
            &["CompletableFuture", "Future", "Mono"],
            &["Iterator", "Iterable", "Stream", "Flux"],
        );
    }
    Shape::Function
}

// ---------------------------------------------------------------- C#

fn csharp(item: &Item<'_>, text: &str, out: &mut Vec<Badge>) -> Shape {
    let s = text.trim();
    let head = s.split(['(', '{', '=', ';', ':']).next().unwrap_or(s);
    let shape = if has_word(head, "interface") {
        Some(Shape::Contract)
    } else if has_word(head, "enum") {
        Some(Shape::Enum)
    } else if has_word(head, "struct") || has_word(head, "class") || has_word(head, "record") {
        Some(Shape::Struct)
    } else {
        None
    };
    if has_word(head, "abstract") {
        abstract_word(out);
    }
    if has_word(head, "virtual") {
        badge(
            out,
            Glyph::Override,
            "virtual",
            "A subtype may replace it.",
            Ink::Plain,
        );
    }
    if has_word(head, "override") {
        badge(
            out,
            Glyph::Override,
            "override",
            "It replaces what its base type does.",
            Ink::Plain,
        );
    }
    if has_word(head, "unsafe") {
        badge(
            out,
            Glyph::Unsafe,
            "unsafe",
            "Calling it is unsafe: pointers the runtime does not check.",
            Ink::Amber,
        );
    }
    if has_word(head, "extern") {
        badge(
            out,
            Glyph::Ffi,
            "extern",
            "Implemented outside .NET.",
            Ink::Amber,
        );
    }
    if has_word(head, "static") && shape.is_none() {
        statics(out);
    }
    if has_word(head, "async") {
        is_async(out);
    }
    if let Some(shape) = shape {
        if let Some((_, tail)) = s.split_once(':') {
            let base = ident(tail.trim_start());
            if !base.is_empty() {
                badge(
                    out,
                    Glyph::Owner,
                    format!("extends {base}"),
                    format!("It builds on {base}."),
                    Ink::Plain,
                );
            }
        }
        return shape;
    }
    let Some(open) = s.find('(') else {
        return Shape::Constant;
    };
    let before = &s[..open];
    let words: Vec<&str> = before.split_whitespace().collect();
    let params = between(&s[open..], '(', ')').unwrap_or("");
    takes(out, &split_top(params));
    if let Some(last) = words.last().filter(|word| word.contains('<')) {
        generic(out, between(last, '<', '>'));
    }
    if words.len() >= 2 {
        let name_at = words.len() - 1;
        let ret = words[name_at - 1];
        returns(
            out,
            ret,
            &["Task", "ValueTask"],
            &["IEnumerable", "IAsyncEnumerable", "IEnumerator"],
        );
    }
    let _ = item;
    Shape::Function
}

// ---------------------------------------------------------------- C++

fn cpp(item: &Item<'_>, text: &str, out: &mut Vec<Badge>) -> Shape {
    let mut s = text.trim();
    let mut generics: Option<(Vec<String>, String)> = None;
    if let Some(rest) = s.strip_prefix("template") {
        let rest = rest.trim_start();
        if rest.starts_with('<') {
            let body = between(rest, '<', '>');
            if let Some(body) = body {
                let names: Vec<String> = split_top(body)
                    .into_iter()
                    .filter_map(|p| {
                        p.split_whitespace()
                            .last()
                            .map(|w| w.trim_start_matches('.').to_owned())
                    })
                    .filter(|w| !w.is_empty() && w != "class" && w != "typename")
                    .collect();
                if !names.is_empty() {
                    generics = Some((names.clone(), names_word(&names, 3)));
                }
            }
            s = rest
                .get(body.map_or(0, str::len) + 2..)
                .unwrap_or("")
                .trim_start();
        }
    }
    let head = s.split(['(', '{', ';']).next().unwrap_or(s);
    if has_word(head, "enum") {
        return Shape::Enum;
    }
    if has_word(head, "struct") || has_word(head, "class") {
        if let Some((_, tail)) = head.split_once(':') {
            let base = tail
                .trim()
                .trim_start_matches("public ")
                .trim_start_matches("protected ")
                .trim_start_matches("private ");
            let base = ident(base);
            if !base.is_empty() {
                badge(
                    out,
                    Glyph::Owner,
                    format!("extends {base}"),
                    format!("It builds on {base}."),
                    Ink::Plain,
                );
            }
        }
        return Shape::Struct;
    }
    if has_word(head, "using") || has_word(head, "typedef") {
        return Shape::Alias;
    }
    let Some(open) = s.find('(') else {
        return Shape::Constant;
    };
    if has_word(head, "virtual") {
        badge(
            out,
            Glyph::Override,
            "virtual",
            "A subtype may replace it.",
            Ink::Plain,
        );
    }
    if has_word(head, "static") {
        statics(out);
    }
    if has_word(head, "constexpr") || has_word(head, "consteval") {
        badge(
            out,
            Glyph::Const,
            "constexpr",
            "It can run while compiling.",
            Ink::Plain,
        );
    }
    let params = between(&s[open..], '(', ')').unwrap_or("");
    let after = s.get(open + params.len() + 2..).unwrap_or("");
    if has_word(after, "noexcept") {
        badge(
            out,
            Glyph::Const,
            "noexcept",
            "It promises not to throw.",
            Ink::Plain,
        );
    }
    if has_word(after, "const") {
        badge(
            out,
            Glyph::Reads,
            "reads it",
            "A const member: it does not change the object.",
            Ink::Plain,
        );
    }
    if after.contains("= 0") {
        abstract_word(out);
    }
    takes(out, &split_top(params));
    if let Some((names, word)) = generics {
        badge(
            out,
            Glyph::Generic,
            word,
            format!("Generic over {}.", names.join(", ")),
            Ink::Teal,
        );
    }
    let ret = head.split_whitespace().rev().nth(1).unwrap_or("");
    if ret.starts_with("std::optional") || ret == "optional" {
        badge(
            out,
            Glyph::Maybe,
            "maybe",
            "Sometimes there's nothing: it returns an optional.",
            Ink::Plain,
        );
    } else if ret.starts_with("std::expected") {
        badge(
            out,
            Glyph::Fail,
            "can fail",
            "It returns an expected: an error is a normal outcome.",
            Ink::Coral,
        );
    } else if ret == "bool" {
        badge(
            out,
            Glyph::Maybe,
            "yes / no",
            "It answers with a bool.",
            Ink::Plain,
        );
    }
    let _ = item;
    Shape::Function
}
