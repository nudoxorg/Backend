//! `verify`: the whole battery over every scene, then every canary, as one
//! pass/fail report (the lead's review gate).
//!
//! Stages per scene: **determinism** (the scene's script, or a pointer
//! sweep, captured twice at three times: identical digests), **motion**
//! (alignment over that run), **storm** (N seeds, shrunk on failure),
//! **lint** (the settled scene at 2x), **matrix** (every cell settled,
//! linted, settled == reduced), **perf** (release only: p95 budgets).
//!
//! Then every canary (the bench with one deliberate defect) must fail the
//! stage and check it was built for. A canary that passes means a check has
//! gone blind, and the verdict is FAIL.
//!
//! The verdict is PASS only when every stage ran and passed; a debug build
//! cannot run perf and reports INCOMPLETE, never PASS.

use super::align::{self, Tolerance};
use super::json::Json;
use super::lint;
use super::matrix::{self, Axes};
use super::perf;
use super::storm::{self, StormConfig};
use super::{GalleryError, Scene, Shot, bench, capture, compose, find, observe};
use backend_gui_harness::Script;
use sha2::{Digest, Sha256};
use std::fmt::Write as _;
use std::path::Path;

/// A stage's outcome.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Ran and held.
    Pass,
    /// Ran and failed.
    Fail,
    /// Did not run (with why).
    NotRun,
    /// Ran but the scene publishes nothing it could check.
    NotCovered,
}

impl Outcome {
    const fn name(&self) -> &'static str {
        match self {
            Self::Pass => "PASS",
            Self::Fail => "FAIL",
            Self::NotRun => "NOT RUN",
            Self::NotCovered => "NOT COVERED",
        }
    }
}

/// One stage of one scene.
#[derive(Clone, Debug)]
pub struct Stage {
    /// `determinism`, `motion`, `storm`, `lint`, `matrix`, `perf`.
    pub name: &'static str,
    /// Outcome.
    pub outcome: Outcome,
    /// One line of evidence.
    pub summary: String,
    /// Failure details.
    pub details: Vec<String>,
    /// The checks that failed (for canary matching).
    pub failed_checks: Vec<String>,
}

impl Stage {
    fn new(name: &'static str) -> Self {
        Self {
            name,
            outcome: Outcome::Pass,
            summary: String::new(),
            details: Vec::new(),
            failed_checks: Vec::new(),
        }
    }

    fn fail(&mut self, check: &str, detail: String) {
        self.outcome = Outcome::Fail;
        if !self.failed_checks.iter().any(|known| known == check) {
            self.failed_checks.push(check.to_owned());
        }
        self.details.push(detail);
    }
}

/// A scene's stages.
#[derive(Clone, Debug)]
pub struct SceneReport {
    /// The scene.
    pub scene: &'static str,
    /// Its stages.
    pub stages: Vec<Stage>,
}

/// A canary's verdict.
#[derive(Clone, Debug)]
pub struct CanaryReport {
    /// The canary scene.
    pub scene: &'static str,
    /// The stage and check it must fail.
    pub expected: (&'static str, &'static str),
    /// Whether that stage failed that check.
    pub caught: bool,
    /// What the stage said.
    pub evidence: String,
}

/// The canaries and the stage/check each must fail.
pub const EXPECTED: [(&str, &str, &str); 18] = [
    ("canary-stuck-hover", "storm", "hover"),
    ("canary-jump", "motion", "continuity"),
    ("canary-clipped-text", "lint", "clip"),
    ("canary-leaked-frames", "motion", "idle"),
    ("canary-stuck-popup", "storm", "stack"),
    ("canary-off-slot", "motion", "slot"),
    ("canary-lagging-group", "motion", "lockstep"),
    ("canary-low-contrast", "lint", "contrast"),
    ("canary-small-target", "lint", "target"),
    ("canary-overlap", "lint", "overlap"),
    ("canary-slow-frame", "storm", "budget"),
    ("canary-unrequested-motion", "motion", "unrequested"),
    ("canary-panic", "storm", "panic"),
    ("canary-hover-race", "storm", "fresh"),
    ("canary-offscreen", "lint", "offscreen"),
    ("canary-dead-focus", "storm", "focus"),
    ("canary-wall-clock", "determinism", "determinism"),
    ("canary-residue", "matrix", "settled!=reduced"),
];

