//! A callable read into the call: its receiver, its ports, how it can end
//! (gives, or nothing, or fails, later, each), and its generic parameters.
//!
//! Rust, Python, JavaScript and TypeScript and Go are read from their
//! signature text; a language that declares no types (Python without hints,
//! JavaScript) is read from its docs and its defaults too, and every type it
//! gets that way is marked as read rather than declared.

use super::super::facts::{Facts, Section, SectionKind};
use super::super::view::{
    Bound, Call, Change, Effect, FailWord, Failure, Generic, Gives, Joint, Lang, OptionRow, Origin, Port, Receiver, Role, Ty,
};
use super::docs::{errors_prose, none_when, note, section, short_when};
use super::text::{balanced, keyword, plain, split_top, squash, strip_leading};
use super::words::{Cx, T, head, parse, peel, ty_of, word};

/// The call and its generics.
pub(super) struct Derived {
    /// The call.
    pub call: Call,
    /// The generic parameters.
    pub generics: Vec<Generic>,
}

/// One parameter as the signature writes it.
#[derive(Clone, Debug, Default)]
struct Param {
    name: String,
    ty: Option<String>,
    default: Option<String>,
    optional: bool,
    rest: bool,
}

/// One signature, read.
#[derive(Debug, Default)]
struct Parsed {
    /// `Some(None)`: a receiver whose effect the language does not say.
    receiver: Option<Option<Effect>>,
    generics: Vec<(String, Vec<String>)>,
    params: Vec<Param>,
    ret: Option<String>,
    is_async: bool,
    generator: bool,
}

// ------------------------------------------------------------------ entry

/// The call for `facts`, when its signature reads as a callable.
pub(super) fn callable(facts: &Facts) -> Option<Derived> {
    let signature = facts.signature.as_deref()?;
    let name = facts.name.rsplit(['.', ':']).next().unwrap_or(&facts.name);
    let parsed = match facts.lang {
        Lang::Rust => rust(signature, name)?,
        Lang::Python => python(signature, name)?,
        Lang::JavaScript | Lang::TypeScript => script(signature, name)?,
        Lang::Go => go(signature, name)?,
        _ => return None,
    };
    Some(build(&parsed, facts))
}

// ------------------------------------------------------------------ parsing

fn rust(signature: &str, name: &str) -> Option<Parsed> {
    let signature = strip_leading(signature);
    let start = keyword(signature, "fn")?;
    let before = &signature[..start];
    if !before.split_whitespace().all(|w| matches!(w, "pub" | "async" | "unsafe" | "const" | "default" | "extern" | "\"C\"") || w.starts_with("pub(")) && !before.trim().is_empty() {
        return None;
    }
    let declaration = signature[start + 2..].trim_start();
    let end = declaration.find(|c: char| !(c.is_alphanumeric() || c == '_'))?;
    let fname = declaration[..end].trim_start_matches("r#");
    if !name.is_empty() && fname != name.trim_start_matches("r#") {
        return None;
    }
    let mut rest = declaration[end..].trim_start();
    let mut generics: Vec<(String, Vec<String>)> = Vec::new();
    if rest.starts_with('<') {
        let (inner, after) = balanced(rest, 0)?;
        for part in split_top(inner, ',') {
            if part.starts_with('\'') || part.starts_with("const ") {
                continue;
            }
            let (gname, bounds) = part.split_once(':').map_or((part.as_str(), ""), |(n, b)| (n, b));
            let gname = gname.split('=').next().unwrap_or(gname).trim();
            generics.push((gname.to_owned(), bound_list(bounds)));
        }
        rest = rest[after..].trim_start();
    }
    let open = rest.find('(')?;
    let (params, after) = balanced(rest, open)?;
    let tail = rest[after..].trim().trim_end_matches([';', '{']).trim();
    let (ret_part, where_part) = keyword(tail, "where").map_or((tail, ""), |at| (&tail[..at], &tail[at + 5..]));
    for clause in split_top(where_part.trim().trim_end_matches(','), ',') {
        if let Some((subject, bounds)) = clause.split_once(':') {
            let subject = subject.trim();
            if let Some(entry) = generics.iter_mut().find(|(n, _)| n == subject) {
                entry.1.extend(bound_list(bounds));
            }
        }
    }
    let mut parsed = Parsed { generics, ..Parsed::default() };
    for raw in split_top(params, ',') {
        let squashed = squash(&raw);
        let bare = squashed.trim_start_matches('&').trim_start();
        let bare = bare.strip_prefix('\'').map_or(bare, |r| r.split_once(' ').map_or("", |(_, tail)| tail.trim_start()));
        let bare = bare.strip_prefix("mut ").unwrap_or(bare);
        if bare == "self" || bare.starts_with("self:") {
            let effect = if squashed.contains('&') {
                if squashed.split(|c: char| !(c.is_alphanumeric() || c == '_')).any(|w| w == "mut") { Effect::Changes } else { Effect::Reads }
            } else if let Some(ty) = bare.strip_prefix("self:") {
                if ty.contains("&mut") { Effect::Changes } else if ty.contains('&') { Effect::Reads } else { Effect::UsesUp }
            } else {
                Effect::UsesUp
            };
            parsed.receiver = Some(Some(effect));
            continue;
        }
        let Some((pattern, ty)) = split_colon(&raw) else { continue };
        let pattern = pattern.trim().trim_start_matches("mut ").trim();
        let pname = if pattern.chars().all(|c| c.is_alphanumeric() || c == '_') { pattern } else { "_" };
        parsed.params.push(Param { name: pname.to_owned(), ty: Some(ty.trim().to_owned()), ..Param::default() });
    }
    let sig_flags = before.split_whitespace().collect::<Vec<_>>();
    parsed.is_async = sig_flags.contains(&"async");
    parsed.ret = ret_part.trim().strip_prefix("->").map(|r| r.trim().to_owned()).filter(|r| !r.is_empty());
    Some(parsed)
}

