//! Small token lexer used by source policy checks.

use std::collections::BTreeMap;

#[derive(Clone, Debug)]
pub(super) struct RustToken {
    pub(super) text: String,
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) line: usize,
    pub(super) literal: bool,
}

impl RustToken {
    pub(super) fn as_str(&self) -> &str {
        &self.text
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "this small lexer keeps comments and literals out of policy scans"
)]
pub(super) fn tokenize_rust(source: &str) -> Vec<RustToken> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0usize;
    let mut line = 1usize;
    while index < bytes.len() {
        match bytes[index] {
            b'\n' => {
                line += 1;
                index += 1;
            }
            b' ' | b'\t' | b'\r' => index += 1,
            b'/' if bytes.get(index + 1) == Some(&b'/') => {
                index += 2;
                while index < bytes.len() && bytes[index] != b'\n' {
                    index += 1;
                }
            }
            b'/' if bytes.get(index + 1) == Some(&b'*') => {
                index += 2;
                let mut depth = 1usize;
                while index < bytes.len() && depth > 0 {
                    if bytes.get(index..index + 2) == Some(b"/*") {
                        depth += 1;
                        index += 2;
                    } else if bytes.get(index..index + 2) == Some(b"*/") {
                        depth = depth.saturating_sub(1);
                        index += 2;
                    } else {
                        if bytes[index] == b'\n' {
                            line += 1;
                        }
                        index += 1;
                    }
                }
            }
            b'"' => {
                let start = index;
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        index = index.saturating_add(2);
                    } else {
                        let current = bytes[index];
                        index += 1;
                        if current == b'\n' {
                            line += 1;
                        }
                        if current == b'"' {
                            break;
                        }
                    }
                }
                tokens.push(RustToken {
                    text: source[start..index].to_owned(),
                    start,
                    end: index,
                    line,
                    literal: true,
                });
            }
            b'r' if raw_string_start(bytes, index).is_some() => {
                let start = index;
                let Some((hashes, mut cursor)) = raw_string_start(bytes, index) else {
                    index += 1;
                    continue;
                };
                while cursor < bytes.len() {
                    if bytes[cursor] == b'\n' {
                        line += 1;
                    }
                    if bytes[cursor] == b'"'
                        && bytes.get(cursor + 1..cursor + 1 + hashes)
                            == Some(&vec![b'#'; hashes][..])
                    {
                        cursor += hashes + 1;
                        break;
                    }
                    cursor += 1;
                }
                index = cursor;
                tokens.push(RustToken {
                    text: source[start..index].to_owned(),
                    start,
                    end: index,
                    line,
                    literal: true,
                });
            }
            b'\'' if !is_lifetime_start(bytes, index) => {
                let start = index;
                index += 1;
                while index < bytes.len() {
                    if bytes[index] == b'\\' {
                        index = index.saturating_add(2);
                    } else {
                        let current = bytes[index];
                        index += 1;
                        if current == b'\n' {
                            line += 1;
                        }
                        if current == b'\'' {
                            break;
                        }
                    }
                }
                tokens.push(RustToken {
                    text: source[start..index].to_owned(),
                    start,
                    end: index,
                    line,
                    literal: true,
                });
            }
            byte if is_ident_start(byte) => {
                let start = index;
                index += 1;
                while index < bytes.len() && is_ident_continue(bytes[index]) {
                    index += 1;
                }
                tokens.push(RustToken {
                    text: source[start..index].to_owned(),
                    start,
                    end: index,
                    line,
                    literal: false,
                });
            }
            _ => {
                let start = index;
                let text = if bytes.get(index..index + 2) == Some(b"::") {
                    index += 2;
                    "::"
                } else if bytes.get(index..index + 2) == Some(b"=>") {
                    index += 2;
                    "=>"
                } else {
                    index += 1;
                    &source[start..index]
                };
                tokens.push(RustToken {
                    text: text.to_owned(),
                    start,
                    end: index,
                    line,
                    literal: false,
                });
            }
        }
    }
    tokens
}