fn digest(image: &image::RgbaImage) -> String {
    format!("{:x}", Sha256::digest(image.as_raw()))
}

/// The script a scene's checks drive it with: its declared script, or a
/// pointer sweep across it.
///
/// # Errors
/// A capture failure.
pub fn drive(scene: &Scene) -> Result<Script, GalleryError> {
    let mut probe = Shot::new(scene);
    probe.frame_ms = 0;
    let declared = super::run(scene, &probe, &mut |_, _, _| Ok(()))?;
    Ok(if declared.is_empty() {
        perf::sweep(scene.size.0, scene.size.1)
    } else {
        declared
    })
}

fn determinism(scene: &Scene, script: &Script) -> Stage {
    let mut stage = Stage::new("determinism");
    let end = script.end_ms();
    let mut shot = Shot::new(scene);
    shot.scale = 1;
    shot.script = Some(script.clone());
    shot.times = vec![0, end / 2, end + 600];
    let runs = (0..2)
        .map(|_| capture(scene, &shot))
        .collect::<Result<Vec<_>, _>>();
    match runs {
        Ok(runs) => {
            let a = runs[0]
                .iter()
                .map(|frame| digest(&frame.image))
                .collect::<Vec<_>>();
            let b = runs[1]
                .iter()
                .map(|frame| digest(&frame.image))
                .collect::<Vec<_>>();
            for (index, (x, y)) in a.iter().zip(&b).enumerate() {
                if x != y {
                    stage.fail(
                        "determinism",
                        format!(
                            "t={} ms: {} vs {}",
                            shot.times.get(index).copied().unwrap_or(0),
                            &x[..16],
                            &y[..16]
                        ),
                    );
                }
            }
            stage.summary = format!(
                "{} frames x2 at t={:?}: {}",
                a.len(),
                shot.times,
                a.iter().map(|d| &d[..12]).collect::<Vec<_>>().join(" ")
            );
        }
        Err(error) => stage.fail("determinism", format!("capture failed: {error}")),
    }
    stage
}

fn motion(scene: &Scene, script: &Script) -> Stage {
    let mut stage = Stage::new("motion");
    let mut shot = Shot::new(scene);
    shot.scale = 1;
    shot.probe = true;
    shot.script = Some(script.clone());
    shot.times = vec![];
    shot.until_ms = script.end_ms() + 1_200;
    match observe(scene, &shot) {
        Ok((observed, _, _)) => {
            let alignment = align::analyze(&observed, Tolerance::default());
            let evaluated = alignment
                .stats
                .iter()
                .filter(|(_, stat)| stat.evaluated > 0)
                .map(|(check, stat)| format!("{} {}", check.name(), stat.evaluated))
                .collect::<Vec<_>>();
            stage.summary = format!(
                "{} frames, {} tracks; evaluated: {}",
                alignment.frames,
                alignment.tracks.len(),
                if evaluated.is_empty() {
                    "nothing (no motion published)".to_owned()
                } else {
                    evaluated.join(", ")
                }
            );
            for finding in alignment.findings.iter().take(12) {
                stage.fail(
                    finding.check.name(),
                    format!(
                        "{} {} ms {}: {}",
                        finding.check.name(),
                        finding.at_ms,
                        finding.key,
                        finding.detail
                    ),
                );
            }
            for finding in alignment.findings.iter().skip(12) {
                if !stage
                    .failed_checks
                    .iter()
                    .any(|c| c == finding.check.name())
                {
                    stage.failed_checks.push(finding.check.name().to_owned());
                }
            }
            if evaluated.is_empty() && stage.outcome == Outcome::Pass {
                stage.outcome = Outcome::NotCovered;
            }
        }
        Err(error) => stage.fail("run", format!("run failed: {error}")),
    }
    stage
}

