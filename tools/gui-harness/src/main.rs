//! Command line control plane for the deterministic GUI harness.

use backend_gui_harness::{
    CaptureConfig, DiffPolicy, GuiState, TransitionScript, Viewport, animation_frames, compare,
    diff_image, validate_run,
};
use std::{env, process::ExitCode};

fn main() -> ExitCode {
    match run(env::args().skip(1).collect()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("backend-gui-harness: {error}");
            ExitCode::FAILURE
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
        "list-states" => list_states(args.get(1).is_some_and(|arg| arg == "--json")),
        "list-viewports" => list_viewports(),
        "plan-animation" => {
            let viewport = args.get(1).ok_or("plan-animation requires WIDTHxHEIGHT@SCALE")?;
            let reduced = args.get(2).is_some_and(|arg| arg == "--reduced-motion");
            let viewport = parse_viewport(viewport)?;
            let config = CaptureConfig::deterministic(viewport);
            let frames = animation_frames(&config, reduced);
            println!("{}", serde_json::to_string_pretty(&frames).map_err(|e| e.to_string())?);
            Ok(())
        }
        "validate" => {
            let states = GuiState::catalog();
            let scripts = TransitionScript::baseline();
            validate_run(&states, &scripts).map_err(|e| e.to_string())?;
            println!("validated {} states and {} transition scripts", states.len(), scripts.len());
            Ok(())
        }
        "compare" => compare_command(&args[1..]),
        "capture" => Err("capture requires an in-process desktop adapter; use the library's capture_gpui_state API from the desktop harness test target".to_owned()),
        other => Err(format!("unknown command {other:?}; use --help")),
    }
}

fn list_states(json: bool) -> Result<(), String> {
    let states = GuiState::catalog();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&states).map_err(|e| e.to_string())?
        );
    } else {
        for state in states {
            let page = state.page.map_or("none", |page| page.as_str());
            let overlay = state.overlay.map_or("none", |overlay| overlay.as_str());
            println!("{}\tpage={page}\toverlay={overlay}", state.id);
        }
    }
    Ok(())
}

fn list_viewports() -> Result<(), String> {
    for viewport in Viewport::required() {
        println!("{}", viewport.suffix());
    }
    Ok(())
}

fn compare_command(args: &[String]) -> Result<(), String> {
    let baseline = args
        .first()
        .ok_or("compare requires BASELINE.png ACTUAL.png DIFF.png")?;
    let actual = args
        .get(1)
        .ok_or("compare requires BASELINE.png ACTUAL.png DIFF.png")?;
    let diff = args
        .get(2)
        .ok_or("compare requires BASELINE.png ACTUAL.png DIFF.png")?;
    let baseline = image::open(baseline)
        .map_err(|e| e.to_string())?
        .into_rgba8();
    let actual = image::open(actual).map_err(|e| e.to_string())?.into_rgba8();
    let metrics = compare(&baseline, &actual, DiffPolicy::default()).map_err(|e| e.to_string())?;
    if metrics.changed() {
        diff_image(&baseline, &actual, 0)
            .save(diff)
            .map_err(|e| e.to_string())?;
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&metrics).map_err(|e| e.to_string())?
    );
    if metrics.within_policy {
        Ok(())
    } else {
        Err("comparison exceeded the configured pixel policy".to_owned())
    }
}

fn parse_viewport(value: &str) -> Result<Viewport, String> {
    let (size, scale) = value
        .split_once('@')
        .ok_or("viewport must be WIDTHxHEIGHT@SCALE")?;
    let (width, height) = size
        .split_once('x')
        .ok_or("viewport must be WIDTHxHEIGHT@SCALE")?;
    let scale = scale.strip_suffix('x').unwrap_or(scale);
    let width = width
        .parse::<u32>()
        .map_err(|_| "invalid viewport width".to_owned())?;
    let height = height
        .parse::<u32>()
        .map_err(|_| "invalid viewport height".to_owned())?;
    let scale = scale
        .parse::<u8>()
        .map_err(|_| "invalid viewport scale".to_owned())?;
    Viewport::new(width, height, scale).map_err(|error| error.to_string())
}

fn print_help() {
    println!(
        "backend-gui-harness\n\nCommands:\n  list-states [--json]\n  list-viewports\n  plan-animation WIDTHxHEIGHT@SCALE [--reduced-motion]\n  validate\n  compare BASELINE.png ACTUAL.png DIFF.png\n\nThe capture_gpui_state library API is used by the desktop adapter to render real GPUI scenes."
    );
}
