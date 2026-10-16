/// Validate JSON syntax and return parse diagnostics for violations.
///
/// TypeScript's JSON parser enforces strict JSON rules when parsing `.json` files.
/// This validates property names must be double-quoted string literals (TS1327),
/// and that every property value and array element is one of tsc's
/// `validateJsonValue` shapes: a double-quoted string, a numeric literal
/// (optionally `-`-prefixed), `true`/`false`/`null`, or a nested object/array
/// (TS1328). Violations include single-quoted strings, computed property
/// names (`[expr]`), and unquoted identifiers used as a property name or
/// value.
fn legacy_json_syntax_diagnostics(source: &str) -> Vec<ParseDiagnostic> {
    let mut diagnostics = Vec::new();
    let bytes = source.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    let is_ident_start = |b: u8| b.is_ascii_alphabetic() || b == b'_' || b == b'$';
    let is_ident_part = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';

    // Root-level recovery for invalid bare identifier runs in JSON files.
    // tsc emits a specific sequence:
    //   first identifier  -> TS1005 "'{' expected." + TS1136
    //   next identifiers  -> TS1005 "',' expected." + TS1136
    //   end of run        -> TS1005 "'}' expected."
    //
    // Valid JSON roots `true` / `false` / `null` are explicitly allowed.
    // Leading trivia (whitespace and comments) is skipped first, so a bare
    // identifier root preceded by a `// header` comment still triggers the
    // recovery rather than being masked by the unrecognized `/`.
    let j = skip_trivia(bytes, 0);
    if j < len && is_ident_start(bytes[j]) {
        let mut spans: Vec<(usize, usize)> = Vec::new();
        let mut k = j;
        loop {
            let start = k;
            while k < len && is_ident_part(bytes[k]) {
                k += 1;
            }
            spans.push((start, k));

            // Trivia (whitespace and comments alike) separates identifiers in
            // this recovery run, matching how tsc's scanner tokenizes the same
            // bytes — so `foo /*c*/ bar` recovers identically to `foo bar`.
            k = skip_trivia(bytes, k);

            if k < len && is_ident_start(bytes[k]) {
                continue;
            }
            break;
        }

        if k >= len {
            let single_keyword_root = spans.len() == 1
                && std::str::from_utf8(&bytes[spans[0].0..spans[0].1])
                    .map(|s| matches!(s, "true" | "false" | "null"))
                    .unwrap_or(false);

            if !single_keyword_root {
                for (idx, (start, _end)) in spans.iter().enumerate() {
                    let expected_msg = if idx == 0 {
                        "'{' expected."
                    } else {
                        "',' expected."
                    };
                    diagnostics.push(ParseDiagnostic {
                        start: *start as u32,
                        length: 1,
                        message: expected_msg.to_string(),
                        code: tsz_common::diagnostics::diagnostic_codes::EXPECTED,
                        related: None,
                    });
                    diagnostics.push(ParseDiagnostic {
                        start: *start as u32,
                        length: 1,
                        message: tsz_common::diagnostics::diagnostic_messages::PROPERTY_ASSIGNMENT_EXPECTED.to_string(),
                        code: tsz_common::diagnostics::diagnostic_codes::PROPERTY_ASSIGNMENT_EXPECTED,
                        related: None,
                    });
                }
                if let Some((_, end)) = spans.last() {
                    diagnostics.push(ParseDiagnostic {
                        start: *end as u32,
                        length: 1,
                        message: "'}' expected.".to_string(),
                        code: tsz_common::diagnostics::diagnostic_codes::EXPECTED,
                        related: None,
                    });
                }
            }
        }
    }

    // Track whether we're inside an object and expecting a property name.
    // JSON property names must be double-quoted strings per the JSON spec.
    // We use a simple state machine: after `{` or `,` inside an object,
    // the next non-whitespace token must be `"` (property name) or `}` (end).
    let mut object_depth: i32 = 0;
    let mut array_depth: i32 = 0;
    let mut expecting_property_name = false;
    let mut expecting_value = false;

    while i < len {
        // Skip whitespace and JSON comment trivia (`//` line and `/* */`
        // block). tsc's JSON scanner treats both as trivia. This runs before
        // value/property classification below so a comment sitting between a
        // property's `:` and its value is skipped rather than mis-reported as
        // TS1328. Strings are consumed atomically by the string-skip block
        // further down, so a `//` or `/* */` *inside* a string never reaches
        // this point and stays part of the value.
        i = skip_trivia(bytes, i);
        if i >= len {
            break;
        }

        let b = bytes[i];

        // When expecting a property value (just past a property's `:`) or an
        // array element (just past `[` or a `,` inside an array), check what
        // the value starts with. Valid starts fall through to the normal
        // handling below (e.g. `{`/`[` still open a nested object/array, `"`
        // is still skipped as a string); this block only decides validity and
        // reports TS1328/TS1327 for the ones that are not.
        if expecting_value && (object_depth > 0 || array_depth > 0) {
            let is_valid_value_start = match b {
                // `"`/`{`/`[` are real value starts. `}`/`]`/`,` are not a
                // value themselves, but an empty array/object or a recovery
                // point for a missing element/value; leave those to the
                // dedicated structural handling below rather than guessing
                // at a diagnostic here.
                b'"' | b'{' | b'[' | b'}' | b']' | b',' | b'0'..=b'9' => true,
                b'-' => i + 1 < len && bytes[i + 1].is_ascii_digit(),
                b't' => matches_json_keyword(bytes, i, len, b"true", is_ident_part),
                b'f' => matches_json_keyword(bytes, i, len, b"false", is_ident_part),
                b'n' => matches_json_keyword(bytes, i, len, b"null", is_ident_part),
                b'\'' => {
                    // Single-quoted value: same quote-style diagnostic as a
                    // single-quoted property name, not TS1328.
                    diagnostics.push(ParseDiagnostic {
                        start: i as u32,
                        length: 1,
                        message: tsz_common::diagnostics::diagnostic_messages::STRING_LITERAL_WITH_DOUBLE_QUOTES_EXPECTED.to_string(),
                        code: tsz_common::diagnostics::diagnostic_codes::STRING_LITERAL_WITH_DOUBLE_QUOTES_EXPECTED,
                        related: None,
                    });
                    true
                }
                _ => false,
            };
            if !is_valid_value_start {
                diagnostics.push(ParseDiagnostic {
                    start: i as u32,
                    length: 1,
                    message: tsz_common::diagnostics::diagnostic_messages::PROPERTY_VALUE_CAN_ONLY_BE_STRING_LITERAL_NUMERIC_LITERAL_TRUE_FALSE_NULL_OBJECT.to_string(),
                    code: tsz_common::diagnostics::diagnostic_codes::PROPERTY_VALUE_CAN_ONLY_BE_STRING_LITERAL_NUMERIC_LITERAL_TRUE_FALSE_NULL_OBJECT,
                    related: None,
                });
            }
            expecting_value = false;
        }

        if b == b'{' {
            object_depth += 1;
            expecting_property_name = true;
            i += 1;
            continue;
        }

        if b == b'}' {
            object_depth -= 1;
            expecting_property_name = false;
            i += 1;
            continue;
        }

        if b == b'[' && !expecting_property_name {
            array_depth += 1;
            expecting_value = true;
            i += 1;
            continue;
        }

        if b == b']' && array_depth > 0 {
            array_depth -= 1;
            i += 1;
            continue;
        }

        if b == b',' {
            // After a comma, the innermost open container decides what comes
            // next: a property name for an object, another element for an
            // array.
            if object_depth > array_depth {
                expecting_property_name = true;
            } else if array_depth > 0 {
                expecting_value = true;
            }
            i += 1;
            continue;
        }

        if b == b':' {
            expecting_property_name = false;
            expecting_value = true;
            i += 1;
            continue;
        }

        // When expecting a property name, check what we got
        if expecting_property_name && object_depth > 0 {
            if b == b'"' {
                // Valid double-quoted property name - skip past the string via
                // the shared quoted-literal primitive (honors `\` escapes and
                // terminates a single-line string at a raw newline, so an
                // unterminated name does not swallow following lines/comments).
                expecting_property_name = false;
                i = skip_quoted_literal(bytes, i, b'"');
                continue;
            }

            // Not a double-quoted string in property name position → TS1327
            diagnostics.push(ParseDiagnostic {
                start: i as u32,
                length: 1,
                message: tsz_common::diagnostics::diagnostic_messages::STRING_LITERAL_WITH_DOUBLE_QUOTES_EXPECTED.to_string(),
                code: tsz_common::diagnostics::diagnostic_codes::STRING_LITERAL_WITH_DOUBLE_QUOTES_EXPECTED,
                related: None,
            });
            expecting_property_name = false;
        }

        // Skip over strings (double-, single-, and back-quoted) via the shared
        // quoted-literal primitive so their contents are never misread as
        // structural tokens (braces, commas, colons) — and so a `//` or `/*`
        // inside a string is left as text, never treated as a comment on the
        // next top-of-loop trivia skip.
        if b == b'"' || b == b'\'' || b == b'`' {
            i = skip_quoted_literal(bytes, i, b);
            continue;
        }

        i += 1;
    }

    diagnostics
}

