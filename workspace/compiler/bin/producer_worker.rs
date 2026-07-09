//! Sandboxed producer worker — runs in-process interpreters out-of-process.
//!
//! Modes:
//! - `lower <lang> <root>` — one-shot lower; print IR JSON Index on stdout.
//! - `serve` — line-oriented JSON protocol for [`sandbox::WorkerPool`].
//!
//! Crash/OOM kills this process only; the indexer pool restarts a slot.
//! Process env is scrubbed by the pool parent (`env_clear`); never re-inherit.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use ir::entry::Index;
use sandbox::worker::{JobRequest, JobResponse, WorkerLang};

fn main() -> ExitCode {
	let mut args = std::env::args().skip(1);
	match args.next().as_deref() {
		Some("lower") => {
			let lang = args.next().unwrap_or_default();
			let root = args.next().map(PathBuf::from).unwrap_or_default();
			let Some(lang) = WorkerLang::parse(&lang) else {
				eprintln!("unknown lang (want nix|typescript|python)");
				return ExitCode::from(1);
			};
			match lower(lang, &root) {
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
			Ok(JobRequest::Ping) => JobResponse::Pong { rss: current_rss() },
			Ok(JobRequest::Shutdown) => {
				let _ = writeln!(
					stdout,
					"{}",
					serde_json::to_string(&JobResponse::Ok {
						body: String::new()
					})
					.unwrap_or_default()
				);
				let _ = stdout.flush();
				return ExitCode::SUCCESS;
			}
			Ok(JobRequest::Lower { lang, root }) => match lower(lang, &root) {
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

fn lower(lang: WorkerLang, root: &std::path::Path) -> Result<String, String> {
	if !root.exists() {
		return Err(format!("root does not exist: {}", root.display()));
	}
	let index: Index = match lang {
		WorkerLang::Nix => compiler::languages::nix::lower_package(root).map_err(|e| e.to_string())?,
		WorkerLang::Typescript => {
			let name = root
				.file_name()
				.and_then(|s| s.to_str())
				.unwrap_or("package")
				.to_string();
			let package = compiler::languages::typescript::TypescriptPackage { name };
			let collected = package.generate_ir(root).map_err(|e| e.to_string())?;
			collected.index().into_index()
		}
		WorkerLang::Python => {
			let ctx = compiler::languages::python::context::PythonContext::new();
			ctx.lower_package(root)
		}
	};
	serde_json::to_string(&index).map_err(|e| e.to_string())
}

/// Best-effort RSS in bytes for pool watermarking.
fn current_rss() -> Option<u64> {
	#[cfg(target_os = "linux")]
	{
		let statm = std::fs::read_to_string("/proc/self/statm").ok()?;
		let pages: u64 = statm.split_whitespace().nth(1)?.parse().ok()?;
		let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) as u64 };
		Some(pages.saturating_mul(page))
	}
	#[cfg(target_os = "macos")]
	{
		// task_info is heavy; approximate via getrusage maxrss (bytes on macOS).
		// SAFETY: getrusage with RUSAGE_SELF is well-defined.
		unsafe {
			let mut usage: libc::rusage = std::mem::zeroed();
			if libc::getrusage(libc::RUSAGE_SELF, &mut usage) == 0 {
				// macOS: ru_maxrss is bytes.
				Some(usage.ru_maxrss as u64)
			} else {
				None
			}
		}
	}
	#[cfg(not(any(target_os = "linux", target_os = "macos")))]
	{
		None
	}
}
