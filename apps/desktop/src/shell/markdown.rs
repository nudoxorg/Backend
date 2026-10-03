//! Rich local Markdown rendering and its typed link-event bridge.
//!
//! The GPUI component owns Markdown parsing, inline layout, selection, code
//! blocks, tables, and pointer hit testing. This small adapter adds stable
//! heading endpoints and turns a clicked destination into an application
//! action; the reader resolves that action against its admitted page data.

use crate::model::document_identity::DocumentPaintIdentity;
use facet::ActiveFacet as _;
use facet::tokens::ty;
use facet::{Measure, Space};
use gpui::{
    App, ClickEvent, ElementId, InteractiveElement as _, IntoElement, ParentElement, SharedString,
    StatefulInteractiveElement as _, Styled, Window, div, px,
};
use gpui_component::text::{MarkdownExtensions, MarkdownNode, PreparedMarkdown, TextView};
use std::cell::RefCell;
use std::collections::VecDeque;
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
        .background_parse()
        .selectable(true)
        .on_link_click(emit_link_action)
        .markdown_extensions(readme_extensions().clone())
}

/// Owner documents provide the same scoped heading identity used by their
/// native anchor projections. Recent bounds from another visit cannot match.
pub(crate) fn scoped_view(
    id: impl Into<ElementId>,
    source: impl Into<SharedString>,
    scope: DocumentPaintIdentity,
) -> TextView {
    let extensions = scoped_extensions(scope);
    TextView::markdown(id, source)
        .background_parse()
        .selectable(true)
        .on_link_click(emit_link_action)
        .markdown_extensions((*extensions).clone())
}

fn readme_extensions() -> &'static MarkdownExtensions {
    static EXTENSIONS: OnceLock<MarkdownExtensions> = OnceLock::new();
    EXTENSIONS.get_or_init(|| extensions_for(None))
}

// Extension revision is part of the component's parse identity. Reusing an
// immutable registry avoids reparsing on each frame; the cache is neutral UI
// data and holds no owner receipt, source bytes, or native action lease.
fn scoped_extensions(scope: DocumentPaintIdentity) -> Arc<MarkdownExtensions> {
    const MAX_SCOPES: usize = 16;
    thread_local! {
        static EXTENSIONS: RefCell<VecDeque<(DocumentPaintIdentity, Arc<MarkdownExtensions>)>> = RefCell::new(VecDeque::new());
    }
    EXTENSIONS.with(|cache| {
        let mut entries = cache.borrow_mut();
        if let Some(at) = entries.iter().position(|(current, _)| *current == scope)
            && let Some((_, extensions)) = entries.remove(at)
        {
            entries.push_back((scope, Arc::clone(&extensions)));
            return extensions;
        }
        let extensions = Arc::new(extensions_for(Some(scope)));
        if entries.len() >= MAX_SCOPES {
            entries.pop_front();
        }
        entries.push_back((scope, Arc::clone(&extensions)));
        extensions
    })
}

