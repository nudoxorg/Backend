//! What the docs say, read for the page: the lede, what is left of the docs
//! once the errors section has become "If it fails", and the short "when"
//! words the call's outcome rows carry.

use super::super::facts::{Facts, Section, SectionKind};
use super::super::view::{Block, Docs};
use super::text::{clean, first_sentence, lower_first, plain};

/// The first paragraph of the docs, its first sentence: the lede.
pub(super) fn lede(facts: &Facts) -> Option<String> {
    facts.docs.iter().find_map(|block| match block {
        Block::Para(text) => Some(first_sentence(&clean(text))),
        _ => None,
    })
}

/// The docs after the lede: the first paragraph's remainder, the other
/// blocks, no errors section.
pub(super) fn docs(facts: &Facts) -> Docs {
    let mut blocks = Vec::new();
    let mut skip = false;
    let mut first = true;
    for block in &facts.docs {
        match block {
            Block::Head(title) => {
                let lower = title.to_ascii_lowercase();
                skip = matches!(lower.trim(), "errors" | "error");
                if matches!(lower.trim(), "example" | "examples") || skip {
                    continue;
                }
                blocks.push(block.clone());
            }
            Block::Para(text) if first => {
                first = false;
                let text = clean(text);
                let head = first_sentence(&text);
                let rest = text.strip_prefix(head.as_str()).map_or("", str::trim);
                if !rest.is_empty() {
                    blocks.push(Block::Para(rest.to_owned()));
                }
            }
            Block::Para(_) | Block::Item(_) if skip => {}
            Block::Code(_) => {
                skip = false;
                blocks.push(block.clone());
            }
            Block::Para(text) => blocks.push(Block::Para(clean(text))),
            Block::Item(text) => blocks.push(Block::Item(clean(text))),
            _ => blocks.push(block.clone()),
        }
    }
    // Sections the docs' conventions carry (JSDoc, Python docstrings): the
    // ones that are not the errors, the parameters or the return read as
    // paragraphs after the rest.
    for section in &facts.sections {
        if matches!(section.kind, SectionKind::Examples) && !section.body.trim().is_empty() && !facts.docs.iter().any(|b| matches!(b, Block::Code(_))) {
            blocks.push(Block::Code(section.body.trim().to_owned()));
        }
        if matches!(section.kind, SectionKind::Safety | SectionKind::Deprecated | SectionKind::Panics) && !section.body.trim().is_empty() {
            let title = match section.kind {
                SectionKind::Safety => "Safety",
                SectionKind::Deprecated => "Deprecated",
                _ => "Panics",
            };
            blocks.push(Block::Head(title.to_owned()));
            blocks.push(Block::Para(section.body.trim().to_owned()));
        }
    }
    Docs { blocks }
}

/// The prose of the errors section: from the sections, else the paragraphs
/// under an "Errors" heading.
pub(super) fn errors_prose(facts: &Facts) -> Option<(String, Vec<(String, String)>)> {
    if let Some(section) = facts.sections.iter().find(|s| s.kind == SectionKind::Errors) {
        let body = section.body.trim().to_owned();
        if !body.is_empty() || !section.entries.is_empty() {
            return Some((body, section.entries.clone()));
        }
    }
    let mut under = false;
    let mut prose = Vec::new();
    for block in &facts.docs {
        match block {
            Block::Head(title) => under = title.eq_ignore_ascii_case("errors"),
            Block::Para(text) if under => prose.push(text.clone()),
            _ => {}
        }
    }
    (!prose.is_empty()).then(|| (prose.join(" "), Vec::new()))
}

/// The section of `kind`, when the docs have one.
pub(super) fn section(facts: &Facts, kind: SectionKind) -> Option<&Section> {
    facts.sections.iter().find(|s| s.kind == kind)
}

/// "if it isn't valid JSON": the first sentence of a failure's prose as a
/// one-line "when".
pub(super) fn short_when(prose: &str) -> String {
    let sentence = plain(&first_sentence(prose));
    let mut text = sentence.trim_end_matches('.').to_owned();
    for lead in [
        "This conversion can fail if",
        "This function can fail if",
        "This method can fail if",
        "This can fail if",
        "It can fail if",
        "Returns an error if",
        "Returns an `Err` if",
        "Returns Err if",
        "Returns an error when",
        "Errors if",
        "Fails if",
        "Will fail if",
        "Fails when",
        "Can fail if",
        "Raises if",
        "Throws if",
        "Raised when",
        "Raised if",
        "Thrown when",
        "Thrown if",
    ] {
        if let Some(rest) = text.strip_prefix(lead) {
            text = format!("if{rest}");
            break;
        }
    }
    // One line: cut at the aside.
    for cut in [", for example", ", e.g.", " (e.g.", "; "] {
        if let Some(at) = text.find(cut) {
            text.truncate(at);
        }
    }
    if text.chars().count() > 96 {
        if let Some(at) = text[..text.char_indices().nth(96).map_or(text.len(), |(at, _)| at)].rfind(", ") {
            text.truncate(at);
        }
    }
    text
}