/// Returns whether `bytes[i..]` starts with the exact keyword `word` (`true`,
/// `false`, or `null`), not just a longer identifier with `word` as a prefix
/// (e.g. `truest` must not match `true`).
fn matches_json_keyword(
    bytes: &[u8],
    i: usize,
    len: usize,
    word: &[u8],
    is_ident_part: impl Fn(u8) -> bool,
) -> bool {
    let end = i + word.len();
    end <= len && &bytes[i..end] == word && (end == len || !is_ident_part(bytes[end]))
}

// The linked value checker uses strict serde JSON. Validate the same complete
// grammar here, retaining native recovery diagnostics where they are available.
// This deliberately diagnoses JSONC module text; tsconfig JSONC is separate.
fn validate_json_syntax_with_checkpoint(
    source: &str,
    checkpoint: Option<&dyn tsz_common::ExecutionCheckpoint>,
) -> Result<Vec<ParseDiagnostic>, tsz_common::ProjectExecutionStop> {
    use serde::Deserialize;
    if let Some(checkpoint) = checkpoint {
        checkpoint.checkpoint(0)?;
    }
    let mut reader = CheckpointJsonReader {
        bytes: source.as_bytes(),
        position: 0,
        pending: 0,
        checkpoint,
        stopped: None,
    };
    let parsed = {
        let mut deserializer = serde_json::Deserializer::from_reader(&mut reader);
        DiscardJsonValue::deserialize(&mut deserializer).and_then(|_| deserializer.end())
    };
    reader.finish()?;
    let Err(error) = parsed else {
        return Ok(Vec::new());
    };

    // Preserve TS1327/1328 and identifier-root recovery without permitting an
    // unbounded second scan of malformed input. All successful source admission
    // comes from the strict native deserializer above, never this recovery pass.
    // The two legacy traversals scan at most 8 KiB between stop polls.
    const MAX_RECOVERY_BYTES: usize = 4096;
    if source.len() <= MAX_RECOVERY_BYTES {
        if let Some(checkpoint) = checkpoint {
            checkpoint.checkpoint(2 * source.len() as u64)?;
        }
        let diagnostics = legacy_json_syntax_diagnostics(source);
        if let Some(checkpoint) = checkpoint {
            checkpoint.checkpoint(0)?;
        }
        if !diagnostics.is_empty() {
            return Ok(diagnostics);
        }
    }
    let eof = error.classify() == serde_json::error::Category::Eof;
    let mut start = if eof {
        source.len()
    } else {
        json_error_byte_offset(source, error.line(), error.column(), checkpoint)?
    };
    while !source.is_char_boundary(start) {
        start -= 1;
    }
    let length = if eof {
        0
    } else {
        source[start..].chars().next().map_or(0, |ch| ch.len_utf8())
    };
    Ok(vec![ParseDiagnostic {
        start: start as u32,
        length: length as u32,
        message: format!("Invalid native JSON module: {error}"),
        code: if eof {
            tsz_common::diagnostics::diagnostic_codes::EXPECTED
        } else {
            tsz_common::diagnostics::diagnostic_codes::UNEXPECTED_TOKEN
        },
        related: None,
    }])
}

