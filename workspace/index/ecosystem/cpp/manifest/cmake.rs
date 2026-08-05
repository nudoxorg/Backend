//! Parser for `CMakeLists.txt` manifest files (REGISTRYLESS §8).
//!
//! Extracts dependency declarations and the project description using a
//! small hand-rolled tokenizer that handles CMake's command syntax:
//! case-insensitive command names, `#` line comments, quoted arguments,
//! and argument lists that span multiple lines inside balanced parentheses.
//!
//! The following CMake commands are recognised:
//!
//! - `project(<name> ... DESCRIPTION "<text>" ...)` → `facts.description`.
//! - `find_package(<Name> ...)` → [`DependencyMechanism::FindPackage`].
//! - `pkg_check_modules(<PREFIX> [REQUIRED|QUIET|IMPORTED_TARGET|GLOBAL] <names>...)`
//!   → [`DependencyMechanism::PkgConfig`].
//! - `FetchContent_Declare(<n> GIT_REPOSITORY <url> GIT_TAG <ref> ...)`
//!   → [`DependencyMechanism::FetchContent`] with the repository URL as token.
//!
//! All other commands are silently skipped. The parser never panics.

use super::{CppManifest, DependencyMechanism, DependencyRecord};

/// Parse a `CMakeLists.txt` file.
///
/// Returns a [`CppManifest`] with any extracted description and dependency
/// records. All extraction is best-effort; unrecognised or malformed
/// constructs are silently skipped.
pub fn parse(text: &str) -> CppManifest {
    let mut manifest = CppManifest::default();
    let mut position = 0;
    let bytes = text.as_bytes();

    while position < bytes.len() {
        skip_whitespace_and_comments(text, &mut position);
        if position >= bytes.len() {
            break;
        }

        // Read the command name (identifier characters).
        let command_start = position;
        while position < bytes.len() && is_identifier_char(bytes[position]) {
            position += 1;
        }
        if position == command_start {
            // Not an identifier — skip one character and retry.
            position += 1;
            continue;
        }
        let command = &text[command_start..position];

        // Skip whitespace between command name and `(`.
        skip_whitespace_and_comments(text, &mut position);

        if position >= bytes.len() || bytes[position] != b'(' {
            continue;
        }
        // Consume the opening `(`.
        position += 1;

        // Collect the balanced argument body as individual argument tokens.
        let arguments = collect_arguments(text, &mut position);

        // Dispatch to the appropriate handler.
        if command.eq_ignore_ascii_case("project") {
            if manifest.facts.description.is_none() {
                manifest.facts.description = extract_project_description(&arguments);
            }
        } else if command.eq_ignore_ascii_case("find_package") {
            if let Some(name) = arguments.first().filter(|s| !s.is_empty()) {
                manifest.push_dependency(DependencyRecord::new(
                    name.clone(),
                    DependencyMechanism::FindPackage,
                ));
            }
        } else if command.eq_ignore_ascii_case("pkg_check_modules")
            || command.eq_ignore_ascii_case("pkg_search_module")
        {
            for token in extract_pkg_check_modules_deps(&arguments) {
                manifest
                    .push_dependency(DependencyRecord::new(token, DependencyMechanism::PkgConfig));
            }
        } else if command.eq_ignore_ascii_case("fetchcontent_declare") {
            if let Some(url) = extract_fetchcontent_url(&arguments) {
                manifest.push_dependency(DependencyRecord::new(
                    url,
                    DependencyMechanism::FetchContent,
                ));
            }
        }
    }

    manifest
}

// ── Tokenizer ────────────────────────────────────────────────────────────────

/// Advance `position` past any whitespace (including newlines) and `#`-line
/// comments.
fn skip_whitespace_and_comments(text: &str, position: &mut usize) {
    let bytes = text.as_bytes();
    while *position < bytes.len() {
        let byte = bytes[*position];
        if byte == b'#' {
            // Skip to end of line.
            while *position < bytes.len() && bytes[*position] != b'\n' {
                *position += 1;
            }
        } else if byte.is_ascii_whitespace() {
            *position += 1;
        } else {
            break;
        }
    }
}

