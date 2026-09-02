//! Defines lower python behavior for `compiler-driver`, whose purpose is to run bounded native toolchains and lower their output into canonical IR.
//! This module owns the lower python invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
#![allow(
    clippy::indexing_slicing,
    reason = "every line, quote, and operator position is proven in bounds by the preceding scan of the same borrowed line before it is indexed"
)]

use compiler_ir::{EntityKind, PrimitiveType, SemanticProductConstructor};

use crate::{
    lower::{
        FactSet, FactType, LEAF_PRODUCT, SemanticFact, UnsupportedDeclaration, UnsupportedLane,
        UnsupportedReason, push_fact,
    },
    types::LoweringUnsupported,
};

/// Emits every provable top-level Python declaration: `def` functions,
/// `class` records, and closed-literal assignments.
///
/// Lines are parsed structurally by indentation and exact statement heads;
/// triple-quoted spans and comments never become declarations, and an
/// assignment whose value is outside the closed literal recipe is recorded
/// exactly instead of guessed.
pub(super) fn collect<'source>(
    source: &'source [u8],
    facts: &mut FactSet<'source>,
    unsupported: &mut UnsupportedLane<'source>,
) -> Result<(), LoweringUnsupported> {
    let mut open_triple: Option<u8> = None;
    for line in source.split(|byte| *byte == b'\n') {
        let line = match close_triple(line, &mut open_triple) {
            Some(rest) => rest,
            None => continue,
        };
        let Some(code) = strip_comment(line) else {
            continue;
        };
        if code.first().is_some_and(u8::is_ascii_whitespace) {
            // Nested statements belong to their enclosing scope. The raw
            // line's leading indentation is inspected before any trimming.
            continue;
        }
        let Some(code) = trim_ascii(code) else {
            continue;
        };
        let body = code.strip_prefix(b"async ").unwrap_or(code);
        if let Some(head) = body.strip_prefix(b"def ") {
            function(head, facts)?;
            continue;
        }
        if let Some(head) = body.strip_prefix(b"class ") {
            record(head, facts);
            continue;
        }
        assignment(code, facts, unsupported);
    }
    Ok(())
}

/// Parses one function head: the name, its balanced parameter list, and an
/// optional return annotation before the terminating colon.
fn function<'source>(
    head: &'source [u8],
    facts: &mut FactSet<'source>,
) -> Result<(), LoweringUnsupported> {
    let Some(name) = leading_identifier(head) else {
        return Ok(());
    };
    let fact_type = return_annotation(head);
    push_fact(
        facts,
        SemanticFact::new(
            EntityKind::Function,
            name,
            fact_type,
            SemanticProductConstructor::function(0, 0),
        ),
    )?;
    Ok(())
}

/// Parses one class head: the name and optional base list. The class body is
/// excluded by the indentation filter.
fn record<'source>(head: &'source [u8], facts: &mut FactSet<'source>) {
    if let Some(name) = leading_identifier(head) {
        let _ = push_fact(
            facts,
            SemanticFact::new(
                EntityKind::Record,
                name,
                FactType::Opaque,
                SemanticProductConstructor::PRODUCT,
            ),
        );
    }
}

/// Parses one top-level assignment: a proven identifier, one equals sign, and
/// a closed literal value.
fn assignment<'source>(
    code: &'source [u8],
    facts: &mut FactSet<'source>,
    unsupported: &mut UnsupportedLane<'source>,
) {
    let Some(equals) = code.iter().position(|byte| *byte == b'=') else {
        return;
    };
    // Only a bare equals sign is an assignment; comparison and augmented
    // operators never declare a value.
    let augmented = equals > 0
        && matches!(
            code[equals - 1],
            b'!' | b'<'
                | b'>'
                | b'+'
                | b'-'
                | b'*'
                | b'/'
                | b'%'
                | b'='
                | b'&'
                | b'|'
                | b'^'
                | b':'
        );
    if equals == 0 || augmented || code.get(equals + 1) == Some(&b'=') {
        return;
    }
    let Some(name) = trim_ascii(&code[..equals]).and_then(is_identifier) else {
        return;
    };
    let Some(value) = trim_ascii(&code[equals + 1..]) else {
        return;
    };
    let fact_type = if value == b"True" || value == b"False" {
        Some(PrimitiveType::Bool)
    } else if !value.is_empty() && value.iter().all(u8::is_ascii_digit) {
        Some(PrimitiveType::I32)
    } else if value.first() == Some(&b'"') || value.first() == Some(&b'\'') {
        Some(PrimitiveType::String)
    } else {
        None
    };
    match fact_type {
        Some(primitive) => {
            let _ = push_fact(
                facts,
                SemanticFact::new(
                    EntityKind::Constant,
                    name,
                    FactType::Primitive(primitive),
                    LEAF_PRODUCT,
                ),
            );
        }
        None => unsupported.record(UnsupportedDeclaration {
            name,
            reason: UnsupportedReason::ClosedValueType,
        }),
    }
}

