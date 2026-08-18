//! Probe: does lindsey's remote escape hatch actually reach a live `nudox-serve`,
//! and can the app render what comes back?
//!
//! This exists because "the GUI is wired to the index" is not a claim a compile
//! can support. It drives the **real** `SearchStore` through the **real**
//! `SearchAccess::search_remote` — the same method
//! `views::omni_search`'s "Search remote INDEX" button calls — against a real
//! server, then rasterises the rows the store actually holds afterwards.
//!
//! It is specifically a regression test for a defect this probe found:
//! `search_remote_inner` drives `heart::client::http::NudoxClient::search`,
//! which is **async reqwest**, and reqwest's futures abort with "there is no
//! reactor running, must be called from the context of a Tokio 1.x runtime"
//! unless polled inside a Tokio runtime. GPUI's executor is not one, so the
//! button was dead on arrival against any live server. See
//! `SearchStore::search_remote_inner` for the fix.
//!
//! `harness = false` for the same reason as `shot_probe.rs`: macOS platform
//! construction touches AppKit, which aborts off the process main thread.
//!
//! Requires a live index (see docs/TESTING.md, "A live index with real
//! embeddings"):
//!
//! ```text
//! nu .config/scripts/local-backends.nu up
//! nu .config/scripts/local-backends.nu serve
//! nu .config/scripts/local-backends.nu ingest rust itoa 1.0.11
//! ```
//!
//! Skips (exit 0, loudly) when no server is listening, so it never turns a
//! missing optional backend into a red suite.

use std::sync::Arc;

use gpui::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, Styled as _, div, px, rgb,
    size,
};

use lindsey::stores::SearchStore;
use lindsey::stores::search_model::{RemoteStatus, SearchAccess};
use nudox_engine::runtime::{Engine, EngineConfig};