pub(super) fn raw_string_start(bytes: &[u8], index: usize) -> Option<(usize, usize)> {
    if bytes.get(index) != Some(&b'r') {
        return None;
    }
    let mut cursor = index + 1;
    let mut hashes = 0usize;
    while bytes.get(cursor) == Some(&b'#') {
        hashes += 1;
        cursor += 1;
    }
    (bytes.get(cursor) == Some(&b'"')).then_some((hashes, cursor + 1))
}

pub(super) fn is_lifetime_start(bytes: &[u8], index: usize) -> bool {
    bytes.get(index + 1).copied().is_some_and(is_ident_start)
        // A lifetime may be one identifier character (`'_` is common in
        // formatter bounds).  A quote immediately after that character is a
        // character literal instead.
        && bytes.get(index + 2) != Some(&b'\'')
}

pub(super) fn is_ident_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

pub(super) fn is_ident_continue(byte: u8) -> bool {
    is_ident_start(byte) || byte.is_ascii_digit()
}

pub(super) fn matching_delimiters(tokens: &[RustToken]) -> BTreeMap<usize, usize> {
    let mut stack = Vec::<(String, usize)>::new();
    let mut pairs = BTreeMap::new();
    for (index, token) in tokens.iter().enumerate() {
        if matches!(token.text.as_str(), "(" | "[" | "{") {
            stack.push((token.text.clone(), index));
        } else if matches!(token.text.as_str(), ")" | "]" | "}") {
            let Some(expected) = (match token.text.as_str() {
                ")" => Some("("),
                "]" => Some("["),
                "}" => Some("{"),
                _ => None,
            }) else {
                continue;
            };
            if let Some(position) = stack.iter().rposition(|(open, _)| open == expected) {
                let (_, open) = stack.remove(position);
                pairs.insert(open, index);
            }
        }
    }
    pairs
}

pub(super) fn bracket_attribute(
    tokens: &[RustToken],
    index: usize,
) -> Option<(usize, Vec<RustToken>)> {
    let open = if tokens.get(index + 1).is_some_and(|token| token.text == "!") {
        index + 2
    } else {
        index + 1
    };
    if tokens.get(open).is_none_or(|token| token.text != "[") {
        return None;
    }
    let close = matching_bracket(tokens, open)?;
    Some((close, tokens[open + 1..close].to_vec()))
}