fn storms(scene: &Scene, seeds: u64, out: &Path) -> Stage {
    let mut stage = Stage::new("storm");
    let mut base = Shot::new(scene);
    base.scale = 1;
    let mut passed = 0;
    for seed in 1..=seeds {
        let config = StormConfig::new(seed);
        match storm::storm(scene, &base, &config) {
            Ok(report) if report.run.passed() => passed += 1,
            Ok(report) => {
                let first = report.run.violations.first();
                let repro = report.minimal.as_ref().map(|minimal| {
                    let path = out.join(format!("{}-seed{seed}.min.txt", scene.id));
                    let _ = std::fs::write(&path, storm::with_tail(minimal).to_string());
                    path
                });
                for violation in &report.run.violations {
                    if !stage.failed_checks.iter().any(|c| c == violation.check) {
                        stage.failed_checks.push(violation.check.to_owned());
                    }
                }
                stage.fail(
                    first.map_or("storm", |violation| violation.check),
                    format!(
                        "seed {seed}: {}{}",
                        first.map_or_else(String::new, |violation| format!(
                            "{} {} ms {}: {}",
                            violation.check, violation.at_ms, violation.key, violation.detail
                        )),
                        repro.map_or_else(String::new, |path| format!(
                            "  (shrunk to {} acts: {})",
                            report.minimal.as_ref().map_or(0, |m| m.events.len()),
                            path.display()
                        ))
                    ),
                );
            }
            Err(error) => stage.fail("storm", format!("seed {seed}: {error}")),
        }
    }
    stage.summary = format!("{passed}/{seeds} seeds pass (settle == fresh compared on each)");
    stage
}

fn lints(scene: &Scene) -> Stage {
    let mut stage = Stage::new("lint");
    let mut shot = Shot::new(scene);
    shot.script = Some(Script::new());
    shot.probe = true;
    shot.times = vec![matrix::SETTLE_MS];
    shot.frame_ms = 0;
    match capture(scene, &shot) {
        Ok(frames) => {
            if let Some(frame) = frames.first() {
                let linted = lint::lint(&frame.image, &frame.ledger, frame.drawn.viewport);
                stage.summary = format!(
                    "{} texts, {} targets, contrast measured on {}{}",
                    linted.coverage.texts,
                    linted.coverage.targets,
                    linted.coverage.contrast,
                    linted
                        .lowest_contrast
                        .as_ref()
                        .map_or_else(String::new, |(key, ratio)| format!(
                            " (lowest {ratio:.2}:1 {key})"
                        ))
                );
                for item in &linted.lints {
                    stage.fail(
                        item.rule.name(),
                        format!("{} {}: {}", item.rule.name(), item.key, item.detail),
                    );
                }
                if linted.coverage.texts == 0
                    && linted.coverage.targets == 0
                    && stage.outcome == Outcome::Pass
                {
                    stage.outcome = Outcome::NotCovered;
                    stage.summary = "nothing published: wrap text in probe::text and \
                                     interactive elements in probe::target"
                        .to_owned();
                }
            }
        }
        Err(error) => stage.fail("lint", format!("capture failed: {error}")),
    }
    stage
}

