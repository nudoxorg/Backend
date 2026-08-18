//! Probe: does lindsey's federated search path actually reach a live
//! `nudox-serve`, and can the app render what comes back?
//!
//! This exists because "the GUI is wired to the index" is not a claim a
//! compile can support. It drives the **real** `SearchStore` — the same one
//! `views::omni_search` renders — through a real query against a live server,
//! then rasterises the rows the store actually holds afterwards.
//!
//! # What this pins today
//!
//! The old escape hatch this file used to test (`search_remote`, a
//! user-visible "Search remote INDEX" button, a separate `RemoteStatus`) is
//! gone. Local and remote results are now one federated list —
//! `heart::surface::Federated` over the local engine (always) and a
//! `RemoteClient` (when `NUDOX_SERVER_URL` parses), merged by
//! `stores::search::SearchStore` and drained into `sections[0]`
//! (`docs/LOCAL-REMOTE-CONTRACT.md` §2). This probe now exercises exactly
//! that path: `set_input` alone is enough to reach the live server, with no
//! separate remote call for the reader (or this probe) to remember to make.
//!
//! The three contract properties that do not need a live server to verify —
//! a duplicate key across two sources converging to one row, a degraded
//! source not failing the answer, and a hit with no `StableReference`
//! producing no fabricated key — are pinned deterministically as unit tests
//! in `stores::search::tests` (`a_repeat_key_replaces_rather_than_appends`,
//! `a_degraded_frame_sets_offline_without_dropping_rows`,
//! `a_hit_with_no_reference_has_no_key`). This probe is the one thing only an
//! E2E run can prove: that the wiring to a *real* `nudox-serve` process, over
//! a real socket, actually works.
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
use lindsey::stores::search_model::{SearchAccess, SectionStatus};
use nudox_engine::runtime::{Engine, EngineConfig};

/// The env var lindsey itself reads (`src/stores/search.rs`), same default.
fn base_url() -> String {
    std::env::var("NUDOX_SERVER_URL").unwrap_or_else(|_| "http://127.0.0.1:8080".to_owned())
}

/// One rendered row, flattened out of the store for rendering and printing.
#[derive(Clone)]
struct Row {
    title: String,
    detail: String,
}

/// Renders the rows the **store** is holding after a real federated search.
/// Not a mock: every string below came off the wire in this process, and
/// nothing here can tell which plane (local or remote) any given row came
/// from — that is the point.
struct FederatedHits {
    query: String,
    base: String,
    hits: usize,
    rows: Vec<Row>,
}

impl Render for FederatedHits {
    fn render(&mut self, _window: &mut gpui::Window, _cx: &mut Context<Self>) -> impl IntoElement {
        let mut column = div()
            .size_full()
            .bg(rgb(0x11131a))
            .text_color(rgb(0xe6e8ef))
            .p(px(28.))
            .child(div().text_color(rgb(0x8be9c8)).child(format!(
                "federated search — remote {} live — query {:?}",
                self.base, self.query
            )))
            .child(
                div()
                    .mt(px(4.))
                    .text_color(rgb(0x7d8499))
                    .child(format!("SearchStore::set_input → {} rows, one list", self.hits)),
            );

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
    // `SearchStore::new`'s remote source is read from `NUDOX_SERVER_URL` at
    // construction — set it explicitly so this probe is federating against
    // exactly the server it just proved is reachable, regardless of what the
    // caller's environment already had set.
    // SAFETY: single-threaded `harness = false` binary; nothing else reads or
    // writes the process environment concurrently.
    unsafe {
        std::env::set_var("NUDOX_SERVER_URL", &base);
    }

    let platform = gpui_platform::current_platform(true);
    let text_system = platform.text_system();
    let mut cx = gpui::HeadlessAppContext::with_platform(
        text_system,
        Arc::new(gpui_component_assets::Assets),
        || gpui_platform::current_headless_renderer(),
    );

    // A real engine over the built-in fixtures, federated with the live
    // server — `SearchStore::new` builds both sources itself; there is no
    // separate remote call for this probe (or a reader) to remember to make.
    let engine = Engine::start_with_fixtures(EngineConfig::default());
    let store = cx.update(|cx| cx.new(|_| SearchStore::new(engine)));

    cx.update(|cx| {
        store.update(cx, |store, cx| {
            store.set_input(query_text.as_str().into(), cx);
        });
    });

    // Drive both executors until the one section settles.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let mut settled = false;
    while std::time::Instant::now() < deadline {
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(25));
        let status = cx.update(|cx| store.read(cx).snapshot().sections[0].status);
        if matches!(
            status,
            SectionStatus::Ready | SectionStatus::Offline | SectionStatus::Unavailable
        ) {
            settled = true;
            break;
        }
    }

    if !settled {
        panic!(
            "the search never settled within 30s. If this is the \"there is no \
             reactor running\" hazard again, `EngineScopedRemote` is not entering \
             the engine's runtime before `RemoteClient::serve` spawns its pump — \
             see `stores::search::EngineScopedRemote`."
        );
    }

    let (offline, rows) = cx.update(|cx| {
        let snap = store.read(cx).snapshot();
        (snap.offline, snap.sections[0].rows.clone())
    });
    assert!(
        !offline,
        "the answer degraded (a configured source did not answer) against a \
         server this probe just proved was reachable — investigate before \
         trusting the rest of this run"
    );

    let hits = rows.len();
    let flat: Vec<Row> = rows
        .iter()
        .map(|row| {
            // `PreparedRow`'s fields are the store's own render model; print
            // whatever its Debug carries so the evidence is the store's
            // state, not a re-fetch.
            let text = format!("{row:?}");
            let title = text.chars().take(110).collect::<String>();
            Row {
                title: title.clone(),
                detail: text.chars().skip(110).take(120).collect::<String>(),
            }
        })
        .collect();

    println!("FEDERATED_OK {hits} hits from {base} for {query_text:?}, one list");
    for row in flat.iter().take(8) {
        println!("  {} {}", row.title, row.detail);
    }
    assert!(
        hits > 0,
        "the federated search returned zero hits for {query_text:?} — ingest a \
         package first (`local-backends.nu ingest rust itoa 1.0.11`); an empty \
         result proves nothing"
    );

    // ── Rasterise the store's real rows ─────────────────────────────────────
    let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/shots");
    std::fs::create_dir_all(&out).expect("create tests/shots");

    let window = cx
        .open_window(size(px(1100.), px(720.)), |_, cx| {
            cx.new(|_| FederatedHits {
                query: query_text.clone(),
                base: base.clone(),
                hits,
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
        "FEDERATED_PAINTED {}x{} ({non_blank} opaque px) -> {}",
        image.width(),
        image.height(),
        path.display()
    );
}