/// Extracts the optional `-> annotation` after the balanced parameter list.
fn return_annotation(head: &[u8]) -> FactType {
    let mut depth = 0_u32;
    let mut quote: Option<u8> = None;
    let mut ordinal = 0;
    while ordinal < head.len() {
        let byte = head[ordinal];
        match quote {
            Some(open) => {
                if byte == open {
                    quote = None;
                }
            }
            None if byte == b'"' || byte == b'\'' => quote = Some(byte),
            None if byte == b'(' => depth = depth.saturating_add(1),
            None if byte == b')' => depth = depth.saturating_sub(1),
            None if depth == 0 && head[ordinal..].starts_with(b"->") => {
                let rest = trim_ascii(&head[ordinal + 2..]).unwrap_or(&[]);
                let annotation = rest
                    .iter()
                    .position(|byte| *byte == b':')
                    .and_then(|end| trim_ascii(&rest[..end]))
                    .unwrap_or(rest);
                if annotation == b"bool" {
                    return FactType::Primitive(PrimitiveType::Bool);
                }
                if annotation == b"int" {
                    return FactType::Primitive(PrimitiveType::I32);
                }
                if annotation == b"str" {
                    return FactType::Primitive(PrimitiveType::String);
                }
                return FactType::Opaque;
            }
            None => {}
        }
        ordinal += 1;
    }
    FactType::Opaque
}

/// Consumes triple-quoted spans across lines and returns the scannable
/// remainder of this line. A line that opens a span contributes no
/// declaration; a line that closes a span contributes only its remainder.
fn close_triple<'line>(line: &'line [u8], open_triple: &mut Option<u8>) -> Option<&'line [u8]> {
    let mut origin = 0;
    while let Some((position, quote)) = find_triple(&line[origin..]) {
        let absolute = origin + position;
        if *open_triple == Some(quote) {
            // The open span closes here; the remainder is scannable again.
            *open_triple = None;
            origin = absolute + 3;
        } else if open_triple.is_none() {
            // A span opens on this line: only a same-line close leaves a
            // scannable remainder.
            *open_triple = Some(quote);
            origin = absolute + 3;
            match find_triple(&line[origin..]) {
                Some((close, _)) => {
                    origin += close + 3;
                    *open_triple = None;
                }
                None => return None,
            }
        } else {
            // A different-quote triple is content inside the open span.
            origin = absolute + 3;
        }
    }
    if open_triple.is_some() {
        None
    } else {
        Some(line)
    }
}

/// Finds the earliest triple-quote sequence and its quote byte.
fn find_triple(haystack: &[u8]) -> Option<(usize, u8)> {
    let double = find_sequence(haystack, b"\"\"\"");
    let single = find_sequence(haystack, b"'''");
    match (double, single) {
        (Some(double), Some(single)) if double.0 <= single.0 => Some(double),
        (Some(_), Some(single)) => Some(single),
        (Some(double), None) => Some(double),
        (None, Some(single)) => Some(single),
        (None, None) => None,
    }
}

fn find_sequence(haystack: &[u8], needle: &[u8]) -> Option<(usize, u8)> {
    let limit = haystack.len().checked_sub(needle.len())?;
    (0..=limit)
        .find(|start| &haystack[*start..*start + needle.len()] == needle)
        .map(|start| (start, if needle[0] == b'"' { b'"' } else { b'\'' }))
}

/// Strips one unquoted comment tail from a source line.
fn strip_comment(line: &[u8]) -> Option<&[u8]> {
    let mut quote: Option<u8> = None;
    for (ordinal, byte) in line.iter().enumerate() {
        match quote {
            Some(open) => {
                if *byte == open {
                    quote = None;
                }
            }
            None if *byte == b'"' || *byte == b'\'' => quote = Some(*byte),
            None if *byte == b'#' => return Some(&line[..ordinal]),
            None => {}
        }
    }
    Some(line)
}

fn leading_identifier(head: &[u8]) -> Option<&[u8]> {
    let trimmed = trim_ascii(head)?;
    let mut end = 0;
    for (ordinal, byte) in trimmed.iter().enumerate() {
        if *byte == b'_' || byte.is_ascii_alphanumeric() {
            end = ordinal + 1;
        } else {
            break;
        }
    }
    if end == 0 {
        None
    } else {
        is_identifier(&trimmed[..end])
    }
}

fn is_identifier(name: &[u8]) -> Option<&[u8]> {
    let (first, rest) = name.split_first()?;
    let valid = (*first == b'_' || first.is_ascii_alphabetic())
        && rest
            .iter()
            .all(|byte| *byte == b'_' || byte.is_ascii_alphanumeric());
    valid.then_some(name)
}

fn trim_ascii(bytes: &[u8]) -> Option<&[u8]> {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .map_or(bytes.len(), core::convert::identity);
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(0, |index| index + 1);
    if start >= end {
        None
    } else {
        Some(&bytes[start..end])
    }
}
