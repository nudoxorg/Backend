//! Small process wrapper for the production GPUI cold-launch journeys.
//!
//! The restart test starts and reaps this process, while the production
//! journey runner records real screenshots and semantic checks under the
//! fixture root.

#![deny(unsafe_code)]

#[cfg(unix)]
use backend_desktop::harness::journey as live_journey;
#[cfg(unix)]
use backend_desktop::harness::journey::{Options, Parts, Plan, Verdict};
#[cfg(unix)]
use serde_json::json;
#[cfg(unix)]
use std::path::PathBuf;

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
    let name = args.first().map(String::as_str).unwrap_or("first-launch");
    if args.len() > 1 {
        return Err("usage: backend-journey-gui [first-launch|choose-project]".to_owned());
    }
    let parts = Parts::load(&Parts::dir())?;
    let source = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/bin/gui.rs");
    let plan = Plan::parse(name, &source, &script(name)?, &parts)?;
    let root = std::env::current_dir().map_err(|error| error.to_string())?;
    let evidence = root.join(".gui-journey-evidence").join(name);
    let options = Options {
        scale: 1,
        ..Options::default()
    };
    let outcome = live_journey::run(&plan, &evidence, &options)?;
    if outcome.verdict != Verdict::Pass {
        return Err(format!(
            "GUI journey {} failed:\n{}",
            outcome.verdict, outcome.report
        ));
    }
    println!(
        "{}",
        json!({
            "journey": name,
            "verdict": outcome.verdict.to_string(),
            "content": outcome.content,
            "report": outcome.report,
            "evidence": evidence,
        })
    );
    Ok(())
}

#[cfg(unix)]
fn script(name: &str) -> Result<String, String> {
    let common = "size 640x480\nstart clean\n";
    match name {
        "first-launch" => Ok(format!(
            "{common}check empty-library\n  route like \"orbit\"\n  line \"0 projects · 0 packages\" in shelf\n  link \"Add a folder\" in reader\n"
        )),
        "choose-project" => {
            let project = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("fixtures/polyglot");
            let project = std::fs::canonicalize(&project).unwrap_or(project);
            let project = serde_json::to_string(&project.to_string_lossy().into_owned())
                .map_err(|error| error.to_string())?;
            Ok(format!(
                "{common}check empty-library\n  line \"0 projects · 0 packages\" in shelf\nclick \"Add a folder\" in reader\ntype {project}\nawait text \"no project file here\" in reader within 20s\nkey enter\nawait text \"polyglot\" in shelf within 20s\ncheck project-on-shelf\n  route like \"orbit\"\n  text \"polyglot\" in shelf\nrestart\nawait text \"polyglot\" in shelf within 20s\ncheck project-restored\n  route like \"orbit\"\n  text \"polyglot\" in shelf\n"
            ))
        }
        other => Err(format!("unknown GUI journey {other:?}")),
    }
}
