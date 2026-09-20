//! MCP protocol and bounded codec tests.

#![allow(clippy::expect_used, clippy::naive_bytecount)]

use super::*;
use backend_library::{Basis, Frontier, ViewRoot, object_version, view_key, view_state_root};

#[derive(Default)]
struct Fake;

impl Product for Fake {
    fn revision(&mut self) -> Result<ViewRoot, String> {
        Ok(empty_view())
    }
    fn packages(&mut self) -> Result<ReplyDto, String> {
        Ok(ReplyDto::error(1, "unused"))
    }
    fn index(&mut self, _: &str) -> Result<ReplyDto, String> {
        Ok(ReplyDto::error(1, "unused"))
    }
    fn remove(&mut self, _: &str) -> Result<ReplyDto, String> {
        Ok(ReplyDto::error(1, "unused"))
    }
    fn search(&mut self, _: &str, _: u16) -> Result<ReplyDto, String> {
        Ok(ReplyDto::error(1, "unused"))
    }
    fn names(&mut self, _: &str, _: u16) -> Result<ReplyDto, String> {
        Ok(ReplyDto::error(1, "unused"))
    }
    fn document(&mut self, _: &str) -> Result<ReplyDto, String> {
        Ok(ReplyDto::error(1, "unused"))
    }
    fn outline(&mut self, _: &str) -> Result<ReplyDto, String> {
        Ok(ReplyDto::error(1, "unused"))
    }
    fn graph(&mut self, _: &str) -> Result<ReplyDto, String> {
        Ok(ReplyDto::error(1, "unused"))
    }
}

fn empty_view() -> ViewRoot {
    let root = view_state_root(&[]);
    let basis = Basis::new(root, object_version(b"source"));
    ViewRoot::new_incomplete(
        view_key(b"mcp-test"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, root, 0),
        Vec::new(),
        Vec::new(),
    )
    .expect("empty view")
}

#[test]
fn handshake_lists_complete_tool_surface() {
    let mut server = Server::new(Fake, "/project".to_owned());
    let initialized = server.handle(br#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1"}}}"#).expect("response");
    assert_eq!(initialized["result"]["protocolVersion"], STABLE_PROTOCOL);
    let tools = server
        .handle(br#"{"jsonrpc":"2.0","id":"tools","method":"tools/list","params":{}}"#)
        .expect("response");
    assert_eq!(tools["result"]["tools"].as_array().map(Vec::len), Some(10));
    assert_eq!(tools["id"], "tools");
}

#[test]
fn notifications_have_no_reply_and_resource_is_pinned() {
    let mut server = Server::new(Fake, "/project".to_owned());
    assert!(
        server
            .handle(br#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#)
            .is_none()
    );
    let resource = server.handle(br#"{"jsonrpc":"2.0","id":2,"method":"resources/read","params":{"uri":"backend://workspace/current"}}"#).expect("response");
    let text = resource["result"]["contents"][0]["text"]
        .as_str()
        .expect("text");
    assert!(text.contains("revision"));
    assert!(text.contains("/project"));
}

#[test]
fn newline_codec_is_bounded_and_compact() {
    let mut reader = io::BufReader::new(&b"{}\n"[..]);
    assert_eq!(read_line(&mut reader).expect("read"), Some(b"{}".to_vec()));
    let mut output = Vec::new();
    write_message(&mut output, &json!({"jsonrpc":"2.0","id":1,"result":{}})).expect("write");
    assert_eq!(output.last(), Some(&b'\n'));
    assert_eq!(output.iter().filter(|byte| **byte == b'\n').count(), 1);
}
