//! Bounded, owner-observed file addresses beside one independently checked
//! Cargo file. The list never supplies bytes or semantic symbol links.

use crate::model::browse::CargoSourceInventoryModel;
use crate::navigation::{CargoSourceRoute, Intent, Route};
use crate::shell::bodies::{Ctx, Leaf};
use crate::shell::focus::Target;
use crate::shell::kit::{quiet, text};
use crate::shell::reader::Reader;
use backend_library::{CargoPackageSourceInventoryCoverageV1 as Coverage, CargoPackageSourceInventoryGapV1 as Gap};
use facet::{Control, Space, tokens::ty};
use gpui::{App, AppContext as _, ClickEvent, Context, ElementId, InteractiveElement, ParentElement, SharedString, StatefulInteractiveElement, Styled, Window, div};
use std::rc::Rc;
use std::sync::Arc;

const FILE_WINDOW: usize = 24;

struct WindowPage {
    start: usize,
}

pub(super) fn leaf(
    model: &Arc<CargoSourceInventoryModel>,
    route: &CargoSourceRoute,
    ctx: &mut Ctx<'_>,
    window: &mut Window,
    cx: &mut Context<Reader>,
) -> Leaf {
    let selected = model.paths.binary_search(&route.file).ok();
    let initial = selected.map_or(0, |at| page_start(at));
    let pager_id: ElementId = format!(
        "cargo-inventory-page-{}-{:x?}",
        ctx.place_key, model.source_revision
    ).into();
    let state = window.use_keyed_state(pager_id, cx, move |_, _| WindowPage { start: initial });
    let last = page_start(model.paths.len().saturating_sub(1));
    let start = state.read(cx).start.min(last);
    let end = start.saturating_add(FILE_WINDOW).min(model.paths.len());
    let measure = ctx.measure;
    let palette = ctx.palette;
    let mut column = div().flex().flex_col().gap(measure.space(Space::Tight));
    column = column.child(text(ty::BODY, &measure, palette.ink1).child("Cargo files"));
    let coverage = coverage_words(&model.coverage, model.paths.len());
    column = column.child(quiet(coverage, &measure, palette));
    if model.paths.is_empty() {
        column = column.child(quiet("No addressable files were observed.", &measure, palette));
    } else {
        column = column.child(quiet(
            format!("Files {}–{end} of {}", start + 1, model.paths.len()),
            &measure,
            palette,
        ));
    }
    if selected.is_none() && !model.paths.is_empty() {
        column = column.child(quiet(
            "The open file is outside this bounded listing or changed since it was listed.",
            &measure,
            palette,
        ));
    }
    if start > 0 {
        let previous = start.saturating_sub(FILE_WINDOW);
        let state = state.clone();
        let id: SharedString = "cargo-inventory-previous".into();
        let act: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_, app| {
            state.update(app, |page, cx| {
                page.start = previous;
                cx.notify();
            });
        });
        ctx.targets.push(Target {
            id: id.clone(),
            label: "Previous Cargo files".into(),
            act: act.clone(),
            peek: None,
            source: None,
        });
        column = column.child(ctx.targets.track(
            id.clone(),
            facet::controls::button(id, "Previous files", &measure)
                .ghost()
                .size(Control::Small)
                .on_click(move |window, app| act(window, app)),
        ));
    }
    for path in &model.paths[start..end] {
        let Some(destination) = CargoSourceRoute::new(
            route.project.clone(),
            route.package.clone(),
            path.clone(),
            None,
        ) else {
            continue;
        };
        let id: SharedString = format!("cargo-inventory-file-{}", path.as_str()).into();
        let leaving = Route::CargoSource(route.clone());
        let links = ctx.links.clone();
        let recall = ctx.targets.recall();
        let action_id = id.clone();
        let act: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_, app| {
            recall.focus(action_id.clone());
            recall.remember_leave(leaving.clone(), action_id.clone());
            links.dispatch(Intent::Navigate(Route::CargoSource(destination.clone())), app);
        });
        ctx.targets.push(Target {
            id: id.clone(),
            label: format!("Open Cargo file {}", path.as_str()).into(),
            act: act.clone(),
            peek: None,
            source: None,
        });
        let mut words = div().flex().flex_wrap().min_w_0();
        for chunk in chunks(path.as_str()) {
            words = words.child(text(ty::MONO_ROW, &measure, palette.ink1).child(chunk));
        }
        let click = act.clone();
        let row = div()
            .id(id.clone())
            .flex()
            .min_w_0()
            .px(measure.space(Space::Tight))
            .py(measure.space(Space::Tight))
            .border_l_2()
            .border_color(if path == &route.file { palette.peri.line.hsla() } else { palette.line1.hsla() })
            .cursor_pointer()
            .on_click(move |_: &ClickEvent, window, app| click(window, app))
            .child(words);
        column = column.child(ctx.targets.track(id, row));
    }
    if end < model.paths.len() {
        let next = end;
        let state = state.clone();
        let id: SharedString = "cargo-inventory-next".into();
        let act: Rc<dyn Fn(&mut Window, &mut App)> = Rc::new(move |_, app| {
            state.update(app, |page, cx| {
                page.start = next;
                cx.notify();
            });
        });
        ctx.targets.push(Target {
            id: id.clone(),
            label: "Next Cargo files".into(),
            act: act.clone(),
            peek: None,
            source: None,
        });
        column = column.child(ctx.targets.track(
            id.clone(),
            facet::controls::button(id, "Next files", &measure)
                .ghost()
                .size(Control::Small)
                .on_click(move |window, app| act(window, app)),
        ));
    }
    Leaf::new(column)
}

fn page_start(index: usize) -> usize {
    index / FILE_WINDOW * FILE_WINDOW
}

fn chunks(path: &str) -> Vec<String> {
    let mut result = Vec::new();
    let mut current = String::new();
    for letter in path.chars() {
        current.push(letter);
        if current.chars().count() == 12 {
            result.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        result.push(current);
    }
    result
}

fn coverage_words(coverage: &Coverage, count: usize) -> String {
    match coverage {
        Coverage::Complete => format!("All {count} safely addressable files were listed. Each file is checked again when opened."),
        Coverage::Truncated { limit } => format!("Showing the first {limit} files. More files exist; this listing has no continuation yet."),
        Coverage::Partial { reason } => {
            let reason = match reason {
                Gap::DirectoryEntryLimit => "a folder exceeded the per-folder entry limit",
                Gap::ScanEntryLimit => "the scan reached its entry limit",
                Gap::DepthLimit => "a folder was deeper than the supported scan",
                Gap::UnaddressablePath => "some paths could not be represented exactly",
                Gap::DirectoryUnavailable => "a folder changed or could not be opened safely",
            };
            format!("Partial file list: {reason}. The {count} shown paths are exact; omitted files remain unknown.")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_pages_keep_long_paths_exact_and_selected_file_visible() {
        assert_eq!(page_start(0), 0);
        assert_eq!(page_start(23), 0);
        assert_eq!(page_start(24), 24);
        assert_eq!(page_start(511), 504);
        let path = "src/very_long_component_name/ümlaut/README.md";
        assert_eq!(chunks(path).concat(), path);
        assert!(chunks(path).iter().all(|chunk| chunk.chars().count() <= 12));
    }

    #[test]
    fn partial_inventory_never_claims_a_complete_empty_tree() {
        let text = coverage_words(&Coverage::Partial { reason: Gap::DirectoryUnavailable }, 0);
        assert!(text.contains("Partial"));
        assert!(text.contains("unknown"));
        assert!(!text.contains("All 0"));
    }
}
