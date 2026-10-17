//! Read-only installed MCP onboarding. Never opens a workspace or edits client config.

use crate::options::{Format, Options};
use backend_present::Fault;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub(super) const fn help_text() -> &'static str {
    "nudox mcp-config — print installed stdio MCP setup without starting a service\n\n\
     nudox mcp-config [--project PATH]\n\
     nudox mcp-config --format json [--project PATH]\n\n\
     Prints complete mcpServers JSON and shell-quoted Claude Code/Codex registration instructions. JSON format prints only the configuration. The selected project follows normal CLI discovery: --project, then BACKEND_PROJECT, then the Git root or nearest language manifest from this directory. It is recorded as an absolute path. Existing client configuration is never read, written or removed; inspect an existing nudox entry before registering. Repeating this command only prints the same setup.\n\n\
     Stdio: the client launches MCP; its first owner-backed tool call starts the matched local daemon. It does not start an HTTP listener or index automatically. Index with nudox add . --wait, inspect its terminal receipt, then search. HTTP is separate: explicitly keep backend-mcp --project /absolute/project --http 127.0.0.1:41449 running and configure http://127.0.0.1:41449/mcp with the selected Bearer credential."
}

#[derive(Debug, Eq, PartialEq)]
struct Launchers {
    mcp: PathBuf,
}

fn executable(path: &Path) -> bool {
    backend_runtime::observed_cli_executable(path).is_some()
}

/// Match only this executable's bundle. PATH is considered solely for aliases
/// which resolve to these already-selected launchers, never for another install.
fn launchers(
    current: &Path,
    invoked: Option<&Path>,
    search: &[PathBuf],
) -> Result<Launchers, String> {
    let current = std::fs::canonicalize(current)
        .map_err(|error| format!("cannot locate this CLI: {error}"))?;
    let bin = current
        .parent()
        .ok_or("this CLI has no installed directory")?;
    if bin.file_name().is_some_and(|name| name == ".libexec")
        || current
            .file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with(".elf"))
    {
        return Err("cannot derive a supported public MCP launcher from this internal CLI image; use the public MCP path shown by the installer, not this internal payload".into());
    }
    let name = |stem: &str| format!("{stem}{}", std::env::consts::EXE_SUFFIX);
    let cli = bin.join(name("backend-cli"));
    let mcp = if executable(&bin.join(name("nudox-mcp"))) {
        bin.join(name("nudox-mcp"))
    } else {
        bin.join(name("backend-mcp"))
    };
    let locald = if executable(&bin.join(name("nudox-locald"))) {
        bin.join(name("nudox-locald"))
    } else {
        bin.join(name("backend-locald"))
    };
    // The known macOS wrappers exec these raw siblings. A script's execute
    // bit alone cannot establish that its required payload still exists.
    let raw_mcp = bin.join(name("backend-mcp"));
    let raw_locald = bin.join(name("backend-locald"));
    for path in [&mcp, &locald, &raw_mcp, &raw_locald] {
        if !executable(path)
            || std::fs::canonicalize(path)
                .ok()
                .as_deref()
                .and_then(Path::parent)
                != Some(bin)
        {
            return Err(format!(
                "missing executable companion: {}; install the matched CLI, MCP and local daemon together",
                path.display()
            ));
        }
    }
    let mut candidates = Vec::new();
    if let Some(invoked) = invoked.filter(|path| path.is_absolute()) {
        if let Some(parent) = invoked.parent() {
            candidates.push(parent.to_path_buf());
        }
    }
    candidates.extend(
        search
            .iter()
            .filter(|path| path.is_absolute())
            .take(256)
            .cloned(),
    );
    for parent in candidates {
        let same = |path: &Path, target: &Path| {
            std::fs::canonicalize(path)
                .ok()
                .zip(std::fs::canonicalize(target).ok())
                .is_some_and(|(a, b)| a == b)
        };
        let cli_wrapper = bin.join(name("nudox-cli"));
        if !["nudox", "backend-cli", "nudox-cli"].iter().any(|stem| {
            let alias = parent.join(name(stem));
            same(&alias, &cli) || (executable(&cli_wrapper) && same(&alias, &cli_wrapper))
        }) {
            continue;
        }
        let public_mcp = ["nudox-mcp", "backend-mcp"]
            .into_iter()
            .map(|stem| parent.join(name(stem)))
            .find(|alias| same(alias, &mcp));
        let public_locald = ["nudox-locald", "backend-locald"]
            .into_iter()
            .map(|stem| parent.join(name(stem)))
            .find(|alias| same(alias, &locald));
        if let (Some(mcp), Some(_)) = (public_mcp, public_locald) {
            return Ok(Launchers { mcp });
        }
    }
    Ok(Launchers { mcp })
}

