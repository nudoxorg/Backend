//! The gallery command line (`facet-gallery`, and any binary that serves its
//! own scenes through [`main`]): renders scenes headless (or in a window) for review.
//!
//! ```text
//! facet-gallery list
//! facet-gallery capture --scene ID|all [--size WxH] [--time MS] [--theme abyss|glacier]
//!                       [--text-scale PCT] [--density comfortable|compact|dense]
//!                       [--contrast normal|high] [--reduced-motion] [--scale 1|2]
//!                       [--native-a11y]
//!                       [--input SCRIPT | --input-file FILE | --no-script]
//!                       [--frame-ms MS] --out DIR
//! facet-gallery sequence --scene ID --times 0,32,64 [--frame-ms 16] [...] --out DIR
//! facet-gallery film --scene ID --times 0,40,80 [--onion] [--frames] [--columns N] [...] --out DIR
//! facet-gallery sheet --scenes a,b,c --out FILE [--time MS] [--tile-width PX] [--columns N] [...]
//! facet-gallery motion-report --scene ID [--input …] [--times 0,40,80] [--until MS] [--out FILE.json] [...]
//! facet-gallery storm --scene ID|all [--seed N] [--seeds N] [--acts N] [--span MS]
//!                     [--budget-ms MS] [--no-fresh] [--shrink RUNS] [--replay FILE] [--out DIR]
//! facet-gallery matrix --scene ID|all [--full] [--widths ..] [--text-scales ..] [--themes ..]
//!                      [--densities ..] [--motion on,off] [--scale 1|2] [--tile-width PX] [--one-sheet] --out DIR
//! facet-gallery perf --scene ID|all [--sizes 1440x900,2560x1440] [--input …]
//! facet-gallery soak --scene ID --input-file FILE [--until MS] [shot options] --out FILE.json
//! facet-gallery verify [--scenes all|a,b] [--seeds N] [--matrix gate|full|quick|none] [--quick]
//!                      [--no-canaries] --out DIR
//! facet-gallery lint --scene ID|all [--input …] [--time MS] [shot options]
//! facet-gallery legibility --scene ID --scale 2 [--input …] [--from MS] [--to MS] [--frames] [--dump] [--out DIR]
//! facet-gallery legibility --catalog FILE --scale 2 [--only a,b] [--frames] [--dump] [--out DIR]
//! facet-gallery window --scene ID [--theme …] [--text-scale PCT] [--reduced-motion] [--native-trace FILE.jsonl]
//! ```
//!
//! Every capture prints the SHA-256 of its raw RGBA pixels, so determinism is
//! checked by capturing twice and comparing the digests.
//!
//! `--input` plays a timed input script while capturing (pointer, keys,
//! modifier holds, resizes, text scale, density, theme, motion; syntax in
//! `backend_gui_harness::script`), e.g.
//! `--input "move 420,310 @0; key cmd-k @200; hold alt @400"`. Without
//! `--input` a scene plays the default script it declares (`--no-script`
//! plays nothing). Artifacts of a scripted capture carry `-in<8 hex>`, the
//! script's digest.

use super::json::Json;
use super::{Frame, GalleryError, Scene, Shot, compose};
use super::{align, lint, matrix, perf, storm, verify};
use crate::gallery;
use crate::motion::pulse;
use crate::probe::Ledger;
use crate::tokens::Appearance;
use crate::{Contrast, Density};
use backend_gui_harness::Script;
use gpui::{Bounds, TitlebarOptions, WindowBounds, WindowOptions, px, size};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

type Result<T> = std::result::Result<T, GalleryError>;

/// The scenes a gallery binary serves: its own list, plus the harness's
/// canaries (always available by id, never part of `all`).
#[derive(Clone, Copy)]
pub struct Registry {
    /// The binary's name, for messages.
    pub name: &'static str,
    /// Every scene, in order.
    pub all: fn() -> Vec<Scene>,
}

static REGISTRY: std::sync::OnceLock<Registry> = std::sync::OnceLock::new();

fn registry() -> Registry {
    REGISTRY.get().copied().unwrap_or(Registry {
        name: "facet-gallery",
        all: gallery::all,
    })
}

fn all_scenes() -> Vec<Scene> {
    (registry().all)()
}

/// Runs the gallery command line over `registry`'s scenes.
#[must_use]
pub fn main(registry: Registry) -> ExitCode {
    let _ = REGISTRY.set(registry);
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    match run(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}: {error}", registry.name);
            ExitCode::FAILURE
        }
    }
}

fn fail<T>(message: impl Into<String>) -> Result<T> {
    Err(GalleryError(message.into()))
}

const FLAGS: [&str; 12] = [
    "dump",
    "one-sheet",
    "full",
    "quick",
    "no-canaries",
    "reduced-motion",
    "onion",
    "frames",
    "help",
    "no-script",
    "no-fresh",
    "native-a11y",
];

struct Options {
    values: HashMap<String, String>,
    flags: HashSet<String>,
}

impl Options {
    fn parse(args: &[String]) -> Result<Self> {
        let mut values = HashMap::new();
        let mut flags = HashSet::new();
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            let Some(name) = arg.strip_prefix("--") else {
                return fail(format!("unexpected argument `{arg}`"));
            };
            if FLAGS.contains(&name) {
                flags.insert(name.to_owned());
            } else {
                let Some(value) = iter.next() else {
                    return fail(format!("--{name} needs a value"));
                };
                values.insert(name.to_owned(), value.clone());
            }
        }
        Ok(Self { values, flags })
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.values.get(name).map(String::as_str)
    }

    fn require(&self, name: &str) -> Result<&str> {
        self.get(name)
            .map_or_else(|| fail(format!("--{name} is required")), Ok)
    }

    fn flag(&self, name: &str) -> bool {
        self.flags.contains(name)
    }

    fn number<T: std::str::FromStr>(&self, name: &str) -> Result<Option<T>> {
        self.get(name)
            .map(|value| {
                value
                    .parse::<T>()
                    .map_err(|_| GalleryError(format!("--{name}: `{value}` is not a number")))
            })
            .transpose()
    }

    fn times(&self, name: &str) -> Result<Option<Vec<u64>>> {
        self.get(name)
            .map(|list| {
                list.split(',')
                    .map(|part| {
                        part.trim().parse::<u64>().map_err(|_| {
                            GalleryError(format!("--{name}: `{part}` is not a time in ms"))
                        })
                    })
                    .collect()
            })
            .transpose()
    }

    /// The shot for `scene` from `--size --theme --text-scale --reduced-motion --scale`.
    fn shot(&self, scene: &Scene) -> Result<Shot> {
        let mut shot = Shot::new(scene);
        if let Some(size) = self.get("size") {
            let Some((width, height)) = size.split_once('x') else {
                return fail(format!("--size `{size}` is not WxH"));
            };
            shot.size = (
                width
                    .parse()
                    .map_err(|_| GalleryError(format!("--size width `{width}`")))?,
                height
                    .parse()
                    .map_err(|_| GalleryError(format!("--size height `{height}`")))?,
            );
        }
        shot.appearance = match self.get("theme").unwrap_or("abyss") {
            "abyss" => Appearance::Abyss,
            "glacier" => Appearance::Glacier,
            other => return fail(format!("--theme `{other}`: expected abyss or glacier")),
        };
        if let Some(percent) = self.number::<f32>("text-scale")? {
            shot.text_scale = percent / 100.0;
        }
        shot.reduced_motion = self.flag("reduced-motion");
        shot.density = match self.get("density").unwrap_or("comfortable") {
            "comfortable" => Density::Comfortable,
            "compact" => Density::Compact,
            "dense" => Density::Dense,
            other => {
                return fail(format!(
                    "--density `{other}`: expected comfortable, compact or dense"
                ));
            }
        };
        shot.contrast = match self.get("contrast").unwrap_or("normal") {
            "normal" => Contrast::Normal,
            "high" => Contrast::High,
            other => return fail(format!("--contrast `{other}`: expected normal or high")),
        };
        if let Some(scale) = self.number::<u8>("scale")? {
            shot.scale = scale;
        }
        shot.capture_native_accessibility = self.flag("native-a11y");
        if let Some(time) = self.number::<u64>("time")? {
            shot.times = vec![time];
        }
        if let Some(frame_ms) = self.number::<u64>("frame-ms")? {
            shot.frame_ms = frame_ms;
        }
        shot.script = self.script()?;
        Ok(shot)
    }

    /// `--input SCRIPT`, `--input-file FILE`, `--no-script`, or the scene's
    /// declared default (`None`).
    fn script(&self) -> Result<Option<Script>> {
        let source = match (self.get("input"), self.get("input-file")) {
            (Some(_), Some(_)) => return fail("give --input or --input-file, not both"),
            (Some(inline), None) => inline.to_owned(),
            (None, Some(path)) => std::fs::read_to_string(path)
                .map_err(|error| GalleryError(format!("--input-file {path}: {error}")))?,
            (None, None) if self.flag("no-script") => return Ok(Some(Script::new())),
            (None, None) => return Ok(None),
        };
        Script::parse(&source)
            .map(Some)
            .map_err(|error| GalleryError(error.to_string()))
    }
}

