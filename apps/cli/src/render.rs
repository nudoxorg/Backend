//! Rendering: one presentation answer becomes bytes on a stream.
//!
//! This module contains no layout and no dispatch. Human output is
//! [`backend_present::text::answer`], Markdown output is
//! [`backend_present::markdown::answer`] — the same function the MCP text block
//! calls — and JSON is [`backend_present::answer_value`], the same value the
//! MCP puts in `structuredContent`. All this module decides is which of the
//! three the caller asked for, and what exit code a fault means.

use backend_present::{
    Answer, DEFAULT_RESPONSE_BUDGET_BYTES, Detail, Fault, FaultSlug, bounded_text, encode_answer,
    fault_value, markdown, oversized_fault, text,
};
use std::process::ExitCode;

use crate::options::{Format, Options};

/// The process answered the question.
pub const EXIT_OK: u8 = 0;
/// The local endpoint or a stream failed.
pub const EXIT_IO: u8 = 1;
/// The engine admitted the request and refused it.
pub const EXIT_REFUSED: u8 = 2;
/// The arguments did not match the command grammar.
pub const EXIT_USAGE: u8 = 64;

/// Renders one answer in the caller's chosen format.
#[must_use]
pub fn answer(answer: &Answer, options: &Options) -> String {
    match options.format() {
        Format::Human => bounded_text(&text::answer(answer, options.theme())),
        Format::Markdown => markdown_text(answer),
        Format::Json => json_with_detail(answer, options.detail()),
    }
}

/// Renders one answer as the exact Markdown the MCP text block carries.
#[must_use]
pub fn markdown_text(answer: &Answer) -> String {
    bounded_text(&markdown::answer(answer))
}

/// Renders one answer as the stable typed JSON projection.
#[must_use]
pub fn json(answer: &Answer) -> String {
    json_with_detail(answer, Detail::Summary)
}

/// Renders the bounded typed projection shared with MCP.
#[must_use]
pub fn json_with_detail(answer: &Answer, detail: Detail) -> String {
    match encode_answer(answer, detail, None, DEFAULT_RESPONSE_BUDGET_BYTES) {
        Ok(payload) => String::from_utf8(payload.bytes.into_vec())
            .unwrap_or_else(|_| bounded_fault(&Fault::usage("response", "invalid UTF-8"))),
        Err(error) => bounded_fault(&oversized_fault(error)),
    }
}

/// Renders one fault in the caller's chosen format.
#[must_use]
pub fn fault(fault: &Fault, options: &Options) -> String {
    match options.format() {
        Format::Human => bounded_text(&text::fault(fault, options.theme())),
        Format::Markdown => bounded_text(&markdown::fault(fault)),
        Format::Json => bounded_fault(fault),
    }
}

/// Returns the exit code one fault means.
///
/// The three classes a caller scripts against are distinct on purpose: a
/// refusal is the engine's answer, a usage error is the caller's mistake, and
/// an endpoint failure is the machine's.
#[must_use]
pub fn exit_code(fault: &Fault) -> ExitCode {
    ExitCode::from(match fault.slug() {
        FaultSlug::Usage => EXIT_USAGE,
        FaultSlug::Endpoint | FaultSlug::Transport => EXIT_IO,
        _ => EXIT_REFUSED,
    })
}

fn encode(value: &serde_json::Value) -> String {
    let mut out = serde_json::to_string_pretty(value)
        .unwrap_or_else(|_| "{\"kind\":\"fault\",\"slug\":\"protocol\"}".to_owned());
    out.push('\n');
    out
}

fn bounded_fault(fault: &Fault) -> String {
    let value = fault_value(fault);
    let bytes = serde_json::to_vec_pretty(&value).unwrap_or_default();
    if bytes.len() + 1 <= DEFAULT_RESPONSE_BUDGET_BYTES {
        return String::from_utf8(bytes)
            .map(|bytes| format!("{bytes}\n"))
            .unwrap_or_else(|_| encode(&fault_value(&Fault::usage("response", "invalid UTF-8"))));
    }
    encode(&fault_value(&oversized_fault(
        backend_present::BudgetExceeded {
            bytes: bytes.len(),
            budget: DEFAULT_RESPONSE_BUDGET_BYTES,
        },
    )))
}
