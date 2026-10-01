//! Small text helpers the derivation shares: balanced splitting, comment
//! stripping, and sentences.

/// Splits `text` at `sep` outside any bracket pair.
pub(super) fn split_top(text: &str, sep: char) -> Vec<String> {
    let mut out = Vec::new();
    let mut depth = 0_i32;
    let mut quote: Option<char> = None;
    let mut current = String::new();
    let mut previous = '\0';
    for ch in text.chars() {
        if let Some(open) = quote {
            current.push(ch);
            if ch == open && previous != '\\' {
                quote = None;
            }
            previous = ch;
            continue;
        }
        match ch {
            '"' | '`' => quote = Some(ch),
            // A single quote opens a string only where a value can start
            // (`= 'x'`, `('x')`); elsewhere it is a Rust lifetime (`<'a, T>`).
            '\'' if matches!(
                current.trim_end().chars().next_back(),
                Some('=' | '(' | '[' | ':')
            ) =>
            {
                quote = Some(ch)
            }
            '<' | '(' | '[' | '{' => depth += 1,
            '>' if previous == '-' || previous == '=' => {}
            '>' | ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
        if ch == sep && depth <= 0 && quote.is_none() {
            out.push(std::mem::take(&mut current).trim().to_owned());
        } else {
            current.push(ch);
        }
        previous = ch;
    }
    let tail = current.trim();
    if !tail.is_empty() {
        out.push(tail.to_owned());
    }
    out.into_iter().filter(|part| !part.is_empty()).collect()
}

/// The text inside the first bracket pair opened at byte `open` (which must
/// be an opening bracket), and the byte after its closer.
pub(super) fn balanced(text: &str, open: usize) -> Option<(&str, usize)> {
    let opener = text[open..].chars().next()?;
    let closer = match opener {
        '(' => ')',
        '[' => ']',
        '{' => '}',
        '<' => '>',
        _ => return None,
    };
    let mut depth = 0_i32;
    let mut previous = '\0';
    for (at, ch) in text[open..].char_indices() {
        if ch == opener {
            depth += 1;
        } else if ch == closer && !(closer == '>' && (previous == '-' || previous == '=')) {
            depth -= 1;
            if depth == 0 {
                let end = open + at;
                return Some((
                    &text[open + opener.len_utf8()..end],
                    end + closer.len_utf8(),
                ));
            }
        }
        previous = ch;
    }
    None
}

/// Removes leading whitespace, `//` and `/* */` comments, and `#[..]` / `@x`
/// attribute and decorator lines.
pub(super) fn strip_leading(mut source: &str) -> &str {
    loop {
        source = source.trim_start();
        if source.starts_with("//") || source.starts_with("///") {
            match source.find('\n') {
                Some(at) => source = &source[at + 1..],
                None => return "",
            }
        } else if source.starts_with("/*") {
            match source.find("*/") {
                Some(at) => source = &source[at + 2..],
                None => return "",
            }
        } else if source.starts_with("#[") || source.starts_with("#![") {
            let Some(start) = source.find('[') else {
                return "";
            };
            match balanced(source, start) {
                Some((_, end)) => source = &source[end..],
                None => return "",
            }
        } else if source.starts_with('@') && !source.starts_with("@ ") {
            // A Python decorator or a TypeScript one: a whole line, or a call.
            match source.find('\n') {
                Some(at) => source = &source[at + 1..],
                None => return "",
            }
        } else {
            return source;
        }
    }
}

/// Rustdoc's intra-doc links as plain markup: `[`Name`]` and `[`Name`][path]`
/// become `` `Name` ``; `[`Name`](target)` becomes `[Name](target)`.
pub(super) fn clean(markup: &str) -> String {
    let mut out = String::with_capacity(markup.len());
    let mut rest = markup;
    while let Some(at) = rest.find("[`") {
        out.push_str(&rest[..at]);
        let after = &rest[at + 2..];
        let Some(end) = after.find("`]") else {
            // No closer: not a code link. Leave the rest for the link pass.
            out.push_str(&rest[at..]);
            rest = "";
            break;
        };
        let inner = &after[..end];
        let mut tail = &after[end + 2..];
        if let Some(target) = tail
            .strip_prefix('(')
            .and_then(|t| t.find(')').map(|close| (&t[..close], &t[close + 1..])))
        {
            out.push_str(&format!("[{inner}]({})", target.0));
            tail = target.1;
        } else if let Some(reference) = tail
            .strip_prefix('[')
            .and_then(|t| t.find(']').map(|close| &t[close + 1..]))
        {
            out.push_str(&format!("`{inner}`"));
            tail = reference;
        } else {
            out.push_str(&format!("`{inner}`"));
        }
        rest = tail;
    }
    out.push_str(rest);
    unlink_code(&out)
}

/// `[the `x` module](self)` (a link whose words hold code and whose target is
/// not a web address) reads as its words: the code stays code.
fn unlink_code(markup: &str) -> String {
    let mut out = String::with_capacity(markup.len());
    let mut rest = markup;
    while let Some(open) = rest.find('[') {
        out.push_str(&rest[..open]);
        let after = &rest[open + 1..];
        let linked = after.find("](").and_then(|close| {
            let label = &after[..close];
            let tail = &after[close + 2..];
            let end = tail.find(')')?;
            (label.contains('`') && !label.contains('[') && !tail[..end].starts_with("http"))
                .then(|| (label, &tail[end + 1..]))
        });
        match linked {
            Some((label, tail)) => {
                out.push_str(label);
                rest = tail;
            }
            None => {
                out.push('[');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// Collapses runs of whitespace to one space.
pub(super) fn squash(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The byte offset of the first `word` in `text` outside brackets, at word
/// boundaries.
pub(super) fn keyword(text: &str, word: &str) -> Option<usize> {
    let mut depth = 0_i32;
    let mut previous = '\0';
    for (at, ch) in text.char_indices() {
        if depth == 0
            && text[at..].starts_with(word)
            && !text[..at]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
            && !text[at + word.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_alphanumeric() || c == '_')
        {
            return Some(at);
        }
        match ch {
            '<' | '(' | '[' | '{' => depth += 1,
            '>' if previous == '-' || previous == '=' => {}
            '>' | ')' | ']' | '}' => depth -= 1,
            _ => {}
        }
        previous = ch;
    }
    None
}

/// The first sentence of `markup`: up to a `.`, `!` or `?` followed by
/// space or the end, outside code and links.
pub(super) fn first_sentence(markup: &str) -> String {
    let mut code = false;
    let mut bracket = 0_u8;
    let chars: Vec<(usize, char)> = markup.char_indices().collect();
    for (index, &(at, ch)) in chars.iter().enumerate() {
        match ch {
            '`' => code = !code,
            '[' if !code => bracket = bracket.saturating_add(1),
            ']' if !code => bracket = bracket.saturating_sub(1),
            '.' | '!' | '?' if !code && bracket == 0 => {
                let next = chars.get(index + 1).map(|&(_, c)| c);
                let letters = markup[..at].chars().filter(|c| c.is_alphabetic()).count();
                if next.is_none_or(char::is_whitespace) && letters >= 3 {
                    return markup[..at + ch.len_utf8()].trim().to_owned();
                }
            }
            _ => {}
        }
    }
    markup.trim().to_owned()
}

/// Markup as plain words: code ticks, `**`, and link targets dropped.
pub(super) fn plain(markup: &str) -> String {
    let mut out = String::new();
    let mut chars = markup.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '`' | '*' => {}
            '[' => {
                // `[label](target)` or `[label][ref]` keeps the label.
                let mut label = String::new();
                for next in chars.by_ref() {
                    if next == ']' {
                        break;
                    }
                    label.push(next);
                }
                out.push_str(label.trim_matches('`'));
                match chars.peek() {
                    Some('(') => {
                        for next in chars.by_ref() {
                            if next == ')' {
                                break;
                            }
                        }
                    }
                    Some('[') => {
                        chars.next();
                        for next in chars.by_ref() {
                            if next == ']' {
                                break;
                            }
                        }
                    }
                    _ => {}
                }
            }
            _ => out.push(ch),
        }
    }
    squash(&out)
}

/// A description of a kind of failure without its lead-in: "The error was
/// caused by a failure to read" says "a failure to read".
pub(super) fn lead_out(doc: &str) -> String {
    let lower = doc.to_ascii_lowercase();
    let mut rest = doc.trim();
    for subject in [
        "the error",
        "this error",
        "the failure",
        "this failure",
        "the problem",
        "this kind",
    ] {
        for verb in [
            " was caused by ",
            " is caused by ",
            " is due to ",
            " was due to ",
            " happens when ",
            " occurs when ",
        ] {
            let lead = format!("{subject}{verb}");
            if lower.starts_with(&lead) {
                rest = &doc[lead.len()..];
                return rest.trim().trim_end_matches('.').to_owned();
            }
        }
    }
    rest.trim_end_matches('.').to_owned()
}

/// `text` with its first letter in lower case.
pub(super) fn lower_first(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_lowercase().chain(chars).collect()
    })
}

/// The last `::`, `.` or `\` separated segment of a path.
pub(super) fn last_segment(path: &str) -> &str {
    path.rsplit(['.', ':', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_outside_brackets_and_lifetimes() {
        assert_eq!(
            split_top("a: &'a str, b: Vec<(i32, i32)>, c: [u8; 4]", ','),
            ["a: &'a str", "b: Vec<(i32, i32)>", "c: [u8; 4]"]
        );
        assert_eq!(
            split_top("x: Fn(A) -> B, y: T", ','),
            ["x: Fn(A) -> B", "y: T"]
        );
    }

    #[test]
    fn sentences_skip_code_and_links() {
        assert_eq!(
            first_sentence("Deserialize an instance of type `T` from a string of JSON text. More."),
            "Deserialize an instance of type `T` from a string of JSON text."
        );
        assert_eq!(
            first_sentence(
                "Read [the guide][crate::v1.2] before calling `value.get()` again. A second."
            ),
            "Read [the guide][crate::v1.2] before calling `value.get()` again."
        );
    }

    #[test]
    fn intra_doc_links_become_code() {
        assert_eq!(
            clean(
                "See the [`serde_json::value`] module and [`Map`][crate::Map] or [`x`](http://a)."
            ),
            "See the `serde_json::value` module and `Map` or [x](http://a)."
        );
        assert_eq!(
            clean("plain [text](t) and `code`"),
            "plain [text](t) and `code`"
        );
        assert_eq!(
            clean("See the [`serde_json::value` module documentation](self) for usage."),
            "See the `serde_json::value` module documentation for usage."
        );
    }

    #[test]
    fn markup_reads_as_plain_words() {
        assert_eq!(
            plain("Call `x.get()` on the [Value](crate::Value) and **stop**."),
            "Call x.get() on the Value and stop."
        );
    }
}
