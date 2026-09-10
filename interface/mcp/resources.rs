//! Defines resources behavior for `interface-mcp`, whose purpose is to serve the one shared local library to agents over MCP.
//! This module owns the resources invariants and typed state transitions.
//! Its narrow surface prevents representation and policy details from leaking outward.
//! Three readable resources: the shelf, the schema card, and any symbol page by address.
//!
//! Resources are computed on every read, never cached. A package finishes compiling in another
//! process while this one is idle, and a cached shelf would report the world as it was when this
//! server started — which is exactly the lie `packages` exists to prevent.

use interface_library::{
    Library, Reply,
    render::{
        common::RenderContext,
        markdown,
    },
};
use interface_protocol::mcp::ResourceDescriptor;

use crate::{arguments, card};

/// URI of the whole shelf.
pub const PACKAGES_URI: &str = "nudox://packages";
/// Prefix of the per-symbol page resource; everything after it is an address.
pub const SYMBOL_URI_PREFIX: &str = "nudox://symbol/";

/// Every resource this server publishes.
#[must_use]
pub fn descriptors() -> Vec<ResourceDescriptor> {
    vec![
        ResourceDescriptor {
            uri: PACKAGES_URI.to_owned(),
            name: "packages".to_owned(),
            description: "Every package on this shelf with its status and declaration counts — \
                          the complete answer to what can be read here."
                .to_owned(),
            mime_type: "text/markdown",
        },
        ResourceDescriptor {
            uri: card::SCHEMA_CARD_URI.to_owned(),
            name: "schema card".to_owned(),
            description: "Address grammar, the eleven relation kinds and what each one answers, \
                          coverage signals, and worked calls. Read this once per session."
                .to_owned(),
            mime_type: "text/markdown",
        },
        ResourceDescriptor {
            uri: format!("{SYMBOL_URI_PREFIX}{{address}}"),
            name: "symbol page".to_owned(),
            description: "One declaration page, addressed by any address a result printed; the \
                          same Markdown the show tool returns."
                .to_owned(),
            mime_type: "text/markdown",
        },
    ]
}

/// Reads one resource, or names the URI that matched nothing.
///
/// # Errors
///
/// Returns the exact unmatched URI, so a client learns what this server does publish.
pub fn read(library: &Library, uri: &str, context: &RenderContext) -> Result<String, String> {
    if uri == card::SCHEMA_CARD_URI {
        return Ok(card::render());
    }
    if uri == PACKAGES_URI {
        return Ok(markdown::render(&Reply::Packages(library.shelf()), context));
    }
    let Some(address) = uri.strip_prefix(SYMBOL_URI_PREFIX) else {
        return Err(format!(
            "`{uri}` is not published here; this server publishes {PACKAGES_URI}, \
             {}, and {SYMBOL_URI_PREFIX}{{address}}",
            card::SCHEMA_CARD_URI
        ));
    };
    let mut object = serde_json::Map::new();
    object.insert(
        "address".to_owned(),
        serde_json::Value::String(address.to_owned()),
    );
    match arguments::decode("show", &object) {
        Ok(command) => Ok(markdown::render(
            &library.execute(command, &mut |_| {}),
            context,
        )),
        Err(fault) => Ok(markdown::render_fault(&fault)),
    }
}
