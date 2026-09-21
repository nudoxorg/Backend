//! Executable live matrix runner for the desktop visual gate.
//!
//! This binary intentionally lives beside the product crate: every capture
//! starts the same local service/bootstrap as the visible application and
//! renders the production `Workspace` through GPUI CE's headless renderer.
//! A failed scenario is written to the run report and makes the command fail;
//! no fixture image is generated to hide an unavailable live capability.

use backend_desktop::{
    capture_live_workspace_journey, capture_live_workspace_with_semantics,
    production_action_inventory, LiveCapture,
};
use backend_gui_harness::{
    animation_frames, verify_run, ActionDescriptor, CaptureConfig, CaptureSession, FocusState,
    GuiState, InputStep, OverlayState, PageState, Viewport,
};
use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Default, Serialize)]
struct RunReport {
    schema: u32,
    command: String,
    output: String,
    viewports: Vec<String>,
    requested_states: usize,
    attempted_captures: usize,
    captured_captures: usize,
    captured_frames: usize,
    failures: Vec<Failure>,
    manifests: Vec<String>,
    semantic_artifacts: Vec<String>,
    journey_artifacts: Vec<String>,
    verified_manifests: usize,
    verified_frames: usize,
    registered_actions: Vec<ActionDescriptor>,
    action_tree_source: String,
    journeys: Vec<JourneyResult>,
    pass: bool,
}

#[derive(Debug, Serialize)]
struct Failure {
    state: String,
    viewport: String,
    error: String,
}

#[derive(Debug, Serialize)]
struct JourneyResult {
    id: String,
    from: String,
    to: String,
    viewport: String,
    frame_count: usize,
    steps: Vec<InputStep>,
    final_semantics: Option<backend_desktop::WorkspaceSemanticProbe>,
    passed: bool,
    error: Option<String>,
}

#[derive(Debug, Serialize)]
struct JourneyArtifact {
    id: String,
    from: String,
    to: String,
    steps: Vec<InputStep>,
    actions: Vec<ActionDescriptor>,
    observations: Vec<JourneyObservation>,
}

#[derive(Debug, Serialize)]
struct JourneyObservation {
    frame: usize,
    label: String,
    time_ms: u64,
    input_index: Option<usize>,
    semantics: Option<backend_desktop::WorkspaceSemanticProbe>,
}

struct JourneySpec {
    id: &'static str,
    from: GuiState,
    to: GuiState,
    steps: Vec<InputStep>,
    /// The adapter-owned environment values that are changed by the script.
    /// Keeping them beside the endpoint makes the final assertion prove the
    /// product actually applied the locale/direction, rather than merely
    /// recording those input events in the artifact.
    locale: &'static str,
    direction: &'static str,
}

fn main() -> std::process::ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("backend-desktop-gui-harness: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run(args: Vec<String>) -> Result<(), String> {
    let command = args.first().map(String::as_str).unwrap_or("help");
    match command {
        "help" | "--help" | "-h" => {
            print_help();
            Ok(())
        }
        "capture" => capture_command(&args[1..]),
        other => Err(format!("unknown command {other:?}; use --help")),
    }
}