fn matrix_stage(scene: &Scene, axes: Option<&Axes>, out: &Path) -> Stage {
    let mut stage = Stage::new("matrix");
    let Some(axes) = axes else {
        stage.outcome = Outcome::NotRun;
        stage.summary = "skipped (--matrix none)".to_owned();
        return stage;
    };
    match matrix::run(scene, axes, 1, &mut |_| {}) {
        Ok(results) => {
            let failing = results
                .iter()
                .filter(|r| !r.linted.lints.is_empty() || r.equals_reduced == Some(false))
                .collect::<Vec<_>>();
            let compared = results
                .iter()
                .filter(|r| r.cell.motion && r.equals_reduced.is_some())
                .count();
            let linted: usize = results
                .iter()
                .map(|r| r.linted.coverage.texts + r.linted.coverage.targets)
                .sum();
            stage.summary = format!(
                "{}/{} cells pass, {linted} boxes linted, {compared} settled==reduced comparisons",
                results.len() - failing.len(),
                results.len()
            );
            if linted == 0 && compared == 0 {
                stage.outcome = Outcome::NotCovered;
            }
            for result in failing.iter().take(10) {
                for item in &result.linted.lints {
                    stage.fail(
                        item.rule.name(),
                        format!(
                            "{}: {} {}: {}",
                            result.cell.label(),
                            item.rule.name(),
                            item.key,
                            item.detail
                        ),
                    );
                }
                if result.equals_reduced == Some(false) && !result.ambient {
                    stage.fail(
                        "settled!=reduced",
                        format!(
                            "{}: settled frame differs from a reduced-motion boot",
                            result.cell.label()
                        ),
                    );
                }
            }
            if let Ok(sheets) = matrix::sheets(scene, axes, &results, 320) {
                for (name, sheet) in sheets {
                    let _ = sheet.save(out.join(format!("{name}.png")));
                }
            }
        }
        Err(error) => stage.fail("matrix", format!("matrix failed: {error}")),
    }
    stage
}

fn perf_stage(scene: &Scene) -> Stage {
    let mut stage = Stage::new("perf");
    if cfg!(debug_assertions) {
        stage.outcome = Outcome::NotRun;
        stage.summary = "debug build: budgets need `--release`".to_owned();
        return stage;
    }
    let mut lines = Vec::new();
    for budget in perf::BUDGETS {
        match perf::measure(scene, budget.size, None) {
            Ok(timing) => {
                lines.push(format!(
                    "{}x{} p95 {:.2} ms",
                    budget.size.0,
                    budget.size.1,
                    timing.p95.as_secs_f64() * 1000.0
                ));
                if !timing.passed() {
                    stage.fail("budget", perf::line(&timing));
                }
            }
            Err(error) => stage.fail("perf", format!("{error}")),
        }
    }
    stage.summary = lines.join(", ");
    stage
}

/// Runs every stage for one scene.
#[must_use]
pub fn scene_report(scene: &Scene, seeds: u64, axes: Option<&Axes>, out: &Path) -> SceneReport {
    let mut stages = Vec::new();
    match drive(scene) {
        Ok(script) => {
            stages.push(determinism(scene, &script));
            stages.push(motion(scene, &script));
        }
        Err(error) => {
            let mut stage = Stage::new("determinism");
            stage.fail("run", format!("{error}"));
            stages.push(stage);
        }
    }
    stages.push(storms(scene, seeds, out));
    stages.push(lints(scene));
    stages.push(matrix_stage(scene, axes, out));
    stages.push(perf_stage(scene));
    SceneReport {
        scene: scene.id,
        stages,
    }
}

/// Runs one stage by name on `scene` (the canaries' entry point).
///
/// # Errors
/// A capture failure driving the scene.
pub fn stage(scene: &Scene, name: &str, out: &Path) -> Result<Stage, GalleryError> {
    let gate = Axes {
        widths: vec![720],
        text_scales: vec![100],
        themes: vec![crate::tokens::Appearance::Abyss],
        densities: vec![crate::Density::Comfortable],
        motion: vec![true, false],
    };
    Ok(match name {
        "determinism" => determinism(scene, &drive(scene)?),
        "motion" => motion(scene, &drive(scene)?),
        "storm" => storms(scene, 1, out),
        "lint" => lints(scene),
        "matrix" => matrix_stage(scene, Some(&gate), out),
        other => return Err(GalleryError(format!("no stage `{other}`"))),
    })
}

