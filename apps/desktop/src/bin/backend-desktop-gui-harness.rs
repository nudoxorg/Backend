//! Executable live matrix runner for the desktop visual gate.
//!
//! This binary intentionally lives beside the product crate: every capture
//! starts the same local service/bootstrap as the visible application and
//! renders the production `Workspace` through GPUI CE's headless renderer.
//! A failed scenario is written to the run report and makes the command fail;
//! no fixture image is generated to hide an unavailable live capability.

use backend_desktop::{
    LiveCapture, capture_live_workspace_journey, capture_live_workspace_with_semantics,
    production_action_inventory,
};
use backend_gui_harness::{
    ActionDescriptor, CaptureConfig, CaptureSession, FocusState, GuiState, InputStep, OverlayState,
    PageState, Viewport, animation_frames_for_state, preflight_viewport, verify_run,
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
    requested_frames: usize,
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
    actions: Vec<backend_desktop::WorkspaceActionProbe>,
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

#[derive(Debug)]
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

    let (states, journeys) =
        select_capture_targets(state_filter.as_deref(), journey_filter.as_deref(), smoke)?;

    std::fs::create_dir_all(&output).map_err(|error| format!("create output: {error}"))?;
    let scales = resolve_scales(viewport_filter, scale_filter, smoke)?;
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
        action_tree_source: "rendered-workspace-action-tree".to_owned(),
        ..RunReport::default()
    };

    for scale in scales {
        let configs = viewport_filter
            .map_or_else(
                || CaptureConfig::required_at_scale(scale),
                |viewport| Ok(vec![CaptureConfig::deterministic(viewport)]),
            )
            .map_err(|error| error.to_string())?
            .into_iter()
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
                let frames = animation_frames_for_state(&config, state);
                report.requested_frames += frames.len();
                match capture_live_workspace_with_semantics(
                    config.clone(),
                    state.clone(),
                    &[],
                    &frames,
                ) {
                    Ok(live) => match write_capture(&mut session, live, &mut report, None) {
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
            for journey in &journeys {
                report.attempted_captures += 1;
                let mut journey_config = config_for_journey(&config, journey.id);
                let journey_frames = animation_frames_for_state(&journey_config, &journey.from);
                report.requested_frames += journey_frames.len();
                let journey_viewport = match preflight_viewport(
                    journey_config.viewport,
                    &journey.steps,
                    &journey_frames,
                ) {
                    Ok(viewport) => viewport,
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
                            error: Some(error.to_string()),
                        });
                        report.failures.push(Failure {
                            state: format!("journey--{}", journey.id),
                            viewport: config.viewport.suffix(),
                            error: error.to_string(),
                        });
                        continue;
                    }
                };
                journey_config.viewport = journey_viewport;
                let journey_suffix = journey_viewport.suffix();
                if !report
                    .viewports
                    .iter()
                    .any(|suffix| suffix == &journey_suffix)
                {
                    report.viewports.push(journey_suffix.clone());
                }
                let journey_root = output.join(&journey_suffix);
                let mut journey_session = CaptureSession::new(journey_config.clone(), journey_root)
                    .map_err(|error| error.to_string())?;
                if let Some(baseline) = baseline.as_deref() {
                    journey_session =
                        journey_session.with_baseline_root(baseline.join(&journey_suffix));
                }
                let mut live = match capture_live_workspace_journey(
                    journey_config.clone(),
                    journey.from.clone(),
                    &journey.steps,
                    &journey_frames,
                ) {
                    Ok(live) => live,
                    Err(error) => {
                        report.journeys.push(JourneyResult {
                            id: journey.id.to_owned(),
                            from: journey.from.id.clone(),
                            to: journey.to.id.clone(),
                            viewport: journey_suffix.clone(),
                            frame_count: 0,
                            steps: journey.steps.clone(),
                            final_semantics: None,
                            passed: false,
                            error: Some(error.clone()),
                        });
                        report.failures.push(Failure {
                            state: format!("journey--{}", journey.id),
                            viewport: journey_suffix.clone(),
                            error,
                        });
                        continue;
                    }
                };
                let final_semantics = live.semantics.last().cloned();
                let passed = final_semantics.as_ref().is_some_and(|probe| {
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
                        viewport: journey_suffix.clone(),
                        error: "journey produced identical animation frames".to_owned(),
                    });
                }
                match write_capture(&mut journey_session, live, &mut report, Some(journey.id)) {
                    Ok(()) => report.captured_captures += 1,
                    Err(write_error) => report.failures.push(Failure {
                        state: format!("journey--{}", journey.id),
                        viewport: journey_suffix.clone(),
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
                        actions: final_semantics
                            .as_ref()
                            .map_or_else(Vec::new, |probe| probe.actions.clone()),
                        observations,
                    },
                ) {
                    report.failures.push(Failure {
                        state: format!("journey--{}", journey.id),
                        viewport: journey_suffix.clone(),
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
                    viewport: journey_suffix.clone(),
                    frame_count,
                    steps: journey.steps.clone(),
                    final_semantics,
                    passed,
                    error,
                });
                if !passed {
                    report.failures.push(Failure {
                        state: format!("journey--{}", journey.id),
                        viewport: journey_suffix,
                        error: "journey endpoint was not reached".to_owned(),
                    });
                }
            }
        }
    }
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
    report.pass = report.attempted_captures > 0
        && report.requested_frames > 0
        && report.failures.is_empty()
        && report.captured_captures == report.attempted_captures
        && report.captured_frames == report.requested_frames
        && report.verified_manifests == report.manifests.len()
        && report.verified_frames == report.captured_frames;
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
    session: &mut CaptureSession,
    mut live: LiveCapture,
    report: &mut RunReport,
    script_id: Option<&str>,
) -> Result<(), String> {
    if live.capture.frames.is_empty() || live.semantics.len() != live.capture.frames.len() {
        return Err("capture produced no frames or an incomplete semantic timeline".to_owned());
    }
    if !live.capture.state.reduced_motion
        && live.capture.frames.len() > 1
        && live
            .capture
            .frames
            .windows(2)
            .all(|frames| frames[0].image == frames[1].image)
    {
        return Err("normal-motion capture produced identical animation evidence".to_owned());
    }
    if live.capture.state.reduced_motion
        && live
            .capture
            .frames
            .windows(2)
            .any(|frames| frames[0].image != frames[1].image)
    {
        return Err("reduced-motion capture changed after settling".to_owned());
    }
    if live.capture.frames.iter().any(|frame| {
        let mut pixels = frame.image.pixels();
        pixels
            .next()
            .is_some_and(|first| pixels.all(|pixel| pixel == first))
    }) {
        return Err("capture contains a uniform/blank frame".to_owned());
    }
    let revisions = live
        .semantics
        .iter()
        .map(|probe| probe.data_revision.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    if revisions.len() != 1 {
        return Err(format!(
            "admitted data revision changed during capture: {} revisions",
            revisions.len()
        ));
    }
    let revision = revisions
        .into_iter()
        .next()
        .filter(|revision| !revision.trim().is_empty() && *revision != "undetermined")
        .ok_or_else(|| "semantic probe did not expose an admitted data revision".to_owned())?;
    if live
        .semantics
        .iter()
        .any(|probe| probe.coordinate.trim().is_empty())
    {
        return Err("capture has no deterministic live project coordinate".to_owned());
    }
    if live
        .semantics
        .iter()
        .any(|probe| probe.coordinate_revision != probe.data_revision)
    {
        return Err("live coordinate was selected against a different data revision".to_owned());
    }
    session.config.data_revision = revision.to_owned();
    session.config.theme = live.capture.state.theme;
    let state_id = live.capture.state.id.clone();
    let semantic_path = format!("semantics/{}.json", state_id);
    session
        .writer
        .write_json(&semantic_path, &live.semantics)
        .map_err(|error| error.to_string())?;
    let manifest_result = session.write_set(&mut live.capture, script_id, Some(&semantic_path));
    let manifest_path = format!(
        "{}/manifests/{}.json",
        session.config.viewport.suffix(),
        state_id
    );
    report.manifests.push(manifest_path);
    report.captured_frames += live.capture.frames.len();
    manifest_result.map_err(|error| error.to_string())?;
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

fn select_capture_targets(
    state_filter: Option<&str>,
    journey_filter: Option<&str>,
    smoke: bool,
) -> Result<(Vec<GuiState>, Vec<JourneySpec>), String> {
    if state_filter.is_some() && journey_filter.is_some() {
        return Err("--state and --journey cannot be combined".to_owned());
    }

    let mut states = GuiState::catalog();
    if smoke && state_filter.is_none() {
        states.clear();
    }
    if let Some(filter) = state_filter {
        states.retain(|state| state.id == filter);
        if states.is_empty() {
            return Err(format!("unknown state {filter:?}"));
        }
    }

    let mut journeys = journey_catalog();
    if let Some(filter) = journey_filter {
        journeys.retain(|journey| journey.id == filter);
        if journeys.is_empty() {
            return Err(format!("unknown journey {filter:?}"));
        }
    }

    // A state filter selects exactly that state. A journey filter selects
    // exactly that journey. With neither filter the complete catalog remains
    // the default, preserving full matrix coverage.
    if state_filter.is_some() {
        journeys.clear();
    } else if journey_filter.is_some() {
        states.clear();
    }
    Ok((states, journeys))
}

fn resolve_scales(
    viewport_filter: Option<Viewport>,
    scale_filter: Option<u8>,
    smoke: bool,
) -> Result<Vec<u8>, String> {
    if let (Some(viewport), Some(scale)) = (viewport_filter, scale_filter)
        && viewport.scale != scale
    {
        return Err(format!(
            "viewport scale {} conflicts with --scale {scale}",
            viewport.scale
        ));
    }
    Ok(viewport_filter.map_or_else(
        || {
            scale_filter.map_or_else(
                || if smoke { vec![1] } else { vec![1, 2] },
                |scale| vec![scale],
            )
        },
        |viewport| vec![viewport.scale],
    ))
}

fn config_for_journey(base: &CaptureConfig, id: &str) -> CaptureConfig {
    let mut config = base.clone();
    if matches!(id, "omnibar-ime-composition" | "rtl-reduced-motion") {
        config.ime_mode = "marked-text".to_owned();
    }
    if id == "rtl-reduced-motion" {
        config.text_direction = "rtl".to_owned();
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_filter_captures_only_the_requested_state() {
        let (states, journeys) =
            select_capture_targets(Some("browse"), None, false).expect("state selection");
        assert_eq!(
            states
                .iter()
                .map(|state| state.id.as_str())
                .collect::<Vec<_>>(),
            ["browse"]
        );
        assert!(journeys.is_empty());
    }

    #[test]
    fn journey_filter_captures_only_the_requested_journey() {
        let (states, journeys) = select_capture_targets(None, Some("palette-open-dismiss"), false)
            .expect("journey selection");
        assert!(states.is_empty());
        assert_eq!(
            journeys
                .iter()
                .map(|journey| journey.id)
                .collect::<Vec<_>>(),
            ["palette-open-dismiss"]
        );
    }

    #[test]
    fn no_filter_keeps_the_full_state_and_journey_catalogs() {
        let (states, journeys) = select_capture_targets(None, None, false).expect("catalog");
        assert_eq!(states.len(), GuiState::catalog().len());
        assert_eq!(journeys.len(), journey_catalog().len());
    }

    #[test]
    fn state_and_journey_filters_are_rejected_together() {
        let error = select_capture_targets(Some("browse"), Some("palette-open-dismiss"), false)
            .expect_err("conflicting filters");
        assert!(error.contains("cannot be combined"));
    }

    #[test]
    fn viewport_scale_conflicts_are_rejected() {
        let viewport = Viewport::new(1440, 1000, 2).expect("viewport");
        let error = resolve_scales(Some(viewport), Some(1), false).expect_err("scale conflict");
        assert!(error.contains("conflicts"));
    }

    #[test]
    fn arbitrary_viewport_uses_its_requested_scale_once() {
        let viewport = Viewport::new(1440, 1000, 1).expect("viewport");
        assert_eq!(
            resolve_scales(Some(viewport), None, false).expect("scale"),
            [1]
        );
        assert_eq!(
            resolve_scales(Some(viewport), Some(1), false).expect("scale"),
            [1]
        );
    }
}