fn capture_command(args: &[String]) -> Result<(), String> {
    let output = option(args, "--output")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".artifacts/gui-harness"));
    let state_filter = option(args, "--state");
    let journey_filter = option(args, "--journey");
    if let Some(filter) = journey_filter.as_deref()
        && !journey_catalog().iter().any(|journey| journey.id == filter)
    {
        return Err(format!("unknown journey {filter:?}"));
    }
    let viewport_filter = option(args, "--viewport")
        .map(|value| parse_viewport(&value))
        .transpose()?;
    let scale_filter = option(args, "--scale")
        .map(|value| value.parse::<u8>().map_err(|_| "--scale expects 1 or 2"))
        .transpose()?
        .map(|scale| {
            if matches!(scale, 1 | 2) {
                Ok(scale)
            } else {
                Err("--scale expects 1 or 2")
            }
        })
        .transpose()?;
    let baseline = option(args, "--baseline").map(PathBuf::from);
    let smoke = args.iter().any(|arg| arg == "--smoke");

    std::fs::create_dir_all(&output).map_err(|error| format!("create output: {error}"))?;
    let mut states = GuiState::catalog();
    if smoke {
        states.clear();
    }
    if let Some(filter) = state_filter.as_deref() {
        states.retain(|state| state.id == filter);
        if states.is_empty() {
            return Err(format!("unknown state {filter:?}"));
        }
    }
    let scales: Vec<u8> = scale_filter.map_or_else(
        || if smoke { vec![1] } else { vec![1, 2] },
        |scale| vec![scale],
    );
    let registered_actions = production_action_inventory();
    if registered_actions.is_empty() {
        return Err("production window registered no actions".to_owned());
    }
    let mut report = RunReport {
        schema: 1,
        command: "backend-desktop-gui-harness capture".to_owned(),
        output: output.to_string_lossy().into_owned(),
        requested_states: states.len(),
        viewports: Vec::new(),
        registered_actions,
        action_tree_source: "production-window-bindings".to_owned(),
        ..RunReport::default()
    };

    for scale in scales {
        let configs = CaptureConfig::required_at_scale(scale)
            .map_err(|error| error.to_string())?
            .into_iter()
            .filter(|config| viewport_filter.is_none_or(|viewport| viewport == config.viewport))
            .filter(|config| {
                !smoke || (config.viewport.width, config.viewport.height) == (640, 480)
            })
            .collect::<Vec<_>>();
        for config in configs {
            report.viewports.push(config.viewport.suffix());
            let viewport_root = output.join(config.viewport.suffix());
            let mut session = CaptureSession::new(config.clone(), &viewport_root)
                .map_err(|error| error.to_string())?;
            if let Some(baseline) = baseline.as_deref() {
                session = session.with_baseline_root(baseline.join(config.viewport.suffix()));
            }
            for state in &states {
                report.attempted_captures += 1;
                let frames = animation_frames(&config, state.reduced_motion);
                match capture_live_workspace_with_semantics(
                    config.clone(),
                    state.clone(),
                    &[],
                    &frames,
                ) {
                    Ok(live) => match write_capture(&session, live, &mut report, None) {
                        Ok(()) => {
                            report.captured_captures += 1;
                        }
                        Err(error) => report.failures.push(Failure {
                            state: state.id.clone(),
                            viewport: config.viewport.suffix(),
                            error,
                        }),
                    },
                    Err(error) => report.failures.push(Failure {
                        state: state.id.clone(),
                        viewport: config.viewport.suffix(),
                        error,
                    }),
                }
            }
            for journey in journey_catalog().into_iter().filter(|journey| {
                journey_filter
                    .as_deref()
                    .is_none_or(|filter| filter == journey.id)
            }) {
                report.attempted_captures += 1;
                let journey_config = config_for_journey(&config, journey.id);
                let journey_session = CaptureSession::new(journey_config.clone(), &viewport_root)
                    .map_err(|error| error.to_string())?;
                let mut live = match capture_live_workspace_journey(
                    journey_config.clone(),
                    journey.from.clone(),
                    &journey.steps,
                    &animation_frames(&journey_config, journey.from.reduced_motion),
                ) {
                    Ok(live) => live,
                    Err(error) => {
                        report.journeys.push(JourneyResult {
                            id: journey.id.to_owned(),
                            from: journey.from.id.clone(),
                            to: journey.to.id.clone(),
                            viewport: config.viewport.suffix(),
                            frame_count: 0,
                            steps: journey.steps.clone(),
                            final_semantics: None,
                            passed: false,
                            error: Some(error.clone()),
                        });
                        report.failures.push(Failure {
                            state: format!("journey--{}", journey.id),
                            viewport: config.viewport.suffix(),
                            error,
                        });
                        continue;
                    }
                };
                let final_semantics = live.semantics.last().cloned();
                let passed = final_semantics
                    .as_ref()
                    .is_some_and(|probe| {
                        journey_matches(probe, &journey.to, journey.locale, journey.direction)
                    });
                let error = (!passed).then(|| "journey endpoint was not reached".to_owned());
                live.capture.state.id = format!("journey--{}", journey.id);
                let frame_count = live.capture.frames.len();
                let observations = live
                    .capture
                    .frames
                    .iter()
                    .enumerate()
                    .map(|(frame, record)| JourneyObservation {
                        frame,
                        label: record.label.clone(),
                        time_ms: record.time_ms,
                        input_index: record.input_index,
                        semantics: live.semantics.get(frame).cloned(),
                    })
                    .collect::<Vec<_>>();
                let varied = live.capture.frames.first().is_none_or(|first| {
                    live.capture.frames.len() <= 1
                        || live
                            .capture
                            .frames
                            .iter()
                            .skip(1)
                            .any(|frame| frame.image != first.image)
                });
                if !varied {
                    report.failures.push(Failure {
                        state: format!("journey--{}", journey.id),
                        viewport: config.viewport.suffix(),
                        error: "journey produced identical animation frames".to_owned(),
                    });
                }
                match write_capture(&journey_session, live, &mut report, Some(journey.id)) {
                    Ok(()) => report.captured_captures += 1,
                    Err(write_error) => report.failures.push(Failure {
                        state: format!("journey--{}", journey.id),
                        viewport: config.viewport.suffix(),
                        error: write_error,
                    }),
                }
                let journey_artifact = format!("journeys/{}.json", journey.id.replace('/', "-"));
                if let Err(write_error) = journey_session.writer.write_json(
                    &journey_artifact,
                    &JourneyArtifact {
                        id: journey.id.to_owned(),
                        from: journey.from.id.clone(),
                        to: journey.to.id.clone(),
                        steps: journey.steps.clone(),
                        actions: report.registered_actions.clone(),
                        observations,
                    },
                ) {
                    report.failures.push(Failure {
                        state: format!("journey--{}", journey.id),
                        viewport: config.viewport.suffix(),
                        error: write_error.to_string(),
                    });
                } else {
                    report.journey_artifacts.push(format!(
                        "{}/{}",
                        journey_session.config.viewport.suffix(),
                        journey_artifact
                    ));
                }
                report.journeys.push(JourneyResult {
                    id: journey.id.to_owned(),
                    from: journey.from.id.clone(),
                    to: journey.to.id.clone(),
                    viewport: config.viewport.suffix(),
                    frame_count,
                    steps: journey.steps.clone(),
                    final_semantics,
                    passed,
                    error,
                });
                if !passed {
                    report.failures.push(Failure {
                        state: format!("journey--{}", journey.id),
                        viewport: config.viewport.suffix(),
                        error: "journey endpoint was not reached".to_owned(),
                    });
                }
            }
        }
    }
    report.pass =
        report.failures.is_empty() && report.captured_captures == report.attempted_captures;
    match verify_run(&output) {
        Ok(verified) => {
            report.verified_manifests = verified.manifests;
            report.verified_frames = verified.frames;
        }
        Err(error) => report.failures.push(Failure {
            state: "artifact-verifier".to_owned(),
            viewport: "all".to_owned(),
            error: error.to_string(),
        }),
    }
    report.pass =
        report.failures.is_empty() && report.captured_captures == report.attempted_captures;
    let report_path = output.join("run-report.json");
    std::fs::write(
        &report_path,
        serde_json::to_vec_pretty(&report).map_err(|error| error.to_string())?,
    )
    .map_err(|error| format!("write {}: {error}", report_path.display()))?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
    );
    if report.pass {
        Ok(())
    } else {
        Err(format!(
            "{} of {} live captures failed; see {}",
            report.failures.len(),
            report.attempted_captures,
            report_path.display()
        ))
    }
}

