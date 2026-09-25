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
//! - `project(<name> ... HOMEPAGE_URL "<url>" ...)` (CMake 3.12+) →
//!   `facts.repository`, and sets `facts.documentation`.
//! - `set(CPACK_RESOURCE_FILE_LICENSE "<path>")` → `facts.has_license_file`
//!   (the dedicated CPack variable naming the packaged license file).
//! - `find_package(<Name> ...)` → [`DependencyMechanism::FindPackage`].
//! - `pkg_check_modules(<PREFIX> [REQUIRED|QUIET|IMPORTED_TARGET|GLOBAL]
//!   <names>...)` → [`DependencyMechanism::PkgConfig`].
//! - `FetchContent_Declare(<n> GIT_REPOSITORY <url> GIT_TAG <ref> ...)` →
//!   [`DependencyMechanism::FetchContent`] with the repository URL as token.
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
            // CMake 3.12+ `project(... HOMEPAGE_URL "...")`.
            if manifest.facts.repository.is_none()
                && let Some(url) = extract_keyword_value(&arguments, "HOMEPAGE_URL")
            {
                manifest.facts.repository = Some(url);
                manifest.facts.documentation = true;
            }
        } else if command.eq_ignore_ascii_case("set") {
            // `set(CPACK_RESOURCE_FILE_LICENSE "LICENSE")` — the dedicated
            // CPack variable naming the packaged license file, same idea as
            // Cargo's `license-file` key.
            if !manifest.facts.has_license_file
                && arguments
                    .first()
                    .is_some_and(|name| name.eq_ignore_ascii_case("CPACK_RESOURCE_FILE_LICENSE"))
                && arguments.get(1).is_some_and(|v| !v.trim().is_empty())
            {
                manifest.facts.has_license_file = true;
            }
        } else if command.eq_ignore_ascii_case("find_package") {
            if let Some(name) = arguments.first().filter(|s| !s.is_empty()) {
                let mut record =
                    DependencyRecord::new(name.clone(), DependencyMechanism::FindPackage);
                if let Some(version) = arguments.get(1).filter(|text| {
                    text.chars()
                        .next()
                        .is_some_and(|char| char.is_ascii_digit())
                }) {
                    record.requirement = Some(version.clone());
                }
                manifest.push_dependency(record);
            }
        } else if command.eq_ignore_ascii_case("pkg_check_modules")
            || command.eq_ignore_ascii_case("pkg_search_module")
        {
            for (token, requirement) in extract_pkg_check_modules_deps(&arguments) {
                let mut record = DependencyRecord::new(token, DependencyMechanism::PkgConfig);
                record.requirement = requirement;
                manifest.push_dependency(record);
            }
        } else if command.eq_ignore_ascii_case("fetchcontent_declare")
            && let Some((url, tag)) = extract_fetchcontent(&arguments)
        {
            let mut record = DependencyRecord::new(url, DependencyMechanism::FetchContent);
            record.requirement = tag;
            manifest.push_dependency(record);
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
/// - Unquoted tokens (any sequence of non-whitespace, non-paren, non-`#`
///   chars).
/// - Nested `(...)` (depth tracking; inner parens are consumed but not returned
///   as separate tokens — they are concatenated into the parent token).
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
            // Quoted argument. Collected as raw bytes and decoded once at the
            // end (NOT `byte as char` per-byte — that reinterprets each byte
            // of a multi-byte UTF-8 sequence as its own Latin-1 code point,
            // corrupting any non-ASCII content, e.g. project descriptions in
            // Japanese/emoji/etc.). `"` and `\` are single ASCII bytes that
            // can never occur as a lead or continuation byte of a valid
            // multi-byte UTF-8 sequence, so byte-level scanning for them
            // never splits a multi-byte character; `from_utf8_lossy` is a
            // pure safety net (never panics even if that invariant were
            // somehow violated).
            *position += 1;
            let mut value_bytes: Vec<u8> = Vec::new();
            while *position < bytes.len() {
                let inner = bytes[*position];
                if inner == b'\\' && *position + 1 < bytes.len() {
                    *position += 1;
                    value_bytes.push(bytes[*position]);
                } else if inner == b'"' {
                    *position += 1;
                    break;
                } else {
                    value_bytes.push(inner);
                }
                *position += 1;
            }
            arguments.push(String::from_utf8_lossy(&value_bytes).into_owned());
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

// ── Command handlers
// ──────────────────────────────────────────────────────────

/// Extract the `DESCRIPTION` value from a `project(...)` argument list.
///
/// Returns the string following the `DESCRIPTION` keyword (case-insensitive),
/// or `None` when the keyword is absent.
fn extract_project_description(arguments: &[String]) -> Option<String> {
    extract_keyword_value(arguments, "DESCRIPTION")
}

/// Extract the value immediately following a single-valued keyword argument
/// (case-insensitive) from a command's already-tokenized argument list, e.g.
/// the `HOMEPAGE_URL` in `project(foo HOMEPAGE_URL "https://...")`. Returns
/// `None` when the keyword is absent or its value is empty/whitespace.
fn extract_keyword_value(arguments: &[String], keyword: &str) -> Option<String> {
    let mut iterator = arguments.iter();
    while let Some(arg) = iterator.next() {
        if arg.eq_ignore_ascii_case(keyword)
            && let Some(value) = iterator.next()
        {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_owned());
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
fn extract_pkg_check_modules_deps(arguments: &[String]) -> Vec<(String, Option<String>)> {
    let mut tokens = Vec::new();
    let mut arguments_iterator = arguments.iter();

    // Skip the PREFIX argument.
    let _ = arguments_iterator.next();

    for argument in arguments_iterator {
        let upper = argument.to_ascii_uppercase();
        if PKG_CHECK_SKIP_KEYWORDS.contains(&upper.as_str()) {
            continue;
        }
        let (name, requirement) = split_version_constraint(argument);
        if !name.is_empty() {
            tokens.push((
                name.to_owned(),
                requirement
                    .filter(|text| !text.is_empty())
                    .map(str::to_owned),
            ));
        }
    }

    tokens
}

/// Split `glib-2.0>=2.40` into the module name and the constraint.
fn split_version_constraint(spec: &str) -> (&str, Option<&str>) {
    match spec.find(['>', '<', '=', '!']) {
        Some(position) if position > 0 => {
            (spec[..position].trim_end(), Some(spec[position..].trim()))
        }
        _ => (spec.trim(), None),
    }
}

/// Extract the `GIT_REPOSITORY` URL from a `FetchContent_Declare` argument
/// list.
///
/// Returns the value following the `GIT_REPOSITORY` keyword, or `None` when
/// absent.
fn extract_fetchcontent(arguments: &[String]) -> Option<(String, Option<String>)> {
    let mut url = None;
    let mut tag = None;
    let mut index = 1;
    while index < arguments.len() {
        let keyword = &arguments[index];
        let value = arguments
            .get(index + 1)
            .map(|text| text.trim())
            .filter(|text| !text.is_empty());
        if keyword.eq_ignore_ascii_case("GIT_REPOSITORY") {
            url = value.map(str::to_owned);
        } else if keyword.eq_ignore_ascii_case("GIT_TAG") {
            tag = value.map(str::to_owned);
        }
        index += 1;
    }
    url.map(|url| (url, tag))
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
        assert_eq!(manifest.dependencies[1].requirement.as_deref(), Some("1.1"));
        assert!(manifest.dependencies[0].requirement.is_none());
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
            manifest.dependencies[0].requirement.as_deref(),
            Some(">=2.40")
        );
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
        assert_eq!(
            manifest.dependencies[0].requirement.as_deref(),
            Some("v3.11.2")
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

    // ── `HOMEPAGE_URL` / `CPACK_RESOURCE_FILE_LICENSE` (P6 gap fill) ─────────

    #[test]
    fn parse_cmake_project_homepage_url() {
        let text = r#"project(MyLib VERSION 1.0 HOMEPAGE_URL "https://github.com/example/mylib" LANGUAGES CXX)"#;
        let manifest = parse(text);
        assert_eq!(
            manifest.facts.repository.as_deref(),
            Some("https://github.com/example/mylib")
        );
        assert!(manifest.facts.documentation);
    }

    #[test]
    fn parse_cmake_project_no_homepage_url_leaves_repository_none() {
        let text = r#"project(MyLib VERSION 1.0)"#;
        let manifest = parse(text);
        assert!(manifest.facts.repository.is_none());
        assert!(!manifest.facts.documentation);
    }

    #[test]
    fn parse_cmake_cpack_resource_file_license() {
        let text = r#"set(CPACK_RESOURCE_FILE_LICENSE "${CMAKE_SOURCE_DIR}/LICENSE")"#;
        let manifest = parse(text);
        assert!(manifest.facts.has_license_file);
    }

    #[test]
    fn parse_cmake_set_unrelated_variable_does_not_set_license_flag() {
        let text = r#"set(CMAKE_CXX_STANDARD 17)"#;
        let manifest = parse(text);
        assert!(!manifest.facts.has_license_file);
    }

    #[test]
    fn parse_cmake_set_cpack_license_case_insensitive_command() {
        let text = r#"SET(CPACK_RESOURCE_FILE_LICENSE LICENSE.txt)"#;
        let manifest = parse(text);
        assert!(manifest.facts.has_license_file);
    }

    #[test]
    fn parse_cmake_real_world_project_shape() {
        // Shaped after a real top-level CMakeLists.txt (nlohmann/json-style).
        let text = r#"
cmake_minimum_required(VERSION 3.14)
project(nlohmann_json
    VERSION 3.11.2
    DESCRIPTION "JSON for Modern C++"
    HOMEPAGE_URL "https://github.com/nlohmann/json"
    LANGUAGES CXX
)
set(CPACK_RESOURCE_FILE_LICENSE "${CMAKE_CURRENT_SOURCE_DIR}/LICENSE.MIT")
find_package(Threads REQUIRED)
"#;
        let manifest = parse(text);
        assert_eq!(
            manifest.facts.description.as_deref(),
            Some("JSON for Modern C++")
        );
        assert_eq!(
            manifest.facts.repository.as_deref(),
            Some("https://github.com/nlohmann/json")
        );
        assert!(manifest.facts.documentation);
        assert!(manifest.facts.has_license_file);
        assert!(manifest.dependencies.iter().any(|d| d.token == "Threads"));
    }

    // ── Hostile-input hardening ──────────────────────────────────────────────

    #[test]
    fn parse_cmake_unterminated_quoted_description_no_panic() {
        let text = r#"project(foo DESCRIPTION "unterminated"#;
        let manifest = parse(text);
        let _ = manifest;
    }

    #[test]
    fn parse_cmake_unicode_description() {
        let text = "project(foo DESCRIPTION \"日本語 😀 説明\")\n";
        let manifest = parse(text);
        assert_eq!(
            manifest.facts.description.as_deref(),
            Some("日本語 😀 説明")
        );
    }

    #[test]
    fn parse_cmake_crlf_line_endings() {
        let text = "find_package(ZLIB REQUIRED)\r\nfind_package(OpenSSL)\r\n";
        let manifest = parse(text);
        assert_eq!(manifest.dependencies.len(), 2);
    }

    #[test]
    fn parse_cmake_duplicate_project_calls_first_wins() {
        let text = r#"
project(first DESCRIPTION "first description" HOMEPAGE_URL "https://first.example")
project(second DESCRIPTION "second description" HOMEPAGE_URL "https://second.example")
"#;
        let manifest = parse(text);
        assert_eq!(
            manifest.facts.description.as_deref(),
            Some("first description")
        );
        assert_eq!(
            manifest.facts.repository.as_deref(),
            Some("https://first.example")
        );
    }

    #[test]
    fn parse_cmake_enormous_quoted_argument_no_panic() {
        let huge = "x".repeat(2 * 1024 * 1024);
        let text = format!(r#"project(foo DESCRIPTION "{huge}")"#);
        let manifest = parse(&text);
        assert_eq!(
            manifest.facts.description.as_deref().map(str::len),
            Some(huge.len())
        );
    }

    #[test]
    fn parse_cmake_null_bytes_in_input_no_panic() {
        let text = "find_package(ZLIB)\0garbage\0more";
        let manifest = parse(text);
        assert!(manifest.dependencies.iter().any(|d| d.token == "ZLIB"));
    }
}