/// The env var lindsey itself reads (`src/stores/search.rs`), same default.
fn base_url() -> String {
    std::env::var("NUDOX_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_owned())
}

/// One remote row, flattened out of the store for rendering and printing.
#[derive(Clone)]
struct Row {
    title: String,
    detail: String,
}

/// Renders the rows the **store** is holding after a real remote search. Not a
/// mock: every string below came off the wire in this process.
struct RemoteHits {
    query: String,
    base: String,
    hits: usize,
    elapsed_ms: u64,
    rows: Vec<Row>,
}

impl Render for RemoteHits {
    fn render(&mut self, _window: &mut gpui::Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let mut column = div()
            .size_full()
            .bg(rgb(0x11131a))
            .text_color(rgb(0xe6e8ef))
            .p(px(28.))
            .child(div().text_color(rgb(0x8be9c8)).child(format!(
                "remote INDEX — {} — query {:?}",
                self.base, self.query
            )))
            .child(div().mt(px(4.)).text_color(rgb(0x7d8499)).child(format!(
                "SearchStore::search_remote → {} hits in {} ms, live from nudox-serve",
                self.hits, self.elapsed_ms
            )));

        for row in self.rows.iter().take(8) {
            column = column.child(
                div()
                    .mt(px(14.))
                    .p(px(10.))
                    .bg(rgb(0x1e2230))
                    .child(div().child(row.title.clone()))
                    .child(
                        div()
                            .mt(px(2.))
                            .text_color(rgb(0x9aa3b8))
                            .child(row.detail.clone()),
                    ),
            );
        }
        column
    }
}

fn main() {
    let base = base_url();
    let query_text = std::env::var("NUDOX_REMOTE_QUERY").unwrap_or_else(|_| "Buffer".to_owned());

    // Skip rather than fail when the optional live backend is absent.
    let probe_addr = base
        .trim_start_matches("http://")
        .trim_end_matches('/')
        .to_owned();
    match probe_addr
        .parse()
        .map_err(|e| format!("{e}"))
        .and_then(|addr| {
            std::net::TcpStream::connect_timeout(&addr, std::time::Duration::from_millis(500))
                .map_err(|e| format!("{e}"))
        }) {
        Ok(_) => {}
        Err(error) => {
            println!(
                "SKIP: no nudox-serve on {base} ({error}). See docs/TESTING.md, \
                 \"A live index with real embeddings\"."
            );
            return;
        }
    }

    let platform = gpui_platform::current_platform(true);
    let text_system = platform.text_system();
    let mut cx = gpui::HeadlessAppContext::with_platform(
        text_system,
        Arc::new(gpui_component_assets::Assets),
        || gpui_platform::current_headless_renderer(),
    );

    // A real engine over the built-in fixtures. Its corpus is irrelevant here —
    // the escape hatch is precisely the path for terms the *local* corpus does
    // not know — but `SearchStore` is generic over a live `SearchEngine` and
    // handing it a real one keeps this on the production type.
    let engine = Engine::start_with_fixtures(EngineConfig::default());

    let store = cx.update(|cx| cx.new(|_| SearchStore::new(engine)));

    // The exact sequence the zero-hit screen performs: the store holds the
    // query text, then the button calls `search_remote`.
    cx.update(|cx| {
        store.update(cx, |store, cx| {
            store.set_input(query_text.as_str().into(), cx);
            store.search_remote(cx);
        });
    });

    // Drive both executors until the request settles.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut settled = None;
    while std::time::Instant::now() < deadline {
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(25));
        let status = cx.update(|cx| store.read(cx).snapshot().remote.clone());
        match status {
            RemoteStatus::Loading | RemoteStatus::Idle => continue,
            other => {
                settled = Some(other);
                break;
            }
        }
    }

    let (hits, elapsed_ms, rows) = match settled {
        None => panic!(
            "`search_remote` never left `Loading` within 30s. If this is the \
             \"there is no reactor running\" hazard again, the escape hatch is \
             polling reqwest outside a Tokio runtime — see \
             `SearchStore::search_remote_inner`."
        ),
        Some(RemoteStatus::NotConfigured) => panic!(
            "the store reported NotConfigured — NUDOX_SERVER_URL ({base}) did not \
             parse into a client, so no request was ever attempted"
        ),
        Some(RemoteStatus::Unreachable { kind, detail }) => {
            panic!("remote search against {base} failed: {kind:?} — {detail}")
        }
        Some(RemoteStatus::Ready {
            hits,
            elapsed_ms,
            rows,
        }) => (hits, elapsed_ms, rows),
        Some(other) => panic!("unexpected terminal status: {other:?}"),
    };

    // `PreparedRow`'s fields are the store's own render model; print whatever
    // its Debug carries so the evidence is the store's state, not a re-fetch.
    let flat: Vec<Row> = rows
        .iter()
        .map(|row| {
            let text = format!("{row:?}");
            let title = text.chars().take(110).collect::<String>();
            Row {
                title: title.clone(),
                detail: text.chars().skip(110).take(120).collect::<String>(),
            }
        })
        .collect();

    println!("REMOTE_OK {hits} hits from {base} for {query_text:?} in {elapsed_ms} ms");
    for row in rows.iter().take(8) {
        println!("  {row:?}");
    }
    assert!(
        hits > 0,
        "the live index returned zero hits for {query_text:?} — ingest a package first \
         (`local-backends.nu ingest rust itoa 1.0.11`); an empty result proves nothing"
    );
    assert_eq!(
        rows.len().min(hits),
        rows.len(),
        "every returned hit must have become a render row"
    );

    // ── Rasterise the store's real rows ─────────────────────────────────────
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/shots");
    std::fs::create_dir_all(&out).expect("create tests/shots");

    let window = cx
        .open_window(size(px(1100.), px(720.)), |_, cx| {
            cx.new(|_| RemoteHits {
                query: query_text.clone(),
                base: base.clone(),
                hits,
                elapsed_ms,
                rows: flat,
            })
        })
        .expect("open headless window");
    cx.run_until_parked();

    let image = cx
        .capture_screenshot(window.into())
        .expect("capture_screenshot");
    let path = out.join("remote-index.png");
    image.save(&path).expect("save png");

    let non_blank = image.pixels().filter(|p| p.0[3] > 0).count();
    assert!(
        non_blank > (image.width() * image.height() / 2) as usize,
        "the frame is mostly transparent ({non_blank} opaque px) — nothing painted",
    );
    println!(
        "REMOTE_PAINTED {}x{} ({non_blank} opaque px) -> {}",
        image.width(),
        image.height(),
        path.display()
    );
}
