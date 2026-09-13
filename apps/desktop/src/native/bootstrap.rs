//! Native process bootstrap and initial durable-view hydration.

use super::{
    App, Bounds, DEFAULT_INPUT_CONTEXT, DesktopHost, Duration, ExitCode, FocusGlobalSearch,
    KeyBinding, Model, NativeApp, UnixSubscriptionTransport, ViewRoot, WindowBounds, WindowOptions,
    default_bindings, px, size,
};
use gpui::AppContext;

pub(crate) fn run() -> ExitCode {
    let (host, root, cursor, transport) = match open_initial_revision() {
        Ok(initial) => initial,
        Err(error) => {
            eprintln!("backend-desktop: {error}");
            return ExitCode::from(70);
        }
    };
    let model = match Model::try_new_at(root.clone(), cursor, root.basis().root) {
        Ok(model) => model,
        Err(error) => {
            eprintln!("backend-desktop: admit initial revision: {error}");
            return ExitCode::from(70);
        }
    };
    gpui_platform::application().run(move |cx: &mut App| {
        cx.bind_keys(default_bindings().as_keybindings(Some(DEFAULT_INPUT_CONTEXT)));
        cx.bind_keys([KeyBinding::new("cmd-k", FocusGlobalSearch, None)]);
        let bounds = Bounds::centered(None, size(px(1360.0), px(860.0)), cx);
        if let Err(error) = cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            move |_, cx| cx.new(|cx| NativeApp::new(host, root, model, transport, cx)),
        ) {
            eprintln!("backend-desktop: open native window: {error}");
        }
        cx.activate(true);
    });
    ExitCode::SUCCESS
}

fn open_initial_revision() -> Result<
    (
        DesktopHost,
        ViewRoot,
        backend_library::Cursor,
        UnixSubscriptionTransport,
    ),
    String,
> {
    const ATTEMPTS: usize = 12;
    let mut last_error = "local service did not become ready".to_owned();
    for attempt in 0..ATTEMPTS {
        match DesktopHost::start() {
            Ok(host) => {
                let mut transport = UnixSubscriptionTransport::connect(host.endpoint())
                    .map_err(|error| format!("open snapshot transport: {error}"))?;
                match transport.bootstrap_root() {
                    Ok((root, cursor)) => return Ok((host, root, cursor, transport)),
                    Err(bootstrap) => {
                        last_error = format!("hydrate initial snapshot: {bootstrap}");
                    }
                }
                drop(transport);
                drop(host);
            }
            Err(error) => last_error = error.to_string(),
        }
        if attempt + 1 < ATTEMPTS {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
    Err(last_error)
}