fn scene(id: &str) -> Result<Scene> {
    all_scenes()
        .into_iter()
        .chain(super::bench::CANARIES.iter().copied())
        .find(|scene| scene.id == id)
        .map_or_else(
            || {
                let known = all_scenes()
                    .iter()
                    .map(|scene| scene.id)
                    .collect::<Vec<_>>()
                    .join(", ");
                fail(format!("no scene `{id}` (known: {known})"))
            },
            Ok,
        )
}

fn run(args: &[String]) -> Result<()> {
    let Some((command, rest)) = args.split_first() else {
        return fail("usage: facet-gallery list|capture|film|sheet|motion-report|window …");
    };
    let options = Options::parse(rest)?;
    match command.as_str() {
        "list" => {
            for scene in all_scenes() {
                println!(
                    "{:<24} {:>5}x{:<5} {}",
                    scene.id, scene.size.0, scene.size.1, scene.title
                );
            }
            Ok(())
        }
        "capture" => capture(&options),
        "film" => film(&options),
        "sequence" => sequence(&options),
        "sheet" => sheet(&options),
        "motion-report" => motion_report(&options),
        "legibility" => legibility(&options),
        "storm" => storm(&options),
        "lint" => lint(&options),
        "matrix" => matrix(&options),
        "perf" => perf(&options),
        "soak" => soak(&options),
        "verify" => verify(&options),
        "window" => window(&options),
        other => fail(format!("unknown command `{other}`")),
    }
}

fn theme_name(appearance: Appearance) -> &'static str {
    match appearance {
        Appearance::Abyss => "abyss",
        Appearance::Glacier => "glacier",
    }
}

/// Stream a long native script, discarding each frame's observations.
fn soak(options: &Options) -> Result<()> {
    let scene = scene(options.require("scene")?)?;
    let mut shot = options.shot(&scene)?;
    if shot.frame_ms == 0 {
        return fail("soak needs --frame-ms > 0 so events exercise frame lifetimes");
    }
    resolve_script(&scene, &mut shot)?;
    let script_end = shot.script.as_ref().map_or(0, Script::end_ms);
    shot.until_ms = options.number::<u64>("until")?.unwrap_or(script_end + 2_000);
    let report = super::soak::measure(&scene, &shot)?;
    let path = PathBuf::from(options.require("out")?);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(GalleryError::from_display)?;
    }
    std::fs::write(&path, format!("{report}\n")).map_err(GalleryError::from_display)?;
    println!("{report}");
    Ok(())
}

/// The first 8 hex digits of the script's canonical form (empty script: none).
fn script_tag(script: Option<&Script>) -> String {
    match script {
        Some(script) if !script.is_empty() => {
            let digest = format!("{:x}", Sha256::digest(script.to_string().as_bytes()));
            format!("-in{}", &digest[..8])
        }
        _ => String::new(),
    }
}

#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
fn suffix(shot: &Shot, time: u64) -> String {
    format!(
        "{}-{}pct{}{}{}{}-t{time}{}@{}x",
        theme_name(shot.appearance),
        (shot.text_scale * 100.0).round() as u32,
        match shot.density {
            Density::Comfortable => "",
            Density::Compact => "-compact",
            Density::Dense => "-dense",
        },
        match shot.contrast {
            Contrast::Normal => "",
            Contrast::High => "-hc",
        },
        if shot.capture_native_accessibility { "-a11y" } else { "" },
        script_tag(shot.script.as_ref()),
        if shot.reduced_motion { "-rm" } else { "" },
        shot.scale
    )
}

/// Resolves the scene's declared default script into the shot, so file
/// names and reports name the script that actually played.
fn resolve_script(scene: &Scene, shot: &mut Shot) -> Result<()> {
    if shot.script.is_none() {
        shot.script = Some(gallery::declared(scene, shot)?);
    }
    Ok(())
}

fn out_dir(options: &Options) -> Result<PathBuf> {
    let dir = PathBuf::from(options.require("out")?);
    std::fs::create_dir_all(&dir)
        .map_err(|error| GalleryError(format!("{}: {error}", dir.display())))?;
    Ok(dir)
}

fn digest(image: &image::RgbaImage) -> String {
    format!("{:x}", Sha256::digest(image.as_raw()))
}

fn save(image: &image::RgbaImage, path: &Path) -> Result<()> {
    if path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("png")) {
        // Large film sheets are evidence, so prefer quick lossless encoding to
        // expensive filtering that can dwarf the actual native rendering.
        use image::ImageEncoder as _;
        let file = std::fs::File::create(path).map_err(GalleryError::from_display)?;
        let mut writer = std::io::BufWriter::new(file);
        image::codecs::png::PngEncoder::new_with_quality(
            &mut writer,
            image::codecs::png::CompressionType::Fast,
            image::codecs::png::FilterType::NoFilter,
        ).write_image(image.as_raw(),image.width(),image.height(),image::ColorType::Rgba8.into())
            .map_err(|error| GalleryError(format!("{}: {error}",path.display())))?;
        std::io::Write::flush(&mut writer).map_err(GalleryError::from_display)?;
    } else {
        image.save(path).map_err(|error| GalleryError(format!("{}: {error}", path.display())))?;
    }
    println!(
        "{}  {}x{}  rgba-sha256 {}",
        path.display(),
        image.width(),
        image.height(),
        digest(image)
    );
    Ok(())
}

