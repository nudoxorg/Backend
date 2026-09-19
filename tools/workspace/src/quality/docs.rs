//! Local Markdown destination gates.

use super::{SourceFile, Violation};
use std::collections::{BTreeMap, BTreeSet};

/// Checks local Markdown destinations against a repository file/directory
/// inventory.  Remote URLs, anchors, and explicit absolute filesystem paths
/// are intentionally outside this gate.
#[must_use]
pub fn validate_markdown_links(
    files: &[SourceFile],
    known_paths: &BTreeSet<String>,
) -> Vec<Violation> {
    let known = normalized_inventory(known_paths);
    let mut violations = Vec::new();
    for file in files {
        if !file.path.to_ascii_lowercase().ends_with(".md") {
            continue;
        }
        for (line, target) in markdown_destinations(&file.contents) {
            let Some(candidate) = local_markdown_destination(&file.path, &target) else {
                continue;
            };
            if !known.contains(&candidate)
                && !known
                    .iter()
                    .any(|path| path.starts_with(&(candidate.clone() + "/")))
            {
                violations.push(Violation::BrokenMarkdownLink {
                    path: file.path.clone(),
                    line,
                    target,
                });
            }
        }
    }
    violations
}

fn normalized_inventory(paths: &BTreeSet<String>) -> BTreeSet<String> {
    paths
        .iter()
        .map(|path| normalize_relative_path(path))
        .collect()
}

fn local_markdown_destination(source: &str, target: &str) -> Option<String> {
    let target = target.trim().trim_matches('<').trim_matches('>');
    if target.is_empty() || target.starts_with('#') || target.starts_with("//") {
        return None;
    }
    let scheme = target.find(':');
    if scheme.is_some_and(|index| {
        target[..index].chars().all(|character| {
            character.is_ascii_alphabetic() || character == '+' || character == '-'
        })
    }) {
        return None;
    }
    if target.starts_with('/') && is_absolute_filesystem_path(target) {
        return None;
    }
    let target = target
        .split_once('#')
        .map_or(target, |(path, _)| path)
        .split_once('?')
        .map_or_else(
            || target.split_once('#').map_or(target, |(path, _)| path),
            |(path, _)| path,
        );
    let target = percent_decode(target)?;
    if target.starts_with('/') {
        return Some(normalize_relative_path(target.trim_start_matches('/')));
    }
    let parent = source.rsplit_once('/').map_or("", |(parent, _)| parent);
    Some(normalize_relative_path(&format!("{parent}/{target}")))
}

fn is_absolute_filesystem_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    lower.starts_with("/users/")
        || lower.starts_with("/home/")
        || lower.starts_with("/private/")
        || lower.starts_with("/tmp/")
        || lower.starts_with("/var/")
        || path
            .get(1..3)
            .is_some_and(|drive| drive == ":\\" || drive == ":/")
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = bytes.get(index + 1).and_then(|byte| hex(*byte))?;
            let low = bytes.get(index + 2).and_then(|byte| hex(*byte))?;
            output.push((high << 4) | low);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).ok()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn normalize_relative_path(path: &str) -> String {
    let mut components = Vec::new();
    for part in path.replace('\\', "/").split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if components.last().is_some_and(|component| component != "..") {
                    components.pop();
                } else {
                    components.push("..".to_owned());
                }
            }
            component => components.push(component.to_owned()),
        }
    }
    components.join("/")
}

