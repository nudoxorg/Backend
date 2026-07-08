//! Nudox rustdoc driver — zero-disk-JSON doc pass.
//!
//! Cargo invokes this binary via `RUSTDOC=<driver>`. The driver:
//!   1. Receives the exact argv that cargo would pass to the system rustdoc.
//!   2. Strips `--out-dir <path>` from the args so rustdoc writes JSON to stdout
//!      instead of a file (rustdoc's built-in stdout mode).
//!   3. Invokes the system rustdoc, captures its stdout.
//!   4. Parses the JSON from the pipe — never touches the filesystem.
//!   5. Lowers `rustdoc_types::Crate → Vec<ir::kind::Entry>` via `RustdocParser`.
//!   6. Writes IR JSON to `NUDOX_IR_OUT` and source-map JSON to `NUDOX_SOURCE_MAP_OUT`.
//!
//! No rustc_private linkage required; the sysroot rustdoc is used as-is.

use std::{
    collections::HashMap,
    env,
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::Arc,
};

use ir::kind::Entry;
use rust_lowering::context::RustdocParser;

fn main() {
    // argv[0] is this binary; the rest are cargo's args for rustdoc.
    let at_args: Vec<String> = env::args().collect();

    let ir_out        = required_env("NUDOX_IR_OUT");
    let sm_out        = required_env("NUDOX_SOURCE_MAP_OUT");
    let workspace_root = PathBuf::from(env::var("NUDOX_WORKSPACE_ROOT").unwrap_or_default());

    // ── doc pass via system rustdoc → stdout ─────────────────────────────────
    // Strip --out-dir so rustdoc writes JSON to stdout (no disk file).
    let rustdoc_args = strip_out_dir(&at_args[1..]);
    let system_rustdoc = system_rustdoc_path();

    let result = Command::new(&system_rustdoc)
        .args(&rustdoc_args)
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .output()
        .unwrap_or_else(|e| {
            eprintln!("rustdoc-driver: failed to spawn {system_rustdoc:?}: {e}");
            std::process::exit(1);
        });

    if !result.status.success() {
        std::process::exit(result.status.code().unwrap_or(1));
    }

    // ── parse JSON from pipe ──────────────────────────────────────────────────
    let rustdoc_crate: rustdoc_types::Crate =
        serde_json::from_slice(&result.stdout).unwrap_or_else(|e| {
            eprintln!("rustdoc-driver: JSON parse failed: {e}");
            eprintln!("(first 512 bytes: {:?})", &result.stdout[..result.stdout.len().min(512)]);
            std::process::exit(1);
        });

    // ── source map ────────────────────────────────────────────────────────────
    let source_map = build_source_map(&rustdoc_crate, &workspace_root);

    // ── IR lowering ───────────────────────────────────────────────────────────
    let mut parser = RustdocParser::from_doc(rustdoc_crate).unwrap_or_else(|e| {
        eprintln!("rustdoc-driver: IR init failed: {e}");
        std::process::exit(1);
    });
    let entries: Vec<Entry> = parser.parse().unwrap_or_else(|e| {
        eprintln!("rustdoc-driver: IR lowering failed: {e}");
        std::process::exit(1);
    });

    // ── write outputs ─────────────────────────────────────────────────────────
    write_json(&ir_out,  &entries,    "IR");
    write_json(&sm_out, &source_map, "source map");
}

// ─── Arg manipulation ─────────────────────────────────────────────────────────

/// Remove `--out-dir <value>` pairs from args.  With no out-dir, rustdoc's JSON
/// backend writes the assembled Crate to stdout (see librustdoc json/mod.rs).
fn strip_out_dir(args: &[String]) -> Vec<String> {
    let mut out = Vec::with_capacity(args.len());
    let mut skip_next = false;
    for arg in args {
        if skip_next {
            skip_next = false;
            continue;
        }
        if arg == "--out-dir" {
            skip_next = true;
            continue;
        }
        if arg.starts_with("--out-dir=") {
            continue;
        }
        out.push(arg.clone());
    }
    out
}

/// Find the system rustdoc to delegate to.  Checks NUDOX_RUSTDOC_SYSTEM first,
/// then looks for `rustdoc` on PATH (the sysroot rustdoc from the Nix devshell).
fn system_rustdoc_path() -> PathBuf {
    if let Ok(p) = env::var("NUDOX_RUSTDOC_SYSTEM") {
        return PathBuf::from(p);
    }
    // rustc --print sysroot → <sysroot>/bin/rustdoc
    if let Ok(out) = Command::new("rustc").arg("--print").arg("sysroot").output() {
        if out.status.success() {
            let sysroot = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
            let candidate = sysroot.join("bin/rustdoc");
            if candidate.exists() {
                return candidate;
            }
        }
    }
    PathBuf::from("rustdoc")
}

// ─── Source map ───────────────────────────────────────────────────────────────

fn build_source_map(krate: &rustdoc_types::Crate, workspace: &Path) -> HashMap<String, String> {
    let mut map = HashMap::new();
    let mut file_cache: HashMap<PathBuf, Option<(Arc<str>, Vec<usize>)>> = HashMap::new();

    for (id, item) in &krate.index {
        if !matches!(&item.inner, rustdoc_types::ItemEnum::Function(_)) {
            continue;
        }
        let Some(span)    = &item.span               else { continue };
        let Some(summary) = krate.paths.get(id)      else { continue };
        let fq_name = summary.path.join("::");

        let source_file = workspace.join(&span.filename);
        let cached = file_cache.entry(source_file.clone()).or_insert_with(|| {
            let source = fs::read_to_string(&source_file).ok()?;
            let offsets = line_start_offsets(&source);
            Some((Arc::<str>::from(source), offsets))
        });
        let Some((source, line_starts)) = cached.as_ref() else { continue };

        let Some(&start) = line_starts.get(span.begin.0.saturating_sub(1)) else { continue };
        let end = line_starts.get(span.end.0).copied().unwrap_or(source.len());
        let raw: String = source[start..end].lines().collect::<Vec<_>>().join("\n");
        if !raw.is_empty() {
            map.insert(fq_name, raw);
        }
    }
    map
}

fn line_start_offsets(source: &str) -> Vec<usize> {
    let mut offsets = vec![0usize];
    for (idx, byte) in source.bytes().enumerate() {
        if byte == b'\n' { offsets.push(idx + 1); }
    }
    offsets
}

// ─── Helpers ──────────────────────────────────────────────────────────────────

fn required_env(key: &str) -> PathBuf {
    PathBuf::from(env::var(key).unwrap_or_else(|_| {
        eprintln!("rustdoc-driver: {key} must be set by the caller");
        std::process::exit(1);
    }))
}

fn write_json<T: serde::Serialize>(path: &Path, value: &T, label: &str) {
    let bytes = serde_json::to_vec(value).unwrap_or_else(|e| {
        eprintln!("rustdoc-driver: {label} serialisation failed: {e}");
        std::process::exit(1);
    });
    fs::write(path, &bytes).unwrap_or_else(|e| {
        eprintln!("rustdoc-driver: write {label} to {path:?} failed: {e}");
        std::process::exit(1);
    });
}