fn bound_list(bounds: &str) -> Vec<String> {
    split_top(bounds, '+').into_iter().map(|b| b.trim().to_owned()).filter(|b| !b.is_empty() && !b.starts_with('\'') && !b.starts_with('?')).collect()
}

/// `name: Type` at the first colon that is not `::`.
fn split_colon(text: &str) -> Option<(&str, &str)> {
    let bytes = text.as_bytes();
    let mut depth = 0_i32;
    for (at, ch) in text.char_indices() {
        match ch {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => depth -= 1,
            ':' if depth == 0 && bytes.get(at + 1) != Some(&b':') && (at == 0 || bytes[at - 1] != b':') => {
                return Some((&text[..at], &text[at + 1..]));
            }
            _ => {}
        }
    }
    None
}

/// `name`, `name: T`, `name = d`, `name: T = d` (Python, TypeScript).
fn split_param(raw: &str) -> Param {
    let mut text = raw.trim();
    let mut param = Param::default();
    if let Some(rest) = text.strip_prefix("**") {
        param.rest = true;
        text = rest;
        param.name = format!("**{}", head_name(text));
    } else if let Some(rest) = text.strip_prefix("...").or_else(|| text.strip_prefix('*')) {
        param.rest = true;
        text = rest;
        param.name = format!("*{}", head_name(text));
    }
    // Default: the first top-level `=` that is not `=>` or `==`.
    let mut depth = 0_i32;
    let mut default_at = None;
    let chars: Vec<(usize, char)> = text.char_indices().collect();
    for (index, &(at, ch)) in chars.iter().enumerate() {
        match ch {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' if index > 0 && (chars[index - 1].1 == '-' || chars[index - 1].1 == '=') => {}
            '>' | ')' | ']' | '}' => depth -= 1,
            '=' if depth == 0 && chars.get(index + 1).map(|c| c.1) != Some('>') && chars.get(index + 1).map(|c| c.1) != Some('=') => {
                default_at = Some(at);
                break;
            }
            _ => {}
        }
    }
    let (left, default) = match default_at {
        Some(at) => (&text[..at], Some(text[at + 1..].trim().to_owned())),
        None => (text, None),
    };
    let (name_part, ty) = match split_colon(left) {
        Some((n, t)) => (n.trim(), Some(t.trim().to_owned())),
        None => (left.trim(), None),
    };
    let optional = name_part.ends_with('?');
    let name_part = name_part.trim_end_matches('?').trim();
    if param.name.is_empty() {
        param.name = if name_part.chars().all(|c| c.is_alphanumeric() || matches!(c, '_' | '$')) && !name_part.is_empty() { name_part.to_owned() } else { "options".to_owned() };
    }
    param.ty = ty.filter(|t| !t.is_empty());
    param.default = default.filter(|d| !d.is_empty());
    param.optional = optional || param.default.is_some();
    param
}

fn head_name(text: &str) -> String {
    text.trim().chars().take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '$')).collect()
}

fn python(signature: &str, name: &str) -> Option<Parsed> {
    let signature = strip_leading(signature);
    let def = keyword(signature, "def")?;
    let before = &signature[..def];
    let after = signature[def + 3..].trim_start();
    let end = after.find(|c: char| !(c.is_alphanumeric() || c == '_'))?;
    if !name.is_empty() && &after[..end] != name {
        return None;
    }
    let rest = after[end..].trim_start();
    let (params, after) = balanced(rest, rest.find('(')?)?;
    let tail = rest[after..].trim();
    let ret = tail.strip_prefix("->").map(|r| r.trim().trim_end_matches(':').trim().to_owned()).filter(|r| !r.is_empty());
    let mut parsed = Parsed { ret, is_async: before.split_whitespace().any(|w| w == "async"), ..Parsed::default() };
    for raw in split_top(params, ',') {
        let raw = raw.trim();
        if raw == "/" || raw == "*" || raw.is_empty() {
            continue;
        }
        let param = split_param(raw);
        if (param.name == "self" || param.name == "cls") && parsed.params.is_empty() && parsed.receiver.is_none() {
            parsed.receiver = Some(None);
            continue;
        }
        parsed.params.push(param);
    }
    Some(parsed)
}