/// Returns `true` for characters valid in a CMake command name or unquoted
/// argument (letters, digits, underscore, hyphen, dot, slash, colon, plus).
fn is_identifier_char(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(byte, b'_' | b'-' | b'.' | b'/' | b':' | b'+' | b'@' | b'~')
}

/// Collect individual argument tokens from the current position until the
/// matching closing `)`. Advances `position` past the `)`.
///
/// Handles:
/// - Quoted strings `"..."` (returned with quotes stripped).
/// - Unquoted tokens (any sequence of non-whitespace, non-paren, non-`#` chars).
/// - Nested `(...)` (depth tracking; inner parens are consumed but not
///   returned as separate tokens — they are concatenated into the parent token).
/// - `#`-line comments (skipped).
fn collect_arguments(text: &str, position: &mut usize) -> Vec<String> {
    let mut arguments: Vec<String> = Vec::new();
    let bytes = text.as_bytes();
    let mut depth: u32 = 1;

    while *position < bytes.len() && depth > 0 {
        let byte = bytes[*position];

        if byte == b'#' {
            // Skip line comment.
            while *position < bytes.len() && bytes[*position] != b'\n' {
                *position += 1;
            }
        } else if byte.is_ascii_whitespace() {
            *position += 1;
        } else if byte == b'"' {
            // Quoted argument.
            *position += 1;
            let mut value = String::new();
            while *position < bytes.len() {
                let inner = bytes[*position];
                if inner == b'\\' && *position + 1 < bytes.len() {
                    *position += 1;
                    value.push(bytes[*position] as char);
                } else if inner == b'"' {
                    *position += 1;
                    break;
                } else {
                    value.push(inner as char);
                }
                *position += 1;
            }
            arguments.push(value);
        } else if byte == b'(' {
            depth += 1;
            *position += 1;
        } else if byte == b')' {
            depth -= 1;
            if depth == 0 {
                *position += 1;
                break;
            }
            *position += 1;
        } else {
            // Unquoted token.
            let start = *position;
            while *position < bytes.len() {
                let b = bytes[*position];
                if b.is_ascii_whitespace() || b == b')' || b == b'(' || b == b'"' || b == b'#' {
                    break;
                }
                *position += 1;
            }
            let token = &text[start..*position];
            if !token.is_empty() {
                arguments.push(token.to_owned());
            }
        }
    }

    arguments
}

// ── Command handlers ──────────────────────────────────────────────────────────

/// Extract the `DESCRIPTION` value from a `project(...)` argument list.
///
/// Returns the string following the `DESCRIPTION` keyword (case-insensitive),
/// or `None` when the keyword is absent.
fn extract_project_description(arguments: &[String]) -> Option<String> {
    let mut iterator = arguments.iter();
    while let Some(arg) = iterator.next() {
        if arg.eq_ignore_ascii_case("DESCRIPTION") {
            if let Some(value) = iterator.next() {
                let trimmed = value.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_owned());
                }
            }
        }
    }
    None
}

/// Keywords to skip in the `pkg_check_modules` argument list (first position
/// is the output prefix; subsequent positions may be flag keywords).
const PKG_CHECK_SKIP_KEYWORDS: &[&str] = &[
    "REQUIRED",
    "QUIET",
    "IMPORTED_TARGET",
    "GLOBAL",
    "NO_CMAKE_PATH",
    "NO_CMAKE_ENVIRONMENT_PATH",
];

/// Extract pkg-config module name tokens from `pkg_check_modules` /
/// `pkg_search_module` arguments.
///
/// The first argument is the CMake output variable prefix — skipped. The
/// following arguments are module specs such as `glib-2.0>=2.40`. Version
/// constraint suffixes (`>=`, `<=`, `=`, `>`, `<`) and everything after them
/// are stripped; the package name token is returned.
fn extract_pkg_check_modules_deps(arguments: &[String]) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut arguments_iterator = arguments.iter();

    // Skip the PREFIX argument.
    let _ = arguments_iterator.next();

    for argument in arguments_iterator {
        let upper = argument.to_ascii_uppercase();
        if PKG_CHECK_SKIP_KEYWORDS.contains(&upper.as_str()) {
            continue;
        }
        // Strip version constraint suffix: split on `>=`, `<=`, `!=`, `>`, `<`, `=`.
        let name = strip_version_constraint(argument);
        if !name.is_empty() {
            tokens.push(name.to_owned());
        }
    }

    tokens
}

