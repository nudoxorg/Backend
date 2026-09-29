//! The design board's own page models (`Nudox-Design-System/v6/moments/data/page6/<id>.json`)
//! read into [`Facts`] and use sites: the same seven pages the board draws,
//! so a capture sits beside its still, and the classifier's verbs can be
//! compared with the board's tags. The facts come from the board's raw
//! fields (declaration, docs, members, use sites); everything the page says
//! is derived from them by [`derive`](super::derive), never copied from the
//! board's own derivation.

use super::derive::uses::{Reader, Rel, Site, read_all};
use super::facts::{Facts, Member, Owes, Receives, Section, SectionKind, Site as Source};
use super::view::{Block, Do, Kind, Lang, Uses, View};
use super::{compile, with_uses};
use serde_json::Value;
use std::path::PathBuf;

pub(crate) fn board_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../Nudox-Design-System/v6/moments/data/page6")
}

pub(crate) fn text(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or_default().to_owned()
}

fn lang(name: &str) -> Lang {
    Lang::from_name(name)
}

fn kind_of(v: &Value) -> Kind {
    match text(v, "kind").as_str() {
        "function" | "fn" => Kind::Function,
        "method" => Kind::Method,
        "enum" => Kind::Enum,
        "struct" => Kind::Struct,
        "trait" => Kind::Trait,
        _ => Kind::Other,
    }
}

fn receives(how: &str) -> Receives {
    match how {
        "reads" => Receives::Reads,
        "changes" => Receives::Changes,
        "uses up" | "consumes" => Receives::UsesUp,
        _ => Receives::Makes,
    }
}

fn ty_text(v: Option<&Value>) -> String {
    v.and_then(|v| v.get("text")).and_then(Value::as_str).unwrap_or("()").to_owned()
}

/// A method's signature as the source would write it, from the board's
/// derived model (the board keeps no raw text for methods).
fn method_signature(name: &str, sig: &Value) -> String {
    let recv = match sig.get("recv").and_then(|r| r.get("how")).and_then(Value::as_str) {
        Some("reads") => "&self",
        Some("changes") => "&mut self",
        Some("uses up") => "self",
        _ => "",
    };
    let mut params: Vec<String> = Vec::new();
    if !recv.is_empty() {
        params.push(recv.to_owned());
    }
    for input in sig.get("ins").and_then(Value::as_array).into_iter().flatten() {
        params.push(format!("{}: {}", text(input, "name"), ty_text(input.get("type"))));
    }
    let out = sig.get("out");
    let mut ret = ty_text(out.and_then(|o| o.get("type")));
    if out.is_some_and(|o| o.get("none").is_some()) {
        ret = format!("Option<{ret}>");
    }
    if out.is_some_and(|o| o.get("fails").is_some()) {
        ret = format!("Result<{ret}>");
    }
    if ret == "()" {
        format!("pub fn {name}({})", params.join(", "))
    } else {
        format!("pub fn {name}({}) -> {ret}", params.join(", "))
    }
}

fn from_type(word: &str) -> &'static str {
    match word {
        "a number" => "f64",
        "yes or no" => "bool",
        "text" => "&str",
        "a list" | "a list of Value" => "Vec<Value>",
        "a map of text to Value" => "Map<String, Value>",
        _ => "Value",
    }
}

fn blocks(v: &Value) -> Vec<Block> {
    v.get("docs")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|b| {
            let t = text(b, "t");
            match text(b, "k").as_str() {
                "h" => Block::Head(t),
                "li" => Block::Item(t),
                "code" => Block::Code(t),
                _ => Block::Para(t),
            }
        })
        .collect()
}