fn save_native_accessibility(
    evidence: &backend_gui_harness::NativeAccessibilityFrame,
    image_path: &Path,
) -> Result<String> {
    let path = image_path.with_extension("a11y.json");
    let bytes = serde_json::to_vec_pretty(evidence).map_err(GalleryError::from_display)?;
    std::fs::write(&path, &bytes)
        .map_err(|error| GalleryError(format!("{}: {error}", path.display())))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn capture(options: &Options) -> Result<()> {
    let dir = out_dir(options)?;
    let scenes = match options.require("scene")? {
        "all" => all_scenes(),
        id => vec![scene(id)?],
    };
    for scene in scenes {
        let mut shot = options.shot(&scene)?;
        resolve_script(&scene, &mut shot)?;
        if let Some(script) = shot.script.as_ref().filter(|script| !script.is_empty()) {
            println!("{}: input\n{script}", scene.id);
        }
        for frame in gallery::capture(&scene, &shot)? {
            let path = dir.join(format!("{}-{}.png", scene.id, suffix(&shot, frame.time_ms)));
            save(&frame.image, &path)?;
            if let Some(evidence) = &frame.native_accessibility {
                save_native_accessibility(evidence, &path)?;
            }
        }
    }
    Ok(())
}

/// Export genuine native frames without retaining the sequence's RGBA images.
fn sequence(options: &Options) -> Result<()> {
    let scene=scene(options.require("scene")?)?;
    let mut shot=options.shot(&scene)?;
    shot.times=options.times("times")?.ok_or_else(||GalleryError("sequence requires --times".to_owned()))?;
    if shot.times.is_empty() || shot.frame_ms==0 { return fail("sequence needs capture times and a real simulated frame loop"); }
    shot.probe=true;
    shot.until_ms=options.number::<u64>("until")?.unwrap_or(0);
    resolve_script(&scene,&mut shot)?;
    let directory=out_dir(options)?;
    let mut exported=0;
    let script=gallery::run(&scene,&shot,&mut |tick,_,_| {
        if let Some(image)=tick.image {
            let path=directory.join(format!("{}-{}.png",scene.id,suffix(&shot,tick.drawn.at_ms)));
            save(image,&path)?;
            let native_accessibility = if let Some(evidence) = &tick.native_accessibility {
                let sidecar_sha256 = save_native_accessibility(evidence, &path)?;
                Some(Json::obj([
                    ("path", Json::str(path.with_extension("a11y.json").to_string_lossy().into_owned())),
                    ("sha256", Json::str(sidecar_sha256)),
                    ("frame_number", Json::num(evidence.frame_number as f64)),
                    ("screenshot_sha256", Json::str(evidence.screenshot_sha256.clone())),
                ]))
            } else {
                None
            };
            let state=Json::obj([
                ("scene",Json::str(scene.id)),("time_ms",Json::num(tick.drawn.at_ms as f64)),
                ("image",Json::str(path.to_string_lossy())),("rgba_sha256",Json::str(digest(image))),
                ("state",tick.state.clone().unwrap_or(Json::Null)),
                ("cpu_ms",Json::num(tick.drawn.cpu.as_secs_f64()*1000.0)),
                ("input_cpu_ms",Json::num(tick.drawn.input_cpu.as_secs_f64()*1000.0)),
                ("requested",Json::Bool(tick.drawn.requested())),
                ("native_accessibility", native_accessibility.unwrap_or(Json::Null)),
            ]);
            std::fs::write(path.with_extension("json"),format!("{state}\n")).map_err(GalleryError::from_display)?;
            exported+=1;
        }
        Ok(())
    })?;
    if exported==0 { return fail("sequence captured no native frames"); }
    let manifest=Json::obj([
        ("scene",Json::str(scene.id)),("frames",Json::num(exported)),
        ("frame_ms",Json::num(shot.frame_ms as f64)),("input",Json::str(script.to_string())),
        ("capture_times",Json::Arr(shot.times.iter().map(|&at|Json::num(at as f64)).collect())),
        ("notes",Json::str("Each image is a genuine GPUI draw. Images are written and released per frame. No interpolation; CPU times are diagnostic and include probes, excluding PNG export.")),
    ]);
    std::fs::write(directory.join("SEQUENCE.json"),format!("{manifest}\n")).map_err(GalleryError::from_display)?;
    Ok(())
}

fn film(options: &Options) -> Result<()> {
    let dir = out_dir(options)?;
    let scene = scene(options.require("scene")?)?;
    let mut shot = options.shot(&scene)?;
    resolve_script(&scene, &mut shot)?;
    shot.times = options
        .times("times")?
        .unwrap_or_else(|| (0..=15).map(|i| i * 40).collect());
    let frames = gallery::capture(&scene, &shot)?;
    if options.flag("frames") {
        for frame in &frames {
            let path = dir.join(format!("{}-{}.png", scene.id, suffix(&shot, frame.time_ms)));
            save(&frame.image, &path)?;
            if let Some(evidence) = &frame.native_accessibility {
                save_native_accessibility(evidence, &path)?;
            }
        }
    }
    let images = frames.iter().map(|frame| &frame.image).collect::<Vec<_>>();
    let script = shot.script.clone().unwrap_or_default();
    let lines = frames
        .iter()
        .map(|frame| {
            // Name the acts delivered since the previous tile.
            let acts = script
                .events
                .iter()
                .filter(|event| {
                    event.at_ms <= frame.time_ms
                        && frames
                            .iter()
                            .rev()
                            .find(|earlier| earlier.time_ms < frame.time_ms)
                            .is_none_or(|earlier| event.at_ms > earlier.time_ms)
                })
                .map(|event| event.act.to_string())
                .collect::<Vec<_>>();
            if acts.is_empty() {
                format!("{}  t = {} ms", scene.id, frame.time_ms)
            } else {
                format!("t = {} ms  {}", frame.time_ms, acts.join("; "))
            }
        })
        .collect::<Vec<_>>();
    let label_width = shot.size.0;
    let labels = compose::labels(&lines, label_width, shot.scale, shot.appearance)?;
    let columns = options
        .number::<usize>("columns")?
        .unwrap_or(frames.len().min(5));
    let stem = format!("{}-{}", scene.id, suffix(&shot, 0).replace("-t0", ""));
    let strip = compose::film(
        &images,
        &labels,
        columns,
        compose::background(shot.appearance),
    );
    save(&strip, &dir.join(format!("{stem}-film.png")))?;
    if options.flag("onion") {
        save(
            &compose::onion(&images),
            &dir.join(format!("{stem}-onion.png")),
        )?;
    }
    Ok(())
}

/// The legibility law on a film (`gallery::legible`): samples the scene's
/// script every 16 ms from `--from` to `--to` (default: the script's end plus
/// 600 ms) at 2x and fails if two readable texts intersect, or a drawn text
/// lingers under legibility, in any frame. `--out DIR` writes
/// `<scene>-legibility.{txt,json}` and, with `--frames`, every frame that
/// broke the law.
///
/// `--catalog FILE` runs every film the catalog lists instead (see
/// [`catalog`]) and prints the ledger.
fn legibility(options: &Options) -> Result<()> {
    if let Some(path) = options.get("catalog") {
        return catalog(options, Path::new(path));
    }
    let scene = scene(options.require("scene")?)?;
    let mut shot = options.shot(&scene)?;
    resolve_script(&scene, &mut shot)?;
    let dir = options.get("out").map(PathBuf::from);
    let span = (options.number::<u64>("from")?, options.number::<u64>("to")?);
    let (report, _) = film_legibility(options, &scene, shot, span, dir.as_deref())?;
    if report.passed() {
        Ok(())
    } else {
        fail(format!("{}: {} legibility findings", scene.id, report.findings.len()))
    }
}

/// Films one scene under the law from `span.0` (default 0) to `span.1`
/// (default: the script's end plus 600 ms): prints its verdict, writes its
/// outputs into `dir` (with `--dump` the per-frame texts, with `--frames`
/// every frame that broke the law) and returns the report with the measured
/// film.
fn film_legibility(
    options: &Options,
    scene: &Scene,
    mut shot: Shot,
    span: (Option<u64>, Option<u64>),
    dir: Option<&Path>,
) -> Result<(gallery::legible::Report, Vec<gallery::legible::Measured>)> {
    if shot.scale != 2 {
        return fail("legibility measures glyph contrast from pixels: it needs --scale 2");
    }
    shot.probe = true;
    let step = gallery::legible::STEP_MS;
    let end = shot.script.as_ref().map_or(0, Script::end_ms) + 600;
    let from = span.0.unwrap_or(0) / step * step;
    let to = span.1.unwrap_or(end);
    shot.times = (from..=to).step_by(usize::try_from(step).unwrap_or(16)).collect();
    shot.until_ms = to;
    let keep = options.flag("frames");
    let mut film = Vec::new();
    let mut kept = Vec::new();
    gallery::run(scene, &shot, &mut |tick, window, _| {
        if let Some(image) = tick.image {
            film.push(gallery::legible::measure(
                image,
                window.painted_texts(),
                shot.scale,
                tick.drawn.at_ms,
                !tick.ledger.any_live(),
            ));
            if keep {
                kept.push((tick.drawn.at_ms, image.clone()));
            }
        }
        Ok(())
    })?;
    let report = gallery::legible::judge(&film);
    let text = gallery::legible::text(scene.id, &report);
    print!("{text}");
    if let Some(dir) = dir {
        std::fs::create_dir_all(dir).map_err(GalleryError::from_display)?;
        std::fs::write(dir.join(format!("{}-legibility.txt", scene.id)), &text)
            .map_err(GalleryError::from_display)?;
        std::fs::write(
            dir.join(format!("{}-legibility.json", scene.id)),
            format!("{}\n", gallery::legible::json(scene.id, &report)),
        )
        .map_err(GalleryError::from_display)?;
        if options.flag("dump") {
            let lines = film
                .iter()
                .map(|frame| gallery::legible::frame_json(frame).to_string())
                .collect::<Vec<_>>()
                .join("\n");
            std::fs::write(dir.join(format!("{}-texts.jsonl", scene.id)), format!("{lines}\n"))
                .map_err(GalleryError::from_display)?;
        }
        for (at, image) in &kept {
            let broken = report
                .findings
                .iter()
                .any(|finding| (finding.from_ms..=finding.to_ms).contains(at));
            if broken {
                save(image, &dir.join(format!("{}-{at:05}.png", scene.id)))?;
            }
        }
    }
    Ok((report, film))
}

/// `legibility --catalog FILE`: every film in the transition catalog, under
/// the law, as one ledger.
///
/// The catalog lists one film per line, `<scene> [<script>] [from=MS]
/// [to=MS]` (`#` starts a comment; the script is relative to the catalog; a
/// scene that plays on its own clock names its film's end with `to=`). A film's transitions are
/// its script's instants, each named by its act line's trailing `# comment`
/// (`gallery::legible::windows`); a film without a script plays the scene's
/// own and is one transition. A binary films the scenes it serves and names
/// the rest as skipped. `--only a,b` keeps the films whose scene or script
/// stem is listed. Prints one ledger line per transition (PASS, FAIL, or NOT
/// EXERCISED when nothing drawn changed) and writes `DIR/<film>/…` plus
/// `DIR/ledger.txt` with `--out DIR`. Fails when any transition fails.
fn catalog(options: &Options, path: &Path) -> Result<()> {
    let source = std::fs::read_to_string(path)
        .map_err(|error| GalleryError(format!("--catalog {}: {error}", path.display())))?;
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    let only: Option<Vec<&str>> = options.get("only").map(|only| only.split(',').map(str::trim).collect());
    let out = options.get("out").map(PathBuf::from);
    let known: HashSet<&str> = all_scenes()
        .iter()
        .map(|scene| scene.id)
        .chain(super::bench::CANARIES.iter().map(|scene| scene.id))
        .collect();
    let mut ledger = String::new();
    let (mut pass, mut failed, mut idle, mut skipped) = (0, 0, 0, Vec::new());
    for line in source.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        let mut words = line.split_whitespace();
        let Some(scene_id) = words.next() else { continue };
        let words: Vec<&str> = words.collect();
        let bound = |name: &str| -> Result<Option<u64>> {
            words
                .iter()
                .find_map(|word| word.strip_prefix(name))
                .map(|value| value.parse::<u64>().map_err(|_| GalleryError(format!("{scene_id}: `{name}{value}` is not ms"))))
                .transpose()
        };
        let span = (bound("from=")?, bound("to=")?);
        let script = words.iter().copied().find(|word| !word.contains('='));
        let stem = script.map_or(scene_id, |script| script.trim_end_matches(".txt"));
        let name = if script.is_some() { stem.to_owned() } else { scene_id.to_owned() };
        if only.as_ref().is_some_and(|only| !only.contains(&scene_id) && !only.contains(&stem)) {
            continue;
        }
        if !known.contains(scene_id) {
            skipped.push(name);
            continue;
        }
        let scene = scene(scene_id)?;
        let mut shot = options.shot(&scene)?;
        let windows = match script {
            Some(script) => {
                let file = base.join(script);
                let source = std::fs::read_to_string(&file)
                    .map_err(|error| GalleryError(format!("{}: {error}", file.display())))?;
                shot.script = Some(Script::parse(&source).map_err(GalleryError::from_display)?);
                gallery::legible::windows(&source)
            }
            None => {
                resolve_script(&scene, &mut shot)?;
                vec![gallery::legible::Window {
                    label: scene.id.to_owned(),
                    act: "the scene's own script".to_owned(),
                    from_ms: 0,
                    to_ms: None,
                }]
            }
        };
        let started = std::time::Instant::now();
        let dir = out.as_ref().map(|out| out.join(&name));
        let (report, film) = film_legibility(options, &scene, shot, span, dir.as_deref())?;
        let rows = gallery::legible::ledger(&film, &report, &windows);
        for row in &rows {
            match row.verdict() {
                gallery::legible::Verdict::Pass => pass += 1,
                gallery::legible::Verdict::Fail => failed += 1,
                gallery::legible::Verdict::NotExercised => idle += 1,
            }
        }
        let text = gallery::legible::ledger_text(&name, &rows);
        println!("ledger  {name}  ({scene_id}, {} frames, {:.0} s)", film.len(), started.elapsed().as_secs_f32());
        print!("{text}");
        ledger.push_str(&text);
    }
    let summary = format!(
        "catalog  {}  {pass} pass  {failed} fail  {idle} not exercised{}\n",
        path.display(),
        if skipped.is_empty() {
            String::new()
        } else {
            format!("  (skipped, not served by {}: {})", registry().name, skipped.join(", "))
        }
    );
    print!("{summary}");
    if let Some(out) = &out {
        std::fs::create_dir_all(out).map_err(GalleryError::from_display)?;
        std::fs::write(out.join("ledger.txt"), format!("{ledger}{summary}")).map_err(GalleryError::from_display)?;
    }
    if failed == 0 {
        Ok(())
    } else {
        fail(format!("{failed} catalog transitions break the legibility law"))
    }
}