/// Runs the canaries: each must fail its stage and check.
#[must_use]
pub fn canaries(out: &Path) -> Vec<CanaryReport> {
    let mut reports = Vec::new();
    for (id, stage_name, check) in EXPECTED {
        let Some(scene) = find(id) else {
            continue;
        };
        let (caught, evidence) = match stage(&scene, stage_name, out) {
            Ok(stage) => (
                stage.failed_checks.iter().any(|failed| failed == check),
                stage
                    .details
                    .iter()
                    .find(|detail| detail.contains(check))
                    .or_else(|| stage.details.first())
                    .cloned()
                    .unwrap_or_else(|| format!("{} passed: {}", stage.name, stage.summary)),
            ),
            Err(error) => (false, format!("could not run: {error}")),
        };
        reports.push(CanaryReport {
            scene: scene.id,
            expected: (stage_name, check),
            caught,
            evidence,
        });
    }
    reports
}

/// The verdict of a whole run.
#[must_use]
pub fn verdict(
    scenes: &[SceneReport],
    canaries: &[CanaryReport],
    ran_canaries: bool,
) -> &'static str {
    let failed = scenes
        .iter()
        .flat_map(|scene| &scene.stages)
        .any(|stage| stage.outcome == Outcome::Fail)
        || canaries.iter().any(|canary| !canary.caught);
    let incomplete = !ran_canaries
        || scenes
            .iter()
            .flat_map(|scene| &scene.stages)
            .any(|stage| matches!(stage.outcome, Outcome::NotRun | Outcome::NotCovered));
    if failed {
        "FAIL"
    } else if incomplete {
        "INCOMPLETE"
    } else {
        "PASS"
    }
}

/// The stage columns of the table, in order.
pub const STAGES: [&str; 6] = ["determinism", "motion", "storm", "lint", "matrix", "perf"];

/// One screen: scene x stage -> PASS / FAIL / NOT COVERED / NOT RUN, the
/// canary tally, and the verdict.
#[must_use]
pub fn table(scenes: &[SceneReport], canaries: &[CanaryReport], ran_canaries: bool) -> String {
    let mut out = String::new();
    let width = scenes
        .iter()
        .map(|scene| scene.scene.len())
        .max()
        .unwrap_or(5)
        .max(5);
    let _ = write!(out, "{:<width$}", "scene");
    for stage in STAGES {
        let _ = write!(out, "  {stage:<11}");
    }
    out.push('\n');
    for scene in scenes {
        let _ = write!(out, "{:<width$}", scene.scene);
        for stage in STAGES {
            let cell = scene
                .stages
                .iter()
                .find(|candidate| candidate.name == stage)
                .map_or("NOT RUN", |found| found.outcome.name());
            let _ = write!(out, "  {cell:<11}");
        }
        out.push('\n');
    }
    if ran_canaries {
        let missed = canaries
            .iter()
            .filter(|canary| !canary.caught)
            .map(|canary| {
                format!(
                    "{} ({}/{})",
                    canary.scene, canary.expected.0, canary.expected.1
                )
            })
            .collect::<Vec<_>>();
        let _ = writeln!(
            out,
            "canaries: {}/{} caught{}",
            canaries.len() - missed.len(),
            canaries.len(),
            if missed.is_empty() {
                String::new()
            } else {
                format!("; MISSED: {}", missed.join(", "))
            }
        );
    } else {
        out.push_str("canaries: NOT RUN\n");
    }
    let _ = writeln!(
        out,
        "verdict: {} ({} build)",
        verdict(scenes, canaries, ran_canaries),
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    );
    out
}