// `IgnoredAny` skips numeric representation and serde's normal recursive
// container descent. Use the same deserialize_any path as the native Value
// checker, dropping entries immediately instead of allocating another tree.
struct DiscardJsonValue;
impl<'de> serde::Deserialize<'de> for DiscardJsonValue {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(Self)
    }
}
impl<'de> serde::de::Visitor<'de> for DiscardJsonValue {
    type Value = Self;
    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a native JSON value")
    }
    fn visit_bool<E>(self, _: bool) -> Result<Self, E> {
        Ok(Self)
    }
    fn visit_i64<E>(self, _: i64) -> Result<Self, E> {
        Ok(Self)
    }
    fn visit_u64<E>(self, _: u64) -> Result<Self, E> {
        Ok(Self)
    }
    fn visit_f64<E>(self, _: f64) -> Result<Self, E> {
        Ok(Self)
    }
    fn visit_str<E>(self, _: &str) -> Result<Self, E> {
        Ok(Self)
    }
    fn visit_unit<E>(self) -> Result<Self, E> {
        Ok(Self)
    }
    fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut sequence: A) -> Result<Self, A::Error> {
        while sequence.next_element::<Self>()?.is_some() {}
        Ok(Self)
    }
    fn visit_map<A: serde::de::MapAccess<'de>>(self, mut map: A) -> Result<Self, A::Error> {
        while map.next_entry::<String, Self>()?.is_some() {}
        Ok(Self)
    }
}

