#![cfg(target_os = "linux")]
#![forbid(unsafe_code)]

use std::{
    fs,
    io::Read,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const DISPLAY: &str = ":99";
const DEADLINE: Duration = Duration::from_secs(30);

struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
        }
        let _ = self.0.wait();
    }
}

fn command_exists(name: &str) {
    let status = Command::new(name)
        .arg("-help")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    assert!(status.is_ok(), "required Linux GUI tool is missing: {name}");
}

fn wait_for_x11(xvfb: &mut ChildGuard) {
    let socket = Path::new("/tmp/.X11-unix/X99");
    let deadline = Instant::now() + DEADLINE;
    while !socket.exists() {
        assert!(
            xvfb.0.try_wait().expect("poll Xvfb").is_none(),
            "Xvfb exited before publishing its display socket"
        );
        assert!(Instant::now() < deadline, "Xvfb did not become ready");
        thread::sleep(Duration::from_millis(20));
    }
}

/// The desktop writes its diagnostics to a file rather than a pipe: nothing
/// drains a pipe while the window is awaited, so a chatty startup would block
/// on a full pipe and never map its window.
fn desktop_log(log: &Path) -> String {
    fs::read_to_string(log).unwrap_or_else(|error| format!("<unreadable desktop log: {error}>"))
}

fn visible_window(desktop: &mut Child, log: &Path) -> String {
    let pid = desktop.id().to_string();
    let deadline = Instant::now() + DEADLINE;
    loop {
        if let Some(status) = desktop.try_wait().expect("poll desktop") {
            panic!(
                "backend-desktop exited with {status} before publishing a window:\n{}",
                desktop_log(log)
            );
        }
        let output = Command::new("xdotool")
            .args(["search", "--onlyvisible", "--pid", &pid])
            .env("DISPLAY", DISPLAY)
            .output()
            .expect("query visible desktop window");
        if output.status.success()
            && let Some(window) = String::from_utf8_lossy(&output.stdout)
                .lines()
                .find(|line| !line.is_empty())
        {
            return window.to_owned();
        }
        assert!(
            Instant::now() < deadline,
            "backend-desktop did not publish a visible X11 window:\n{}",
            desktop_log(log)
        );
        thread::sleep(Duration::from_millis(50));
    }
}

/// A window is mapped before its first frame is drawn, so an immediate
/// capture is a blank surface that compresses to a few hundred bytes. Retry
/// until the capture carries rendered content or the deadline passes.
fn rendered_capture(window: &str, screenshot: &Path, log: &Path) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let capture = Command::new("import")
            .args(["-display", DISPLAY, "-window", window])
            .arg(screenshot)
            .output()
            .expect("capture desktop window");
        assert!(
            capture.status.success(),
            "desktop screenshot failed: {}",
            String::from_utf8_lossy(&capture.stderr)
        );
        let mut png = Vec::new();
        fs::File::open(screenshot)
            .expect("open desktop screenshot")
            .read_to_end(&mut png)
            .expect("read desktop screenshot");
        if png.len() > 1024 {
            return png;
        }
        assert!(
            Instant::now() < deadline,
            "desktop screenshot stayed implausibly empty ({} bytes):\n{}",
            png.len(),
            desktop_log(log)
        );
        thread::sleep(Duration::from_millis(250));
    }
}

fn wait_for_exit(child: &mut Child, deadline: Duration) -> bool {
    let end = Instant::now() + deadline;
    loop {
        if child.try_wait().expect("poll desktop").is_some() {
            return true;
        }
        if Instant::now() >= end {
            return false;
        }
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn linux_desktop_opens_captures_and_closes_a_real_window() {
    for tool in ["Xvfb", "openbox", "xdotool", "import"] {
        command_exists(tool);
    }

    let root = std::env::temp_dir().join(format!("nudox-linux-window-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let project = root.join("project");
    let data = root.join("data");
    let home = root.join("home");
    let runtime = root.join("runtime");
    fs::create_dir_all(&project).expect("create desktop project");
    fs::create_dir_all(&data).expect("create desktop data root");
    fs::create_dir_all(&home).expect("create desktop home");
    fs::create_dir_all(&runtime).expect("create desktop runtime");
    fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700))
        .expect("secure desktop runtime");
    fs::write(
        project.join("Cargo.toml"),
        "[package]\nname = \"linux-window-smoke\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
    )
    .expect("write desktop project manifest");

    let mut xvfb = ChildGuard(
        Command::new("Xvfb")
            .args([DISPLAY, "-screen", "0", "1280x720x24", "-nolisten", "tcp"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn Xvfb"),
    );
    wait_for_x11(&mut xvfb);
    let mut window_manager = ChildGuard(
        Command::new("openbox")
            .env("DISPLAY", DISPLAY)
            .env("HOME", &home)
            .env("XDG_RUNTIME_DIR", &runtime)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn Openbox"),
    );
    thread::sleep(Duration::from_millis(100));
    assert!(
        window_manager.0.try_wait().expect("poll Openbox").is_none(),
        "Openbox exited before the desktop launch"
    );

    let log = root.join("desktop.log");
    let log_file = fs::File::create(&log).expect("create desktop log");
    let mut desktop = ChildGuard(
        Command::new(env!("CARGO_BIN_EXE_backend-desktop"))
            .current_dir(&project)
            .env("DISPLAY", DISPLAY)
            .env("HOME", &home)
            .env("XDG_DATA_HOME", &data)
            .env("XDG_RUNTIME_DIR", &runtime)
            .env("BACKEND_PROJECT", &project)
            .env("NUDOX_DATA_ROOT", data.join("workspace"))
            .env("LIBGL_ALWAYS_SOFTWARE", "1")
            .env("MESA_LOADER_DRIVER_OVERRIDE", "llvmpipe")
            .env("NO_AT_BRIDGE", "1")
            .env("RUST_BACKTRACE", "1")
            .stdout(Stdio::null())
            .stderr(log_file)
            .spawn()
            .expect("spawn backend-desktop"),
    );

    let window = visible_window(&mut desktop.0, &log);
    let screenshot = root.join("desktop.png");
    let png = rendered_capture(&window, &screenshot, &log);
    assert!(png.starts_with(b"\x89PNG\r\n\x1a\n"), "capture is not PNG");

    let closed = Command::new("xdotool")
        .args(["windowclose", &window])
        .env("DISPLAY", DISPLAY)
        .status()
        .expect("request desktop window close");
    assert!(closed.success(), "xdotool could not close desktop window");
    if !wait_for_exit(&mut desktop.0, Duration::from_secs(10)) {
        let _ = desktop.0.kill();
        let _ = desktop.0.wait();
        panic!(
            "backend-desktop did not exit after window close:\n{}",
            desktop_log(&log)
        );
    }

    drop(desktop);
    drop(window_manager);
    drop(xvfb);
    fs::remove_dir_all(&root).expect("remove Linux desktop smoke fixture");
}