fn write_capture(
    session: &CaptureSession,
    mut live: LiveCapture,
    report: &mut RunReport,
    script_id: Option<&str>,
) -> Result<(), String> {
    if live.capture.frames.iter().any(|frame| {
        let mut pixels = frame.image.pixels();
        pixels
            .next()
            .is_some_and(|first| pixels.all(|pixel| pixel == first))
    }) {
        return Err("capture contains a uniform/blank frame".to_owned());
    }
    let state_id = live.capture.state.id.clone();
    let manifest = session
        .write_set(&mut live.capture, script_id)
        .map_err(|error| error.to_string())?;
    let manifest_path = format!(
        "{}/manifests/{}.json",
        session.config.viewport.suffix(),
        state_id
    );
    report.manifests.push(manifest_path);
    report.captured_frames += manifest.frames.len();
    let semantic_path = format!("semantics/{}.json", state_id);
    session
        .writer
        .write_json(&semantic_path, &live.semantics)
        .map_err(|error| error.to_string())?;
    report.semantic_artifacts.push(format!(
        "{}/{}",
        session.config.viewport.suffix(),
        semantic_path
    ));
    Ok(())
}

fn journey_catalog() -> Vec<JourneySpec> {
    vec![
        JourneySpec {
            id: "palette-open-dismiss",
            from: GuiState::new("journey-start", None, None),
            to: GuiState::new("browse", Some(PageState::Browse), None),
            steps: vec![
                InputStep::key("cmd-shift-h"),
                InputStep::key("cmd-shift-p"),
                InputStep::key("tab"),
                InputStep::Modifiers {
                    shift: true,
                    control: false,
                    alt: false,
                    command: false,
                },
                InputStep::key("tab"),
                InputStep::Modifiers {
                    shift: false,
                    control: false,
                    alt: false,
                    command: false,
                },
                InputStep::Wait { milliseconds: 16 },
                InputStep::key("escape"),
            ],
            locale: "en-US",
            direction: "ltr",
        },
        JourneySpec {
            id: "settings-open-dismiss",
            from: GuiState::new("journey-start", None, None),
            to: GuiState::new("browse", Some(PageState::Browse), None),
            steps: vec![
                InputStep::key("cmd-shift-h"),
                InputStep::key("cmd-,"),
                InputStep::Wait { milliseconds: 16 },
                InputStep::key("escape"),
            ],
            locale: "en-US",
            direction: "ltr",
        },
        JourneySpec {
            id: "omnibar-ime-composition",
            from: GuiState::new("journey-start", None, None),
            to: GuiState::new(
                "browse--omnibar",
                Some(PageState::Browse),
                Some(OverlayState::Omnibar),
            )
            .with_focus(FocusState::Omnibar),
            steps: vec![
                InputStep::key("cmd-shift-h"),
                InputStep::key("cmd-k"),
                InputStep::ImeCompose {
                    value: "日本".to_owned(),
                },
                InputStep::Wait { milliseconds: 32 },
                InputStep::ImeCommit {
                    value: "日本語".to_owned(),
                },
                InputStep::Wait { milliseconds: 32 },
                InputStep::ImeCompose {
                    value: "取り消し".to_owned(),
                },
                InputStep::Wait { milliseconds: 32 },
                InputStep::ImeCancel,
                InputStep::Wait { milliseconds: 32 },
            ],
            locale: "en-US",
            direction: "ltr",
        },
        JourneySpec {
            id: "focus-pointer-scroll-resize",
            from: GuiState::new("journey-start", None, None),
            to: GuiState::new("browse", Some(PageState::Browse), None),
            steps: vec![
                InputStep::key("cmd-shift-h"),
                InputStep::FocusNext,
                InputStep::FocusPrevious,
                InputStep::key("tab"),
                InputStep::key("shift-tab"),
                InputStep::key("enter"),
                InputStep::key("space"),
                InputStep::PointerMove {
                    x: 320.0,
                    y: 240.0,
                    pressed_button: None,
                },
                InputStep::Click {
                    x: 320.0,
                    y: 240.0,
                    button: "left".to_owned(),
                },
                InputStep::PointerDown {
                    x: 320.0,
                    y: 240.0,
                    button: "left".to_owned(),
                },
                InputStep::PointerMove {
                    x: 360.0,
                    y: 260.0,
                    pressed_button: Some("left".to_owned()),
                },
                InputStep::PointerUp {
                    x: 360.0,
                    y: 260.0,
                    button: "left".to_owned(),
                },
                InputStep::Scroll {
                    x: 320.0,
                    y: 240.0,
                    delta_x: 0.0,
                    delta_y: 120.0,
                },
                InputStep::Resize {
                    width: 800,
                    height: 600,
                },
            ],
            locale: "en-US",
            direction: "ltr",
        },
        JourneySpec {
            id: "theme-locale-scale",
            from: GuiState::new("journey-start", None, None),
            to: GuiState::new("browse", Some(PageState::Browse), None)
                .with_theme(backend_gui_harness::ThemeState::Vellum),
            steps: vec![
                InputStep::key("cmd-shift-h"),
                InputStep::Theme {
                    value: "vellum".to_owned(),
                },
                InputStep::Locale {
                    value: "ar-EG".to_owned(),
                },
                InputStep::Scale { factor: 2 },
                InputStep::Wait { milliseconds: 32 },
            ],
            locale: "ar-EG",
            direction: "ltr",
        },
        JourneySpec {
            id: "rtl-reduced-motion",
            from: GuiState::new("journey-start", None, None).with_reduced_motion(true),
            to: GuiState::new("browse", Some(PageState::Browse), None)
                .with_theme(backend_gui_harness::ThemeState::Vellum)
                .with_reduced_motion(true),
            steps: vec![
                InputStep::key("cmd-shift-h"),
                InputStep::Locale {
                    value: "ar-EG".to_owned(),
                },
                InputStep::Theme {
                    value: "vellum".to_owned(),
                },
                InputStep::Wait { milliseconds: 32 },
            ],
            locale: "ar-EG",
            direction: "rtl",
        },
    ]
}