struct CheckpointJsonReader<'source, 'budget> {
    bytes: &'source [u8],
    position: usize,
    pending: u64,
    checkpoint: Option<&'budget dyn tsz_common::ExecutionCheckpoint>,
    stopped: Option<tsz_common::ProjectExecutionStop>,
}
impl CheckpointJsonReader<'_, '_> {
    fn finish(&mut self) -> Result<(), tsz_common::ProjectExecutionStop> {
        if let Some(stopped) = self.stopped {
            return Err(stopped);
        }
        if let Some(checkpoint) = self.checkpoint {
            checkpoint.checkpoint(std::mem::take(&mut self.pending))?;
        }
        Ok(())
    }
}
impl std::io::Read for CheckpointJsonReader<'_, '_> {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.stopped.is_some() {
            return Err(std::io::Error::other("native JSON scan stopped"));
        }
        let count = buffer.len().min(4096).min(self.bytes.len() - self.position);
        if count == 0 {
            return Ok(0);
        }
        self.pending += count as u64;
        if self.pending >= 4096 {
            if let Some(checkpoint) = self.checkpoint {
                if let Err(stopped) = checkpoint.checkpoint(std::mem::take(&mut self.pending)) {
                    self.stopped = Some(stopped);
                    return Err(std::io::Error::other("native JSON scan stopped"));
                }
            } else {
                self.pending = 0;
            }
        }
        buffer[..count].copy_from_slice(&self.bytes[self.position..self.position + count]);
        self.position += count;
        Ok(count)
    }
}

fn json_error_byte_offset(
    source: &str,
    target_line: usize,
    column: usize,
    checkpoint: Option<&dyn tsz_common::ExecutionCheckpoint>,
) -> Result<usize, tsz_common::ProjectExecutionStop> {
    let mut offset = 0;
    let mut line = 1;
    let mut pending = 0;
    while line < target_line && offset < source.len() {
        if source.as_bytes()[offset] == b'\n' {
            line += 1;
        }
        offset += 1;
        pending += 1;
        if pending == 4096 {
            if let Some(checkpoint) = checkpoint {
                checkpoint.checkpoint(pending)?;
            }
            pending = 0;
        }
    }
    if let Some(checkpoint) = checkpoint {
        checkpoint.checkpoint(pending)?;
    }
    Ok(offset
        .saturating_add(column.saturating_sub(1))
        .min(source.len()))
}