fn markdown_destinations(source: &str) -> Vec<(usize, String)> {
    let mut result = Vec::new();
    let mut references = BTreeMap::<String, String>::new();
    let mut reference_uses = Vec::<(usize, String)>::new();
    let mut fenced = false;
    let mut html_comment = false;
    for (line_number, line) in source.lines().enumerate() {
        let line = strip_html_comment(line, &mut html_comment);
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        let line = strip_markdown_code(&line);
        if let Some((label, target)) = reference_definition(&line)
            && !target.is_empty()
        {
            references.insert(normalize_reference_label(&label), target.clone());
            result.push((line_number + 1, target));
        }
        let bytes = line.as_bytes();
        let mut index = 0;
        while index + 1 < bytes.len() {
            if bytes[index] == b']' && bytes[index + 1] == b'(' {
                let has_label = line[..index]
                    .rfind('[')
                    .is_some_and(|open| !line[open + 1..index].contains(']'));
                if !has_label {
                    index += 1;
                    continue;
                }
                let start = index + 2;
                let (target, consumed) = parse_inline_destination(&line[start..]);
                if !target.is_empty() {
                    result.push((line_number + 1, target));
                }
                index = start + consumed;
            } else {
                index += 1;
            }
        }
        // Resolve explicit and collapsed reference links (`[text][id]` and
        // `[text][]`) after collecting definitions. Forward references are
        // valid Markdown, so hold their labels for the second pass below.
        let mut marker = 0usize;
        while marker < bytes.len() {
            let Some(relative_open) = line[marker..].find('[') else {
                break;
            };
            let open = marker + relative_open;
            let Some(relative_close) = line[open + 1..].find(']') else {
                break;
            };
            let close = open + 1 + relative_close;
            let after = close + 1;
            if line[after..].starts_with('(') || line[after..].starts_with(':') {
                marker = after;
                continue;
            }
            if line[after..].starts_with('[')
                && let Some(relative_end) = line[after + 1..].find(']')
            {
                let end = after + 1 + relative_end;
                let label = line[open + 1..close].to_owned();
                let id = if line[after + 1..end].trim().is_empty() {
                    label
                } else {
                    line[after + 1..end].to_owned()
                };
                reference_uses.push((line_number + 1, normalize_reference_label(&id)));
                marker = end + 1;
            } else {
                marker = close + 1;
            }
        }
    }
    for (line, reference) in reference_uses {
        result.push((
            line,
            references.get(&reference).cloned().unwrap_or(reference),
        ));
    }
    result
}

fn reference_definition(line: &str) -> Option<(String, String)> {
    let line = line.trim_start();
    let close = line.find("]:")?;
    if !line.starts_with('[') || close == 1 {
        return None;
    }
    let (target, _) = parse_inline_destination(line[close + 2..].trim());
    Some((line[1..close].to_owned(), target))
}

fn normalize_reference_label(label: &str) -> String {
    label
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

fn strip_markdown_code(line: &str) -> String {
    let mut output = String::with_capacity(line.len());
    let mut inline = false;
    for character in line.chars() {
        if character == '`' {
            inline = !inline;
            output.push(' ');
        } else if inline {
            output.push(' ');
        } else {
            output.push(character);
        }
    }
    output
}

fn strip_html_comment(line: &str, in_comment: &mut bool) -> String {
    let mut output = String::new();
    let mut remaining = line;
    loop {
        if *in_comment {
            let Some(end) = remaining.find("-->") else {
                break;
            };
            *in_comment = false;
            remaining = &remaining[end + 3..];
            continue;
        }
        let Some(start) = remaining.find("<!--") else {
            output.push_str(remaining);
            break;
        };
        output.push_str(&remaining[..start]);
        *in_comment = true;
        remaining = &remaining[start + 4..];
    }
    output
}

fn parse_inline_destination(value: &str) -> (String, usize) {
    let value = value.trim_start();
    if let Some(rest) = value.strip_prefix('<') {
        if let Some(end) = rest.find('>') {
            return (rest[..end].to_owned(), end + 1);
        }
        return (String::new(), value.len());
    }
    let mut depth = 0usize;
    let mut end = 0;
    for (index, character) in value.char_indices() {
        match character {
            '(' => depth += 1,
            ')' if depth == 0 => break,
            ')' => depth = depth.saturating_sub(1),
            c if c.is_whitespace() && depth == 0 => break,
            _ => {}
        }
        end = index + character.len_utf8();
    }
    (value[..end].to_owned(), end)
}