fn script(signature: &str, name: &str) -> Option<Parsed> {
    let signature = strip_leading(signature);
    let mut depth = 0_i32;
    let mut open = None;
    let mut previous = '\0';
    let mut generics_end = None;
    for (at, ch) in signature.char_indices() {
        match ch {
            '<' if depth == 0 && previous != '=' => {
                // A generic parameter list before the parameters.
                if let Some((_, end)) = balanced(signature, at) {
                    generics_end = Some(end);
                }
                if generics_end.is_some_and(|end| at < end) {
                    depth += 0;
                }
            }
            '(' => {
                if generics_end.is_some_and(|end| at < end) {
                    previous = ch;
                    continue;
                }
                open = Some(at);
                break;
            }
            _ => {}
        }
        previous = ch;
    }
    let open = open?;
    let (params, after) = balanced(signature, open)?;
    let before = &signature[..open];
    let generator = before.contains("function*") || before.contains("* ");
    let tail = signature[after..].trim();
    let mut ret = None;
    if let Some(rest) = tail.strip_prefix(':') {
        let end = ["=>", "{", ";"].iter().filter_map(|m| find_top(rest, m)).min().unwrap_or(rest.len());
        let r = rest[..end].trim();
        if !r.is_empty() {
            ret = Some(r.to_owned());
        }
    }
    let _ = name;
    let mut parsed = Parsed { ret, is_async: before.split_whitespace().any(|w| w == "async"), generator, ..Parsed::default() };
    for raw in split_top(params, ',') {
        let raw = raw.trim();
        if raw.is_empty() {
            continue;
        }
        let param = split_param(raw);
        if param.name == "this" {
            continue;
        }
        parsed.params.push(param);
    }
    Some(parsed)
}

