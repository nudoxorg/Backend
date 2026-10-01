//! Rich local Markdown rendering and its typed link-event bridge.
//!
//! The GPUI component owns Markdown parsing, inline layout, selection, code
//! blocks, tables, and pointer hit testing. This small adapter adds stable
//! heading endpoints and turns a clicked destination into an application
//! action; the reader resolves that action against its admitted page data.

use facet::ActiveFacet as _;
use facet::tokens::ty;
use facet::{Measure, Space};
use gpui::{
    App, ClickEvent, ElementId, IntoElement, ParentElement, SharedString, Styled, Window, div, px,
};
use gpui_component::text::{MarkdownExtensions, MarkdownNode, TextView};
use std::sync::{Arc, OnceLock};

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
pub(crate) fn view(id: impl Into<ElementId>, source: impl Into<SharedString>) -> TextView {
    TextView::markdown(id, source)
        .selectable(true)
        .on_link_click(emit_link_action)
        .markdown_extensions(readme_extensions().clone())
}

fn readme_extensions() -> &'static MarkdownExtensions {
    static EXTENSIONS: OnceLock<MarkdownExtensions> = OnceLock::new();
    EXTENSIONS.get_or_init(|| {
        MarkdownExtensions::default()
            .block_parser(move |node, context| {
                let markdown::mdast::Node::Heading(heading) = node else {
                    return None;
                };
                let position = heading.position.as_ref()?;
                let offset = position.start.offset.saturating_add(context.offset());
                let element_id = format!("readme-heading-{offset}");
                let raw = context.node_source(node)?;
                let inline = heading_inline_source(raw);
                Some(
                    MarkdownNode::new(
                        "readme-heading-anchor",
                        HeadingData {
                            element_id: Arc::from(element_id),
                            level: heading.depth,
                        },
                    )
                    .markdown(inline),
                )
            })
            .block_renderer("readme-heading-anchor", move |node, window, cx| {
                let Some(data) = node.data::<HeadingData>() else {
                    return div().into_any_element();
                };
                let id = SharedString::from(data.element_id.to_string());
                let facet = cx.facet();
                let measure = Measure::new(window.viewport_size().width, &facet);
                let palette = facet.palette();
                let role = measure.role(match data.level {
                    1 => ty::HEAD,
                    2 => ty::TITLE,
                    _ => ty::PROSE,
                });
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
                        .pt(measure.space(Space::Snug))
                        .pb(measure.space(Space::Tight))
                        .text_size(px(role.size))
                        .font_weight(gpui::FontWeight::BOLD)
                        .text_color(palette.ink0.hsla())
                        .child(inline),
                )
                .into_any_element()
            })
    })
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

#[cfg(test)]
mod tests {
    use super::readme_extensions;

    #[test]
    fn markdown_plugin_registration_is_stable_between_render_builds() {
        assert!(std::ptr::eq(readme_extensions(), readme_extensions()));
    }
}