/// The facts a board page gives, from its raw fields.
pub(crate) fn facts_of(v: &Value) -> Facts {
    let lang = lang(&text(v, "lang"));
    let mut facts = Facts::new(&text(v, "name"), kind_of(v), lang, &text(v, "pkg"));
    facts.version = Some(text(v, "version"));
    facts.path = v.get("path").and_then(Value::as_array).map(|p| p.iter().filter_map(Value::as_str).map(ToOwned::to_owned).collect()).unwrap_or_default();
    facts.signature = Some(text(v, "decl"));
    facts.docs = blocks(v);
    if let Some(source) = v.get("source") {
        facts.site = Some(Source { file: text(source, "file"), line: u32::try_from(source.get("line").and_then(Value::as_u64).unwrap_or(1)).unwrap_or(1), open: Some(format!("/board/{}", text(source, "file"))) });
    }
    if facts.kind == Kind::Method {
        facts.owner = facts.path.last().cloned();
    }
    facts.links = vec![("Error".to_owned(), "addr:Error".to_owned()), ("Value".to_owned(), "addr:Value".to_owned())];
    if let Some(shape) = v.get("shape") {
        for case in shape.get("cases").and_then(Value::as_array).into_iter().flatten() {
            let ty = case.get("type").filter(|t| !t.is_null());
            let signature = match ty {
                Some(t) => format!("{}({})", text(case, "name"), ty_text(Some(t))),
                None => text(case, "name"),
            };
            facts.made_of.push(Member { name: text(case, "name"), signature: Some(signature), summary: Some(text(case, "doc")), more: Some(text(case, "more")).filter(|m| !m.is_empty()), link: Some(format!("addr:{}", text(case, "name"))), ..Member::default() });
        }
        for field in shape.get("fields").and_then(Value::as_array).into_iter().flatten() {
            facts.made_of.push(Member { name: text(field, "name"), signature: Some(format!("pub {}: {}", text(field, "name"), ty_text(field.get("type")))), summary: Some(text(field, "doc")), more: Some(text(field, "more")).filter(|m| !m.is_empty()), ..Member::default() });
        }
    }
    for group in v.get("groups").and_then(Value::as_array).into_iter().flatten() {
        for item in group.get("items").and_then(Value::as_array).into_iter().flatten() {
            let name = text(item, "name");
            if let Some(froms) = item.get("froms").and_then(Value::as_array) {
                for word in froms.iter().filter_map(Value::as_str) {
                    facts.does.push(Member { name: "from".to_owned(), signature: Some(format!("fn from(val: {}) -> Value", from_type(word))), receives: Receives::Makes, ..Member::default() });
                }
                continue;
            }
            if name == "parse" {
                facts.does.push(Member { name: "from_str".to_owned(), signature: Some("fn from_str(s: &str) -> Result<Value, Self::Err>".to_owned()), receives: Receives::Makes, ..Member::default() });
                continue;
            }
            let Some(sig) = item.get("sig") else {
                facts.does.push(Member { name, summary: Some(text(item, "doc")), receives: Receives::Makes, signature: Some("fn default() -> Self".to_owned()), ..Member::default() });
                continue;
            };
            let how = sig.get("recv").and_then(|r| r.get("how")).and_then(Value::as_str).unwrap_or("");
            facts.does.push(Member { signature: Some(method_signature(&name, sig)), summary: Some(text(item, "doc")), receives: receives(how), link: Some(format!("addr:{name}")), name, ..Member::default() });
        }
    }
    if let Some(required) = v.get("required").and_then(Value::as_array) {
        for item in required {
            facts.does.push(Member {
                name: text(item, "name"),
                signature: Some("fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>\n    where\n        S: Serializer;".to_owned()),
                summary: Some(text(item, "doc")),
                owes: Owes::Required,
                ..Member::default()
            });
        }
    }
    facts.implementors = v.get("implementors").and_then(|i| i.get("total")).and_then(Value::as_u64).and_then(|n| u32::try_from(n).ok());
    for item in v.get("implements").and_then(Value::as_array).into_iter().flatten() {
        facts.implements.push((text(item, "name"), text(item, "how") == "derive"));
    }
    for sibling in v.get("siblings").and_then(Value::as_array).into_iter().flatten() {
        let kind = match text(sibling, "kind").as_str() {
            "fn" => Kind::Function,
            "method" => Kind::Method,
            "struct" => Kind::Struct,
            "trait" => Kind::Trait,
            "enum" => Kind::Enum,
            _ => Kind::Other,
        };
        facts.beside.push(super::facts::Beside { name: text(sibling, "name"), kind, signature: None, link: Some(format!("addr:{}", text(sibling, "name"))) });
    }
    if let Some(releases) = v.get("history").and_then(|h| h.get("releases")).and_then(Value::as_array) {
        let last = releases.last().and_then(|r| r.get("sig")).cloned();
        facts.releases = releases.iter().map(|r| (text(r, "v"), r.get("sig") != last.as_ref())).collect();
    }
    match text(v, "id").as_str() {
        "rs-from_str" => {
            facts.error_kinds = v
                .get("sig")
                .and_then(|s| s.get("out"))
                .and_then(|o| o.get("fails"))
                .and_then(|f| f.get("kinds"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|k| (text(k, "name"), text(k, "doc")))
                .collect();
            facts.error_tells = Some("Error::classify() → Category".to_owned());
        }
        "py-re.match" => {
            facts.docs = vec![Block::Para(text(v, "summary"))];
            facts.sections = vec![
                Section { kind: SectionKind::Parameters, body: String::new(), entries: vec![("pattern".to_owned(), "a regular expression, text or compiled".to_owned()), ("string".to_owned(), "what to look at".to_owned())] },
                Section { kind: SectionKind::Errors, body: String::new(), entries: vec![("re.error".to_owned(), "if the pattern itself is invalid".to_owned())] },
            ];
        }
        "js-which.sync" => {
            facts.signature = Some("const whichSync = (cmd, opt) => {".to_owned());
            facts.sections = vec![
                Section {
                    kind: SectionKind::Parameters,
                    body: String::new(),
                    entries: vec![
                        ("cmd".to_owned(), "{string} a program name".to_owned()),
                        ("opt".to_owned(), "{object} options".to_owned()),
                        ("opt.nothrow".to_owned(), "{boolean} give null instead of throwing".to_owned()),
                        ("opt.all".to_owned(), "{boolean} give every match, a list".to_owned()),
                        ("opt.path".to_owned(), "{string} search this instead of $PATH".to_owned()),
                        ("opt.pathExt".to_owned(), "{string} extensions to try (Windows)".to_owned()),
                    ],
                },
                Section { kind: SectionKind::Errors, body: String::new(), entries: vec![("Error".to_owned(), "if it isn't on PATH, unless nothrow".to_owned())] },
            ];
        }
        _ => {}
    }
    facts
}

pub(crate) fn sites_of(v: &Value) -> Vec<Site> {
    let page_kind = kind_of(v);
    v.get("uses")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|u| {
            let tag = text(u, "tag");
            let rel = match tag.as_str() {
                "imports" => Rel::Imports,
                _ if matches!(page_kind, Kind::Function | Kind::Method) => Rel::Calls,
                _ => Rel::TypeReference,
            };
            let dir = text(u, "dir");
            let file = text(u, "file");
            Site {
                package: text(u, "pkg"),
                path: if dir.is_empty() { file.clone() } else { format!("/work/{dir}/{file}") },
                file,
                line: u32::try_from(u.get("line").and_then(Value::as_u64).unwrap_or(1)).unwrap_or(1),
                text: text(u, "text"),
                rel,
                exact: u.get("approx").is_none(),
            }
        })
        .collect()
}

