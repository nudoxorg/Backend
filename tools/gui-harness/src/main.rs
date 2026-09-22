//! Command line control plane for the deterministic GUI harness.

use backend_gui_harness::{
    CaptureConfig, ConformancePolicy, DesignContract, DiffPolicy, GuiState, TransitionScript,
    Viewport, animation_frames, compare, diff_image, resolve_contract_root, summarize_contract,
    validate_run, verify_capture_run, verify_run, verify_run_for_baseline_update,
};
use std::{
    env, fs,
    path::{Path, PathBuf},
    process::ExitCode,
};

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
            let mut scripts = TransitionScript::baseline();
            scripts.extend(TransitionScript::stress());
            scripts.extend(TransitionScript::ime_contract());
            validate_run(&states, &scripts).map_err(|e| e.to_string())?;
            println!("validated {} states and {} transition scripts", states.len(), scripts.len());
            Ok(())
        }
        "design-contract" => design_contract_command(&args[1..]),
        "extract-design-contract" => {
            let mut extract_args = vec!["extract".to_owned()];
            extract_args.extend_from_slice(&args[1..]);
            design_contract_command(&extract_args)
        }
        "verify-conformance" | "conformance" => conformance_command(&args[1..]),
        "compare" => compare_command(&args[1..]),
        "verify" => verify_command(&args[1..]),
        "accept-baseline" => accept_baseline_command(&args[1..]),
        "capture" => Err("capture requires an in-process desktop adapter; use the library's capture_gpui_state API from the desktop harness test target".to_owned()),
        other => Err(format!("unknown command {other:?}; use --help")),
    }
}

fn conformance_command(args: &[String]) -> Result<(), String> {
    let run_root = args
        .first()
        .filter(|arg| !arg.starts_with('-'))
        .ok_or("verify-conformance requires RUN_ROOT")?;
    let mut policy = ConformancePolicy::default();
    if args.iter().any(|arg| arg == "--allow-partial") {
        policy.require_full_matrix = false;
    }
    if args.iter().any(|arg| arg == "--allow-missing-report") {
        policy.require_run_report = false;
    }
    if args.iter().any(|arg| arg == "--allow-missing-reference") {
        policy.require_reference = false;
    }
    let contract = if let Some(path) = option(args, "--contract") {
        Some(DesignContract::read_json(Path::new(&path)).map_err(|error| error.to_string())?)
    } else if let Some(root) = option(args, "--contract-root") {
        Some(DesignContract::extract(Path::new(&root)).map_err(|error| error.to_string())?)
    } else {
        let root = resolve_contract_root(None).map_err(|error| error.to_string())?;
        Some(DesignContract::extract(&root).map_err(|error| error.to_string())?)
    };
    let report = verify_capture_run(Path::new(run_root), contract.as_ref(), policy)
        .map_err(|error| error.to_string())?;
    let report_path = Path::new(run_root).join("conformance-report.json");
    fs::write(
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
            "capture conformance failed with {} failure(s); see {}",
            report.failures.len(),
            report_path.display()
        ))
    }
}

fn design_contract_command(args: &[String]) -> Result<(), String> {
    let subcommand = args.first().map(String::as_str).unwrap_or("extract");
    match subcommand {
        "extract" | "summary" => {
            let root = option(args, "--root")
                .map(PathBuf::from)
                .or_else(|| args.get(1).map(PathBuf::from));
            let root = resolve_contract_root(root.as_deref()).map_err(|error| error.to_string())?;
            let contract = DesignContract::extract(&root).map_err(|error| error.to_string())?;
            if subcommand == "summary" {
                print!("{}", summarize_contract(&contract));
                return Ok(());
            }
            let output = option(args, "--output")
                .map(PathBuf::from)
                .or_else(|| args.get(2).map(PathBuf::from));
            if let Some(output) = output {
                contract
                    .write_json(&output)
                    .map_err(|error| error.to_string())?;
                println!("wrote {}", output.display());
            } else {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&contract).map_err(|error| error.to_string())?
                );
            }
            Ok(())
        }
        "validate" => {
            let path = args
                .get(1)
                .cloned()
                .or_else(|| option(args, "--contract"))
                .ok_or("design-contract validate requires CONTRACT.json")?;
            let contract =
                DesignContract::read_json(Path::new(&path)).map_err(|error| error.to_string())?;
            println!("valid design contract {}", contract.contract_id);
            Ok(())
        }
        "help" | "--help" | "-h" => {
            println!(
                "design-contract\n\nCommands:\n  extract [CONTRACT_ROOT] [OUTPUT.json]\n  summary [CONTRACT_ROOT]\n  validate CONTRACT.json\n\nThe extractor reads all four HTML artifacts without modifying the reference archive."
            );
            Ok(())
        }
        other => Err(format!("unknown design-contract command {other:?}")),
    }
}