/// Strip a version constraint from a pkg-config module spec.
///
/// Examples: `glib-2.0>=2.40` → `glib-2.0`, `foo!=1.0` → `foo`,
/// `bar` → `bar`.
fn strip_version_constraint(spec: &str) -> &str {
    // Find the first `>`, `<`, `=`, or `!` character.
    if let Some(position) = spec.find(|c| matches!(c, '>' | '<' | '=' | '!')) {
        spec[..position].trim_end()
    } else {
        spec.trim()
    }
}

/// Extract the `GIT_REPOSITORY` URL from a `FetchContent_Declare` argument list.
///
/// Returns the value following the `GIT_REPOSITORY` keyword, or `None` when
/// absent.
fn extract_fetchcontent_url(arguments: &[String]) -> Option<String> {
    let mut iterator = arguments.iter();
    // Skip the content name argument.
    let _ = iterator.next();
    while let Some(arg) = iterator.next() {
        if arg.eq_ignore_ascii_case("GIT_REPOSITORY") {
            if let Some(url) = iterator.next() {
                let trimmed = url.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_owned());
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cmake_find_package() {
        let text = "cmake_minimum_required(VERSION 3.14)\nfind_package(ZLIB REQUIRED)\nfind_package(OpenSSL 1.1)\n";
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 2);
        assert_eq!(manifest.dependencies[0].token, "ZLIB");
        assert_eq!(
            manifest.dependencies[0].mechanism,
            DependencyMechanism::FindPackage
        );
        assert_eq!(manifest.dependencies[1].token, "OpenSSL");
    }

    #[test]
    fn parse_cmake_project_description() {
        let text = r#"project(MyLib VERSION 1.0 DESCRIPTION "A useful C++ library" LANGUAGES CXX)"#;
        let manifest = parse(text);
        assert_eq!(
            manifest.facts.description.as_deref(),
            Some("A useful C++ library")
        );
    }

    #[test]
    fn parse_cmake_pkg_check_modules() {
        let text = "pkg_check_modules(GLIB REQUIRED glib-2.0>=2.40 gio-2.0)\n";
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 2);
        assert_eq!(manifest.dependencies[0].token, "glib-2.0");
        assert_eq!(
            manifest.dependencies[0].mechanism,
            DependencyMechanism::PkgConfig
        );
        assert_eq!(manifest.dependencies[1].token, "gio-2.0");
    }

    #[test]
    fn parse_cmake_fetchcontent_declare() {
        let text = r#"
FetchContent_Declare(
    json
    GIT_REPOSITORY https://github.com/nlohmann/json.git
    GIT_TAG v3.11.2
)
"#;
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 1);
        assert_eq!(
            manifest.dependencies[0].token,
            "https://github.com/nlohmann/json.git"
        );
        assert_eq!(
            manifest.dependencies[0].mechanism,
            DependencyMechanism::FetchContent
        );
    }

    #[test]
    fn parse_cmake_line_comments_skipped() {
        let text = "# This is a top comment\nfind_package(ZLIB) # inline comment\n";
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 1);
        assert_eq!(manifest.dependencies[0].token, "ZLIB");
    }

    #[test]
    fn parse_cmake_empty_input_does_not_panic() {
        let manifest = parse("");
        assert_eq!(manifest, CppManifest::default());
    }

    #[test]
    fn parse_cmake_case_insensitive_commands() {
        // CMake command names are case-insensitive.
        let text = "FIND_PACKAGE(Boost COMPONENTS filesystem)\n";
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 1);
        assert_eq!(manifest.dependencies[0].token, "Boost");
    }

    #[test]
    fn parse_cmake_multiline_find_package() {
        let text = "find_package(\n    Threads\n    REQUIRED\n)\n";
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 1);
        assert_eq!(manifest.dependencies[0].token, "Threads");
    }
}
