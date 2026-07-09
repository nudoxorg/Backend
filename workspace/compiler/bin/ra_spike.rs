//! P0 spike: load a Cargo workspace via `ra_ap_load_cargo` and dump local
//! `ModuleDef`s (kind, canonical path, first doc line).
//!
//! ```text
//!   buck2 run //workspace/compiler:ra-spike -- [path/to/crate]
//! ```
//!
//! Default root: `tests/fixtures/rust/regular` (manifests are scaffolded into a
//! temp dir when gitignore-stripped `Cargo.toml` files are missing).

use std::env;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, bail};
use ra_ap_hir::{
	Adt, Crate, DisplayTarget, HasAttrs, HirDisplay, Module, ModuleDef, attach_db, db::HirDatabase,
};
use ra_ap_load_cargo::{LoadCargoConfig, ProcMacroServerChoice, load_workspace_at};
use ra_ap_project_model::{CargoConfig, RustLibSource};

fn main() -> ExitCode {
	match run() {
		Ok(()) => ExitCode::SUCCESS,
		Err(e) => {
			eprintln!("ra-spike error: {e:#}");
			ExitCode::from(1)
		}
	}
}

fn run() -> Result<()> {
	let user_root = resolve_root()?;
	let (_keep_alive, load_root) = materialize_workspace(&user_root)?;
	eprintln!("loading workspace at {}", load_root.display());

	let cargo_config = CargoConfig {
		sysroot: Some(RustLibSource::Discover),
		set_test: false,
		no_deps: false,
		..CargoConfig::default()
	};

	// Prefer the fenix sysroot proc-macro server; fall back to None with a warning.
	let mut proc_choice = ProcMacroServerChoice::Sysroot;
	if find_sysroot_proc_macro_srv().is_none() {
		eprintln!(
			"warning: rust-analyzer-proc-macro-srv not found on PATH/sysroot; \
			 loading with ProcMacroServerChoice::None"
		);
		proc_choice = ProcMacroServerChoice::None;
	}

	let load_config = LoadCargoConfig {
		load_out_dirs_from_check: false,
		with_proc_macro_server: proc_choice,
		prefill_caches: false,
		num_worker_threads: 1,
		proc_macro_processes: 1,
	};

	let (db, _vfs, proc_macro) = load_workspace_at(
		&load_root,
		&cargo_config,
		&load_config,
		&|msg| eprintln!("  {msg}"),
	)
	.with_context(|| format!("load_workspace_at({})", load_root.display()))?;

	// Hold the client so expansions stay alive for the walk.
	let _proc_macro = proc_macro;
	if _proc_macro.is_none() {
		eprintln!("warning: no proc-macro server attached (macros will not expand)");
	}

	let mut out = io::stdout().lock();
	let mut first_fn_sig: Option<String> = None;

	let mut local_crates = 0usize;
	for krate in Crate::all(&db) {
		if !krate.origin(&db).is_local() {
			continue;
		}
		local_crates += 1;

		let name = krate
			.display_name(&db)
			.map(|n| n.to_string())
			.unwrap_or_else(|| "<anon>".into());
		let display = krate.to_display_target(&db);
		writeln!(out, "=== crate {name} ===")?;

		let root = krate.root_module(&db);
		walk_module(&db, krate, root, display, &mut out, &mut first_fn_sig)?;
		writeln!(out)?;
	}

	if local_crates == 0 {
		bail!("no local crates found after load — is the path a Cargo project?");
	}

	if let Some(sig) = first_fn_sig {
		writeln!(out, "--- sample function signature ---")?;
		writeln!(out, "{sig}")?;
	}

	writeln!(out, "ok: {local_crates} local crate(s)")?;
	Ok(())
}

fn walk_module(
	db: &dyn HirDatabase,
	krate: Crate,
	module: Module,
	display: DisplayTarget,
	out: &mut impl Write,
	first_fn_sig: &mut Option<String>,
) -> Result<()> {
	for def in module.declarations(db) {
		emit_def(db, krate, def, display, out, first_fn_sig)?;
		if let ModuleDef::Module(child) = def {
			walk_module(db, krate, child, display, out, first_fn_sig)?;
		}
	}
	Ok(())
}

fn emit_def(
	db: &dyn HirDatabase,
	krate: Crate,
	def: ModuleDef,
	display: DisplayTarget,
	out: &mut impl Write,
	first_fn_sig: &mut Option<String>,
) -> Result<()> {
	let kind = def_kind(def);
	// `Crate::edition` returns span::Edition; pass it straight into canonical_path
	// without naming the type (avoids an extra direct dep on ra_ap_span).
	let path = def
		.canonical_path(db, krate.edition(db))
		.unwrap_or_else(|| "<unnamed>".into());
	let doc = def
		.hir_docs(db)
		.map(|d| first_doc_line(d.docs()))
		.filter(|s| !s.is_empty())
		.unwrap_or_default();

	if doc.is_empty() {
		writeln!(out, "{kind:<8} {path}")?;
	} else {
		writeln!(out, "{kind:<8} {path}  [{doc}]")?;
	}

	if first_fn_sig.is_none()
		&& let ModuleDef::Function(f) = def
	{
		// HirDisplay needs an attached db (next-solver TLS).
		let rendered = attach_db(db, || f.display(db, display).to_string());
		if !rendered.is_empty() {
			*first_fn_sig = Some(format!("{path}: {rendered}"));
		}
	}

	Ok(())
}