fn config_for_journey(base: &CaptureConfig, id: &str) -> CaptureConfig {
    let mut config = base.clone();
    if id == "rtl-reduced-motion" {
        config.text_direction = "rtl".to_owned();
        config.ime_mode = "marked-text".to_owned();
    }
    config
}

fn journey_matches(
    probe: &backend_desktop::WorkspaceSemanticProbe,
    expected: &GuiState,
    expected_locale: &str,
    expected_direction: &str,
) -> bool {
    let page = match expected.page {
        None | Some(PageState::Browse) => "browse",
        Some(PageState::Project) => "project",
        Some(PageState::Package) => "package",
        _ => "declaration",
    };
    let overlay = match expected.overlay {
        None => "none",
        Some(OverlayState::Omnibar) => "omnibar",
        Some(OverlayState::Palette) => "palette",
        Some(OverlayState::Notice) => "notice",
        Some(OverlayState::Fault) => "none",
        Some(OverlayState::SettingsAppearance)
        | Some(OverlayState::SettingsEditor)
        | Some(OverlayState::SettingsAgents)
        | Some(OverlayState::SettingsDiagnostics)
        | Some(OverlayState::SettingsLegend)
        | Some(OverlayState::SettingsIndex)
        | Some(OverlayState::SettingsRegistry) => "settings",
        Some(_) => return false,
    };
    let focus = expected.focus.as_str();
    page == probe.page
        && overlay == probe.overlay
        && focus == probe.focus
        && expected.theme.as_str() == probe.theme
        && probe.reduced_motion == expected.reduced_motion
        && probe.locale == expected_locale
        && probe.text_direction == expected_direction
}

fn option(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

fn parse_viewport(value: &str) -> Result<Viewport, String> {
    let (size, scale) = value
        .split_once('@')
        .ok_or("viewport must be WIDTHxHEIGHT@SCALE")?;
    let (width, height) = size
        .split_once('x')
        .ok_or("viewport must be WIDTHxHEIGHT@SCALE")?;
    let scale = scale.strip_suffix('x').unwrap_or(scale);
    Viewport::new(
        width.parse().map_err(|_| "invalid viewport width")?,
        height.parse().map_err(|_| "invalid viewport height")?,
        scale.parse().map_err(|_| "invalid viewport scale")?,
    )
    .map_err(|error| error.to_string())
}

fn print_help() {
    println!(
        "backend-desktop-gui-harness\n\nCommands:\n  capture [--output DIR] [--state ID] [--scale 1|2] [--viewport WIDTHxHEIGHT@SCALE] [--baseline DIR] [--smoke]\n\nThe capture command starts the live desktop host, renders every catalog state at every required viewport and scale, writes PNG frame sequences, manifests, semantic probes, and run-report.json, then exits nonzero when any live state cannot be reached or differs from baseline. --smoke runs only the production-input journey catalog at 640x480@1x for a fast live-index check."
    );
}