fn sheet(options: &Options) -> Result<()> {
    let path = PathBuf::from(options.require("out")?);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| GalleryError(format!("{}: {error}", parent.display())))?;
    }
    let ids = options.require("scenes")?;
    let scenes = if ids == "all" {
        all_scenes()
    } else {
        ids.split(',')
            .map(|id| scene(id.trim()))
            .collect::<Result<Vec<_>>>()?
    };
    let mut frames: Vec<Frame> = Vec::new();
    let mut lines = Vec::new();
    let mut appearance = Appearance::Abyss;
    let mut scale = 2;
    for scene in &scenes {
        let mut shot = options.shot(scene)?;
        resolve_script(scene, &mut shot)?;
        appearance = shot.appearance;
        scale = shot.scale;
        let frame = gallery::capture(scene, &shot)?
            .into_iter()
            .next()
            .map_or_else(|| fail(format!("{}: no frame", scene.id)), Ok)?;
        lines.push(format!("{}  {}", scene.id, suffix(&shot, frame.time_ms)));
        frames.push(frame);
    }
    let tile_width = options.number::<u32>("tile-width")?.unwrap_or(720);
    let label_width = (tile_width / u32::from(scale)).max(200);
    let labels = compose::labels(&lines, label_width, scale, appearance)?;
    let tiles = frames.iter().map(|frame| &frame.image).collect::<Vec<_>>();
    let columns = options.number::<usize>("columns")?.unwrap_or(3);
    save(
        &compose::sheet(
            &tiles,
            &labels,
            columns,
            tile_width,
            compose::background(appearance),
        ),
        &path,
    )
}