/// One board page, compiled and read: the view and its workspace.
pub(crate) fn board(id: &str) -> Option<(View, Uses)> {
    let json = std::fs::read_to_string(board_dir().join(format!("{id}.json"))).ok()?;
    let v: Value = serde_json::from_str(&json).ok()?;
    let facts = facts_of(&v);
    let view = compile(&facts);
    let mut uses = read_all(&sites_of(&v), &Reader::of(&view));
    uses.elsewhere = v.get("workspace").and_then(Value::as_str).map(ToOwned::to_owned);
    let view = with_uses(view, &uses);
    Some((view, uses))
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn load(id: &str) -> Option<Value> {
        serde_json::from_str(&std::fs::read_to_string(board_dir().join(format!("{id}.json"))).ok()?).ok()
    }

    /// Reads the board's use sites through the classifier and compares each
    /// verb with the board's own tag. It reads the design board's data, so it
    /// is run by hand to tune the classifier (`-- --ignored --nocapture`).
    #[test]
    #[ignore = "reads the design board's data; run by hand to tune the classifier"]
    fn the_classifier_agrees_with_the_boards_tags() {
        for id in ["rs-Value", "rs-as_str", "rs-from_str", "rs-AllocationInfo", "rs-Serialize", "py-re.match", "js-which.sync"] {
            let Some(v) = load(id) else { continue };
            let view = compile(&facts_of(&v));
            let sites = sites_of(&v);
            let uses = read_all(&sites, &Reader::of(&view));
            let tags: Vec<String> = v.get("uses").and_then(Value::as_array).into_iter().flatten().map(|u| text(u, "tag")).collect();
            let mut agree = 0;
            let mut differ: BTreeMap<(String, &'static str), (usize, String)> = BTreeMap::new();
            for (place, tag) in uses.all.iter().zip(&tags) {
                if place.verb.word() == tag {
                    agree += 1;
                } else {
                    let entry = differ.entry((tag.clone(), place.verb.word())).or_insert((0, place.text.clone()));
                    entry.0 += 1;
                }
            }
            println!("AGREE {id}: {agree}/{}", tags.len());
            for ((board, mine), (n, example)) in &differ {
                println!("    DIFFER board {board} -> mine {mine} x{n}: {example}");
            }
        }
    }

    #[test]
    fn the_boards_pages_compile_when_the_data_is_there() {
        let Some((view, _)) = board("rs-from_str") else { return };
        let call = view.call.expect("a call");
        assert_eq!(call.ports[0].ty.word, "text");
        assert!(call.fails.is_some());
    }
}