fn config(launchers: &Launchers, project: &Path) -> Result<Value, String> {
    let text = |path: &Path| {
        path.to_str()
            .ok_or("MCP setup paths must be valid UTF-8")
            .map(str::to_owned)
    };
    Ok(json!({"mcpServers":{"nudox":{
        "type":"stdio", "command":text(&launchers.mcp)?,
        "args":["--project",text(project)?]
    }}}))
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn instructions(value: &Value, encoded: &str) -> Result<String, String> {
    let server = &value["mcpServers"]["nudox"];
    let command = server["command"].as_str().ok_or("missing MCP command")?;
    let project = server["args"][1].as_str().ok_or("missing project")?;
    let entry = serde_json::to_string(server).map_err(|error| error.to_string())?;
    Ok(format!(
        "Stdio MCP setup for {project}\n\n{encoded}\n\nExisting configuration is unchanged. Inspect an existing nudox entry before adding; no removal or overwrite command is emitted.\n\nPOSIX shell registration (macOS/Linux):\n  (cd {} && claude mcp get nudox)\n  (cd {} && claude mcp add-json --scope local nudox {})\n  codex mcp list\n  codex mcp add nudox -- {} --project {}\n\nThe MCP client starts this stdio process; the first owner-backed tool call discovers the local daemon beside that MCP release. No owner override is saved, so a later installation upgrade keeps the pair matched. Existing explicit runtime environment overrides remain your responsibility. No HTTP listener starts automatically and registration does not index a project. Run nudox add . --wait from that project, inspect the terminal receipt, then nudox search \"error handling\".\n\nFor HTTP instead, explicitly run:\n  {} --project {} --http 127.0.0.1:41449\nKeep the server running; connect to http://127.0.0.1:41449/mcp using its selected Bearer credential.\n",
        quote(project),
        quote(project),
        quote(&entry),
        quote(command),
        quote(project),
        quote(command),
        quote(project)
    ))
}

pub(super) fn run(words: &[String], options: &Options) -> Result<String, Fault> {
    let fail = |message| Fault::usage("mcp-config", message);
    if !words.is_empty()
        || options.workspace().is_some()
        || options.endpoint().is_some()
        || options.limit().is_some()
    {
        return Err(fail("mcp-config accepts only --project and output formatting; it never selects or initializes a private workspace".to_owned()));
    }
    let current = std::env::current_exe().map_err(|error| fail(error.to_string()))?;
    let invoked = std::env::args_os().next().map(PathBuf::from);
    let search = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).take(256).collect::<Vec<_>>())
        .unwrap_or_default();
    let launchers = launchers(&current, invoked.as_deref(), &search).map_err(fail)?;
    let project = backend_runtime::discover_project(options.project().cloned())
        .map_err(|error| fail(error.to_string()))?;
    let project = std::fs::canonicalize(project)
        .map_err(|error| fail(format!("selected project is unavailable: {error}")))?;
    if !project.is_dir() {
        return Err(fail("selected project is not a directory".to_owned()));
    }
    let value = config(&launchers, &project).map_err(fail)?;
    let encoded = serde_json::to_string_pretty(&value).map_err(|error| fail(error.to_string()))?;
    if options.format() == Format::Json {
        Ok(format!("{encoded}\n"))
    } else {
        instructions(&value, &encoded).map_err(fail)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{
        fs, io,
        os::unix::fs::{PermissionsExt, symlink},
        process::Command,
    };
    type Result = std::result::Result<(), Box<dyn std::error::Error>>;

    fn member(bin: &Path, name: &str) -> io::Result<PathBuf> {
        fs::create_dir_all(bin)?;
        let path = bin.join(name);
        // Observed executable fixture only: setup generation must not execute it.
        fs::write(&path, b"#!/bin/sh\nexit 99\n")?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        Ok(path)
    }
    fn bundle(bin: &Path) -> io::Result<PathBuf> {
        member(bin, "backend-mcp")?;
        member(bin, "backend-locald")?;
        member(bin, "backend-cli")
    }
    fn admit(current: &Path, invoked: Option<&Path>, path: &[PathBuf]) -> io::Result<Launchers> {
        launchers(current, invoked, path).map_err(io::Error::other)
    }
    fn shell_words(line: &str) -> io::Result<Vec<String>> {
        // The shell only parses the printed command as arguments; no MCP,
        // Claude, Codex or local owner process is launched.
        let output = Command::new("/bin/sh")
            .arg("-c")
            .arg(format!("set -- {line}; printf '%s\\0' \"$@\""))
            .env_clear()
            .output()?;
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        Ok(output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|part| !part.is_empty())
            .map(|part| String::from_utf8_lossy(part).into_owned())
            .collect())
    }

    #[test]
    fn installed_setup_json_and_shell_keep_exact_paths_without_executing_or_writing_config()
    -> Result {
        let dir = tempfile::tempdir()?;
        let bin = dir.path().join("custom ' prefix $HOME with spaces/bin");
        let current = bundle(&bin)?;
        let project = dir.path().join("project \"quoted\" λ $(false)");
        fs::create_dir(&project)?;
        let client_config = dir.path().join(".claude.json");
        fs::write(&client_config, b"{\"existing\":true}\n")?;
        let launchers = admit(&current, Some(&current), &[])?;
        let value = config(&launchers, &project)?;
        let encoded = serde_json::to_string_pretty(&value)?;
        assert_eq!(serde_json::from_str::<Value>(&encoded)?, value);
        assert_eq!(value["mcpServers"]["nudox"]["type"], "stdio");
        assert_eq!(
            value["mcpServers"]["nudox"]["args"][1],
            project.to_str().ok_or("UTF8 path")?
        );
        let rendered = instructions(&value, &encoded)?;
        let claude = rendered
            .lines()
            .find(|line| line.contains("&& claude mcp add-json"))
            .ok_or("Claude instruction")?;
        let output = Command::new("/bin/sh")
            .arg("-c")
            .arg(format!(
                "claude() {{ printf '%s\\0' \"$PWD\" \"$@\"; }}; {claude}"
            ))
            .env_clear()
            .output()?;
        assert!(output.status.success());
        assert!(output.stderr.is_empty());
        let words = output
            .stdout
            .split(|byte| *byte == 0)
            .filter(|part| !part.is_empty())
            .map(|part| String::from_utf8_lossy(part).into_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            words[0],
            project.to_str().ok_or("project UTF8")?,
            "local scope registers in the selected project even from another shell directory"
        );
        assert_eq!(
            &words[1..6],
            ["mcp", "add-json", "--scope", "local", "nudox"]
        );
        assert_eq!(
            serde_json::from_str::<Value>(&words[6])?,
            value["mcpServers"]["nudox"]
        );
        let codex = rendered
            .lines()
            .find(|line| line.trim_start().starts_with("codex mcp add"))
            .ok_or("Codex instruction")?;
        let words = shell_words(codex)?;
        assert_eq!(&words[..5], ["codex", "mcp", "add", "nudox", "--"]);
        assert_eq!(words[5], launchers.mcp.to_str().ok_or("MCP UTF8")?);
        assert_eq!(words[7], project.to_str().ok_or("project UTF8")?);
        assert!(
            value["mcpServers"]["nudox"].get("env").is_none(),
            "saved aliases must not override canonical MCP-to-locald discovery across a later upgrade"
        );
        assert_eq!(
            rendered,
            instructions(&config(&launchers, &project)?, &encoded)?
        );
        assert_eq!(fs::read(client_config)?, b"{\"existing\":true}\n");
        assert!(!project.join(".mcp.json").exists());
        Ok(())
    }

    #[test]
    fn stable_installed_aliases_follow_upgrade_and_never_borrow_a_different_running_release()
    -> Result {
        let dir = tempfile::tempdir()?;
        let public = dir.path().join("custom prefix/bin");
        fs::create_dir_all(&public)?;
        let a = dir.path().join("versions/a/bin");
        let b = dir.path().join("versions/b/bin");
        let current_a = bundle(&a)?;
        let current_b = bundle(&b)?;
        for (alias, name) in [
            ("nudox", "backend-cli"),
            ("nudox-mcp", "backend-mcp"),
            ("nudox-locald", "backend-locald"),
        ] {
            symlink(a.join(name), public.join(alias))?;
        }
        let selected_a = admit(&current_a, Some(&public.join("nudox")), &[])?;
        assert_eq!(selected_a.mcp, public.join("nudox-mcp"));
        for (alias, name) in [
            ("nudox", "backend-cli"),
            ("nudox-mcp", "backend-mcp"),
            ("nudox-locald", "backend-locald"),
        ] {
            fs::remove_file(public.join(alias))?;
            symlink(b.join(name), public.join(alias))?;
        }
        let selected_b = admit(&current_b, Some(&public.join("nudox")), &[])?;
        assert_eq!(
            selected_a, selected_b,
            "saved public paths stay stable across installer upgrade"
        );
        assert_eq!(
            admit(&current_a, Some(&public.join("nudox")), &[])?.mcp,
            a.join("backend-mcp"),
            "an already running old CLI cannot borrow the newly selected release"
        );
        Ok(())
    }

    #[test]
    fn macos_bundle_uses_public_environment_wrappers_and_homebrew_aliases() -> Result {
        let dir = tempfile::tempdir()?;
        let bin = dir.path().join("Nudox.app/Contents/MacOS");
        let current = bundle(&bin)?;
        for name in ["nudox-cli", "nudox-mcp", "nudox-locald"] {
            member(&bin, name)?;
        }
        let public = dir.path().join("Homebrew bin");
        fs::create_dir(&public)?;
        for name in ["nudox-cli", "nudox-mcp", "nudox-locald"] {
            symlink(bin.join(name), public.join(name))?;
        }
        let selected = admit(&current, None, &[public.clone()])?;
        assert_eq!(selected.mcp, public.join("nudox-mcp"));
        fs::remove_file(public.join("nudox-locald"))?;
        assert_eq!(
            admit(&current, None, &[public])?.mcp,
            bin.join("nudox-mcp"),
            "an incomplete public companion set cannot be saved as a stable alias"
        );
        fs::remove_file(bin.join("backend-mcp"))?;
        assert!(
            launchers(&current, None, &[]).is_err(),
            "a surviving public wrapper cannot conceal its missing raw payload"
        );
        Ok(())
    }

    #[test]
    fn missing_nonexecutable_or_foreign_companions_refuse_without_path_fallback() -> Result {
        let dir = tempfile::tempdir()?;
        let bin = dir.path().join("selected/bin");
        let current = bundle(&bin)?;
        let foreign = dir.path().join("foreign/bin");
        bundle(&foreign)?;
        fs::remove_file(bin.join("backend-mcp"))?;
        let error = launchers(&current, None, &[foreign.clone()])
            .err()
            .ok_or("missing matched MCP was accepted")?;
        assert!(error.contains(bin.join("backend-mcp").to_str().ok_or("UTF8")?));
        symlink(foreign.join("backend-mcp"), bin.join("backend-mcp"))?;
        assert!(
            launchers(&current, None, &[]).is_err(),
            "foreign symlink is not this bundle"
        );
        fs::remove_file(bin.join("backend-mcp"))?;
        member(&bin, "backend-mcp")?;
        fs::set_permissions(
            bin.join("backend-locald"),
            fs::Permissions::from_mode(0o600),
        )?;
        assert!(
            launchers(&current, None, &[]).is_err(),
            "local daemon must also be executable"
        );
        Ok(())
    }

    #[test]
    fn unknown_internal_payload_is_never_registered_as_a_public_launcher() -> Result {
        let dir = tempfile::tempdir()?;
        let current = bundle(&dir.path().join("bin/.libexec"))?;
        let error = launchers(&current, None, &[])
            .err()
            .ok_or("unknown internal layout was accepted")?;
        assert!(error.contains("public MCP launcher"));
        Ok(())
    }
}
