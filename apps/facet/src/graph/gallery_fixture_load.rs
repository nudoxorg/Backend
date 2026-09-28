//! Native review fixtures have an explicit loading boundary. A read or parse
//! failure is data, never a successfully empty graph.
use super::super::model::World;
use crate::gallery::{declare_state, json::Json};
use crate::measure::{Measure, Set};
use crate::theme::ActiveFacet;
use crate::tokens::ty;
use gpui::{
    AnyElement, AnyView, App, AppContext, Context, InteractiveElement, IntoElement, ParentElement,
    Render, StatefulInteractiveElement, Styled, Window, div, px,
};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Debug)]
pub(super) struct LoadError {
    path: PathBuf,
    stage: &'static str,
    detail: String,
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "graph fixture {} failed at {}: {}",
            self.stage,
            self.path.display(),
            self.detail
        )
    }
}

impl LoadError {
    fn new(path: &Path, stage: &'static str, detail: impl ToString) -> Self {
        let error = Self {
            path: path.to_owned(),
            stage,
            detail: detail.to_string(),
        };
        eprintln!("{error}");
        error
    }
}

pub(super) fn load(path: &Path) -> Result<Arc<World>, LoadError> {
    eprintln!("graph fixture loading {}", path.display());
    let bytes = std::fs::read(path).map_err(|error| {
        LoadError::new(
            path,
            "read",
            format!(
                "{error} (kind={:?}, os={:?})",
                error.kind(),
                error.raw_os_error()
            ),
        )
    })?;
    let world = World::from_json(&bytes).map_err(|error| LoadError::new(path, "parse", error))?;
    if world.is_empty() || world.packages.is_empty() {
        return Err(LoadError::new(
            path,
            "contents",
            "the review fixture contains no symbols or packages",
        ));
    }
    eprintln!(
        "graph fixture loaded {} bytes={} nodes={} packages={} modules={} edges={}",
        path.display(),
        bytes.len(),
        world.len(),
        world.packages.len(),
        world.modules.len(),
        world.edges.len()
    );
    Ok(Arc::new(world))
}

struct Failure(LoadError);
impl gpui::Global for Failure {}

pub(super) fn error_view(error: LoadError, cx: &mut App) -> AnyView {
    cx.set_global(Failure(error.clone()));
    declare_state(
        |cx, _| {
            let error = &cx.global::<Failure>().0;
            Json::obj([
                ("source", Json::str("fixture-error")),
                ("stage", Json::str(error.stage)),
                ("path", Json::str(error.path.to_string_lossy())),
                ("detail", Json::str(&error.detail)),
            ])
        },
        cx,
    );
    cx.new(|_| ErrorView(error)).into()
}

struct ErrorView(LoadError);
impl Render for ErrorView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = cx.palette();
        let facet = cx.facet().clone();
        let stage = self.0.stage;
        let detail: String = self.0.detail.chars().take(240).collect();
        div().size_full().bg(palette.g0.hsla()).overflow_hidden().child(
            div().id("graph-load-error").relative().size_full().p(px(16.0)).child(
                gpui::container_query(move |size, _, _| {
                    let measure = Measure::new(size.width, &facet);
                    let text = |key: &'static str, content: String, role| -> AnyElement {
                        let child = div().w_full().flex_shrink_0().set(role, &measure).text_color(palette.ink2.hsla()).child(content.clone());
                        crate::probe::text(key, content, measure.role(role), 1.0, crate::probe::TextOverflow::Wrap, child).into_any_element()
                    };
                    div().id("graph-load-error-content").w_full().max_h(size.height).overflow_y_scroll()
                        .flex().flex_col().gap(px(12.0))
                        .child(text("graph-load-error-title", "Graph data could not be loaded".into(), ty::TITLE))
                        .child(text("graph-load-error-message", match stage {
                            "read" => "The review app could not read its graph data. Open the complete app bundle or choose a readable fixture directory.",
                            "parse" => "The graph data is not valid. Replace it with the review fixture and reopen the app.",
                            _ => "The review fixture has no graph symbols. Choose a populated fixture and reopen the app.",
                        }.into(), ty::SMALL))
                        .child(text("graph-load-error-detail", detail.clone(), ty::SMALL))
                        .into_any_element()
                })
            )
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "facet-fixture-load-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).expect("isolated fixture directory");
            Self(path)
        }
        fn path(&self) -> PathBuf {
            self.0.join("world.json")
        }
        fn write(&self, bytes: &[u8]) {
            std::fs::write(self.path(), bytes).expect("isolated fixture bytes");
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn fixture_failures_preserve_read_parse_and_empty_causes() {
        let f = Fixture::new();
        let read = load(&f.path()).expect_err("missing file must not produce an empty world");
        assert_eq!(read.stage, "read");
        assert_eq!(read.path, f.path());
        assert!(read.detail.contains("NotFound"));
        f.write(b"not json");
        let parse = load(&f.path()).expect_err("invalid bytes must not produce an empty world");
        assert_eq!(parse.stage, "parse");
        assert!(parse.detail.contains("world.json"));
        f.write(br#"{"packages":[],"modules":[],"nodes":[],"rel":[],"edges":[]}"#);
        assert_eq!(
            load(&f.path())
                .expect_err("empty extraction is not a review graph")
                .stage,
            "contents"
        );
        f.write(include_bytes!("../semantics/tests/fixtures/recipes.json"));
        let world = load(&f.path()).expect("pinned populated fixture loads");
        assert!(!world.is_empty());
        assert!(!world.packages.is_empty());
    }

    fn missing_scene(_: &mut Window, cx: &mut App) -> AnyView {
        let f = Fixture::new();
        error_view(
            load(&f.path()).expect_err("native scene holds an actual missing-file error"),
            cx,
        )
    }

    #[test]
    fn native_fixture_failure_paints_visible_bounded_feedback_instead_of_a_graph() {
        let scene = crate::gallery::Scene {
            id: "graph-fixture-error-test",
            title: "Missing graph fixture",
            size: (480, 400),
            build: missing_scene,
        };
        let mut shot = crate::gallery::Shot::new(&scene);
        shot.text_scale = 2.0;
        shot.probe = true;
        shot.scale = 1;
        let frames =
            crate::gallery::capture(&scene, &shot).expect("load-error scene renders normally");
        let state = frames[0]
            .state
            .as_ref()
            .expect("typed fixture failure state")
            .to_string();
        assert!(state.contains("fixture-error") && state.contains("read"));
        let texts = &frames[0].ledger.texts;
        let title = texts
            .iter()
            .find(|text| text.key == "graph-load-error-title")
            .expect("actual title is painted");
        assert_eq!(title.content, "Graph data could not be loaded");
        assert!(
            title.bounds.x >= 0.0
                && title.bounds.y >= 0.0
                && title.bounds.x + title.bounds.width <= 480.0
        );
        assert!(
            texts
                .iter()
                .any(|text| text.key == "graph-load-error-message"
                    && text.content.contains("could not read"))
        );
        assert!(
            !texts
                .iter()
                .any(|text| text.key.starts_with("graph-find")
                    || text.key.starts_with("graph-where"))
        );
    }
}