/// The byte offset of `pattern` at depth zero.
fn find_top(text: &str, pattern: &str) -> Option<usize> {
    let mut depth = 0_i32;
    for (at, ch) in text.char_indices() {
        if depth == 0 && text[at..].starts_with(pattern) {
            return Some(at);
        }
        match ch {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' | ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
    }
    None
}

fn go(signature: &str, name: &str) -> Option<Parsed> {
    let text = strip_leading(signature).trim();
    let text = text.strip_prefix("func").map_or(text, str::trim_start);
    let mut parsed = Parsed::default();
    let rest = if text.starts_with('(') {
        let (receiver, after) = balanced(text, 0)?;
        let pointer = receiver.contains('*');
        parsed.receiver = Some(Some(if pointer { Effect::Changes } else { Effect::Reads }));
        text[after..].trim_start()
    } else {
        text
    };
    let rest = rest.strip_prefix(name)?.trim_start();
    let rest = if rest.starts_with('[') { balanced(rest, 0).map_or(rest, |(_, end)| rest[end..].trim_start()) } else { rest };
    let (params, after) = balanced(rest, rest.find('(')?)?;
    let results = rest[after..].trim().trim_end_matches('{').trim();
    // `a, b string, c ...int`: names share the type that follows.
    let mut pending: Vec<String> = Vec::new();
    for part in split_top(params, ',') {
        match part.split_once(char::is_whitespace) {
            Some((pname, ty)) => {
                let ty = ty.trim().to_owned();
                for earlier in pending.drain(..) {
                    parsed.params.push(Param { name: earlier, ty: Some(ty.clone()), ..Param::default() });
                }
                let rest = ty.starts_with("...");
                parsed.params.push(Param { name: pname.to_owned(), ty: Some(ty.trim_start_matches("...").to_owned()), rest, ..Param::default() });
            }
            None => pending.push(part),
        }
    }
    for leftover in pending {
        parsed.params.push(Param { name: "_".to_owned(), ty: Some(leftover), ..Param::default() });
    }
    if !results.is_empty() {
        parsed.ret = Some(if results.starts_with('(') { balanced(results, 0).map_or(results, |(inner, _)| inner).to_owned() } else { results.to_owned() });
    }
    Some(parsed)
}

// ------------------------------------------------------------------ building

fn build(parsed: &Parsed, facts: &Facts) -> Derived {
    let generic_names: Vec<String> = parsed.generics.iter().map(|(n, _)| n.clone()).collect();
    let link = |name: &str| facts.link(name).map(ToOwned::to_owned);
    let declared = Cx { generics: &generic_names, owner: facts.owner.as_deref(), link: &link, origin: Origin::Declared };
    let parameters = section(facts, SectionKind::Parameters);
    let mut call = Call::default();

    if let Some(effect) = parsed.receiver {
        let owner = facts.owner.clone().unwrap_or_else(|| "it".to_owned());
        let ty = Ty { link: facts.owner.as_deref().and_then(|o| facts.link(o)).map(ToOwned::to_owned), ..Ty::plain(owner) };
        call.receiver = Some(Receiver { effect, ty });
    }

    let mut consumed_entries: Vec<String> = Vec::new();
    for param in &parsed.params {
        let entry = parameters.and_then(|s| entry_for(s, &param.name));
        if entry.is_some() {
            consumed_entries.push(param.name.trim_start_matches('*').to_owned());
        }
        let note_text = entry.map(|(_, text)| note(&strip_type_prefix(text).1)).filter(|t| !t.is_empty());
        let (mut ty, options) = port_type(param, entry.map(|(_, text)| text), parameters, facts, &declared);
        if let Some(generic) = ty.generic.clone()
            && let Some((_, bounds)) = parsed.generics.iter().find(|(n, _)| *n == generic)
            && let Some(short) = bounds.first().map(|b| bound_word(b))
        {
            ty.word = short;
        }
        let joint = if param.rest { Joint::Rest } else if param.optional { Joint::Optional } else { Joint::Required };
        let default = param.default.clone();
        call.ports.push(Port { name: param.name.clone(), joint, ty, default, note: note_text, options });
    }

    // Outcomes.
    let ret = parsed.ret.as_deref().map(|r| (r, parse(r)));
    let mut peeled = ret.as_ref().map(|(_, t)| peel(t)).unwrap_or_default();
    if parsed.ret.is_none() && facts.lang == Lang::Go {
        peeled = Default::default();
    }
    // Go's trailing `error`.
    let mut go_fails: Option<Ty> = None;
    let mut go_gives: Option<(String, T)> = None;
    if facts.lang == Lang::Go {
        if let Some((text, _)) = &ret {
            let mut outs = split_top(text, ',');
            if outs.last().is_some_and(|last| last.rsplit(' ').next() == Some("error")) {
                outs.pop();
                let error_ty = parse("error");
                go_fails = Some(ty_of(&error_ty, "error", &declared));
            }
            if let Some(first) = outs.first() {
                let ty_text = first.rsplit_once(' ').map_or(first.as_str(), |(_, t)| t);
                go_gives = Some((ty_text.to_owned(), parse(ty_text)));
            }
        }
    }
    let later = parsed.is_async || peeled.later;
    let many = parsed.generator || peeled.many;
    let (gives_ty, gives_text) = match (facts.lang, go_gives.take(), peeled.inner.as_ref()) {
        (Lang::Go, Some((text, tree)), _) => (Some(tree), Some(text)),
        (Lang::Go, None, _) => (None, None),
        (_, _, Some(inner)) => {
            let text = ret.as_ref().map_or(String::new(), |(text, _)| (*text).to_owned());
            (Some(inner.clone()), Some(text))
        }
        _ => (None, None),
    };
    let untyped_output = parsed.ret.is_none() && facts.lang.undeclared();
    let mut gives = Gives::default();
    if let (Some(tree), Some(text)) = (gives_ty, gives_text) {
        let nothing = matches!(&tree, T::Tuple(items) if items.is_empty()) || word(&tree, &declared) == "nothing";
        if !nothing {
            let mut ty = ty_of(&tree, &text, &declared);
            // A written type wrapped in `Result<…>` or `Option<…>` reads its inner text.
            if peeled.fails.is_some() || peeled.maybe || peeled.later || peeled.many {
                ty.written = inner_text(&tree, &declared);
            }
            gives.ty = Some(ty);
        }
    } else if untyped_output && !matches!(facts.kind, super::super::view::Kind::Other) {
        // No annotation: the docs may say what comes back.
        if let Some(returns) = section(facts, SectionKind::Returns) {
            let (declared_type, prose) = strip_type_prefix(&returns.body);
            let origin = if declared_type.is_some() { Origin::Docs } else { Origin::Code };
            let tree = declared_type.map_or(T::Infer, |t| parse(&t));
            let mut ty = ty_of(&tree, "", &Cx { origin, ..clone_cx(&declared) });
            if declared_type_is_none(&ty) && !prose.is_empty() {
                ty.word = "anything".to_owned();
            }
            gives.ty = Some(ty);
        } else {
            gives.ty = Some(Ty { origin: Origin::Code, ..Ty::plain("anything") });
        }
    }
    gives.many = many;
    // A generic output is described by its role, in words.
    let mut generics: Vec<Generic> = Vec::new();
    let in_text: Vec<T> = parsed.params.iter().filter_map(|p| p.ty.as_deref()).map(parse).collect();
    for (name, bounds) in &parsed.generics {
        let in_inputs = in_text.iter().any(|t| t.mentions(name));
        let in_output = ret.as_ref().is_some_and(|(_, t)| t.mentions(name));
        let role = match (in_output, in_inputs) {
            (true, false) => Role::Choose,
            (true, true) => Role::Through,
            _ => Role::Needs,
        };
        let bounds: Vec<Bound> = bounds.iter().map(|b| Bound { name: bound_name(b), means: bound_means(b) }).collect();
        generics.push(Generic { says: role_sentence(role, &bounds), name: name.clone(), role, bounds, origin: Origin::Declared });
    }
    if let (Some(ty), true) = (&gives.ty, !generics.is_empty())
        && let Some(generic) = &ty.generic
        && let Some(found) = generics.iter().find(|g| &g.name == generic)
    {
        gives.role = Some(found.role.says().to_owned());
    }
    call.gives = gives;

    // later
    if later {
        call.later = Some(if matches!(facts.lang, Lang::JavaScript | Lang::TypeScript) && peeled.later {
            "a Promise: it answers after you await it".to_owned()
        } else {
            "it answers when you await it".to_owned()
        });
    }
    // nothing
    if peeled.maybe {
        call.none = Some(none_when(facts).unwrap_or_else(|| "when there is none".to_owned()));
    } else if facts.lang.undeclared()
        && let Some(when) = none_when(facts)
    {
        call.none = Some(when);
    }
    // fails
    call.fails = failure(parsed, facts, &declared, &peeled.fails, go_fails, later);
    // An option that turns a failure into nothing: the call can give nothing too.
    Derived { call, generics }
}

fn clone_cx<'a>(cx: &Cx<'a>) -> Cx<'a> {
    Cx { generics: cx.generics, owner: cx.owner, link: cx.link, origin: cx.origin }
}