fn frame_json(frame: &Frame) -> Json {
    let ledger: &Ledger = &frame.ledger;
    let bounds = |b: &crate::probe::BoundsSample| Json::obj([
        ("x",Json::num(b.x)),("y",Json::num(b.y)),("width",Json::num(b.width)),("height",Json::num(b.height)),
    ]);
    Json::obj([
        ("time_ms", Json::num(frame.time_ms as f64)),
        (
            "frames_requested",
            Json::num(ledger.frames_requested as f64),
        ),
        ("live", Json::Bool(ledger.any_live())),
        ("state", frame.state.clone().unwrap_or(Json::Null)),
        ("scrolls",Json::Arr(ledger.scrolls.iter().map(|scroll|Json::obj([
            ("key",Json::str(scroll.key.clone())),("viewport",bounds(&scroll.viewport)),("content",bounds(&scroll.content)),
        ])).collect())),
        ("stacks", Json::Arr(ledger.stacks.iter().map(|stack| Json::obj([
            ("layer", Json::str(stack.layer.clone())),
            ("entries", Json::Arr(stack.entries.iter().map(|entry| Json::obj([
                ("key", Json::str(entry.key.clone())), ("kind", Json::str(entry.kind.clone())),
                ("phase", Json::str(entry.phase.name())), ("parent", entry.parent.clone().map_or(Json::Null, Json::str)),
                ("pinned", Json::Bool(entry.pinned)),
            ])).collect())),
        ])).collect())),
        (
            "texts",
            Json::Arr(
                ledger
                    .texts
                    .iter()
                    .map(|text| {
                        Json::obj([
                            ("key", Json::str(text.key.clone())),
                            ("content", Json::str(text.content.clone())),
                            ("paint_clip", text.paint_clip.as_ref().map_or(Json::Null, bounds)),
                            ("x", Json::num(f64::from(text.bounds.x))),
                            ("y", Json::num(f64::from(text.bounds.y))),
                            ("width", Json::num(f64::from(text.bounds.width))),
                            ("height", Json::num(f64::from(text.bounds.height))),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "tracks",
            Json::Arr(
                ledger
                    .tracks
                    .iter()
                    .map(|track| {
                        Json::obj([
                            ("key", Json::str(track.key.clone())),
                            ("kind", Json::str(track.kind.name())),
                            ("value", Json::num(f64::from(track.value))),
                            ("target", Json::num(f64::from(track.target))),
                            ("velocity", Json::num(f64::from(track.velocity))),
                            ("started_ms", Json::num(track.started_ms)),
                            ("budget_ms", Json::num(track.budget_ms)),
                            ("at_ms", Json::num(track.at_ms)),
                            ("live", Json::Bool(track.live)),
                            (
                                "overshoot_ratio",
                                Json::num(f64::from(track.overshoot_ratio)),
                            ),
                            (
                                "overshoot_absolute",
                                Json::num(f64::from(track.overshoot_absolute)),
                            ),
                            ("group", track.group.clone().map_or(Json::Null, Json::Str)),
                        ])
                    })
                    .collect(),
            ),
        ),
        (
            "bounds",
            Json::Arr(
                ledger
                    .bounds
                    .iter()
                    .map(|bounds| {
                        Json::obj([
                            ("key", Json::str(bounds.key.clone())),
                            ("x", Json::num(f64::from(bounds.x))),
                            ("y", Json::num(f64::from(bounds.y))),
                            ("width", Json::num(f64::from(bounds.width))),
                            ("height", Json::num(f64::from(bounds.height))),
                        ])
                    })
                    .collect(),
            ),
        ),
    ])
}

fn motion_report(options: &Options) -> Result<()> {
    let scene = scene(options.require("scene")?)?;
    let mut shot = options.shot(&scene)?;
    resolve_script(&scene, &mut shot)?;
    if shot.frame_ms == 0 {
        return fail(
            "motion-report needs the simulated frame loop (--frame-ms > 0): without it motion \
             only advances at inputs and captures, which is not how a window behaves",
        );
    }
    shot.probe = true;
    shot.times = options.times("times")?.unwrap_or_default();
    let script_end = shot.script.as_ref().map_or(0, Script::end_ms);
    shot.until_ms = options
        .number::<u64>("until")?
        .unwrap_or_else(|| script_end.max(shot.times.iter().copied().max().unwrap_or(0)) + 1_200);
    let (observed, frames, script) = gallery::observe(&scene, &shot)?;
    if let Some(filter) = options.get("log") {
        for frame in &observed {
            let tracks = frame
                .ledger
                .tracks
                .iter()
                .filter(|track| filter == "all" || track.key.contains(filter))
                .map(|track| {
                    format!(
                        "{}@{:.0}={:.4}{}(v{:.2},s{:.0},b{:.0})",
                        track.key,
                        track.at_ms,
                        track.value,
                        if track.live { "*" } else { "" },
                        track.velocity,
                        track.started_ms,
                        track.budget_ms
                    )
                })
                .collect::<Vec<_>>();
            println!(
                "frame {:>5} ms  inv {} cb {} cpu {:.2} ms  events {}  {}",
                frame.drawn.at_ms,
                frame.drawn.invalidations,
                frame.drawn.callbacks,
                frame.drawn.cpu.as_secs_f64() * 1000.0,
                frame.events,
                tracks.join(" ")
            );
        }
    }
    let alignment = align::analyze(&observed, align::Tolerance::default());
    print!("{}", align::text(scene.id, &alignment, 40));
    if let Some(path) = options.get("out") {
        let report = Json::obj([
            ("scene", Json::str(scene.id)),
            ("shot", Json::str(suffix(&shot, 0))),
            ("script", Json::str(script.to_string())),
            ("alignment", align::json(scene.id, &alignment)),
            (
                "input_events",
                Json::Arr(
                    script
                        .events
                        .iter()
                        .map(|event| {
                            Json::obj([
                                ("at_ms", Json::num(event.at_ms as f64)),
                                ("act", Json::str(event.act.to_string())),
                            ])
                        })
                        .collect(),
                ),
            ),
            // Preserve every draw, including cold and quiet frames. Consumers
            // can audit event/requested subsets without diluting their tails.
            (
                "frames",
                Json::Arr(
                    observed
                        .iter()
                        .map(|frame| {
                            Json::obj([
                                ("at_ms", Json::num(frame.drawn.at_ms as f64)),
                                ("cpu_ms", Json::num(frame.drawn.cpu.as_secs_f64() * 1000.0)),
                                ("requested", Json::Bool(frame.drawn.requested())),
                                ("invalidations", Json::num(frame.drawn.invalidations as f64)),
                                ("callbacks", Json::num(frame.drawn.callbacks as f64)),
                                ("events", Json::num(frame.events as f64)),
                                ("state", frame.state.clone().unwrap_or(Json::Null)),
                                ("input_cpu_ms",Json::num(frame.drawn.input_cpu.as_secs_f64()*1000.0)),
                                ("input_events",Json::num(frame.drawn.input_events as f64)),
                                ("input_max_ms",Json::num(frame.drawn.input_max.as_secs_f64()*1000.0)),
                            ])
                        })
                        .collect(),
                ),
            ),
            (
                "captures",
                Json::Arr(frames.iter().map(frame_json).collect()),
            ),
        ]);
        std::fs::write(path, report.to_string())
            .map_err(|error| GalleryError(format!("{path}: {error}")))?;
        println!("{path}");
    }
    if alignment.passed() {
        Ok(())
    } else {
        fail(format!(
            "{}: {} motion findings",
            scene.id,
            alignment.findings.len()
        ))
    }
}

fn list<T>(
    options: &Options,
    name: &str,
    parse: impl Fn(&str) -> Option<T>,
) -> Result<Option<Vec<T>>> {
    options
        .get(name)
        .map(|value| {
            value
                .split(',')
                .map(|part| {
                    parse(part.trim())
                        .map_or_else(|| fail(format!("--{name}: `{part}` is not valid")), Ok)
                })
                .collect()
        })
        .transpose()
}

fn axes(options: &Options) -> Result<matrix::Axes> {
    let mut axes = if options.flag("full") {
        matrix::Axes::full()
    } else {
        matrix::Axes::gate()
    };
    if let Some(widths) = list(options, "widths", |part| part.parse().ok())? {
        axes.widths = widths;
    }
    if let Some(scales) = list(options, "text-scales", |part| part.parse().ok())? {
        axes.text_scales = scales;
    }
    if let Some(themes) = list(options, "themes", |part| match part {
        "abyss" => Some(Appearance::Abyss),
        "glacier" => Some(Appearance::Glacier),
        _ => None,
    })? {
        axes.themes = themes;
    }
    if let Some(densities) = list(options, "densities", |part| match part {
        "comfortable" => Some(Density::Comfortable),
        "compact" => Some(Density::Compact),
        "dense" => Some(Density::Dense),
        _ => None,
    })? {
        axes.densities = densities;
    }
    if let Some(motion) = list(options, "motion", |part| match part {
        "on" => Some(true),
        "off" => Some(false),
        _ => None,
    })? {
        axes.motion = motion;
    }
    Ok(axes)
}

fn matrix(options: &Options) -> Result<()> {
    let dir = out_dir(options)?;
    let scenes = match options.require("scene")? {
        "all" => all_scenes(),
        id => vec![scene(id)?],
    };
    let axes = axes(options)?;
    let scale = options.number::<u8>("scale")?.unwrap_or(1);
    let tile_width = options.number::<u32>("tile-width")?.unwrap_or(320);
    let mut failed_cells = 0;
    let mut uncovered = 0;
    for scene in &scenes {
        let mut failures = Vec::new();
        let results = matrix::run(scene, &axes, scale, &mut |result| {
            if !result.linted.lints.is_empty() || result.equals_reduced == Some(false) {
                failures.push(result.cell.label());
            }
        })?;
        let total = results.len();
        let failing = results
            .iter()
            .filter(|result| {
                !result.linted.lints.is_empty() || result.equals_reduced == Some(false)
            })
            .collect::<Vec<_>>();
        let compared = results
            .iter()
            .filter(|result| result.cell.motion && result.equals_reduced.is_some())
            .count();
        let texts: usize = results
            .iter()
            .map(|result| result.linted.coverage.clip_visible_texts)
            .sum();
        let targets: usize = results
            .iter()
            .map(|result| result.linted.coverage.clip_visible_targets)
            .sum();
        let covered = texts + targets > 0 || compared > 0;
        if !covered {
            uncovered += 1;
        }
        println!(
            "{}: {}  ({texts} visible text boxes, {targets} visible targets; {compared} settled==reduced comparisons{})",
            scene.id,
            if covered {
                format!("{} of {total} cells pass", total - failing.len())
            } else {
                format!(
                    "NOT COVERED: {total} cells captured, nothing to check (the scene publishes no probe text or targets, and motion off was not compared)"
                )
            },
            if results.iter().any(|result| result.ambient) {
                "; ambient pulse: comparison reported, not failed"
            } else {
                ""
            }
        );
        for result in failing.iter().take(24) {
            let mut reasons = result
                .linted
                .lints
                .iter()
                .map(|lint| format!("{} {}: {}", lint.rule.name(), lint.key, lint.detail))
                .collect::<Vec<_>>();
            if result.equals_reduced == Some(false) {
                reasons.push("settled frame differs from a reduced-motion boot".to_owned());
            }
            println!(
                "    {:<34} {}",
                result.cell.label(),
                reasons.first().cloned().unwrap_or_default()
            );
            for reason in reasons.iter().skip(1).take(2) {
                println!("    {:<34} {reason}", "");
            }
        }
        if failing.len() > 24 {
            println!("    … {} more failing cells", failing.len() - 24);
        }
        failed_cells += failing.len();
        if options.flag("one-sheet") {
            let (name, sheet) = matrix::one_sheet(scene, &axes, &results, tile_width)?;
            save(&sheet, &dir.join(format!("{name}.png")))?;
        } else {
            for (name, sheet) in matrix::sheets(scene, &axes, &results, tile_width)? {
                save(&sheet, &dir.join(format!("{name}.png")))?;
            }
        }
        let _ = failures;
    }
    if failed_cells > 0 {
        fail(format!("{failed_cells} matrix cell(s) failed"))
    } else if uncovered > 0 {
        fail(format!(
            "{uncovered} scene(s) not covered: sheets written, nothing checked"
        ))
    } else {
        Ok(())
    }
}

fn perf(options: &Options) -> Result<()> {
    let scenes = match options.require("scene")? {
        "all" => all_scenes(),
        id => vec![scene(id)?],
    };
    let sizes = list(options, "sizes", |part| {
        let (w, h) = part.split_once('x')?;
        Some((w.parse().ok()?, h.parse().ok()?))
    })?
    .unwrap_or_else(|| perf::BUDGETS.iter().map(|budget| budget.size).collect());
    let mut failed = 0;
    let script = options.script()?.filter(|script| !script.is_empty());
    for scene in &scenes {
        for &size in &sizes {
            let timing = perf::measure(scene, size, script.clone())?;
            println!("{}", perf::line(&timing));
            println!("    {}", perf::attribution(&timing));
            if !timing.passed() {
                failed += 1;
            }
        }
    }
    if failed == 0 {
        Ok(())
    } else {
        fail(format!("{failed} run(s) over budget or not budgeted"))
    }
}

fn verify(options: &Options) -> Result<()> {
    let dir = out_dir(options)?;
    let scenes = match options.get("scenes").unwrap_or("all") {
        "all" => all_scenes(),
        ids => ids
            .split(',')
            .map(|id| scene(id.trim()))
            .collect::<Result<Vec<_>>>()?,
    };
    let seeds = options.number::<u64>("seeds")?.unwrap_or(3);
    let axes = match options.get("matrix").unwrap_or(if options.flag("quick") {
        "quick"
    } else {
        "gate"
    }) {
        "gate" => Some(matrix::Axes::gate()),
        "full" => Some(matrix::Axes::full()),
        "quick" => Some(matrix::Axes {
            widths: vec![480, 1100, 2560],
            text_scales: vec![100, 200],
            ..matrix::Axes::gate()
        }),
        "none" => None,
        other => {
            return fail(format!(
                "--matrix `{other}`: expected gate, full, quick or none"
            ));
        }
    };
    let sheets = dir.join("sheets");
    let repros = dir.join("repros");
    for path in [&sheets, &repros] {
        std::fs::create_dir_all(path)
            .map_err(|error| GalleryError(format!("{}: {error}", path.display())))?;
    }
    let mut reports = Vec::new();
    for scene in &scenes {
        let started = std::time::Instant::now();
        let report = verify::scene_report(scene, seeds, axes.as_ref(), &sheets);
        println!(
            "{:<24} {}  ({:.0} s)",
            scene.id,
            report
                .stages
                .iter()
                .map(|stage| format!(
                    "{} {}",
                    stage.name,
                    match stage.outcome {
                        verify::Outcome::Pass => "ok",
                        verify::Outcome::Fail => "FAIL",
                        verify::Outcome::NotRun => "-",
                        verify::Outcome::NotCovered => "n/c",
                    }
                ))
                .collect::<Vec<_>>()
                .join("  "),
            started.elapsed().as_secs_f64()
        );
        reports.push(report);
    }
    let ran_canaries = !options.flag("no-canaries");
    let canaries = if ran_canaries {
        let canaries = verify::canaries(&repros);
        for canary in &canaries {
            println!(
                "{:<26} {}/{}  {}",
                canary.scene,
                canary.expected.0,
                canary.expected.1,
                if canary.caught { "caught" } else { "MISSED" }
            );
        }
        canaries
    } else {
        Vec::new()
    };
    save(&verify::overview(&scenes)?, &dir.join("overview.png"))?;
    let text = verify::text(&reports, &canaries, ran_canaries);
    write_text(&dir.join("REPORT.txt"), &text)?;
    write_text(
        &dir.join("report.json"),
        &verify::json(&reports, &canaries, ran_canaries).to_string(),
    )?;
    print!("{}", verify::table(&reports, &canaries, ran_canaries));
    println!("full evidence: {}", dir.join("REPORT.txt").display());
    let _ = text;
    match verify::verdict(&reports, &canaries, ran_canaries) {
        "PASS" => Ok(()),
        verdict => fail(format!("verify: {verdict}")),
    }
}

fn lint(options: &Options) -> Result<()> {
    let scenes = match options.require("scene")? {
        "all" => all_scenes(),
        id => vec![scene(id)?],
    };
    let mut failed = 0;
    let mut uncovered = 0;
    for scene in &scenes {
        let mut shot = options.shot(scene)?;
        if shot.script.is_none() {
            shot.script = Some(Script::new());
        }
        shot.probe = true;
        let end = shot.script.as_ref().map_or(0, Script::end_ms);
        shot.times = vec![options.number::<u64>("time")?.unwrap_or(end + 1_200)];
        let frame = gallery::capture(scene, &shot)?
            .into_iter()
            .next()
            .map_or_else(|| fail(format!("{}: no frame", scene.id)), Ok)?;
        let linted = lint::lint_with_painted(
            &frame.image,
            &frame.ledger,
            frame.drawn.viewport,
            &frame.painted_texts,
        );
        let covered = linted.coverage.clip_visible_texts > 0 || linted.coverage.clip_visible_targets > 0;
        println!(
            "{} {}: {}  {} clip-visible of {} texts ({} hidden, {} under a veil), {} clip-visible of {} targets ({} under a veil), contrast measured on {} ({} not), native ink {} verified/{} unseen/{} unverified{}",
            scene.id,
            suffix(&shot, frame.time_ms),
            if !covered {
                "NOT COVERED"
            } else if linted.lints.is_empty() {
                "PASS"
            } else {
                "FAIL"
            },
            linted.coverage.clip_visible_texts,
            linted.coverage.texts,
            linted.coverage.hidden_texts,
            linted.coverage.occluded_texts,
            linted.coverage.clip_visible_targets,
            linted.coverage.targets,
            linted.coverage.occluded_targets,
            linted.coverage.contrast,
            linted.coverage.contrast_skipped,
            linted.coverage.ink_verified_texts,
            linted.coverage.ink_unseen_texts,
            linted.coverage.ink_unverified_texts,
            linted
                .lowest_contrast
                .as_ref()
                .map_or_else(String::new, |(key, ratio)| format!(
                    ", lowest {ratio:.2}:1 ({key})"
                ))
        );
        for item in &linted.lints {
            println!("    {:<9} {}: {}", item.rule.name(), item.key, item.detail);
        }
        if !covered {
            uncovered += 1;
        } else if !linted.lints.is_empty() {
            failed += 1;
        }
    }
    if failed > 0 {
        fail(format!("{failed} scene(s) failed lints"))
    } else if uncovered > 0 {
        fail(format!(
            "{uncovered} scene(s) not covered: nothing published to lint (wrap text in probe::text \
             and interactive elements in probe::target)"
        ))
    } else {
        Ok(())
    }
}

fn storm_config(options: &Options, seed: u64) -> Result<storm::StormConfig> {
    let mut config = storm::StormConfig::new(seed);
    if let Some(acts) = options.number::<usize>("acts")? {
        config.acts = acts;
    }
    if let Some(span) = options.number::<u64>("span")? {
        config.span_ms = span;
    }
    if let Some(budget) = options.number::<f64>("budget-ms")? {
        config.budget = Duration::from_secs_f64(budget / 1000.0);
    }
    if let Some(runs) = options.number::<usize>("shrink")? {
        config.shrink_runs = runs;
    }
    config.fresh = !options.flag("no-fresh");
    Ok(config)
}

fn write_text(path: &Path, text: &str) -> Result<()> {
    std::fs::write(path, text).map_err(|error| GalleryError(format!("{}: {error}", path.display())))
}

fn print_violations(run: &storm::StormRun, limit: usize) {
    for violation in run.violations.iter().take(limit) {
        println!(
            "    {:<11} {:>6} ms  {}: {}",
            violation.check, violation.at_ms, violation.key, violation.detail
        );
    }
    if run.violations.len() > limit {
        println!("    … {} more", run.violations.len() - limit);
    }
}

fn storm(options: &Options) -> Result<()> {
    let scenes = match options.require("scene")? {
        "all" => all_scenes(),
        id => vec![scene(id)?],
    };
    let first = options.number::<u64>("seed")?.unwrap_or(1);
    let seeds = options.number::<u64>("seeds")?.unwrap_or(1);
    let dir = options.get("out").map(PathBuf::from);
    if let Some(dir) = &dir {
        std::fs::create_dir_all(dir)
            .map_err(|error| GalleryError(format!("{}: {error}", dir.display())))?;
    }
    let mut failed = 0;
    for scene in &scenes {
        let mut base = options.shot(scene)?;
        base.script = None;
        base.scale = options.number::<u8>("scale")?.unwrap_or(1);
        if let Some(path) = options.get("replay") {
            let source = std::fs::read_to_string(path)
                .map_err(|error| GalleryError(format!("--replay {path}: {error}")))?;
            let script = Script::parse(&source).map_err(|error| GalleryError(error.to_string()))?;
            let run = storm::replay(scene, &base, &script, &storm_config(options, first)?);
            println!(
                "{} replay {path}: {} ({} frames, worst draw {:.1} ms)",
                scene.id,
                if run.passed() { "PASS" } else { "FAIL" },
                run.frames,
                run.worst_draw.as_secs_f64() * 1000.0
            );
            print_violations(&run, 12);
            if !run.passed() {
                failed += 1;
            }
            continue;
        }
        for seed in first..first + seeds {
            let config = storm_config(options, seed)?;
            let report = storm::storm(scene, &base, &config)?;
            let run = &report.run;
            println!(
                "{} seed {seed}: {}  {} acts, {} frames, worst draw {:.1} ms, settle==fresh {}",
                scene.id,
                if run.passed() { "PASS" } else { "FAIL" },
                run.script.events.len(),
                run.frames,
                run.worst_draw.as_secs_f64() * 1000.0,
                match (&run.settled, &run.fresh) {
                    (Some(a), Some(b)) if a.as_raw() == b.as_raw() => format!(
                        "identical ({}x{}, rgba-sha256 {})",
                        a.width(),
                        a.height(),
                        &digest(a)[..16]
                    ),
                    (Some(_), Some(_)) => "DIFFERENT".to_owned(),
                    _ => "not compared".to_owned(),
                }
            );
            if run.passed() {
                continue;
            }
            failed += 1;
            print_violations(run, 12);
            if let Some(minimal) = &report.minimal {
                println!(
                    "  shrunk in {} runs to {} acts (same check: {}):",
                    report.shrink_runs,
                    minimal.events.len(),
                    run.first().map_or("?", |violation| violation.check)
                );
                for line in minimal.to_string().lines() {
                    println!("    {line}");
                }
            }
            if let Some(dir) = &dir {
                let stem = dir.join(format!("{}-seed{seed}", scene.id));
                write_text(&stem.with_extension("storm.txt"), &run.script.to_string())?;
                if let Some(minimal) = &report.minimal {
                    let path = stem.with_extension("min.txt");
                    write_text(&path, &storm::with_tail(minimal).to_string())?;
                    println!(
                        "  replay: facet-gallery storm --scene {} --replay {}   film: facet-gallery film --scene {} --input-file {} --scale 1 --out DIR",
                        scene.id,
                        path.display(),
                        scene.id,
                        path.display()
                    );
                }
                if let (Some(settled), Some(fresh)) = (&run.settled, &run.fresh) {
                    save(settled, &stem.with_extension("settled.png"))?;
                    save(fresh, &stem.with_extension("fresh.png"))?;
                }
            }
        }
    }
    if failed == 0 {
        Ok(())
    } else {
        fail(format!("{failed} storm(s) failed"))
    }
}

fn window(options: &Options) -> Result<()> {
    let diagnostics = std::env::var_os("NUDOX_REVIEW_DIAGNOSTICS").is_some();
    if diagnostics { eprintln!("[w-pages-review] window command entered"); }
    let trace_path=options.get("native-trace").map(PathBuf::from);
    let scene = scene(options.require("scene")?)?;
    let shot = options.shot(&scene)?;
    let facet = shot.facet();
    let (width, height) = shot.size;
    #[allow(clippy::cast_precision_loss)]
    let window_size = size(px(width as f32), px(height as f32));
    let failure=std::rc::Rc::new(std::cell::RefCell::new(None));
    let boot_failure=std::rc::Rc::clone(&failure);
    if diagnostics { eprintln!("[w-pages-review] before Application::run"); }
    gpui::Application::with_platform(gpui_platform::current_platform(false))
        .with_assets(crate::icons::Assets)
        .run(move |cx| {
        if diagnostics { eprintln!("[w-pages-review] Application::run callback entered"); }
        if diagnostics { eprintln!("[w-pages-review] gallery bootstrap starting"); }
        if let Err(error) = gallery::bootstrap(facet, false, cx) {
            eprintln!("facet-gallery: {error}");
            *boot_failure.borrow_mut()=Some(error);
            cx.quit();
            return;
        }
        if diagnostics { eprintln!("[w-pages-review] gallery bootstrap complete"); }
        if let Some(path)=&trace_path {
            if let Err(error)=super::native_trace::start(path,cx) {
                eprintln!("facet-gallery: native trace: {error}");
                *boot_failure.borrow_mut()=Some(GalleryError::from_display(error));
                cx.quit();return;
            }
        }
        pulse::thaw(cx);
        let bounds = Bounds::centered(None, window_size, cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions {
                title: Some(format!("facet-gallery \u{b7} {}", scene.id).into()),
                ..TitlebarOptions::default()
            }),
            ..WindowOptions::default()
        };
        if diagnostics { eprintln!("[w-pages-review] open_window starting"); }
        if let Err(error) = cx.open_window(options, move |window, cx| {
            gallery::mount(&scene, window, cx)
        }) {
            eprintln!("facet-gallery: open window: {error}");
            *boot_failure.borrow_mut()=Some(GalleryError::from_display(error));
            cx.quit();
            return;
        }
        if diagnostics { eprintln!("[w-pages-review] open_window returned; windows={}", cx.windows().len()); }
        cx.on_window_closed(move |cx, window| {
            if diagnostics { eprintln!("[w-pages-review] window closed {window:?}; remaining={}", cx.windows().len()); }
            cx.quit();
        }).detach();
        cx.activate(true);
        if diagnostics { eprintln!("[w-pages-review] application activated"); }
    });
    if diagnostics { eprintln!("[w-pages-review] Application::run returned"); }
    if let Some(error)=failure.borrow_mut().take() { return Err(error); }
    Ok(())
}


#[cfg(test)]
mod png_export {
    #[test]
    fn exported_native_evidence_preserves_every_rgba_channel() {
        let image = image::RgbaImage::from_fn(67,53,|x,y| image::Rgba([
            ((x*29+y*7)%256) as u8, ((x*3+y*41)%256) as u8,
            ((x*19+y*13)%256) as u8, ((x*17+y*23)%256) as u8,
        ]));
        let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
        let path = std::env::temp_dir().join(format!("facet-evidence-{}-{nonce}.png",std::process::id()));
        super::save(&image,&path).unwrap_or_else(|error| panic!("{error}"));
        let decoded = image::open(&path).unwrap_or_else(|error| panic!("{error}")).to_rgba8();
        std::fs::remove_file(&path).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!(decoded,image,"PNG evidence changed colors or transparency");
        assert_eq!(super::digest(&decoded),super::digest(&image));
    }
}