#[cfg(test)]
fn validate_json_syntax(source: &str) -> Vec<ParseDiagnostic> {
    validate_json_syntax_with_checkpoint(source, None).unwrap()
}

#[cfg(test)]
#[test]
fn strict_json_modules_retain_errors_in_values_structure_strings_and_trailing_input() {
    for source in [
        r#"{"enabled": }"#,
        r#"{"enabled":true"#,
        r#"{"nested":[1"#,
        "{} trailing",
        "{} }",
        "[] ]",
        "true false",
        r#"{"a":1 "b":2}"#,
        r#"{"text":"\q"}"#,
        "{\"text\":\"raw\nnewline\"}",
    ] {
        assert!(
            !validate_json_syntax(source).is_empty(),
            "invalid JSON module: {source}"
        );
    }
    for source in [
        "{}",
        "[]",
        "true",
        "null",
        "42",
        "-1.5e+2",
        r#""é🦀""#,
        r#"{"nested":[],"items":[{},[true,false,null]],"url":"http://docs"}"#,
    ] {
        assert!(
            validate_json_syntax(source).is_empty(),
            "valid JSON module: {source}"
        );
    }
}

#[cfg(test)]
#[test]
fn jsonc_modules_are_diagnosed_consistently_with_the_strict_native_value_checker() {
    for source in [
        "// header\n{}",
        "/* header */ {}",
        "{} // tail",
        r#"{"enabled": /* value */ true}"#,
        r#"{"enabled":true,}"#,
    ] {
        assert!(
            !validate_json_syntax(source).is_empty(),
            "strict JSON module: {source}"
        );
    }
    // Compiler configuration remains on its separate native JSONC parser.
    assert!(
        crate::config::parse_tsconfig("// config\n{\"compilerOptions\":{\"strict\":true,},}")
            .is_ok()
    );
}

#[cfg(test)]
#[test]
fn json_validation_matches_native_value_numeric_and_depth_limits() {
    for source in [
        r#"{"overflow":1e999}"#.to_owned(),
        format!("{}0{}", "[".repeat(256), "]".repeat(256)),
    ] {
        assert!(
            serde_json::from_str::<serde_json::Value>(&source).is_err(),
            "the linked value checker cannot represent this module"
        );
        assert!(
            !validate_json_syntax(&source).is_empty(),
            "syntax admission must retain the native refusal"
        );
    }
}

#[cfg(test)]
#[test]
fn checkpointed_json_bind_stops_during_long_values_trivia_and_nested_input() {
    use std::sync::atomic::{AtomicU64, Ordering};
    use tsz_common::{ExecutionCheckpoint, ProjectExecutionStop};
    struct StopAtChunk {
        reason: ProjectExecutionStop,
        charged: AtomicU64,
    }
    impl ExecutionCheckpoint for StopAtChunk {
        fn checkpoint(&self, work: u64) -> Result<(), ProjectExecutionStop> {
            let charged = self.charged.fetch_add(work, Ordering::Relaxed) + work;
            if charged >= 8192 {
                Err(self.reason)
            } else {
                Ok(())
            }
        }
    }
    for source in [
        format!(r#"{{"text":"{}"}}"#, "é🦀".repeat(16_384)),
        format!("{}{{}}", " ".repeat(16_384)),
        format!("[{}0]", "0,".repeat(16_384)),
    ] {
        for reason in [
            ProjectExecutionStop::Cancelled,
            ProjectExecutionStop::Deadline,
            ProjectExecutionStop::WorkBudgetExhausted,
        ] {
            let stop = StopAtChunk {
                reason,
                charged: AtomicU64::new(0),
            };
            let result = synthesize_json_bind_result_with_checkpoint(
                "data.json".into(),
                source.clone(),
                Some(&stop),
            );
            assert!(matches!(result, Err(observed) if observed == reason));
            assert_eq!(
                stop.charged.load(Ordering::Relaxed),
                8192,
                "the real JSON bind stops during its second native scan chunk"
            );
        }
    }
}