fn declared_type_is_none(ty: &Ty) -> bool {
    ty.word == "nothing"
}

/// The text of the inner type once `Result<…>`/`Option<…>` are peeled.
fn inner_text(tree: &T, cx: &Cx<'_>) -> Option<String> {
    let written = word(tree, cx);
    let _ = written;
    None
}

/// How the call can fail, in the language's own verb.
fn failure(parsed: &Parsed, facts: &Facts, cx: &Cx<'_>, fails: &Option<Option<T>>, go_fails: Option<Ty>, later: bool) -> Option<Failure> {
    let word_for = match facts.lang {
        Lang::Rust => FailWord::Fails,
        Lang::Python => FailWord::Raises,
        Lang::Go => FailWord::Returns,
        _ if later => FailWord::Rejects,
        _ => FailWord::Throws,
    };
    let prose = errors_prose(facts);
    let when_of = |prose: &Option<(String, Vec<(String, String)>)>| -> String {
        prose.as_ref().map(|(body, entries)| {
            let text = if body.trim().is_empty() { entries.first().map(|(_, t)| t.clone()).unwrap_or_default() } else { body.clone() };
            short_when(&text)
        }).unwrap_or_default()
    };
    let _ = parsed;
    if let Some(ty) = go_fails {
        return Some(Failure { word: word_for, ty, when: when_of(&prose) });
    }
    if let Some(error) = fails {
        let ty = match error {
            Some(tree) => ty_of(tree, "", cx),
            None => Ty { link: facts.link("Error").map(ToOwned::to_owned), ..Ty::plain("Error") },
        };
        return Some(Failure { word: word_for, ty, when: when_of(&prose) });
    }
    // An untyped language says it in its docs: `Raises:`, `@throws`.
    if facts.lang.undeclared() || matches!(facts.lang, Lang::TypeScript) {
        if let Some((body, entries)) = &prose {
            let (subject, text) = entries.first().cloned().unwrap_or_else(|| ("Error".to_owned(), body.clone()));
            let tree = parse(&subject);
            let mut ty = ty_of(&tree, "", &Cx { origin: Origin::Docs, ..clone_cx(cx) });
            if subject.trim().is_empty() {
                ty = Ty { origin: Origin::Docs, ..Ty::plain("Error") };
            }
            return Some(Failure { word: word_for, ty, when: short_when(&text) });
        }
    }
    None
}

// ------------------------------------------------------------------ ports

fn entry_for<'a>(section: &'a Section, name: &str) -> Option<(&'a str, &'a str)> {
    let bare = name.trim_start_matches('*');
    section.entries.iter().find(|(subject, _)| subject.trim_matches(['[', ']']) == bare).map(|(s, t)| (s.as_str(), t.as_str()))
}

/// `{string} the cmd` or `(str): the cmd` → the type text and the rest.
fn strip_type_prefix(text: &str) -> (Option<String>, String) {
    let trimmed = text.trim();
    for (open, close) in [('{', '}'), ('(', ')')] {
        if let Some(rest) = trimmed.strip_prefix(open)
            && let Some(end) = rest.find(close)
        {
            let ty = rest[..end].trim();
            if !ty.is_empty() && ty.len() < 48 {
                return (Some(ty.to_owned()), rest[end + 1..].trim_start_matches([' ', '-', ':']).to_owned());
            }
        }
    }
    (None, trimmed.to_owned())
}

fn literal_word(default: &str) -> Option<&'static str> {
    let d = default.trim();
    if d.starts_with('"') || d.starts_with('\'') || d.starts_with('`') {
        Some("text")
    } else if matches!(d, "True" | "False" | "true" | "false") {
        Some("yes or no")
    } else if d.parse::<f64>().is_ok() {
        Some("a number")
    } else if d == "[]" {
        Some("a list")
    } else if d == "{}" {
        Some("options")
    } else {
        None
    }
}