fn extensions_for(scope: Option<DocumentPaintIdentity>) -> MarkdownExtensions {
    MarkdownExtensions::default()
        .block_parser(move |node, context| {
            let markdown::mdast::Node::Heading(heading) = node else {
                return None;
            };
            let position = heading.position.as_ref()?;
            let offset = position.start.offset.saturating_add(context.offset());
            let element_id = scope.map_or_else(
                || Arc::from(format!("readme-heading-{offset}")),
                |scope| scope.heading_id(offset),
            );
            let element_id = SharedString::from(element_id);
            let inline_id: SharedString = format!("{element_id}:inline").into();
            let raw = context.node_source(node)?;
            let inline: SharedString = heading_inline_source(raw).into();
            // Prepare heading inline marks with the document worker so the
            // first published layout needs no additional parsing task.
            let prepared = context.prepare_inline(&heading.children, inline.as_str());
            Some(
                MarkdownNode::new(
                    "readme-heading-anchor",
                    HeadingData {
                        element_id,
                        inline_id,
                        prepared,
                        level: heading.depth,
                        label: heading_plain_text(node).into(),
                    },
                )
                .markdown(inline),
            )
        })
        .block_renderer_with_context("readme-heading-anchor", move |node, context, window, cx| {
            let Some(data) = node.data::<HeadingData>() else {
                return div().into_any_element();
            };
            let id = data.element_id.clone();
            let facet = cx.facet();
            let measure = Measure::new(window.viewport_size().width, &facet);
            let palette = facet.palette();
            let role = measure.role(match data.level {
                1 => ty::HEAD,
                2 => ty::TITLE,
                _ => ty::PROSE,
            });
            let inline = context.inherit_links(
                TextView::prepared_markdown(
                    ElementId::Name(data.inline_id.clone()),
                    data.prepared.clone(),
                )
                .selectable(true),
            );
            facet::motion::shared::shared(
                ElementId::Name(id),
                div()
                    .id("heading")
                    .role(gpui::Role::Heading)
                    .aria_level(usize::from(data.level))
                    .aria_label(data.label.clone())
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
}

#[derive(Clone)]
struct HeadingData {
    element_id: SharedString,
    inline_id: SharedString,
    prepared: PreparedMarkdown,
    level: u8,
    label: SharedString,
}

// Compute one plain heading label during parsing, alongside its immutable
// anchor. Native labels contain authored words rather than Markdown markers.
fn heading_plain_text(node: &markdown::mdast::Node) -> String {
    use markdown::mdast::Node;
    let mut text = String::new();
    let mut stack = vec![node];
    while let Some(node) = stack.pop() {
        match node {
            Node::Text(value) => text.push_str(&value.value),
            Node::InlineCode(value) => text.push_str(&value.value),
            Node::Image(value) => text.push_str(&value.alt),
            Node::ImageReference(value) => text.push_str(&value.alt),
            Node::Break(_) => text.push('\n'),
            _ => {}
        }
        if let Some(children) = node.children() {
            stack.extend(children.iter().rev());
        }
    }
    text
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
    #![allow(clippy::expect_used, clippy::panic)]
    use super::*;
    use crate::model::browse::{BrowseValue, CargoReadmeDocument, CargoReadmeState};
    use crate::model::pages::{Capacity, PageStore, PageValue};
    use gpui::{
        AppContext as _, Bounds, Context, Pixels, Render, Styled, TestAppContext, VisualTestContext,
    };

    fn document(source: &str) -> Arc<CargoReadmeDocument> {
        let (_, value) = crate::runtime::cargo_readme_reads::tests::fixture_with_contents(source);
        let PageValue::Browse(BrowseValue::CargoReadme(model)) = value else {
            panic!("owner README fixture");
        };
        let CargoReadmeState::Read(document) = &model.state else {
            panic!("complete document");
        };
        Arc::clone(document)
    }

    fn fixture_stamp() -> crate::model::pages::Stamp {
        let (key, _) = crate::runtime::cargo_readme_reads::tests::fixture_value();
        PageStore::new(Capacity::default()).stamp(&crate::model::pages::PageKey::Browse(
            crate::model::browse::BrowseKey::CargoReadme(key),
        ))
    }

    #[test]
    fn heading_inline_preserves_unicode_links_and_setext_text() {
        assert_eq!(
            heading_inline_source("## 🦀 [Guide](#guide) ###"),
            "🦀 [Guide](#guide)"
        );
        assert_eq!(heading_inline_source("Guide 🦀\n========"), "Guide 🦀");
        assert_eq!(heading_inline_source("# C#"), "C#");
        assert_eq!(heading_inline_source("# `#literal`"), "`#literal`");
    }

    #[test]
    fn markdown_plugin_registration_is_stable_between_render_builds() {
        assert!(std::ptr::eq(readme_extensions(), readme_extensions()));
    }

    #[test]
    fn scoped_registry_is_reused_for_the_exact_document_visit() {
        let document = document("# Guide\n\nA current document.\n");
        let stamp = fixture_stamp();
        let first = document.paint(1, stamp).identity();
        let second = document.paint(2, stamp).identity();
        let extensions = scoped_extensions(first);
        assert!(Arc::ptr_eq(&extensions, &scoped_extensions(first)));
        assert!(!Arc::ptr_eq(&extensions, &scoped_extensions(second)));
        assert!(
            Arc::ptr_eq(&extensions, &scoped_extensions(first)),
            "another active scope cannot replace this scope's parser registration"
        );
    }

    struct DocumentFixture {
        document: Arc<CargoReadmeDocument>,
        place: u64,
        stamp: crate::model::pages::Stamp,
        left: f32,
    }
    impl Render for DocumentFixture {
        fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
            let painted = self.document.paint(self.place, self.stamp);
            div()
                .size_full()
                .pl(px(self.left))
                .pt(px(40.0))
                .child(scoped_view(
                    ElementId::Name(SharedString::from(painted.identity().document_id())),
                    SharedString::from(Arc::clone(&self.document.source)),
                    painted.identity(),
                ))
        }
    }

    fn heading_bounds(cx: &mut VisualTestContext, id: &Arc<str>) -> Option<Bounds<Pixels>> {
        cx.update(|window, app| {
            facet::motion::shared::last_bounds(
                ElementId::Name(SharedString::from(Arc::clone(id))),
                window,
                app,
            )
        })
    }

    fn paint_frames(cx: &mut VisualTestContext) {
        for _ in 0..8 {
            cx.run_until_parked();
            cx.update(|window, app| {
                window.simulate_next_frame(app);
                window.refresh();
                window.draw(app).clear(app);
            });
        }
        cx.run_until_parked();
    }

    // Mounted GPUI geometry, not a live-owner/native PNG acceptance claim.
    #[gpui::test]
    fn mounted_markdown_heading_bounds_are_scoped_to_document_and_visit(cx: &mut TestAppContext) {
        cx.update(|app| {
            gpui_component::init(app);
            let _ = facet::fonts::install(app);
            app.set_global(facet::Facet {
                reduced_motion: true,
                ..Default::default()
            });
        });
        let first = document("# Guide\n\nFirst bytes.\n");
        let second = document("# Guide\n\nSecond bytes.\n");
        let stamp = fixture_stamp();
        let first_id = first
            .paint(1, stamp)
            .heading(0)
            .expect("first heading")
            .element_id;
        let second_id = second
            .paint(1, stamp)
            .heading(0)
            .expect("second heading")
            .element_id;
        let return_id = second
            .paint(2, stamp)
            .heading(0)
            .expect("return heading")
            .element_id;
        assert_ne!(first_id, second_id);
        assert_ne!(second_id, return_id);
        let (view, visual) = cx.add_window_view(|_, _| DocumentFixture {
            document: Arc::clone(&first),
            place: 1,
            stamp,
            left: 48.0,
        });
        paint_frames(visual);
        let first_bounds = heading_bounds(visual, &first_id)
            .expect("the plugin painted the exact first heading identity");
        assert!(f32::from(first_bounds.size.height) > 0.0);
        assert_eq!(
            heading_bounds(visual, &second_id),
            None,
            "a new origin cannot borrow a recent old rectangle before its own paint"
        );
        view.update(visual, |fixture, app| {
            fixture.document = Arc::clone(&second);
            fixture.left = 400.0;
            app.notify();
        });
        paint_frames(visual);
        let second_bounds = heading_bounds(visual, &second_id)
            .expect("the same shared API paints the replacement heading identity");
        assert!(f32::from(second_bounds.origin.x) > f32::from(first_bounds.origin.x) + 300.0);
        assert_eq!(
            heading_bounds(visual, &return_id),
            None,
            "an equal document on another visit cannot reuse recent bounds"
        );
        view.update(visual, |fixture, app| {
            fixture.place = 2;
            fixture.left = 600.0;
            app.notify();
        });
        paint_frames(visual);
        let returning = heading_bounds(visual, &return_id).expect("actual returning visit paint");
        assert!(f32::from(returning.origin.x) > f32::from(second_bounds.origin.x) + 150.0);
    }
}
