//! Display projection only. Neither source signatures nor canonical IR change.

use crate::Language;
use backend_library::{DeclarationKind, SourceExcerpt, SourceExcerptExtent};
use std::borrow::Cow;

pub(super) fn text<'a>(
    source: &'a str,
    language: Language,
    kind: Option<DeclarationKind>,
    local_name: &str,
    excerpt: &'a SourceExcerpt,
) -> Option<Cow<'a, str>> {
    let trimmed = source.trim();
    // An exact written prefix is stronger context than a legacy string shape.
    // Preserve the signature itself; an excerpt may continue into its body.
    if let SourceExcerpt::Captured { text, extent } = excerpt {
        if kind == Some(DeclarationKind::Import) {
            // Captured written source owns the declaration. A conservative
            // refusal must not fall back to a lossy fragment that can erase
            // visibility, attributes, or another source-only property.
            return match extent {
                SourceExcerptExtent::Complete => {
                    import_header(text, language).map(|header| fold_header(header, language))
                }
                SourceExcerptExtent::Truncated => None,
            };
        } else if *extent == SourceExcerptExtent::Complete
            && !trimmed.is_empty()
            && text.trim_start().starts_with(trimmed)
        {
            return Some(fold_header(source, language));
        }
    }
    if trimmed.is_empty() || legacy_encoding(trimmed) {
        return None;
    }
    if kind == Some(DeclarationKind::Import) {
        if matches!(
            trimmed.lines().next(),
            Some("value" | "qualifier" | "namespace" | "type" | "default")
        ) {
            return Import::parse(trimmed, local_name)
                .and_then(|import| import.write(language))
                .map(Cow::Owned);
        }
    }
    Some(fold_header(source, language))
}

/// Borrows only the first written import statement, never its following body.
/// This is a lexical boundary over an already classified import site, not a
/// source parser. Unknown/commented/incomplete syntax stays absent or uses the
/// exact fragment contract below; it is never reconstructed speculatively.
fn import_header(text: &str, language: Language) -> Option<&str> {
    let text = text.trim_start();
    let mut at = 0;
    let starts = match language {
        Language::TypeScript | Language::Go => text.strip_prefix("import").is_some_and(|tail| {
            tail.starts_with(char::is_whitespace) || tail.starts_with(['{', '*', '\'', '"', '('])
        }),
        Language::Python => text.starts_with("import ") || text.starts_with("from "),
        Language::Rust => {
            at = rust_import_start(text)?;
            true
        }
        _ => false,
    };
    if !starts {
        return None;
    }
    // A TS import() expression is not a declaration, even if a stale row says so.
    if language == Language::TypeScript
        && text.strip_prefix("import")?.trim_start().starts_with('(')
    {
        return None;
    }
    let mut stack = Vec::new();
    let mut saw_literal = false;
    while at < text.len() {
        let rest = &text[at..];
        if rest.starts_with("//")
            || rest.starts_with("/*")
            || (language == Language::Python && rest.starts_with('#'))
        {
            return None;
        }
        let end = literal_end(text, at, language)?;
        if end > at {
            saw_literal = true;
            at = end;
            continue;
        }
        let ch = rest.chars().next()?;
        match ch {
            '(' | '[' | '{' => {
                stack.push(ch);
                if stack.len() > 96 {
                    return None;
                }
            }
            ')' | ']' | '}' => {
                let open = match ch {
                    ')' => '(',
                    ']' => '[',
                    _ => '{',
                };
                if stack.pop() != Some(open) {
                    return None;
                }
            }
            ';' if stack.is_empty() => return Some(&text[..at + 1]),
            '\n' | '\r' if stack.is_empty() => {
                let continuation = language == Language::Python && text[..at].ends_with('\\');
                let attributes = language == Language::TypeScript
                    && ["with", "assert"].iter().any(|keyword| {
                        rest.trim_start().strip_prefix(keyword).is_some_and(|tail| {
                            tail.starts_with(char::is_whitespace) || tail.starts_with('{')
                        })
                    });
                if language != Language::Rust && !continuation && !attributes {
                    return (language != Language::TypeScript || saw_literal)
                        .then_some(text[..at].trim_end());
                }
            }
            _ => {}
        }
        at += ch.len_utf8();
    }
    (language != Language::Rust
        && stack.is_empty()
        && (language != Language::TypeScript || saw_literal))
        .then_some(text.trim_end())
}