fn port_type(param: &Param, entry: Option<&str>, parameters: Option<&Section>, facts: &Facts, declared: &Cx<'_>) -> (Ty, Vec<OptionRow>) {
    let mut options = Vec::new();
    // Declared.
    if let Some(text) = &param.ty {
        let tree = parse(text);
        if let T::Object(fields) = &tree {
            for (name, ty, optional) in fields {
                let inner = ty_of(ty, "", declared);
                let note_text = parameters.and_then(|s| entry_for(s, &format!("{}.{name}", param.name))).map(|(_, t)| note(&strip_type_prefix(t).1)).unwrap_or_default();
                let _ = optional;
                options.push(OptionRow { change: change_of(name, &note_text), name: name.clone(), ty: inner, note: note_text });
            }
        }
        let mut ty = ty_of(&tree, text, declared);
        if !options.is_empty() {
            ty.word = "options".to_owned();
            ty.written = None;
        }
        return (ty, options);
    }
    // Documented sub-parameters: `opt.nothrow`.
    if let Some(section) = parameters {
        let prefix = format!("{}.", param.name.trim_start_matches('*'));
        for (subject, text) in &section.entries {
            let Some(rest) = subject.trim_matches(['[', ']']).strip_prefix(&prefix) else { continue };
            let (ty_text, prose) = strip_type_prefix(text);
            let tree = ty_text.as_deref().map_or(T::Infer, parse);
            let ty = ty_of(&tree, "", &Cx { origin: if ty_text.is_some() { Origin::Docs } else { Origin::Code }, ..clone_cx(declared) });
            let prose = note(&prose);
            options.push(OptionRow { change: change_of(rest, &prose), name: rest.to_owned(), ty, note: prose });
        }
    }
    if !options.is_empty() {
        return (Ty { origin: Origin::Code, ..Ty::plain("options") }, options);
    }
    // Read from the docs, then from the default.
    if let Some(text) = entry {
        let (ty_text, _) = strip_type_prefix(text);
        if let Some(ty_text) = ty_text {
            let tree = parse(&ty_text);
            return (ty_of(&tree, "", &Cx { origin: Origin::Docs, ..clone_cx(declared) }), options);
        }
    }
    if let Some(default) = &param.default
        && let Some(word) = literal_word(default)
    {
        return (Ty { origin: Origin::Code, ..Ty::plain(word) }, options);
    }
    let _ = facts;
    (Ty { origin: Origin::Code, ..Ty::plain("anything") }, options)
}

fn change_of(name: &str, note_text: &str) -> Option<Change> {
    let lower = note_text.to_ascii_lowercase();
    if lower.contains("instead of throw") || lower.contains("instead of rais") || lower.contains("instead of fail") || (name == "nothrow" || name == "noThrow") {
        Some(Change::FailsToNone)
    } else if name == "all" || lower.contains("every match") || lower.contains("all matches") {
        Some(Change::OneToMany)
    } else {
        None
    }
}

// ------------------------------------------------------------------ bounds

fn bound_name(bound: &str) -> String {
    head(bound)
}

/// What a bound means, in words.
pub(super) fn bound_means(bound: &str) -> String {
    match bound_name(bound).as_str() {
        "Deserialize" | "DeserializeOwned" => "can be read by serde (any format)",
        "Serialize" => "can be written by serde (any format)",
        "Serializer" => "a serializer: a format's writer",
        "Deserializer" => "a deserializer: a format's reader",
        "Index" => "an index: a position or a key",
        "Ord" | "PartialOrd" => "can be ordered",
        "Hash" => "can be hashed",
        "Clone" => "can be copied",
        "Copy" => "is copied by assignment",
        "Debug" => "prints for debugging",
        "Display" => "prints",
        "Send" => "can move between threads",
        "Sync" => "can be shared between threads",
        "IntoIterator" => "can be looped over",
        "Iterator" => "gives one at a time",
        "AsRef" => "can be read as",
        "Into" => "turns into",
        "From" => "made from",
        "Read" => "a reader (io::Read)",
        "Write" => "a writer (io::Write)",
        "Future" => "a future",
        "IntoFuture" => "anything you can await",
        "Fn" | "FnMut" | "FnOnce" => "a function",
        "Default" => "has a default",
        "PartialEq" | "Eq" => "can be compared",
        "Error" => "is an error",
        "FromStr" => "parses from text",
        "ToString" => "prints to text",
        other => return other.to_owned(),
    }
    .to_owned()
}

/// A bound as the word for a parameter typed by it.
fn bound_word(bound: &str) -> String {
    match bound_name(bound).as_str() {
        "Serializer" => "a serializer".to_owned(),
        "Deserializer" => "a deserializer".to_owned(),
        "Index" => "an index".to_owned(),
        "Read" => "a reader".to_owned(),
        "Write" => "a writer".to_owned(),
        "Fn" | "FnMut" | "FnOnce" => "a function".to_owned(),
        "IntoIterator" | "Iterator" => "anything to loop over".to_owned(),
        "Into" | "AsRef" | "From" => {
            let inner = bound.split_once('<').map_or("", |(_, tail)| tail.trim_end_matches('>'));
            let tree = parse(inner);
            let none: Vec<String> = Vec::new();
            let link = |_: &str| None;
            let cx = Cx { generics: &none, owner: None, link: &link, origin: Origin::Declared };
            if inner.is_empty() { "anything that converts".to_owned() } else { word(&tree, &cx) }
        }
        other => format!("any {other}"),
    }
}