pub(super) fn matching_bracket(tokens: &[RustToken], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate().skip(open) {
        match token.text.as_str() {
            "[" => depth += 1,
            "]" => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

pub(super) fn attribute_is_test_only(tokens: &[RustToken], index: usize) -> bool {
    let mut cursor = index;
    while cursor > 0 {
        let previous = cursor - 1;
        if tokens[previous].text == "]" {
            let Some((start, inner)) = attribute_start_before(tokens, previous) else {
                break;
            };
            if is_test_attribute(&inner) {
                return true;
            }
            cursor = start;
        } else {
            break;
        }
    }
    let Some((attribute_end, _)) = bracket_attribute(tokens, index) else {
        return false;
    };
    let mut cursor = attribute_end + 1;
    while tokens.get(cursor).is_some_and(|token| token.text == "#") {
        let Some((next_end, inner)) = bracket_attribute(tokens, cursor) else {
            break;
        };
        if is_test_attribute(&inner) {
            return true;
        }
        cursor = next_end + 1;
    }
    false
}

pub(super) fn attribute_start_before(
    tokens: &[RustToken],
    close: usize,
) -> Option<(usize, Vec<RustToken>)> {
    let mut depth = 0usize;
    for index in (0..close).rev() {
        match tokens[index].text.as_str() {
            "]" => depth += 1,
            "[" if depth == 0 => {
                let start = if index > 0 && matches!(tokens[index - 1].text.as_str(), "!" | "#") {
                    index - 1
                } else {
                    index
                };
                return Some((start, tokens[index + 1..close].to_vec()));
            }
            "[" => depth = depth.saturating_sub(1),
            _ => {}
        }
    }
    None
}

pub(super) fn test_only_ranges(tokens: &[RustToken]) -> Vec<(usize, usize)> {
    let pairs = matching_delimiters(tokens);
    let mut ranges = Vec::new();
    let mut index = 0usize;
    while index < tokens.len() {
        if tokens[index].text != "#" {
            index += 1;
            continue;
        }
        let Some((end, inner)) = bracket_attribute(tokens, index) else {
            index += 1;
            continue;
        };
        if !is_test_attribute(&inner) {
            index = end + 1;
            continue;
        }
        let mut item = end + 1;
        while item < tokens.len() && tokens[item].text == "#" {
            let Some((next_end, _)) = bracket_attribute(tokens, item) else {
                break;
            };
            item = next_end + 1;
        }
        let mut item_keyword = item;
        while tokens.get(item_keyword).is_some_and(|token| {
            matches!(token.text.as_str(), "async" | "default" | "pub" | "unsafe")
        }) {
            item_keyword += 1;
            if tokens
                .get(item_keyword)
                .is_some_and(|token| token.text == "(")
            {
                item_keyword = pairs
                    .get(&item_keyword)
                    .copied()
                    .map_or(item_keyword + 1, |close| close + 1);
            }
        }
        if !tokens.get(item_keyword).is_some_and(|token| {
            matches!(
                token.text.as_str(),
                "const" | "enum" | "fn" | "impl" | "mod" | "static" | "struct" | "trait"
            )
        }) {
            index = item_keyword;
            continue;
        }
        let Some(open) = (item_keyword..tokens.len())
            .find(|candidate| matches!(tokens[*candidate].text.as_str(), "{" | ";"))
        else {
            index = item_keyword;
            continue;
        };
        if tokens[open].text == ";" {
            index = item_keyword;
            continue;
        }
        if let Some(close) = matching_brace(tokens, open) {
            ranges.push((tokens[open].start, tokens[close].end));
            index = close + 1;
        } else {
            index = item;
        }
    }
    // Individual `#[test] fn` items have the same shape as cfg(test) items;
    // the generic attribute detector above already recognizes the `test` name.
    ranges
}

fn matching_brace(tokens: &[RustToken], open: usize) -> Option<usize> {
    let mut depth = 0usize;
    for (index, token) in tokens.iter().enumerate().skip(open) {
        match token.text.as_str() {
            "{" => depth += 1,
            "}" => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return Some(index);
                }
            }
            _ => {}
        }
    }
    None
}

pub(super) fn is_test_attribute(tokens: &[RustToken]) -> bool {
    if tokens.first().map(RustToken::as_str) == Some("test") {
        return true;
    }
    if tokens
        .windows(2)
        .any(|window| window[0].text == "test" && matches!(window[1].text.as_str(), "(" | "::"))
        || (tokens.last().map(RustToken::as_str) == Some("test")
            && tokens.iter().any(|token| token.text == "::"))
        || tokens.first().map(RustToken::as_str) == Some("rstest")
    {
        return true;
    }
    if tokens.first().map(RustToken::as_str) != Some("cfg") {
        return false;
    }
    let names = tokens
        .iter()
        .filter(|token| !token.literal)
        .map(|token| token.text.as_str())
        .collect::<Vec<_>>();
    if names == ["cfg", "(", "test", ")"] {
        return true;
    }
    names.get(2) == Some(&"all") && names.contains(&"test") && !names.contains(&"not")
}

pub(super) fn in_ranges(position: usize, ranges: &[(usize, usize)]) -> bool {
    ranges
        .iter()
        .any(|(start, end)| position >= *start && position <= *end)
}

pub(super) fn is_test_path(path: &str) -> bool {
    let components = path
        .replace('\\', "/")
        .split('/')
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let Some(file) = components.last() else {
        return false;
    };
    components.iter().any(|component| {
        component == "tests"
            || component.ends_with("_tests")
            || component == "test"
            || component.ends_with("_test")
    }) || file == "tests.rs"
        || file.ends_with("_tests.rs")
}

pub(super) fn has_extension(path: &str, extension: &str) -> bool {
    std::path::Path::new(path)
        .extension()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.eq_ignore_ascii_case(extension))
}