/// The source compiler has already classified this site as an import. Keep its
/// written visibility, and start boundary scanning after `use` so a line break
/// inside or after `pub(...)` cannot truncate the statement at its modifier.
fn rust_import_start(text: &str) -> Option<usize> {
    let tail = if let Some(tail) = text.strip_prefix("pub") {
        if !tail.starts_with(char::is_whitespace) && !tail.starts_with('(') {
            return None;
        }
        let tail = tail.trim_start();
        let tail = if let Some(restricted) = tail.strip_prefix('(') {
            restricted.split_once(')')?.1
        } else {
            tail
        };
        tail.trim_start()
    } else {
        text
    };
    let after_use = tail.strip_prefix("use")?;
    (after_use.starts_with(char::is_whitespace) || after_use.starts_with(['{', ':']))
        .then_some(text.len() - after_use.len())
}

/// `value\nmodule\nexported` and `qualifier\nmodule` are the actual
/// source compiler contracts; the local binding belongs to the row name.
/// Older namespace/default/type spellings are retained as explicit variants.
struct Import<'a> {
    flavor: ImportFlavor,
    module: &'a str,
    exported: &'a str,
    local: &'a str,
}

#[derive(Clone, Copy)]
enum ImportFlavor {
    Value,
    Type,
    Qualifier,
    Default,
}

impl<'a> Import<'a> {
    fn parse(text: &'a str, local_name: &'a str) -> Option<Self> {
        let mut lines = text.lines().map(str::trim);
        let (flavor, module) = (lines.next()?, lines.next()?);
        if module.is_empty() {
            return None;
        }
        let (flavor, exported) = match flavor {
            "qualifier" => (ImportFlavor::Qualifier, ""),
            "namespace" => (ImportFlavor::Qualifier, lines.next()?),
            "value" => (ImportFlavor::Value, lines.next()?),
            "type" => (ImportFlavor::Type, lines.next()?),
            "default" => (ImportFlavor::Default, lines.next()?),
            _ => return None,
        };
        if lines.next().is_some() {
            return None;
        }
        let local = if local_name.is_empty() {
            exported
        } else {
            local_name
        };
        if !(identifier(local) || (local == "." && matches!(flavor, ImportFlavor::Qualifier))) {
            return None;
        }
        if !matches!(flavor, ImportFlavor::Qualifier) && !identifier(exported) {
            return None;
        }
        Some(Self {
            flavor,
            module,
            exported,
            local,
        })
    }

    fn write(&self, language: Language) -> Option<String> {
        let Self {
            flavor,
            module,
            exported,
            local,
        } = *self;
        if (local == "." && language != Language::Go)
            || (local.contains('$') && language != Language::TypeScript)
        {
            return None;
        }
        let alias = if exported == local {
            String::new()
        } else {
            format!(" as {local}")
        };
        Some(match language {
            Language::TypeScript => {
                let module = serde_json::to_string(module).ok()?;
                match flavor {
                    // This old producer contract does not distinguish a named
                    // unaliased import from a default import. Do not guess.
                    ImportFlavor::Value if exported == local => return None,
                    ImportFlavor::Value => format!("import {{ {exported}{alias} }} from {module}"),
                    ImportFlavor::Type => {
                        format!("import type {{ {exported}{alias} }} from {module}")
                    }
                    ImportFlavor::Qualifier => format!("import * as {local} from {module}"),
                    ImportFlavor::Default => format!("import {local} from {module}"),
                }
            }
            Language::Python => {
                if exported.contains('$') {
                    return None;
                }
                let relative = module.len() - module.trim_start_matches('/').len();
                let module = if relative == 0 {
                    module.to_owned()
                } else {
                    format!(
                        "{}{}",
                        ".".repeat(relative),
                        module[relative..].replace('/', ".")
                    )
                };
                if !module
                    .split('.')
                    .all(|part| part.is_empty() || (identifier(part) && !part.contains('$')))
                {
                    return None;
                }
                match flavor {
                    ImportFlavor::Qualifier if relative > 0 => return None,
                    ImportFlavor::Qualifier => format!("import {module} as {local}"),
                    _ => format!("from {module} import {exported}{alias}"),
                }
            }
            Language::Rust => {
                if !module
                    .split("::")
                    .all(|part| identifier(part) && !part.contains('$'))
                    || exported.contains('$')
                {
                    return None;
                }
                match flavor {
                    ImportFlavor::Qualifier if module == local => format!("use {module};"),
                    ImportFlavor::Qualifier => format!("use {module} as {local};"),
                    _ => format!("use {module}::{exported}{alias};"),
                }
            }
            Language::Go if matches!(flavor, ImportFlavor::Qualifier) => {
                format!("import {local} {}", serde_json::to_string(module).ok()?)
            }
            Language::Go
            | Language::Unknown
            | Language::Java
            | Language::CSharp
            | Language::C
            | Language::Cxx => return None,
        })
    }
}

