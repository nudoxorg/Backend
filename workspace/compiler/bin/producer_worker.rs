//! Sandboxed producer worker — runs in-process interpreters out-of-process.
//!
//! Modes:
//! - `lower <lang> <root>` — one-shot lower; print IR JSON Index on stdout.
//! - `serve` — line-oriented JSON protocol for [`sandbox::WorkerPool`].
//!
//! Crash/OOM kills this process only; the indexer pool restarts a slot.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use ir::entry::Index;
use sandbox::worker::{JobRequest, JobResponse};

fn main() -> ExitCode {
	let mut args = std::env::args().skip(1);
	match args.next().as_deref() {
		Some("lower") => {
			let lang = args.next().unwrap_or_default();
			let root = args.next().map(PathBuf::from).unwrap_or_default();
			match lower(&lang, &root) {
				Ok(body) => {
					print!("{body}");
					ExitCode::SUCCESS
				}
				Err(e) => {
					eprintln!("producer-worker lower error: {e}");
					ExitCode::from(2)
				}
			}
		}
		Some("serve") => serve(),
		_ => {
			eprintln!("usage: producer-worker lower <nix|typescript|python> <root>");
			eprintln!("       producer-worker serve");
			ExitCode::from(1)
		}
	}
}

fn serve() -> ExitCode {
	let stdin = std::io::stdin();
	let mut stdout = std::io::stdout();
	for line in stdin.lock().lines() {
		let Ok(line) = line else {
			break;
		};
		let line = line.trim();
		if line.is_empty() {
			continue;
		}
		let resp = match serde_json::from_str::<JobRequest>(line) {
			Ok(JobRequest::Ping) => JobResponse::Pong { rss: None },
			Ok(JobRequest::Shutdown) => {
				let _ = writeln!(stdout, "{}", serde_json::to_string(&JobResponse::Ok { body: String::new() }).unwrap_or_default());
				let _ = stdout.flush();
				return ExitCode::SUCCESS;
			}
			Ok(JobRequest::Lower { lang, root }) => match lower(&lang, &root) {
				Ok(body) => JobResponse::Ok { body },
				Err(e) => JobResponse::Err {
					kind: "lower".into(),
					message: e,
				},
			},
			Err(e) => JobResponse::Err {
				kind: "protocol".into(),
				message: e.to_string(),
			},
		};
		match serde_json::to_string(&resp) {
			Ok(s) => {
				if writeln!(stdout, "{s}").is_err() {
					break;
				}
				let _ = stdout.flush();
			}
			Err(_) => break,
		}
	}
	ExitCode::SUCCESS
}

fn lower(lang: &str, root: &std::path::Path) -> Result<String, String> {
	if !root.exists() {
		return Err(format!("root does not exist: {}", root.display()));
	}
	let index: Index = match lang {
		"nix" => compiler::languages::nix::lower_package(root)
			.map_err(|e| e.to_string())?,
		"typescript" | "ts" => {
			let name = root
				.file_name()
				.and_then(|s| s.to_str())
				.unwrap_or("package")
				.to_string();
			let package = compiler::languages::typescript::TypescriptPackage { name };
			let collected = package.generate_ir(root).map_err(|e| e.to_string())?;
			collected.index().into_index()
		}
		"python" | "py" => {
			let ctx = compiler::languages::python::context::PythonContext::new();
			ctx.lower_package(root)
		}
		other => return Err(format!("unknown lang: {other}")),
	};
	serde_json::to_string(&index).map_err(|e| e.to_string())
}