fn def_kind(def: ModuleDef) -> &'static str {
	match def {
		ModuleDef::Module(_) => "module",
		ModuleDef::Function(_) => "fn",
		ModuleDef::Adt(Adt::Struct(_)) => "struct",
		ModuleDef::Adt(Adt::Enum(_)) => "enum",
		ModuleDef::Adt(Adt::Union(_)) => "union",
		ModuleDef::EnumVariant(_) => "variant",
		ModuleDef::Const(_) => "const",
		ModuleDef::Static(_) => "static",
		ModuleDef::Trait(_) => "trait",
		ModuleDef::TypeAlias(_) => "type",
		ModuleDef::BuiltinType(_) => "builtin",
		ModuleDef::Macro(_) => "macro",
	}
}

fn first_doc_line(docs: &str) -> String {
	docs.lines()
		.map(str::trim)
		.find(|l| !l.is_empty())
		.unwrap_or("")
		.to_string()
}

fn resolve_root() -> Result<PathBuf> {
	if let Some(arg) = env::args().nth(1) {
		let p = PathBuf::from(arg);
		if !p.exists() {
			bail!("path does not exist: {}", p.display());
		}
		return Ok(p);
	}

	// Prefer CARGO_MANIFEST_DIR (buck rust_tests / cargo), then cwd.
	let candidates = [
		option_env!("CARGO_MANIFEST_DIR").map(|m| PathBuf::from(m).join("tests/fixtures/rust/regular")),
		env::current_dir()
			.ok()
			.map(|c| c.join("workspace/compiler/tests/fixtures/rust/regular")),
		env::current_dir()
			.ok()
			.map(|c| c.join("tests/fixtures/rust/regular")),
	];
	for c in candidates.into_iter().flatten() {
		if c.join("src/lib.rs").is_file() {
			return Ok(c);
		}
	}
	bail!(
		"usage: ra-spike [path/to/crate-or-workspace]\n\
		 (default fixture tests/fixtures/rust/regular not found from CARGO_MANIFEST_DIR or cwd)"
	);
}

/// Ensure a loadable Cargo project exists at `root`.
///
/// The in-repo `regular` fixture ships sources only (Cargo.toml is gitignored);
/// when manifests are missing we copy into a temp dir and write them there so
/// the source tree stays clean.
fn materialize_workspace(root: &Path) -> Result<(Option<tempfile::TempDir>, PathBuf)> {
	if root.join("Cargo.toml").is_file() {
		return Ok((None, root.to_path_buf()));
	}

	if !root.join("src/lib.rs").is_file() {
		bail!(
			"{} has no Cargo.toml and no src/lib.rs — pass a Cargo project root",
			root.display()
		);
	}

	let tmp = tempfile::tempdir().context("create temp dir for scaffolded fixture")?;
	let dest = tmp.path().to_path_buf();
	copy_tree(root, &dest)?;

	// Same scaffold as tests/rust_compiler_e2e.rs::scaffold_regular.
	fs::write(
		dest.join("Cargo.toml"),
		r#"[package]
name = "calculator"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"

[dependencies]
helper = { path = "helper" }
"#,
	)?;
	if dest.join("helper/src/lib.rs").is_file() {
		fs::create_dir_all(dest.join("helper"))?;
		fs::write(
			dest.join("helper/Cargo.toml"),
			r#"[package]
name = "helper"
version = "0.1.0"
edition = "2021"

[lib]
path = "src/lib.rs"
"#,
		)?;
	}

	eprintln!(
		"scaffolded Cargo.toml into temp dir (fixture sources at {})",
		root.display()
	);
	Ok((Some(tmp), dest))
}

fn copy_tree(src: &Path, dest: &Path) -> io::Result<()> {
	fs::create_dir_all(dest)?;
	for entry in fs::read_dir(src)? {
		let entry = entry?;
		let target = dest.join(entry.file_name());
		if entry.file_type()?.is_dir() {
			copy_tree(&entry.path(), &target)?;
		} else {
			fs::copy(entry.path(), &target)?;
		}
	}
	Ok(())
}

/// Best-effort probe so we can warn before load when the fenix component is missing.
fn find_sysroot_proc_macro_srv() -> Option<PathBuf> {
	// rustup/fenix: $(rustc --print sysroot)/libexec/rust-analyzer-proc-macro-srv
	let output = std::process::Command::new("rustc")
		.args(["--print", "sysroot"])
		.output()
		.ok()?;
	if !output.status.success() {
		return None;
	}
	let sysroot = String::from_utf8(output.stdout).ok()?;
	let sysroot = sysroot.trim();
	let candidates = [
		PathBuf::from(sysroot).join("libexec/rust-analyzer-proc-macro-srv"),
		PathBuf::from(sysroot).join("libexec/rust-analyzer-proc-macro-srv.exe"),
	];
	candidates.into_iter().find(|p| p.is_file())
}
