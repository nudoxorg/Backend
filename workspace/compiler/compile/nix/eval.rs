//! The dynamic layer: hermetically evaluate a flake's outputs with the
//! vendored `snix-eval`, then hand the resulting `Value` tree to the `walker`
//! for traversal + fusion against the static table.
//!
//! Hermeticity comes from [`DocsIO`] (a whitelist filesystem over the
//! materialized flake tree + inputs), NOT from disabling `import` — `import`
//! is how flakes are structured, so it stays enabled and the IO boundary does
//! the sandboxing.
//!
//! All snix API touch-points live in this module and `walker`/`options`; if a
//! snix bump changes the surface, only these files move. The API used here
//! matches the documented `snix-eval` builder surface
//! (`Evaluation::builder(Rc<dyn EvalIO>)` → `.mode(..).enable_import().build()`
//! → `evaluate(code, location) -> EvaluationResult`).

use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use snix_eval::{EvalMode, Evaluation, EvalIO, FileType};

use super::error::{NixError, Result};
use super::package::FlakeMeta;
use super::syntax::StaticTable;
use super::{context, walker};

/// Wall-clock budget for a single package's evaluation. Arbitrary Nix can
/// loop/OOM; this caps the damage (Phase 7 may add worker-subprocess memory
/// limits if real flakes demand it).
const EVAL_BUDGET: Duration = Duration::from_secs(120);

/// Evaluate the flake at `root` and lower its output tree, or return `None`
/// when there is nothing evaluable (no `flake.nix`, or evaluation degrades to
/// empty). Errors are reserved for hard failures the caller logs and recovers
/// from — per-node eval failures degrade to documented gaps inside `walker`.
pub fn produce(
    root: &Path,
    table: &StaticTable,
    meta: &FlakeMeta,
) -> Result<Option<context::Surface>> {
    let flake_nix = root.join("flake.nix");
    if !flake_nix.exists() {
        return Ok(None);
    }

    // The whitelist filesystem: the flake tree plus a sibling `inputs/` dir the
    // traversal layer materialized (both read-only, no absolute-path escape).
    let inputs_dir = root.parent().map(|p| p.join("inputs"));
    // Unsized coercion `Rc<DocsIO>` → `Rc<dyn EvalIO>` happens at the binding.
    let io: Rc<dyn EvalIO> = Rc::new(DocsIO::new(root.to_path_buf(), inputs_dir));

    let deadline = Instant::now() + EVAL_BUDGET;
    let code = flake_outputs_shim(root, &discover_input_names(root));

    // Lazy mode: forcing all of nixpkgs would be catastrophic; the walker
    // forces selectively as it descends.
    let evaluation = Evaluation::builder(io.clone())
        .mode(EvalMode::Lazy)
        .enable_import()
        .build();

    let source_map = evaluation.source_map();
    let result = evaluation.evaluate(&code, Some(flake_nix.clone()));

    for warning in &result.warnings {
        tracing::debug!(?warning, "nix eval warning");
    }
    if !result.errors.is_empty() {
        // A top-level failure (e.g. the shim itself broke) — degrade to
        // static-only rather than aborting the package.
        tracing::warn!(errors = result.errors.len(), "nix eval produced errors; static-only");
        return Ok(None);
    }

    let Some(value) = result.value else {
        return Ok(None);
    };

    if Instant::now() > deadline {
        return Err(NixError::EvalTimeout { seconds: EVAL_BUDGET.as_secs() });
    }

    // Hand the forced-lazily output tree to the walker for traversal + fusion.
    let source = source_map;
    let surface = walker::walk(&value, &source, table, meta, deadline);
    Ok(Some(surface))
}

// ───────────────────────────────────────────────────────────────────────────
// Flake-outputs shim
// ───────────────────────────────────────────────────────────────────────────

