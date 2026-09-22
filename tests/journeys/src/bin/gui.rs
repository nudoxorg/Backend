//! Small process wrapper for the production GPUI cold-launch journeys.
//!
//! The wrapper exists so the restart test can start and reap a real desktop
//! process.  It deliberately uses the same live capture adapter as the
//! shipped GUI harness; there is no in-process fake store or seeded shelf.

#![deny(unsafe_code)]

#[cfg(unix)]
use backend_desktop::harness::capture_live;
#[cfg(unix)]
use backend_gui_harness::{
    CaptureConfig, GuiState, InputStep, PageState, Viewport, animation_frames_for_state,
};
#[cfg(unix)]
use serde_json::json;
#[cfg(unix)]
use std::path::{Path, PathBuf};

#[cfg(unix)]
fn main() -> std::process::ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("backend-journey-gui: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(unix))]
fn main() -> std::process::ExitCode {
    eprintln!("backend-journey-gui requires the Unix desktop subscription transport");
    std::process::ExitCode::FAILURE
}

#[cfg(unix)]
fn run(args: Vec<String>) -> Result<(), String> {
    let journey = args.first().map(String::as_str).unwrap_or("first-launch");
    let viewport = Viewport::new(640, 480, 1).map_err(|error| error.to_string())?;
    let config = CaptureConfig::deterministic(viewport);

    let (state, actions, expected_onboarding, minimum_shelf) = match journey {
        "first-launch" => (
            GuiState::new("onboarding", Some(PageState::Browse), None),
            vec![InputStep::Wait { milliseconds: 32 }],
            true,
            0,
        ),
        "choose-project" => (
            GuiState::new("onboarding", Some(PageState::Browse), None),
            choose_project_steps(),
            false,
            1,
        ),
        other => return Err(format!("unknown GUI journey {other:?}")),
    };
    let frames = animation_frames_for_state(&config, &state);
    let capture = capture_live(config, state, &actions, &frames)?;
    let probe = capture
        .semantic_probes
        .last()
        .ok_or_else(|| "live GUI capture emitted no semantic probes".to_owned())?;
    if capture.frames.is_empty() {
        return Err("live GUI capture emitted no frames".to_owned());
    }
    let status = probe
        .nodes
        .iter()
        .find(|node| node.id == "status-bar")
        .and_then(|node| node.value.as_deref())
        .ok_or_else(|| "live GUI semantics omitted the versioned workspace status".to_owned())?;
    let (data_revision, shelf_count) = parse_status(status)?;
    let onboarding = shelf_count == 0;
    if onboarding != expected_onboarding {
        return Err(format!(
            "GUI journey ended with onboarding={}, expected {expected_onboarding}: {}",
            onboarding,
            serde_json::to_string(probe).unwrap_or_else(|_| "<unserializable>".to_owned())
        ));
    }
    if shelf_count < minimum_shelf {
        let trace = capture
            .semantic_probes
            .iter()
            .map(|probe| {
                format!(
                    "frame={} time={} modal={:?} focus={:?} route={}",
                    probe.frame, probe.time_ms, probe.modal_root, probe.focused, probe.route
                )
            })
            .collect::<Vec<_>>()
            .join("; ");
        return Err(format!(
            "GUI journey admitted {} shelf projects, expected at least {minimum_shelf}; trace: {trace}",
            shelf_count,
        ));
    }
    if data_revision.trim().is_empty() {
        return Err("GUI semantic probe omitted its admitted data revision".to_owned());
    }
    if probe.route.trim().is_empty() {
        return Err("GUI replacement shell omitted its route contract".to_owned());
    }
    if probe.nodes.is_empty() {
        return Err("GUI replacement shell omitted its published action tree".to_owned());
    }
    println!(
        "{}",
        json!({
            "journey": journey,
            "frames": capture.frames.len(),
            "semantic_probes": capture.semantic_probes.len(),
            "onboarding": onboarding,
            "shelf_count": shelf_count,
            "data_revision": data_revision,
            "route": probe.route,
            "screenshot_sha256": probe.screenshot_sha256,
        })
    );
    Ok(())
}

#[cfg(unix)]
fn parse_status(value: &str) -> Result<(&str, u64), String> {
    let revision = value
        .strip_prefix("revision=")
        .and_then(|value| value.split_once(";shelf="))
        .ok_or_else(|| format!("invalid versioned workspace status {value:?}"))?;
    let shelf = revision
        .1
        .parse::<u64>()
        .map_err(|_| format!("invalid shelf count in workspace status {value:?}"))?;
    Ok((revision.0, shelf))
}

#[cfg(unix)]
fn choose_project_steps() -> Vec<InputStep> {
    vec![
        InputStep::key("cmd-n"),
        InputStep::Text {
            value: project_path("tests/journeys/fixtures/polyglot"),
        },
        InputStep::key("enter"),
        InputStep::Wait { milliseconds: 32 },
    ]
}

#[cfg(unix)]
fn project_path(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative);
    canonical_or_original(&path).to_string_lossy().into_owned()
}

#[cfg(unix)]
fn canonical_or_original(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}