fn option(args: &[String], name: &str) -> Option<String> {
    args.windows(2)
        .find(|pair| pair[0] == name)
        .map(|pair| pair[1].clone())
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
    for viewport in Viewport::required_matrix() {
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

fn verify_command(args: &[String]) -> Result<(), String> {
    let root = args.first().ok_or("verify requires RUN_ROOT")?;
    let report = verify_run(Path::new(root)).map_err(|error| error.to_string())?;
    println!(
        "{}",
        serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?
    );
    Ok(())
}

fn accept_baseline_command(args: &[String]) -> Result<(), String> {
    let run_root = args
        .first()
        .ok_or("accept-baseline requires RUN_ROOT BASELINE_ROOT")?;
    let baseline_root = args
        .get(1)
        .ok_or("accept-baseline requires RUN_ROOT BASELINE_ROOT")?;
    if args.len() != 2 {
        return Err("accept-baseline requires RUN_ROOT BASELINE_ROOT".to_owned());
    }
    let run_root = Path::new(run_root);
    let baseline_root = Path::new(baseline_root);
    verify_run_for_baseline_update(run_root).map_err(|error| error.to_string())?;

    let mut manifests = Vec::new();
    collect_manifest_paths(&run_root.join("manifests"), &mut manifests)
        .map_err(|error| error.to_string())?;
    if manifests.is_empty() {
        return Err("run contains no manifests to promote".to_owned());
    }
    let mut copied = 0_usize;
    for manifest_path in manifests {
        let bytes = fs::read(&manifest_path).map_err(|error| error.to_string())?;
        let manifest: backend_gui_harness::CaptureManifest =
            serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
        let capture_root = manifest_path
            .parent()
            .and_then(Path::parent)
            .ok_or_else(|| format!("manifest has no capture root: {}", manifest_path.display()))?;
        let capture_prefix = capture_root.strip_prefix(run_root).map_err(|_| {
            format!(
                "manifest capture root {} is outside run root {}",
                capture_root.display(),
                run_root.display()
            )
        })?;
        for frame in manifest.frames {
            let relative = Path::new(&frame.path);
            if !is_safe_frame_path(relative) {
                return Err(format!("unsafe frame path {:?}", frame.path));
            }
            let source = capture_root.join(relative);
            let destination = baseline_root.join(capture_prefix).join(relative);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent).map_err(|error| error.to_string())?;
            }
            fs::copy(&source, &destination).map_err(|error| {
                format!(
                    "copy {} -> {}: {error}",
                    source.display(),
                    destination.display()
                )
            })?;
            copied += 1;
        }
    }
    println!(
        "accepted {copied} verified frame(s) into {}",
        baseline_root.display()
    );
    Ok(())
}

fn collect_manifest_paths(root: &Path, output: &mut Vec<PathBuf>) -> std::io::Result<()> {
    if !root.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(root)? {
        let path = entry?.path();
        if path.is_dir() {
            collect_manifest_paths(&path, output)?;
        } else if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            output.push(path);
        }
    }
    Ok(())
}

fn is_safe_frame_path(path: &Path) -> bool {
    let mut components = path.components();
    let Some(std::path::Component::Normal(name)) = components.next() else {
        return false;
    };
    name == "frames"
        && components.all(|component| matches!(component, std::path::Component::Normal(_)))
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
        "backend-gui-harness\n\nCommands:\n  list-states [--json]\n  list-viewports\n  plan-animation WIDTHxHEIGHT@SCALE [--reduced-motion]\n  validate\n  design-contract extract [CONTRACT_ROOT] [OUTPUT.json]\n  design-contract summary [CONTRACT_ROOT]\n  design-contract validate CONTRACT.json\n  verify-conformance RUN_ROOT [--contract CONTRACT.json] [--allow-partial]\n  compare BASELINE.png ACTUAL.png DIFF.png\n  verify RUN_ROOT\n  accept-baseline RUN_ROOT BASELINE_ROOT\n\naccept-baseline promotes only a structurally and provenance-verified run; it is the explicit baseline update operation.\nThe capture_gpui_state library API is used by the desktop adapter to render real GPUI scenes."
    );
}