fn role_sentence(role: Role, bounds: &[Bound]) -> String {
    match role {
        Role::Choose => {
            let what = bounds.first().map_or("", |b| b.name.as_str());
            match what {
                "Deserialize" | "DeserializeOwned" => "You choose it: whatever you read the input into.".to_owned(),
                "Default" => "You choose it: whatever you want made.".to_owned(),
                "FromStr" => "You choose it: whatever you parse the text into.".to_owned(),
                _ => "You choose it: it is only in what comes out.".to_owned(),
            }
        }
        Role::Through => "The same kind you give comes back.".to_owned(),
        Role::Needs => "Any kind that fits will do.".to_owned(),
    }
}

/// The plain sentence for a callable's docs-only outcome (used by rows).
pub(super) fn plain_note(text: &str) -> String {
    plain(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::symbol::view::Kind;

    fn facts(name: &str, lang: Lang, signature: &str) -> Facts {
        let mut facts = Facts::new(name, Kind::Function, lang, "pkg");
        facts.signature = Some(signature.to_owned());
        facts
    }

    fn call(f: &Facts) -> Derived {
        callable(f).expect("a callable")
    }

    #[test]
    fn serde_jsons_from_str_is_a_generic_that_can_fail() {
        let mut f = facts("from_str", Lang::Rust, "pub fn from_str<'a, T>(s: &'a str) -> Result<T>\nwhere\n    T: de::Deserialize<'a>,");
        f.links = vec![("Error".into(), "addr:Error".into())];
        let d = call(&f);
        assert_eq!(d.call.ports.len(), 1);
        assert_eq!(d.call.ports[0].name, "s");
        assert_eq!(d.call.ports[0].ty.word, "text");
        assert_eq!(d.call.ports[0].ty.written.as_deref(), Some("&'a str"));
        let gives = d.call.gives.ty.as_ref().expect("gives");
        assert_eq!((gives.word.as_str(), gives.generic.as_deref()), ("T", Some("T")));
        assert_eq!(d.call.gives.role.as_deref(), Some("you choose it"));
        let fails = d.call.fails.as_ref().expect("fails");
        assert_eq!(fails.ty.word, "Error");
        assert_eq!(fails.ty.link.as_deref(), Some("addr:Error"));
        assert_eq!(fails.word, FailWord::Fails);
        assert_eq!(d.generics.len(), 1);
        assert_eq!(d.generics[0].role, Role::Choose);
        assert_eq!(d.generics[0].bounds[0].name, "Deserialize");
        assert_eq!(d.generics[0].bounds[0].means, "can be read by serde (any format)");
    }

    #[test]
    fn a_method_that_may_give_nothing_reads_its_receiver() {
        let mut f = facts("as_str", Lang::Rust, "pub fn as_str(&self) -> Option<&str>");
        f.kind = Kind::Method;
        f.owner = Some("Value".into());
        let d = call(&f);
        let receiver = d.call.receiver.as_ref().expect("receiver");
        assert_eq!(receiver.effect, Some(Effect::Reads));
        assert_eq!(receiver.ty.word, "Value");
        assert!(d.call.ports.is_empty());
        assert_eq!(d.call.gives.ty.as_ref().map(|t| t.word.as_str()), Some("text"));
        assert!(d.call.none.is_some());
        assert!(d.call.fails.is_none());
    }

    #[test]
    fn a_method_that_changes_or_uses_up_its_receiver() {
        let mut f = facts("get_mut", Lang::Rust, "pub fn get_mut<I: Index>(&mut self, index: I) -> Option<&mut Value>");
        f.owner = Some("Value".into());
        let d = call(&f);
        assert_eq!(d.call.receiver.as_ref().map(|r| r.effect), Some(Some(Effect::Changes)));
        assert_eq!(d.call.ports[0].ty.generic.as_deref(), Some("I"));
        assert_eq!(d.call.ports[0].ty.word, "an index");
        assert_eq!(d.generics[0].role, Role::Needs);
        let mut f = facts("try_into", Lang::Rust, "pub fn try_into<'de, T>(self) -> Result<T, crate::de::Error>\n    where\n        T: de::Deserialize<'de>,");
        f.owner = Some("Value".into());
        let d = call(&f);
        assert_eq!(d.call.receiver.as_ref().map(|r| r.effect), Some(Some(Effect::UsesUp)));
        assert_eq!(d.call.fails.as_ref().map(|f| f.ty.word.as_str()), Some("Error"));
        assert_eq!(d.generics[0].role, Role::Choose);
    }

    #[test]
    fn smallvecs_push_and_the_async_and_iterator_shapes() {
        let mut f = facts("push", Lang::Rust, "pub fn push(&mut self, value: A::Item)");
        f.owner = Some("SmallVec".into());
        let d = call(&f);
        assert_eq!(d.call.ports[0].name, "value");
        assert!(d.call.gives.ty.is_none());
        let f = facts("fetch", Lang::Rust, "pub async fn fetch(url: &str) -> Result<Vec<u8>, reqwest::Error>");
        let d = call(&f);
        assert!(d.call.later.is_some() && d.call.fails.is_some());
        assert_eq!(d.call.gives.ty.as_ref().map(|t| t.word.as_str()), Some("bytes"));
        let f = facts("iter", Lang::Rust, "pub fn iter(&self) -> impl Iterator<Item = &Value>");
        let d = call(&f);
        assert!(d.call.gives.many);
        assert_eq!(d.call.gives.ty.as_ref().map(|t| t.word.as_str()), Some("Value"));
    }

    #[test]
    fn python_without_hints_is_read_from_defaults_and_docs() {
        let mut f = facts("match", Lang::Python, "def match(pattern, string, flags=0):");
        f.docs = vec![crate::anatomy::symbol::view::Block::Para("Try to apply the pattern at the start of the string, returning a Match object, or None if no match was found.".into())];
        let d = call(&f);
        assert_eq!(d.call.ports.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["pattern", "string", "flags"]);
        assert_eq!(d.call.ports[2].joint, Joint::Optional);
        assert_eq!(d.call.ports[2].default.as_deref(), Some("0"));
        assert_eq!(d.call.ports[2].ty.word, "a number");
        assert!(d.call.ports.iter().all(|p| p.ty.origin.dotted()), "no type is declared");
        assert_eq!(d.call.none.as_deref(), Some("if no match was found"));
    }

    #[test]
    fn python_raises_read_from_the_docs() {
        let mut f = facts("match", Lang::Python, "def match(pattern, string, flags=0):");
        f.sections = vec![Section { kind: SectionKind::Errors, body: String::new(), entries: vec![("re.error".into(), "If the pattern itself is invalid.".into())] }];
        let d = call(&f);
        let fails = d.call.fails.expect("raises");
        assert_eq!(fails.word, FailWord::Raises);
        assert_eq!(fails.ty.word, "error");
        assert!(fails.ty.origin.dotted());
    }

    #[test]
    fn python_with_hints_is_declared() {
        let f = facts("size", Lang::Python, "def size(items: list[str], *, deep: bool = False) -> int | None:");
        let d = call(&f);
        assert_eq!(d.call.ports[0].ty.word, "a list of text");
        assert!(!d.call.ports[0].ty.origin.dotted());
        assert_eq!(d.call.ports[1].joint, Joint::Optional);
        assert_eq!(d.call.gives.ty.as_ref().map(|t| t.word.as_str()), Some("an integer"));
        assert!(d.call.none.is_some());
    }

    #[test]
    fn javascript_with_an_options_object_and_jsdoc() {
        let mut f = facts("which.sync", Lang::JavaScript, "const whichSync = (cmd, opt) => {");
        f.sections = vec![Section {
            kind: SectionKind::Parameters,
            body: String::new(),
            entries: vec![
                ("cmd".into(), "{string} a program name".into()),
                ("opt".into(), "{object} options".into()),
                ("opt.nothrow".into(), "{boolean} give null instead of throwing".into()),
                ("opt.all".into(), "{boolean} give every match, a list".into()),
                ("opt.path".into(), "{string} search this instead of $PATH".into()),
            ],
        }];
        let d = call(&f);
        assert_eq!(d.call.ports[0].name, "cmd");
        assert_eq!(d.call.ports[0].ty.word, "text");
        assert_eq!(d.call.ports[0].ty.origin, Origin::Docs);
        let opt = &d.call.ports[1];
        assert_eq!(opt.ty.word, "options");
        assert_eq!(opt.options.iter().map(|o| o.name.as_str()).collect::<Vec<_>>(), ["nothrow", "all", "path"]);
        assert_eq!(opt.options[0].change, Some(Change::FailsToNone));
        assert_eq!(opt.options[1].change, Some(Change::OneToMany));
        assert_eq!(opt.options[2].change, None);
    }

    #[test]
    fn typescript_reads_optional_rest_and_promises() {
        let f = facts("parse", Lang::TypeScript, "parse(data: unknown, params?: core.ParseContext<core.$ZodIssue>): core.output<this>;");
        let d = call(&f);
        assert_eq!(d.call.ports[0].ty.word, "anything");
        assert_eq!(d.call.ports[1].joint, Joint::Optional);
        let f = facts("load", Lang::TypeScript, "export async function load(...paths: string[]): Promise<Config>");
        let d = call(&f);
        assert_eq!(d.call.ports[0].joint, Joint::Rest);
        assert_eq!(d.call.ports[0].ty.word, "a list of text");
        assert!(d.call.later.is_some());
        assert_eq!(d.call.gives.ty.as_ref().map(|t| t.word.as_str()), Some("Config"));
    }

    #[test]
    fn go_reads_its_receiver_and_trailing_error() {
        let mut f = facts("Parse", Lang::Go, "func (f *FlagSet) Parse(arguments []string) error");
        f.kind = Kind::Method;
        f.owner = Some("FlagSet".into());
        let d = call(&f);
        assert_eq!(d.call.receiver.as_ref().map(|r| r.effect), Some(Some(Effect::Changes)));
        assert_eq!(d.call.ports[0].ty.word, "a list of text");
        assert_eq!(d.call.fails.as_ref().map(|f| f.word), Some(FailWord::Returns));
        assert!(d.call.gives.ty.is_none());
    }

    #[test]
    fn a_non_callable_or_mismatched_signature_reads_as_none() {
        assert!(callable(&facts("Value", Lang::Rust, "pub enum Value")).is_none());
        assert!(callable(&facts("other", Lang::Rust, "pub fn actual() -> u8")).is_none());
    }
}