/// When a call gives nothing, read from the docs: "if it isn't a String",
/// "if no match was found"; `None` when the docs never say it can.
pub(super) fn none_when(facts: &Facts) -> Option<String> {
    let mut prose = Vec::new();
    for block in &facts.docs {
        if let Block::Para(text) | Block::Item(text) = block {
            prose.push(plain(text));
        }
    }
    for section in &facts.sections {
        if matches!(section.kind, SectionKind::Returns | SectionKind::Other) {
            prose.push(plain(&section.body));
            for (_, text) in &section.entries {
                prose.push(plain(text));
            }
        }
    }
    let all = prose.join(" ");
    let sentences: Vec<String> = split_sentences(&all);
    for (index, sentence) in sentences.iter().enumerate() {
        let lower = sentence.to_ascii_lowercase();
        let mentions = ["none", "null", "undefined", "nothing", "returns nothing"].iter().any(|w| contains_word(&lower, w));
        if !mentions {
            continue;
        }
        // `… or None if no match was found`, `Returns None if …`.
        for marker in ["none if ", "null if ", "undefined if ", "nothing if ", "none when ", "null when "] {
            if let Some(at) = lower.find(marker) {
                let rest = sentence[at + marker.len()..].trim().trim_end_matches('.');
                return Some(format!("if {}", cut_when(rest)));
            }
        }
        if lower.contains("otherwise") {
            if let Some(previous) = index.checked_sub(1).and_then(|p| sentences.get(p)) {
                // `If the Value is a String, returns …` → "if it isn't a String".
                let lower_prev = previous.to_ascii_lowercase();
                if let Some(at) = lower_prev.find(" is a ").or_else(|| lower_prev.find(" is an ")) {
                    let article_end = lower_prev[at..].find("a").map_or(0, |i| i);
                    let _ = article_end;
                    let rest = &previous[at + 4..];
                    let head = rest.split([',', ';']).next().unwrap_or(rest).trim();
                    return Some(format!("if it isn't {head}"));
                }
            }
            return Some("otherwise".to_owned());
        }
    }
    None
}

fn cut_when(text: &str) -> String {
    let text = text.split([',', ';']).next().unwrap_or(text).trim();
    let text = text.strip_prefix("there is ").unwrap_or(text);
    text.to_owned()
}

fn contains_word(haystack: &str, word: &str) -> bool {
    haystack.match_indices(word).any(|(at, _)| {
        !haystack[..at].chars().next_back().is_some_and(char::is_alphanumeric) && !haystack[at + word.len()..].chars().next().is_some_and(char::is_alphanumeric)
    })
}

fn split_sentences(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text.trim();
    while !rest.is_empty() {
        let sentence = first_sentence(rest);
        if sentence.is_empty() {
            break;
        }
        let consumed = rest.find(sentence.as_str()).map_or(sentence.len(), |at| at + sentence.len());
        out.push(sentence);
        rest = rest[consumed.min(rest.len())..].trim_start();
    }
    out
}

/// A one-line note from an entry's prose: its first sentence, plain,
/// without a trailing full stop.
pub(super) fn note(prose: &str) -> String {
    let text = plain(&first_sentence(prose));
    let text = text.trim_start_matches(['-', ':', ' ']);
    text.trim_end_matches('.').to_owned()
}

/// A method's or case's doc for a row: the first sentence, plain.
pub(super) fn row_doc(summary: Option<&str>) -> String {
    summary.map_or_else(String::new, |text| {
        let text = plain(&first_sentence(text));
        let text = text.trim_end_matches('.');
        let text = ["Represents a ", "Represents an ", "Represents "].iter().find_map(|prefix| text.strip_prefix(prefix)).unwrap_or(text);
        lower_first_keep_acronym(text)
    })
}

fn lower_first_keep_acronym(text: &str) -> String {
    let mut chars = text.chars();
    match (chars.next(), chars.next()) {
        (Some(first), Some(second)) if first.is_uppercase() && second.is_lowercase() => lower_first(text),
        _ => text.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::anatomy::symbol::facts::Facts;
    use crate::anatomy::symbol::view::{Kind, Lang};

    fn facts(docs: &[&str]) -> Facts {
        let mut facts = Facts::new("as_str", Kind::Method, Lang::Rust, "serde_json");
        facts.docs = docs.iter().map(|text| Block::Para((*text).to_owned())).collect();
        facts
    }

    #[test]
    fn a_failure_reads_as_one_line() {
        let prose = "This conversion can fail if the structure of the input does not match the structure expected by `T`, for example if `T` is a struct type but the input contains something other than a JSON map. It can also fail if the structure is correct.";
        assert_eq!(short_when(prose), "if the structure of the input does not match the structure expected by T");
        assert_eq!(short_when("Returns an error if the file cannot be read."), "if the file cannot be read");
    }

    #[test]
    fn nothing_is_read_from_a_returns_none_otherwise() {
        let f = facts(&["If the `Value` is a String, returns the associated `str`. Returns None otherwise."]);
        assert_eq!(none_when(&f).as_deref(), Some("if it isn't a String"));
        let f = facts(&["Try to apply the pattern at the start of the string, returning a Match object, or None if no match was found."]);
        assert_eq!(none_when(&f).as_deref(), Some("if no match was found"));
        assert_eq!(none_when(&facts(&["Deserialize an instance of type `T` from a string of JSON text."])), None);
    }

    #[test]
    fn a_row_doc_drops_the_case_boilerplate() {
        assert_eq!(row_doc(Some("Represents a TOML string")), "TOML string");
        assert_eq!(row_doc(Some("JSON null value.")), "JSON null value");
    }
}