/// The report as text: the table, then every stage's evidence.
#[must_use]
pub fn text(scenes: &[SceneReport], canaries: &[CanaryReport], ran_canaries: bool) -> String {
    let mut out = table(scenes, canaries, ran_canaries);
    let _ = writeln!(out, "\n── evidence ──");
    for scene in scenes {
        let _ = writeln!(out, "\n{}", scene.scene);
        for stage in &scene.stages {
            let _ = writeln!(
                out,
                "  {:<12} {:<8} {}",
                stage.name,
                stage.outcome.name(),
                stage.summary
            );
            for detail in stage.details.iter().take(6) {
                let _ = writeln!(out, "      {detail}");
            }
            if stage.details.len() > 6 {
                let _ = writeln!(out, "      … {} more", stage.details.len() - 6);
            }
        }
    }
    if ran_canaries {
        let _ = writeln!(
            out,
            "\ncanaries (each must be caught by the named stage/check)"
        );
        for canary in canaries {
            let _ = writeln!(
                out,
                "  {:<26} {:<24} {}  {}",
                canary.scene,
                format!("{}/{}", canary.expected.0, canary.expected.1),
                if canary.caught { "caught" } else { "MISSED" },
                canary.evidence.chars().take(160).collect::<String>()
            );
        }
    }
    out
}

/// The report as JSON.
#[must_use]
pub fn json(scenes: &[SceneReport], canaries: &[CanaryReport], ran_canaries: bool) -> Json {
    Json::obj([
        (
            "verdict",
            Json::str(verdict(scenes, canaries, ran_canaries)),
        ),
        (
            "build",
            Json::str(if cfg!(debug_assertions) {
                "debug"
            } else {
                "release"
            }),
        ),
        (
            "scenes",
            Json::Arr(
                scenes
                    .iter()
                    .map(|scene| {
                        Json::obj([
                            ("scene", Json::str(scene.scene)),
                            (
                                "stages",
                                Json::Arr(
                                    scene
                                        .stages
                                        .iter()
                                        .map(|stage| {
                                            Json::obj([
                                                ("name", Json::str(stage.name)),
                                                ("outcome", Json::str(stage.outcome.name())),
                                                ("summary", Json::str(stage.summary.clone())),
                                                (
                                                    "failed_checks",
                                                    Json::Arr(
                                                        stage
                                                            .failed_checks
                                                            .iter()
                                                            .map(|c| Json::str(c.clone()))
                                                            .collect(),
                                                    ),
                                                ),
                                                (
                                                    "details",
                                                    Json::Arr(
                                                        stage
                                                            .details
                                                            .iter()
                                                            .map(|d| Json::str(d.clone()))
                                                            .collect(),
                                                    ),
                                                ),
                                            ])
                                        })
                                        .collect(),
                                ),
                            ),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "canaries",
            Json::Arr(
                canaries
                    .iter()
                    .map(|canary| {
                        Json::obj([
                            ("scene", Json::str(canary.scene)),
                            ("stage", Json::str(canary.expected.0)),
                            ("check", Json::str(canary.expected.1)),
                            ("caught", Json::Bool(canary.caught)),
                            ("evidence", Json::str(canary.evidence.clone())),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}

/// A contact sheet of every scene settled at its natural size (the first
/// thing the lead looks at).
///
/// # Errors
/// A capture failure.
pub fn overview(scenes: &[Scene]) -> Result<image::RgbaImage, GalleryError> {
    let mut images = Vec::new();
    let mut lines = Vec::new();
    for scene in scenes {
        let mut shot = Shot::new(scene);
        shot.scale = 1;
        shot.script = Some(Script::new());
        shot.frame_ms = 0;
        shot.times = vec![matrix::SETTLE_MS];
        if let Some(frame) = capture(scene, &shot)?.into_iter().next() {
            images.push(frame.image);
            lines.push(scene.id.to_owned());
        }
    }
    let labels = compose::labels(&lines, 360, 1, crate::tokens::Appearance::Abyss)?;
    let tiles = images.iter().collect::<Vec<_>>();
    Ok(compose::sheet(
        &tiles,
        &labels,
        4,
        720,
        compose::background(crate::tokens::Appearance::Abyss),
    ))
}

/// Every canary scene (for listing).
#[must_use]
pub fn canary_scenes() -> &'static [Scene] {
    bench::CANARIES
}