fn identifier(text: &str) -> bool {
    let mut characters = text.chars();
    characters
        .next()
        .is_some_and(|first| first.is_alphabetic() || matches!(first, '_' | '$'))
        && characters.all(|next| next.is_alphanumeric() || matches!(next, '_' | '$'))
}

/// Legacy strings have no origin discriminator. Only these observed complete
/// machine forms with a hex atom (`x"…"`, not source literal syntax) are
/// distinguishable here. This is deliberately partial legacy cleanup:
/// `tuple[]`, `object[]`, `unknown(error)`, and other ambiguous forms survive.
/// Current semantic producers already choose truthful source output or None.
fn legacy_encoding(text: &str) -> bool {
    const MACHINE_PREFIXES: &[&str] = &[
        "function(parameters=[",
        "nominal(entity(",
        "applied(",
        "external(",
        "reference(mutability=",
        "pointer(mutability=",
        "unknown(",
        "infer(parameter=",
    ];
    text.contains("x\"")
        && MACHINE_PREFIXES
            .iter()
            .any(|prefix| text.starts_with(prefix))
        && balanced_encoding(text)
}

fn balanced_encoding(text: &str) -> bool {
    let mut stack = Vec::new();
    let mut bytes = text.bytes().peekable();
    let mut opened = false;
    while let Some(byte) = bytes.next() {
        match byte {
            b'(' | b'[' => {
                opened = true;
                stack.push(byte);
                if stack.len() > 96 {
                    return false;
                }
            }
            b')' | b']' => {
                if stack.pop() != Some(if byte == b')' { b'(' } else { b'[' }) {
                    return false;
                }
                if stack.is_empty() && bytes.peek().is_some() {
                    return false;
                }
            }
            b'"' => {
                let mut length = 0;
                loop {
                    match bytes.next() {
                        Some(b'"') if length % 2 == 0 => break,
                        Some(value) if value.is_ascii_hexdigit() => length += 1,
                        _ => return false,
                    }
                }
            }
            value
                if value.is_ascii_alphanumeric()
                    || matches!(value, b'-' | b'.' | b'_' | b'=' | b',') => {}
            _ => return false,
        }
    }
    opened && stack.is_empty()
}

/// Compact layout outside literals. Punctuation (including singleton-tuple
/// commas) is retained. Ambiguous interpolated/raw syntax keeps the full input.
fn fold_header(text: &str, language: Language) -> Cow<'_, str> {
    if !matches!(
        language,
        Language::Rust | Language::Python | Language::Go | Language::TypeScript
    ) {
        return Cow::Borrowed(text);
    }
    if language == Language::Python && (text.contains("\\\n") || text.contains("\\\r\n")) {
        return Cow::Borrowed(text);
    }
    let mut output = String::with_capacity(text.len());
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        if rest.starts_with("//")
            || rest.starts_with("/*")
            || (language == Language::Python && rest.starts_with('#'))
            || (language == Language::TypeScript && rest.starts_with(['`', '/']))
            || (language == Language::Cxx && rest.starts_with("R\""))
        {
            return Cow::Borrowed(text);
        }
        let Some(span) = literal_end(text, at, language) else {
            return Cow::Borrowed(text);
        };
        if span > at {
            output.push_str(&text[at..span]);
            at = span;
            continue;
        }
        let ch = rest.chars().next().expect("nonempty suffix");
        if ch.is_whitespace() {
            at += ch.len_utf8();
            while let Some(next) = text[at..]
                .chars()
                .next()
                .filter(|next| next.is_whitespace())
            {
                at += next.len_utf8();
            }
            let closes = text[at..].starts_with([')', ']']);
            if at < text.len() && !output.is_empty() && !output.ends_with(['(', '[']) && !closes {
                output.push(' ');
            }
        } else {
            output.push(ch);
            at += ch.len_utf8();
        }
    }
    if output == text {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(output)
    }
}

