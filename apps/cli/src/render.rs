//! Rendering: one presentation answer becomes bytes on a stream.
//!
//! This module contains no layout. Human output is
//! [`backend_present::text`], Markdown output is
//! [`backend_present::markdown`] — the same function the MCP text block calls
//! — and JSON is the typed DTO projection. All this module decides is which of
//! the three the caller asked for, and what exit code a fault means.

use backend_present::{
    Fault, FaultSlug, OutlineDto, PageDto, ProductDto, RecordListDto, ShelfDto, StatusDto,
    markdown, text,
};
use std::process::ExitCode;

use crate::options::{Format, Options};
use crate::run::Answer;

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
        Format::Human => human(answer, options),
        Format::Markdown => markdown_text(answer),
        Format::Json => json(answer),
    }
}

fn human(answer: &Answer, options: &Options) -> String {
    let theme = options.theme();
    match answer {
        Answer::Page(page) => text::page(page, theme),
        Answer::Records(list) => text::records(list, None, theme),
        Answer::Shelf(shelf) => text::shelf(shelf, theme),
        Answer::Outline(tree) => text::outline(tree, theme),
        Answer::Status(status) => text::status(status, theme),
        Answer::Product(view) => text::product(view, theme),
    }
}

/// Renders one answer as the exact Markdown the MCP text block carries.
#[must_use]
pub fn markdown_text(answer: &Answer) -> String {
    match answer {
        Answer::Page(page) => markdown::page(page),
        Answer::Records(list) => markdown::records(list, None),
        Answer::Shelf(shelf) => markdown::shelf(shelf),
        Answer::Outline(tree) => markdown::outline(tree),
        Answer::Status(status) => markdown::status(status),
        Answer::Product(view) => markdown::product(view),
    }
}

/// Renders one answer as the stable typed JSON projection.
#[must_use]
pub fn json(answer: &Answer) -> String {
    let value = match answer {
        Answer::Page(page) => tagged("page", &PageDto::new(page)),
        Answer::Records(list) => tagged("records", &RecordListDto::new(list)),
        Answer::Shelf(shelf) => tagged("shelf", &ShelfDto::new(shelf)),
        Answer::Outline(tree) => tagged("outline", &OutlineDto::new(tree)),
        Answer::Status(status) => tagged("status", &StatusDto::new(status)),
        Answer::Product(view) => tagged("product", &ProductDto::new(view)),
    };
    encode(&value)
}

/// Renders one fault in the caller's chosen format.
#[must_use]
pub fn fault(fault: &Fault, options: &Options) -> String {
    match options.format() {
        Format::Human => text::fault(fault, options.theme()),
        Format::Markdown => markdown::fault(fault),
        Format::Json => encode(&tagged("fault", &backend_present::FaultDto::new(fault))),
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

fn tagged<T: serde::Serialize>(kind: &str, value: &T) -> serde_json::Value {
    let mut body = serde_json::to_value(value).unwrap_or(serde_json::Value::Null);
    if let serde_json::Value::Object(fields) = &mut body {
        fields.insert(
            "kind".to_owned(),
            serde_json::Value::String(kind.to_owned()),
        );
        return serde_json::Value::Object(fields.clone());
    }
    serde_json::json!({ "kind": kind, "value": body })
}

fn encode(value: &serde_json::Value) -> String {
    let mut out = serde_json::to_string_pretty(value)
        .unwrap_or_else(|_| "{\"kind\":\"fault\",\"slug\":\"protocol\"}".to_owned());
    out.push('\n');
    out
}
