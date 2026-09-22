//! Small production adapter for the shared screenshot and animation harness.
//!
//! The binary intentionally delegates clocks, input, frame sequencing,
//! manifests, and baseline policy to `backend-gui-harness`. Its only product
//! concern is asking the single-state desktop to render a live local index.

#[cfg(feature = "visual-harness")]
use backend_desktop::harness::{capture_journey, capture_journey_with_scale, capture_live};
#[cfg(feature = "visual-harness")]
use backend_desktop::navigation::journey_specs::JourneyId;
#[cfg(feature = "visual-harness")]
use backend_gui_harness::{
    CaptureConfig, CaptureSession, GuiState, Viewport, animation_frames_for_state, parse_state,
};

#[cfg(feature = "visual-harness")]
fn main() -> std::process::ExitCode {
    match run(std::env::args().skip(1).collect()) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("backend-desktop-gui-harness: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

#[cfg(not(feature = "visual-harness"))]
fn main() -> std::process::ExitCode {
    eprintln!("backend-desktop-gui-harness requires the visual-harness feature");
    std::process::ExitCode::FAILURE
}

#[cfg(feature = "visual-harness")]
fn run(args: Vec<String>) -> Result<(), String> {
    if args
        .first()
        .is_none_or(|arg| matches!(arg.as_str(), "help" | "--help" | "-h"))
    {
        println!(
            "backend-desktop-gui-harness\n\n  capture [--output DIR] [--state ID] [--journey ID] [--viewport WIDTHxHEIGHT@SCALE] [--text-scale PERCENT]\n\nCaptures live GPUI frames from the production single-state desktop.\n\nJourneys: cold-empty, picker-cancelled, indexing, ready-multi-project, failure-retry, persisted-restart, mcp-setup."
        );
        return Ok(());
    }
    if args.first().map(String::as_str) != Some("capture") {
        return Err("unknown command; use --help".to_owned());
    }
    let output =
        option(&args[1..], "--output").unwrap_or_else(|| ".artifacts/gui-harness".to_owned());
    if let Some(id) = option(&args[1..], "--journey") {
        let journey =
            JourneyId::parse(&id).ok_or_else(|| format!("unknown onboarding journey {id:?}"))?;
        let viewport = option(&args[1..], "--viewport")
            .map(|value| parse_viewport(&value))
            .transpose()?
            .unwrap_or(Viewport::new(1280, 800, 1).map_err(|error| error.to_string())?);
        let text_scale = option(&args[1..], "--text-scale")
            .map(|value| {
                value
                    .parse::<u16>()
                    .map_err(|_| "invalid text scale".to_owned())
            })
            .transpose()?
            .unwrap_or(100);
        let config = CaptureConfig::deterministic(viewport);
        let mut capture = if text_scale == 100 {
            capture_journey(config.clone(), journey)?
        } else {
            capture_journey_with_scale(config.clone(), journey, text_scale)?
        };
        let session = CaptureSession::new(config, output).map_err(|error| error.to_string())?;
        let manifest = session
            .write_set(&mut capture, None, None)
            .map_err(|error| error.to_string())?;
        println!(
            "captured {} frames for onboarding journey {}",
            manifest.frames.len(),
            journey.as_str()
        );
        return Ok(());
    }
    let state = option(&args[1..], "--state")
        .map(|id| parse_state(&id).map_err(|error| error.to_string()))
        .transpose()?
        .unwrap_or_else(|| {
            GuiState::new(
                "live-package",
                Some(backend_gui_harness::PageState::Package),
                None,
            )
        });
    let viewport = option(&args[1..], "--viewport")
        .map(|value| parse_viewport(&value))
        .transpose()?
        .unwrap_or(Viewport::new(1280, 800, 1).map_err(|error| error.to_string())?);
    let config = CaptureConfig::deterministic(viewport);
    let frames = animation_frames_for_state(&config, &state);
    let mut capture = capture_live(config.clone(), state, &[], &frames)?;
    let session = CaptureSession::new(config, output).map_err(|error| error.to_string())?;
    let manifest = session
        .write_set(&mut capture, None, None)
        .map_err(|error| error.to_string())?;
    println!(
        "captured {} frames for {}",
        manifest.frames.len(),
        manifest.state.id
    );
    Ok(())
}

#[cfg(feature = "visual-harness")]
fn option(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
}

#[cfg(feature = "visual-harness")]
fn parse_viewport(value: &str) -> Result<Viewport, String> {
    let (dimensions, scale) = value
        .split_once('@')
        .ok_or("viewport must be WIDTHxHEIGHT@SCALE")?;
    let (width, height) = dimensions
        .split_once('x')
        .ok_or("viewport must be WIDTHxHEIGHT@SCALE")?;
    Viewport::new(
        width.parse().map_err(|_| "invalid viewport width")?,
        height.parse().map_err(|_| "invalid viewport height")?,
        scale
            .trim_end_matches('x')
            .parse()
            .map_err(|_| "invalid viewport scale")?,
    )
    .map_err(|error| error.to_string())
}
