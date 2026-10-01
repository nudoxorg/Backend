//! Rich local Markdown rendering and its typed link-event bridge.
//!
//! The GPUI component owns Markdown parsing, inline layout, selection, code
//! blocks, tables, and pointer hit testing. This small adapter adds stable
//! heading endpoints and turns a clicked destination into an application
//! action; the reader resolves that action against its admitted page data.

use crate::model::local_package::ReadmeHeading;
use gpui::{
    App, ClickEvent, ElementId, IntoElement, ParentElement, SharedString, Styled, Window, div, px,
};
use gpui_component::text::{MarkdownExtensions, MarkdownNode, TextView};
use std::sync::Arc;

/// Action dispatched by the Markdown component when a rendered link is
/// activated. Keeping the URL in the action crosses GPUI's Send+Sync
/// component callback without capturing shell entities in that callback.
#[derive(Clone, PartialEq, Debug, gpui::Action)]
#[action(namespace = nudox, no_json, no_register)]
pub(crate) struct FollowMarkdownLink {
    /// Exact parsed Markdown destination; resolution stays with the reader.
    pub destination: String,
}

/// Bridges the component's pointer callback into the reader's typed action
/// listener. It does not open arbitrary schemes itself.
pub(crate) fn emit_link_action(
    url: &SharedString,
    _: &ClickEvent,
    window: &mut Window,
    cx: &mut App,
) {
    window.dispatch_action(
        Box::new(FollowMarkdownLink {
            destination: url.to_string(),
        }),
        cx,
    );
}

/// Builds one selectable Markdown document with stable heading anchors.
pub(crate) fn view(
    id: impl Into<ElementId>,
    source: impl Into<SharedString>,
    headings: Arc<[ReadmeHeading]>,
) -> TextView {
    let parser_headings = Arc::clone(&headings);
    let extensions = MarkdownExtensions::default()
        .block_parser(move |node, context| {
            let markdown::mdast::Node::Heading(heading) = node else {
                return None;
            };
            let position = heading.position.as_ref()?;
            let offset = position.start.offset.saturating_add(context.offset());
            let element_id = format!("readme-heading-{offset}");
            let identity = parser_headings
                .iter()
                .find(|candidate| candidate.element_id.as_ref() == element_id)?
                .element_id
                .clone();
            let raw = context.node_source(node)?;
            let inline = heading_inline_source(raw);
            Some(
                MarkdownNode::new(
                    "readme-heading-anchor",
                    HeadingData {
                        element_id: identity,
                        level: heading.depth,
                    },
                )
                .markdown(inline),
            )
        })
        .block_renderer("readme-heading-anchor", |node, _, _| {
            let Some(data) = node.data::<HeadingData>() else {
                return div().into_any_element();
            };
            let id = SharedString::from(data.element_id.to_string());
            let size = match data.level {
                1 => px(22.0),
                2 => px(19.0),
                _ => px(16.0),
            };
            let inline = TextView::markdown(
                ElementId::Name(SharedString::from(format!("{id}:inline"))),
                node.as_markdown(),
            )
            .selectable(true)
            .on_link_click(emit_link_action);
            facet::motion::shared::shared(
                ElementId::Name(id),
                div()
                    .w_full()
                    .pt(px(6.0))
                    .pb(px(2.0))
                    .text_size(size)
                    .font_weight(gpui::FontWeight::BOLD)
                    .child(inline),
            )
        });
    TextView::markdown(id, source)
        .selectable(true)
        .on_link_click(emit_link_action)
        .markdown_extensions(extensions)
}

#[derive(Clone)]
struct HeadingData {
    element_id: Arc<str>,
    level: u8,
}

fn heading_inline_source(source: &str) -> String {
    let line = source.lines().next().unwrap_or_default().trim();
    let content = line
        .strip_prefix('#')
        .map(|_| line.trim_start_matches('#').trim_start())
        .unwrap_or(line);
    let content = content.trim_end();
    let suffix = content
        .chars()
        .rev()
        .take_while(|character| *character == '#')
        .count();
    if suffix > 0 {
        let before = &content[..content.len() - suffix];
        if before.chars().last().is_some_and(char::is_whitespace) {
            return before.trim_end().to_owned();
        }
    }
    content.to_owned()
}