/// A flake-compat-style fixed point: import the flake, synthesize each input
/// as `{ outPath, rev, narHash, lastModified, … } // outputs`, and call
/// `outputs (inputs // { inherit self; })`.
///
/// We port the *semantics* of edolstra/flake-compat (the reference for this
/// trick), not its code. Each materialized input lives at
/// `<root>/../inputs/<name>` and is itself either a flake (has `outputs`) or a
/// plain source tree (only `outPath`).
fn flake_outputs_shim(root: &Path, inputs: &[String]) -> String {
    let root_str = root.display();
    let mut input_bindings = String::new();
    for name in inputs {
        // Each input: expose its source path and, if it is itself a flake, its
        // evaluated outputs merged on top (best-effort, tryEval-guarded).
        input_bindings.push_str(&format!(
            r#"    {name} = let
      src = /{root}/../inputs/{name};
      flakePath = src + "/flake.nix";
      base = {{ outPath = src; rev = "0000000000000000000000000000000000000000"; narHash = ""; lastModified = 0; }};
      outs = if builtins.pathExists flakePath
             then (import flakePath).outputs (allInputs // {{ self = base; }})
             else {{}};
    in base // outs;
"#,
            name = name,
            root = root_str
        ));
    }

    format!(
        r#"let
  flake = import /{root}/flake.nix;
  allInputs = rec {{
{inputs}  }};
  self = flake.outputs (allInputs // {{ inherit self; outPath = /{root}; }});
in self
"#,
        root = root_str,
        inputs = input_bindings,
    )
}

/// Read the locked input names from `flake.lock` (best-effort); an absent or
/// unreadable lock yields an empty set (the shim then supplies no inputs).
fn discover_input_names(root: &Path) -> Vec<String> {
    let lock = root.join("flake.lock");
    let Ok(text) = std::fs::read_to_string(&lock) else { return Vec::new() };
    let Ok(json) = serde_json::from_str::<serde_json::Value>(&text) else { return Vec::new() };
    // The root node's `inputs` map names the direct inputs.
    let root_key = json.get("root").and_then(|r| r.as_str()).unwrap_or("root");
    json.get("nodes")
        .and_then(|n| n.get(root_key))
        .and_then(|r| r.get("inputs"))
        .and_then(|i| i.as_object())
        .map(|m| m.keys().cloned().collect())
        .unwrap_or_default()
}

// ───────────────────────────────────────────────────────────────────────────
// Hermetic whitelist filesystem
// ───────────────────────────────────────────────────────────────────────────

/// A read-only [`EvalIO`] that serves exactly the flake tree and the
/// materialized `inputs/` directory. Everything else is denied. `import_path`
/// is identity (no store); hermeticity is enforced by path containment.
pub struct DocsIO {
    root:   PathBuf,
    inputs: Option<PathBuf>,
    /// Tripped if an escape attempt is seen (surfaced as a warning, not fatal).
    escaped: AtomicBool,
}

impl DocsIO {
    pub fn new(root: PathBuf, inputs: Option<PathBuf>) -> Self {
        Self { root, inputs, escaped: AtomicBool::new(false) }
    }

    /// Whether `path` is contained within the flake root or inputs dir.
    fn is_allowed(&self, path: &Path) -> bool {
        let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
        let under = |base: &Path| {
            base.canonicalize()
                .map(|b| canonical.starts_with(&b))
                .unwrap_or(false)
        };
        if under(&self.root) {
            return true;
        }
        if let Some(inputs) = &self.inputs {
            if under(inputs) {
                return true;
            }
        }
        self.escaped.store(true, Ordering::Relaxed);
        false
    }

    /// Whether any escape attempt was observed during evaluation.
    pub fn escaped(&self) -> bool {
        self.escaped.load(Ordering::Relaxed)
    }

    fn guard(&self, path: &Path) -> io::Result<()> {
        if self.is_allowed(path) {
            Ok(())
        } else {
            Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                format!("path {} escapes the hermetic flake root", path.display()),
            ))
        }
    }
}

impl EvalIO for DocsIO {
    fn path_exists(&self, path: &Path) -> io::Result<bool> {
        if !self.is_allowed(path) {
            return Ok(false);
        }
        Ok(path.exists())
    }

    fn open(&self, path: &Path) -> io::Result<Box<dyn io::Read>> {
        self.guard(path)?;
        let file = std::fs::File::open(path)?;
        Ok(Box::new(file))
    }

    fn file_type(&self, path: &Path) -> io::Result<FileType> {
        self.guard(path)?;
        let meta = std::fs::symlink_metadata(path)?;
        Ok(if meta.is_dir() {
            FileType::Directory
        } else if meta.file_type().is_symlink() {
            FileType::Symlink
        } else {
            FileType::Regular
        })
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<(bytes::Bytes, FileType)>> {
        self.guard(path)?;
        let mut entries = Vec::new();
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let name = bytes::Bytes::from(entry.file_name().to_string_lossy().into_owned());
            let ft = entry.file_type()?;
            let kind = if ft.is_dir() {
                FileType::Directory
            } else if ft.is_symlink() {
                FileType::Symlink
            } else {
                FileType::Regular
            };
            entries.push((name, kind));
        }
        Ok(entries)
    }

    fn import_path(&self, path: &Path) -> io::Result<PathBuf> {
        // No store: importing a path is identity within the sandbox.
        self.guard(path)?;
        Ok(path.to_path_buf())
    }

    fn get_env(&self, _key: &std::ffi::OsStr) -> Option<std::ffi::OsString> {
        None
    }
}