/// `Some(at)` is ordinary syntax; `None` conservatively keeps an unclosed or
/// interpolated literal's entire signature. Every captured literal span is copied.
fn literal_end(text: &str, at: usize, language: Language) -> Option<usize> {
    let rest = &text[at..];
    if language == Language::Rust && rest.starts_with('r') {
        let hashes = rest[1..].bytes().take_while(|byte| *byte == b'#').count();
        if rest[1 + hashes..].starts_with('"') {
            let close = format!("\"{}", "#".repeat(hashes));
            let start = at + hashes + 2;
            return text[start..]
                .find(&close)
                .map(|end| start + end + close.len());
        }
    }
    let quote = rest.chars().next()?;
    if quote == '`' && language == Language::Go {
        return rest[1..].find('`').map(|end| at + end + 2);
    }
    if !matches!(quote, '\'' | '"') {
        return Some(at);
    }
    if language == Language::Rust && quote == '\'' {
        let mut chars = rest[1..].chars();
        if chars.next() != Some('\\') && chars.next() != Some('\'') {
            return Some(at);
        }
    }
    if language == Language::Python {
        let prefix = text[..at]
            .rsplit(|ch: char| !ch.is_alphabetic())
            .next()
            .unwrap_or_default();
        if ["f", "rf", "fr", "t", "tr", "rt"]
            .iter()
            .any(|interpolated| prefix.eq_ignore_ascii_case(interpolated))
        {
            return None;
        }
    }
    let triple =
        language == Language::Python && (rest.starts_with("\"\"\"") || rest.starts_with("'''"));
    let delimiter = if triple { &rest[..3] } else { &rest[..1] };
    let mut cursor = at + delimiter.len();
    while cursor < text.len() {
        let tail = &text[cursor..];
        if tail.starts_with(delimiter) {
            return Some(cursor + delimiter.len());
        }
        let ch = tail.chars().next()?;
        cursor += ch.len_utf8();
        if ch == '\\' {
            cursor += text[cursor..].chars().next()?.len_utf8();
        }
    }
    None
}

/// Fold only physical layout newlines outside source regions that this display
/// scanner can prove are ordinary syntax. Quoted values are copied byte-for-
/// byte; their physical controls are escaped later by the bounded preview.
pub(super) fn fold_preview_layout_newlines(text: &str, language: Language) -> Cow<'_, str> {
    if !matches!(
        language,
        Language::Rust | Language::Python | Language::Go | Language::TypeScript
    ) || (language == Language::Python && (text.contains("\\\n") || text.contains("\\\r\n")))
    {
        return Cow::Borrowed(text);
    }

    let mut output = String::with_capacity(text.len());
    let mut at = 0;
    while at < text.len() {
        let rest = &text[at..];
        if rest.starts_with("//")
            || rest.starts_with("/*")
            || (language == Language::Python && rest.starts_with('#'))
            || (language == Language::TypeScript
                && (rest.starts_with('/') || rest.as_bytes().first() == Some(&0x60)))
        {
            return Cow::Borrowed(text);
        }
        let Some(span) = literal_end(text, at, language) else {
            return Cow::Borrowed(text);
        };
        if span > at {
            output.push_str(&text[at..span]);
            at = span;
            continue;
        }
        let ch = rest.chars().next().expect("nonempty suffix");
        let closes_signature = if language == Language::TypeScript && ch == ')' {
            let after_close = &rest[ch.len_utf8()..];
            after_close.trim_start().starts_with(':')
                || after_close.trim_start().starts_with("=>")
        } else {
            false
        };
        if closes_signature {
            // TypeScript permits a final comma in parameter lists. It is
            // harmless source syntax, but looks like a duplicate argument
            // separator in the compact, single-line preview. The scanner has
            // already copied literals atomically, and comments/templates
            // make this routine return the original text above, so only
            // syntax bytes immediately before this closing paren are eligible.
            let mut comma = output.len();
            while comma > 0 {
                let Some(previous) = output[..comma].chars().next_back() else {
                    break;
                };
                if !matches!(previous, ' ' | '\t') {
                    break;
                }
                comma -= previous.len_utf8();
            }
            if comma > 0 && output[..comma].ends_with(',') {
                output.truncate(comma - 1);
            }
        }
        if ch == '\n' || rest.starts_with("\r\n") {
            at += if rest.starts_with("\r\n") { 2 } else { 1 };
            while output
                .chars()
                .next_back()
                .is_some_and(|previous| matches!(previous, ' ' | '\t'))
            {
                output.pop();
            }
            if !output.ends_with(' ') {
                output.push(' ');
            }
            while text[at..]
                .chars()
                .next()
                .is_some_and(|next| matches!(next, ' ' | '\t'))
            {
                at += text[at..]
                    .chars()
                    .next()
                    .expect("indentation exists")
                    .len_utf8();
            }
        } else {
            output.push(ch);
            at += ch.len_utf8();
        }
    }
    if output == text {
        Cow::Borrowed(text)
    } else {
        Cow::Owned(output)
    }
}
