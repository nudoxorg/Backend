//! Real isolated daemon/MCP source continuation journey; no installed service.
#![cfg(unix)]
#![allow(clippy::expect_used, clippy::panic)]
#[path = "../src/surface_matrix.rs"]
mod surface_matrix;

use serde_json::{Value, json};
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Fixture {
    root: PathBuf,
    child: Option<Child>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
fn daemon(endpoint: &Path, workspace: &Path, authority: &Path) -> Child {
    let args = surface_matrix::locald_args(endpoint, workspace, authority, Some(0), false);
    let mut child = Command::new(env!("CARGO_BIN_EXE_backend-journey-locald"))
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .env_remove("NUDOX_RUSTC")
        .spawn()
        .expect("isolated daemon");
    let until = Instant::now() + Duration::from_secs(60);
    while UnixStream::connect(endpoint).is_err() {
        assert!(child.try_wait().expect("poll").is_none(), "daemon exited");
        assert!(Instant::now() < until, "daemon startup timed out");
        std::thread::sleep(Duration::from_millis(50));
    }
    child
}
fn cli(endpoint: &Path, workspace: &Path, project: &Path, words: &[&str]) -> Value {
    let mut command = Command::new(env!("CARGO_BIN_EXE_backend-journey-cli"));
    command
        .arg("--endpoint")
        .arg(endpoint)
        .arg("--workspace")
        .arg(workspace)
        .arg("--project")
        .arg(project)
        .args(["--format", "json"])
        .env(
            "BACKEND_LOCALD_AUTHORITY_SECRET_FILE",
            workspace
                .parent()
                .expect("fixture root")
                .join("authority.secret"),
        )
        .args(words);
    let output = surface_matrix::run_bounded(command, "source range CLI", None);
    if words.first() == Some(&"search") && !output.status.success() {
        if let Ok(value) = serde_json::from_slice::<Value>(&output.stdout) {
            if value["slug"] == "wrong-basis" {
                return value;
            }
        }
    }
    assert!(
        output.status.success(),
        "CLI {words:?}, status {}: stdout={} stderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("CLI JSON")
}
fn cli_source_attempt(
    endpoint: &Path,
    workspace: &Path,
    project: &Path,
    authority: &Path,
    coordinate: &str,
    cursor: Option<&str>,
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_backend-journey-cli"));
    command
        .arg("--endpoint")
        .arg(endpoint)
        .arg("--workspace")
        .arg(workspace)
        .arg("--project")
        .arg(project)
        .args(["--format", "json", "source", coordinate, "--byte-range"])
        .env("BACKEND_LOCALD_AUTHORITY_SECRET_FILE", authority);
    if let Some(cursor) = cursor {
        command.args(["--cursor", cursor]);
    }
    surface_matrix::run_bounded(command, "source bytes CLI", None)
}

fn cli_source(
    endpoint: &Path,
    workspace: &Path,
    project: &Path,
    authority: &Path,
    coordinate: &str,
    cursor: Option<&str>,
) -> Value {
    let output = cli_source_attempt(endpoint, workspace, project, authority, coordinate, cursor);
    assert!(
        output.status.success(),
        "CLI source: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("source CLI JSON")
}

fn mcp(
    tool: &str,
    endpoint: &Path,
    workspace: &Path,
    project: &Path,
    authority: &Path,
    args: Value,
) -> Value {
    let input = [
        json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"source-range-test","version":"1"}}}),
        json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}),
        json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":tool,"arguments":args}}),
    ].into_iter().map(|value| value.to_string()).collect::<Vec<_>>().join("\n") + "\n";
    let mut command = Command::new(env!("CARGO_BIN_EXE_backend-journey-mcp"));
    command
        .arg("--endpoint")
        .arg(endpoint)
        .arg("--workspace")
        .arg(workspace)
        .arg("--project")
        .arg(project)
        .env("BACKEND_LOCALD_AUTHORITY_SECRET_FILE", authority);
    let output = surface_matrix::run_bounded(command, "source range MCP", Some(input.into_bytes()));
    assert!(
        output.status.success(),
        "MCP: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("UTF-8")
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|value| value["id"] == 9)
        .expect("source reply")
}
fn source(
    endpoint: &Path,
    workspace: &Path,
    project: &Path,
    authority: &Path,
    mut args: Value,
) -> Value {
    args["byte_range"] = json!(true);
    mcp(
        "backend.source",
        endpoint,
        workspace,
        project,
        authority,
        args,
    )
}

#[test]
fn real_source_range_reconstructs_unicode_and_rejects_changed_file() {
    let root = std::env::temp_dir().join(format!("nudox-source-range-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("test root");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
        .expect("private test root");
    let project = root.join("project");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&project).expect("project");
    std::fs::create_dir_all(&workspace).expect("workspace");
    std::fs::set_permissions(&workspace, std::fs::Permissions::from_mode(0o700))
        .expect("private workspace");
    let project = project.canonicalize().expect("canonical project");
    let body = format!(
        "function ferris() {{\r\n{}\r\n}}",
        (0..200)
            .map(|index| format!("  const value_{index} = \"é漢🦀\";\r\n"))
            .collect::<String>()
    );
    // Force the target past the first facts page and well beyond 4 KiB;
    // a page-relative anchor must be restored before it is verified.
    let prefix = (0..400)
        .map(|index| format!("function padding_{index}() {{ return {index}; }}\r\n"))
        .collect::<String>();
    let host = format!("// prefix\r\n{prefix}{body}\r\n");
    let mut file = std::fs::File::create(project.join("entry.ts")).expect("file");
    file.write_all(host.as_bytes()).expect("fixture bytes");
    drop(file);
    let authority = surface_matrix::write_authority(&root);
    let endpoint = root.join("owner.sock");
    let child = daemon(&endpoint, &workspace, &authority);
    let mut fixture = Fixture {
        root: root.clone(),
        child: Some(child),
    };
    cli(
        &endpoint,
        &workspace,
        &project,
        &["add", project.to_str().expect("path")],
    );
    let until = Instant::now() + Duration::from_secs(150);
    let coordinate = loop {
        let result = cli(&endpoint, &workspace, &project, &["search", "ferris"]);
        if let Some(value) = result["records"]
            .as_array()
            .and_then(|rows| rows.iter().find(|row| row["identity"]["name"] == "ferris"))
            .and_then(|row| row["identity"]["coordinate"].as_str())
        {
            break value.to_owned();
        }
        assert!(
            Instant::now() < until,
            "index did not find source: {result}"
        );
        std::thread::sleep(Duration::from_millis(200));
    };
    // Exercise the initially published structural row when present, before
    // waiting for the native selected publication below.
    let initial = mcp(
        "backend.source",
        &endpoint,
        &workspace,
        &project,
        &authority,
        json!({"coordinate":coordinate}),
    );
    if initial["result"]["isError"] == false
        && initial["result"]["structuredContent"]["identity"]["semantic_data"]["value"]
            == json!(backend_library::symbol_key(&coordinate).as_bytes())
    {
        let mut cursor: Option<String> = None;
        let mut raw = String::new();
        let mut moved = false;
        loop {
            let mut args = json!({"coordinate":coordinate});
            if let Some(token) = &cursor {
                args["cursor"] = json!(token);
            }
            let page = source(&endpoint, &workspace, &project, &authority, args);
            if page["result"]["isError"] == true
                && page["result"]["structuredContent"]["slug"] == "cursor-mismatch"
            {
                moved = true;
                break;
            }
            assert_eq!(page["result"]["isError"], false, "structural range: {page}");
            raw.push_str(
                page["result"]["structuredContent"]["source"]["text"]
                    .as_str()
                    .expect("structural raw text"),
            );
            cursor = page["result"]["structuredContent"]["nextCursor"]
                .as_str()
                .map(str::to_owned);
            if cursor.is_none() {
                break;
            }
        }
        if !moved {
            assert_eq!(raw.as_bytes(), body.as_bytes(), "structural exact bytes");
        }
    }
    // Default source must remain readable for compiler-selected rows and
    // explicitly report the retained excerpt's truncation.
    let deadline = Instant::now() + Duration::from_secs(150);
    let ordinary = loop {
        let current = mcp(
            "backend.source",
            &endpoint,
            &workspace,
            &project,
            &authority,
            json!({"coordinate":coordinate}),
        );
        if current["result"]["isError"] == false
            && current["result"]["structuredContent"]["identity"]["semantic_data"]["value"]
                != json!(backend_library::symbol_key(&coordinate).as_bytes())
        {
            break current;
        }
        assert!(
            Instant::now() < deadline,
            "native row was not published: {current}"
        );
        std::thread::sleep(Duration::from_millis(200));
    };
    assert_eq!(
        ordinary["result"]["isError"], false,
        "default native source: {ordinary}"
    );
    assert_eq!(
        ordinary["result"]["structuredContent"]["source"]["extent"], "truncated",
        "bounded native source: {ordinary}"
    );
    assert!(
        ordinary["result"]["structuredContent"]["identity"]["semantic_data"].is_object(),
        "exact selected native identity: {ordinary}"
    );
    assert_ne!(
        ordinary["result"]["structuredContent"]["identity"]["semantic_data"]["value"],
        json!(backend_library::symbol_key(&coordinate).as_bytes()),
        "control must exercise a compiler-owned row, not a structural coordinate hash"
    );
    let mut cursor: Option<String> = None;
    let mut raw = String::new();
    let mut mcp_pages = 0_usize;
    let mut mcp_max_json = 0_usize;
    let mut mcp_max_cursor = 0_usize;
    let mcp_started = Instant::now();
    loop {
        let mut args = json!({"coordinate":coordinate});
        if let Some(token) = &cursor {
            args["cursor"] = json!(token);
        }
        let reply = source(&endpoint, &workspace, &project, &authority, args);
        assert_eq!(reply["result"]["isError"], false, "{reply}");
        mcp_pages += 1;
        mcp_max_json = mcp_max_json.max(serde_json::to_vec(&reply).expect("JSON envelope").len());
        assert!(
            reply["result"]["structuredContent"]["source"]["owner_start"]
                .as_u64()
                .is_some_and(|start| start > 4096),
            "owner must use restored host offsets: {reply}"
        );
        raw.push_str(
            reply["result"]["structuredContent"]["source"]["text"]
                .as_str()
                .expect("raw text"),
        );
        cursor = reply["result"]["structuredContent"]["nextCursor"]
            .as_str()
            .map(str::to_owned);
        mcp_max_cursor = mcp_max_cursor.max(cursor.as_ref().map_or(0, String::len));
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(raw.as_bytes(), body.as_bytes());
    eprintln!(
        "native MCP pages={mcp_pages} source_bytes={} max_json_bytes={mcp_max_json} max_cursor_bytes={mcp_max_cursor} elapsed_ms={}",
        raw.len(),
        mcp_started.elapsed().as_millis()
    );
    let mut cli_cursor: Option<String> = None;
    let mut cli_first_cursor: Option<String> = None;
    let mut cli_first_end: Option<u64> = None;
    let mut cli_raw = String::new();
    let mut cli_pages = 0_usize;
    let mut cli_max_cursor = 0_usize;
    let cli_started = Instant::now();
    loop {
        let page = cli_source(
            &endpoint,
            &workspace,
            &project,
            &authority,
            &coordinate,
            cli_cursor.as_deref(),
        );
        let source = &page["declaration_source_range"];
        cli_pages += 1;
        cli_raw.push_str(source["source"]["text"].as_str().expect("CLI raw text"));
        cli_cursor = source["nextCursor"].as_str().map(str::to_owned);
        cli_max_cursor = cli_max_cursor.max(cli_cursor.as_ref().map_or(0, String::len));
        if cli_first_cursor.is_none() {
            cli_first_cursor.clone_from(&cli_cursor);
            cli_first_end = source["source"]["end"].as_u64();
        }
        if cli_cursor.is_none() {
            break;
        }
    }
    assert_eq!(
        cli_raw.as_bytes(),
        body.as_bytes(),
        "CLI reconstructs exact owner bytes"
    );
    eprintln!(
        "native CLI pages={cli_pages} source_bytes={} max_cursor_bytes={cli_max_cursor} elapsed_ms={}",
        cli_raw.len(),
        cli_started.elapsed().as_millis()
    );
    let first = source(
        &endpoint,
        &workspace,
        &project,
        &authority,
        json!({"coordinate":coordinate}),
    );
    let full = source(
        &endpoint,
        &workspace,
        &project,
        &authority,
        json!({"coordinate":coordinate,"detail":"full"}),
    );
    assert_eq!(
        full["result"]["structuredContent"]["source"]["text"],
        first["result"]["structuredContent"]["source"]["text"],
        "full and default source bytes"
    );
    let next = first["result"]["structuredContent"]["nextCursor"]
        .as_str()
        .expect("first page cursor")
        .to_owned();
    let mut child = fixture.child.take().expect("daemon");
    child.kill().expect("stop");
    child.wait().expect("wait");
    fixture.child = Some(daemon(&endpoint, &workspace, &authority));
    let resumed = source(
        &endpoint,
        &workspace,
        &project,
        &authority,
        json!({"coordinate":coordinate,"cursor":next}),
    );
    assert_eq!(resumed["result"]["isError"], false, "restart: {resumed}");
    let cli_token = cli_first_cursor.as_deref().expect("CLI continuation");
    let resumed_cli = cli_source(
        &endpoint,
        &workspace,
        &project,
        &authority,
        &coordinate,
        Some(cli_token),
    );
    assert_eq!(
        resumed_cli["declaration_source_range"]["source"]["start"],
        cli_first_end.expect("first CLI page end"),
        "CLI owner restart: {resumed_cli}"
    );
    std::fs::write(
        project.join("entry.ts"),
        host.replace("const value", "const other"),
    )
    .expect("edit");
    let stale_cli = cli_source_attempt(
        &endpoint,
        &workspace,
        &project,
        &authority,
        &coordinate,
        Some(cli_token),
    );
    assert!(
        !stale_cli.status.success(),
        "old CLI cursor accepted changed source"
    );
    let stale = source(
        &endpoint,
        &workspace,
        &project,
        &authority,
        json!({"coordinate":coordinate,"cursor":next}),
    );
    assert!(
        stale.get("error").is_some() || stale["result"]["isError"] == true,
        "stale: {stale}"
    );
    for tool in ["backend.read", "backend.document", "backend.source"] {
        let response = mcp(
            tool,
            &endpoint,
            &workspace,
            &project,
            &authority,
            json!({"coordinate":coordinate}),
        );
        let encoded = response.to_string();
        if tool == "backend.document" {
            assert!(
                response["result"]["structuredContent"]["source"].is_null(),
                "stale document returned a source body: {response}"
            );
        } else {
            assert!(
                !encoded.contains("const value"),
                "old body returned by {tool}: {response}"
            );
        }
        assert!(
            response.get("error").is_some()
                || response["result"]["isError"] == true
                || encoded.contains("stale")
                || encoded.contains("source_fault"),
            "freshness not reported by {tool}: {response}"
        );
    }
    let mut reindex = backend_client::Session::connect(&endpoint).expect("reindex session");
    reindex
        .index(&project.to_string_lossy())
        .expect("submit fresh reindex");
    let until = Instant::now() + Duration::from_secs(150);
    loop {
        let current = source(
            &endpoint,
            &workspace,
            &project,
            &authority,
            json!({"coordinate":coordinate}),
        );
        if current["result"]["isError"] == false {
            let first = current["result"]["structuredContent"]["source"]["text"]
                .as_str()
                .expect("new source text");
            assert!(
                first.contains("const other"),
                "reindex returned old body: {current}"
            );
            break;
        }
        assert!(
            Instant::now() < until,
            "reindex did not publish new source: {current}"
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    let obsolete = source(
        &endpoint,
        &workspace,
        &project,
        &authority,
        json!({"coordinate":coordinate,"cursor":next}),
    );
    assert!(
        obsolete.get("error").is_some() || obsolete["result"]["isError"] == true,
        "old cursor survived reindex: {obsolete}"
    );
    let obsolete_cli = cli_source_attempt(
        &endpoint,
        &workspace,
        &project,
        &authority,
        &coordinate,
        Some(cli_token),
    );
    assert!(
        !obsolete_cli.status.success(),
        "old CLI cursor survived reindex"
    );
    std::fs::remove_file(project.join("entry.ts")).expect("remove source");
    for tool in ["backend.read", "backend.document", "backend.source"] {
        let response = mcp(
            tool,
            &endpoint,
            &workspace,
            &project,
            &authority,
            json!({"coordinate":coordinate}),
        );
        assert!(
            response.get("error").is_some() || response["result"]["isError"] == true,
            "unverifiable source appeared current for {tool}: {response}"
        );
    }
}

#[test]
fn real_structural_source_reconstructs_stable_unicode_bytes() {
    let root = std::env::temp_dir().join(format!("nudox-structural-range-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("test root");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))
        .expect("private test root");
    let project = root.join("project");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&project).expect("project");
    std::fs::create_dir_all(&workspace).expect("workspace");
    std::fs::set_permissions(&workspace, std::fs::Permissions::from_mode(0o700))
        .expect("private workspace");
    let project = project.canonicalize().expect("canonical project");
    // Duplicate lexical bindings intentionally prevent semantic admission,
    // leaving the real indexed structural selection as the stable control.
    let body = format!(
        "function structural() {{\r\n{}\r\n}}",
        "  const repeated = \"é漢🦀\";\r\n".repeat(200)
    );
    let prefix = (0..400)
        .map(|index| format!("function padding_{index}() {{ return {index}; }}\r\n"))
        .collect::<String>();
    std::fs::write(project.join("entry.ts"), format!("{prefix}{body}\r\n")).expect("fixture bytes");
    let authority = surface_matrix::write_authority(&root);
    let endpoint = root.join("owner.sock");
    let child = daemon(&endpoint, &workspace, &authority);
    let _fixture = Fixture {
        root: root.clone(),
        child: Some(child),
    };
    cli(
        &endpoint,
        &workspace,
        &project,
        &["add", project.to_str().expect("path")],
    );
    let deadline = Instant::now() + Duration::from_secs(150);
    let coordinate = loop {
        let result = cli(&endpoint, &workspace, &project, &["search", "structural"]);
        if let Some(value) = result["records"]
            .as_array()
            .and_then(|rows| {
                rows.iter()
                    .find(|row| row["identity"]["name"] == "structural")
            })
            .and_then(|row| row["identity"]["coordinate"].as_str())
        {
            break value.to_owned();
        }
        assert!(
            Instant::now() < deadline,
            "structural index did not publish: {result}"
        );
        std::thread::sleep(Duration::from_millis(200));
    };
    let mut health = backend_client::Session::connect(&endpoint).expect("health session");
    let mut previous = None;
    let mut unchanged_since = Instant::now();
    loop {
        let root = health.health().expect("owner health").revision().root();
        if previous != Some(root) {
            previous = Some(root);
            unchanged_since = Instant::now();
        } else if unchanged_since.elapsed() >= Duration::from_secs(3) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "structural publication never settled"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let ordinary = mcp(
        "backend.source",
        &endpoint,
        &workspace,
        &project,
        &authority,
        json!({"coordinate":coordinate}),
    );
    assert_eq!(
        ordinary["result"]["isError"], false,
        "default structural source: {ordinary}"
    );
    assert_eq!(
        ordinary["result"]["structuredContent"]["identity"]["semantic_data"]["value"],
        json!(backend_library::symbol_key(&coordinate).as_bytes()),
        "structural selected identity: {ordinary}"
    );
    let mut cursor: Option<String> = None;
    let mut raw = String::new();
    loop {
        let mut args = json!({"coordinate":coordinate});
        if let Some(token) = &cursor {
            args["cursor"] = json!(token);
        }
        let page = source(&endpoint, &workspace, &project, &authority, args);
        assert_eq!(page["result"]["isError"], false, "structural page: {page}");
        assert!(
            page["result"]["structuredContent"]["source"]["owner_start"]
                .as_u64()
                .is_some_and(|start| start > 4096)
        );
        raw.push_str(
            page["result"]["structuredContent"]["source"]["text"]
                .as_str()
                .expect("raw source"),
        );
        cursor = page["result"]["structuredContent"]["nextCursor"]
            .as_str()
            .map(str::to_owned);
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(raw.as_bytes(), body.as_bytes(), "structural owner bytes");
}

#[test]
fn real_native_duplicate_names_reject_crossed_selected_identity() {
    let root = std::env::temp_dir().join(format!("nudox-duplicate-range-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("test root");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("private root");
    let project = root.join("project");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&project).expect("project");
    std::fs::create_dir_all(&workspace).expect("workspace");
    std::fs::set_permissions(&workspace, std::fs::Permissions::from_mode(0o700))
        .expect("private workspace");
    let project = project.canonicalize().expect("canonical project");
    std::fs::write(
        project.join("a.ts"),
        "export function duplicate() { return 'first'; }\n",
    )
    .expect("first source");
    std::fs::write(
        project.join("b.ts"),
        "export function duplicate() { return 'second'; }\n",
    )
    .expect("second source");
    let authority = surface_matrix::write_authority(&root);
    let endpoint = root.join("owner.sock");
    let child = daemon(&endpoint, &workspace, &authority);
    let _fixture = Fixture {
        root,
        child: Some(child),
    };
    cli(
        &endpoint,
        &workspace,
        &project,
        &["add", project.to_str().expect("path")],
    );
    let deadline = Instant::now() + Duration::from_secs(150);
    let coordinates = loop {
        let result = cli(&endpoint, &workspace, &project, &["search", "duplicate"]);
        let mut values = result["records"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|row| row["identity"]["name"] == "duplicate")
            .filter_map(|row| row["identity"]["coordinate"].as_str().map(str::to_owned))
            .collect::<Vec<_>>();
        values.sort();
        values.dedup();
        if values.len() == 2 {
            break values;
        }
        assert!(
            Instant::now() < deadline,
            "distinct same-name results not published: {result}"
        );
        std::thread::sleep(Duration::from_millis(200));
    };
    let documents = loop {
        let docs = coordinates
            .iter()
            .map(|coordinate| {
                mcp(
                    "backend.source",
                    &endpoint,
                    &workspace,
                    &project,
                    &authority,
                    json!({"coordinate":coordinate}),
                )
            })
            .collect::<Vec<_>>();
        let native = docs.iter().zip(&coordinates).all(|(doc, coordinate)| {
            doc["result"]["isError"] == false
                && doc["result"]["structuredContent"]["identity"]["semantic_data"]["value"]
                    != json!(backend_library::symbol_key(coordinate).as_bytes())
        });
        if native {
            break docs;
        }
        assert!(
            Instant::now() < deadline,
            "native duplicate rows not published: {docs:?}"
        );
        std::thread::sleep(Duration::from_millis(200));
    };
    for (index, coordinate) in coordinates.iter().enumerate() {
        let own =
            documents[index]["result"]["structuredContent"]["identity"]["semantic_data"].clone();
        let other =
            documents[1 - index]["result"]["structuredContent"]["identity"]["semantic_data"]
                .clone();
        let page = source(
            &endpoint,
            &workspace,
            &project,
            &authority,
            json!({"coordinate":coordinate,"selected_symbol":own}),
        );
        assert_eq!(
            page["result"]["isError"], false,
            "own native binding: {page}"
        );
        let text = page["result"]["structuredContent"]["source"]["text"]
            .as_str()
            .expect("source text");
        assert!(
            text.contains(if coordinate.contains("a.ts") {
                "first"
            } else {
                "second"
            }),
            "wrong source: {page}"
        );
        let crossed = source(
            &endpoint,
            &workspace,
            &project,
            &authority,
            json!({"coordinate":coordinate,"selected_symbol":other}),
        );
        assert!(
            crossed.get("error").is_some() || crossed["result"]["isError"] == true,
            "crossed same-name selection accepted: {crossed}"
        );
    }
}

fn cli_selected_source_attempt(
    endpoint: &Path,
    workspace: &Path,
    project: &Path,
    authority: &Path,
    coordinate: &str,
    selected_id: &str,
    cursor: Option<&str>,
) -> std::process::Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_backend-journey-cli"));
    command
        .arg("--endpoint")
        .arg(endpoint)
        .arg("--workspace")
        .arg(workspace)
        .arg("--project")
        .arg(project)
        .args([
            "--format",
            "json",
            "source",
            coordinate,
            "--byte-range",
            "--selected-id",
            selected_id,
        ])
        .env("BACKEND_LOCALD_AUTHORITY_SECRET_FILE", authority);
    if let Some(cursor) = cursor {
        command.args(["--cursor", cursor]);
    }
    surface_matrix::run_bounded(command, "selected source bytes CLI", None)
}

#[test]
fn real_same_file_equal_owner_cursors_are_declaration_bound_after_restart() {
    let root = std::env::temp_dir().join(format!("nudox-same-file-cursor-{}", std::process::id()));
    std::fs::create_dir_all(&root).expect("test root");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700)).expect("private root");
    let project = root.join("project");
    let workspace = root.join("workspace");
    std::fs::create_dir_all(&project).expect("project");
    std::fs::create_dir_all(&workspace).expect("workspace");
    std::fs::set_permissions(&workspace, std::fs::Permissions::from_mode(0o700))
        .expect("private workspace");
    let project = project.canonicalize().expect("canonical project");
    let lines = (0..200)
        .map(|index| format!("  const value_{index} = \"é漢🦀\";\r\n"))
        .collect::<String>();
    let bodies = ["alpha", "bravo"].map(|name| format!("function {name}() {{\r\n{lines}}}"));
    assert_eq!(bodies[0].len(), bodies[1].len(), "equal owner byte lengths");
    assert!(bodies[0].len() > 2048, "both owners require continuation");
    std::fs::write(
        project.join("same.ts"),
        format!("{}\r\n{}\r\n", bodies[0], bodies[1]),
    )
    .expect("same-file source");
    let authority = surface_matrix::write_authority(&root);
    let endpoint = root.join("owner.sock");
    let child = daemon(&endpoint, &workspace, &authority);
    let mut fixture = Fixture {
        root,
        child: Some(child),
    };
    cli(
        &endpoint,
        &workspace,
        &project,
        &["add", project.to_str().expect("path")],
    );
    let deadline = Instant::now() + Duration::from_secs(150);
    let mut coordinates = Vec::new();
    for name in ["alpha", "bravo"] {
        let coordinate = loop {
            let result = cli(&endpoint, &workspace, &project, &["search", name]);
            if let Some(value) = result["records"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|row| row["identity"]["name"] == name)
                .and_then(|row| row["identity"]["coordinate"].as_str())
            {
                break value.to_owned();
            }
            assert!(
                Instant::now() < deadline,
                "declaration {name} missing: {result}"
            );
            std::thread::sleep(Duration::from_millis(200));
        };
        coordinates.push(coordinate);
    }
    let documents = loop {
        let docs = coordinates
            .iter()
            .map(|coordinate| {
                mcp(
                    "backend.source",
                    &endpoint,
                    &workspace,
                    &project,
                    &authority,
                    json!({"coordinate":coordinate}),
                )
            })
            .collect::<Vec<_>>();
        if docs.iter().zip(&coordinates).all(|(doc, coordinate)| {
            doc["result"]["isError"] == false
                && doc["result"]["structuredContent"]["identity"]["semantic_data"]["value"]
                    != json!(backend_library::symbol_key(coordinate).as_bytes())
        }) {
            break docs;
        }
        assert!(
            Instant::now() < deadline,
            "native declarations not published: {docs:?}"
        );
        std::thread::sleep(Duration::from_millis(200));
    };
    let selected = documents
        .iter()
        .map(|doc| doc["result"]["structuredContent"]["identity"]["semantic_data"].clone())
        .collect::<Vec<_>>();
    assert_ne!(selected[0], selected[1], "distinct native identities");
    let mut health = backend_client::Session::connect(&endpoint).expect("health session");
    let mut previous = None;
    let mut unchanged_since = Instant::now();
    loop {
        let revision = health.health().expect("owner health").revision().root();
        if previous != Some(revision) {
            previous = Some(revision);
            unchanged_since = Instant::now();
        } else if unchanged_since.elapsed() >= Duration::from_secs(2) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "same-file publication never settled"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let mut mcp_first = Vec::new();
    let mut cli_first = Vec::new();
    let mut cli_ids = Vec::new();
    for index in 0..2 {
        let page = source(
            &endpoint,
            &workspace,
            &project,
            &authority,
            json!({"coordinate":coordinates[index], "selected_symbol":selected[index]}),
        );
        assert_eq!(page["result"]["isError"], false, "MCP first page: {page}");
        let human = page["result"]["content"][0]["text"]
            .as_str()
            .expect("MCP source text");
        assert!(human.contains("Source bytes match the indexed capture"));
        assert!(human.contains("ignore-policy stability is not established"));
        let data = &page["result"]["structuredContent"];
        assert_eq!(data["source"]["scope"], "native_primary_declaration");
        let id = data["selected_symbol"]
            .as_str()
            .expect("selected id")
            .to_owned();
        assert_ne!(
            id,
            backend_library::encode_id(backend_library::symbol_key(&coordinates[index]).as_bytes()),
            "native selected id"
        );
        let output = cli_selected_source_attempt(
            &endpoint,
            &workspace,
            &project,
            &authority,
            &coordinates[index],
            &id,
            None,
        );
        assert!(
            output.status.success(),
            "CLI first: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        let cli_page: Value = serde_json::from_slice(&output.stdout).expect("CLI page JSON");
        let cp = &cli_page["declaration_source_range"];
        assert_eq!(cp["source"]["scope"], "native_primary_declaration");
        assert_eq!(data["revision"], cp["revision"]);
        assert_eq!(data["selected_symbol"], cp["selected_symbol"]);
        assert_eq!(
            data["source"]["source_identity"],
            cp["source"]["source_identity"]
        );
        assert_eq!(data["source"]["text"], cp["source"]["text"]);
        assert!(data["nextCursor"].as_str().is_some() && cp["nextCursor"].as_str().is_some());
        mcp_first.push(page);
        cli_first.push(cli_page);
        cli_ids.push(id);
    }
    let a = &mcp_first[0]["result"]["structuredContent"];
    let b = &mcp_first[1]["result"]["structuredContent"];
    assert_eq!(a["revision"], b["revision"], "same immutable view");
    assert_eq!(
        a["source"]["source_identity"], b["source"]["source_identity"],
        "same file content identity"
    );
    assert_eq!(
        a["source"]["total"], b["source"]["total"],
        "equal owner length"
    );
    assert_eq!(a["source"]["total"].as_u64(), Some(bodies[0].len() as u64));
    assert_eq!(
        a["source"]["end"], b["source"]["end"],
        "same continuation offset"
    );
    let offset = a["source"]["end"].as_u64().expect("offset") as usize;
    assert!(offset > 0 && offset < bodies[0].len());
    assert!(
        bodies.iter().all(|body| body.is_char_boundary(offset)),
        "offset is valid in both declarations"
    );
    assert_ne!(a["selected_symbol"], b["selected_symbol"]);
    assert_ne!(
        a["source"]["owner_start"], b["source"]["owner_start"],
        "different owner extents"
    );
    let mut child = fixture.child.take().expect("owner");
    child.kill().expect("stop owner");
    child.wait().expect("wait owner");
    fixture.child = Some(daemon(&endpoint, &workspace, &authority));
    let mut mcp_resume_pages = 0_usize;
    let mut cli_resume_pages = 0_usize;
    let mut mcp_resume_elapsed = Duration::ZERO;
    let mut cli_resume_elapsed = Duration::ZERO;
    for index in 0..2 {
        let other = 1 - index;
        let first = &mcp_first[index]["result"]["structuredContent"];
        let mcp_cursor = first["nextCursor"].as_str().expect("MCP cursor");
        let wrong_coordinate = source(
            &endpoint,
            &workspace,
            &project,
            &authority,
            json!({"coordinate":coordinates[other], "selected_symbol":selected[other], "cursor":mcp_cursor}),
        );
        assert!(
            wrong_coordinate.get("error").is_some()
                || wrong_coordinate["result"]["isError"] == true,
            "cross-declaration MCP cursor accepted: {wrong_coordinate}"
        );
        assert!(
            wrong_coordinate
                .to_string()
                .to_ascii_lowercase()
                .contains("cursor"),
            "missing cursor refusal: {wrong_coordinate}"
        );
        let wrong_selected = source(
            &endpoint,
            &workspace,
            &project,
            &authority,
            json!({"coordinate":coordinates[index], "selected_symbol":selected[other], "cursor":mcp_cursor}),
        );
        assert!(
            wrong_selected.get("error").is_some() || wrong_selected["result"]["isError"] == true,
            "crossed MCP selected identity accepted: {wrong_selected}"
        );
        assert!(
            wrong_selected
                .to_string()
                .to_ascii_lowercase()
                .contains("cursor"),
            "missing cursor refusal: {wrong_selected}"
        );
        let cp = &cli_first[index]["declaration_source_range"];
        let cli_cursor = cp["nextCursor"].as_str().expect("CLI cursor");
        let wrong_cli = cli_selected_source_attempt(
            &endpoint,
            &workspace,
            &project,
            &authority,
            &coordinates[other],
            &cli_ids[other],
            Some(cli_cursor),
        );
        assert!(
            !wrong_cli.status.success(),
            "cross-declaration CLI cursor accepted: {}",
            String::from_utf8_lossy(&wrong_cli.stdout)
        );
        let refusal: Value = serde_json::from_slice(&wrong_cli.stdout).expect("CLI fault JSON");
        assert_eq!(
            refusal["slug"], "usage",
            "cross-declaration cursor must be rejected: {refusal}"
        );
        assert_eq!(refusal["operand"], "cursor", "{refusal}");
        let wrong_cli_id = cli_selected_source_attempt(
            &endpoint,
            &workspace,
            &project,
            &authority,
            &coordinates[index],
            &cli_ids[other],
            Some(cli_cursor),
        );
        assert!(
            !wrong_cli_id.status.success(),
            "crossed CLI selected identity accepted: {}",
            String::from_utf8_lossy(&wrong_cli_id.stdout)
        );
        let refusal: Value = serde_json::from_slice(&wrong_cli_id.stdout).expect("CLI fault JSON");
        assert_eq!(
            refusal["slug"], "usage",
            "crossed selected cursor must be rejected: {refusal}"
        );
        assert_eq!(refusal["operand"], "cursor", "{refusal}");
        let mut mcp_raw = first["source"]["text"]
            .as_str()
            .expect("first MCP bytes")
            .to_owned();
        let mut next = Some(mcp_cursor.to_owned());
        while let Some(cursor) = next {
            let started = Instant::now();
            let page = source(
                &endpoint,
                &workspace,
                &project,
                &authority,
                json!({"coordinate":coordinates[index], "selected_symbol":selected[index], "cursor":cursor}),
            );
            mcp_resume_elapsed += started.elapsed();
            mcp_resume_pages += 1;
            assert_eq!(
                page["result"]["isError"], false,
                "MCP restart continuation: {page}"
            );
            let data = &page["result"]["structuredContent"];
            assert_eq!(data["source"]["scope"], "native_primary_declaration");
            mcp_raw.push_str(data["source"]["text"].as_str().expect("MCP bytes"));
            next = data["nextCursor"].as_str().map(str::to_owned);
        }
        assert_eq!(
            mcp_raw.as_bytes(),
            bodies[index].as_bytes(),
            "MCP exact owner bytes"
        );
        let mut cli_raw = cp["source"]["text"]
            .as_str()
            .expect("first CLI bytes")
            .to_owned();
        let mut next = Some(cli_cursor.to_owned());
        while let Some(cursor) = next {
            let started = Instant::now();
            let output = cli_selected_source_attempt(
                &endpoint,
                &workspace,
                &project,
                &authority,
                &coordinates[index],
                &cli_ids[index],
                Some(&cursor),
            );
            cli_resume_elapsed += started.elapsed();
            cli_resume_pages += 1;
            assert!(
                output.status.success(),
                "CLI restart continuation: {}",
                String::from_utf8_lossy(&output.stdout)
            );
            let page: Value =
                serde_json::from_slice(&output.stdout).expect("CLI continuation JSON");
            let data = &page["declaration_source_range"];
            assert_eq!(data["source"]["scope"], "native_primary_declaration");
            cli_raw.push_str(data["source"]["text"].as_str().expect("CLI bytes"));
            next = data["nextCursor"].as_str().map(str::to_owned);
        }
        assert_eq!(
            cli_raw.as_bytes(),
            bodies[index].as_bytes(),
            "CLI exact owner bytes"
        );
    }
    // Retain the old revision and cursor while changing the host file by the
    // same number of bytes. A cached EOF or a matching length cannot prove
    // freshness; neither surface may resume the old source after this edit.
    let original_file = format!("{}\r\n{}\r\n", bodies[0], bodies[1]);
    let edited_file = original_file.replacen("value_0", "value_X", 1);
    assert_ne!(edited_file, original_file);
    assert_eq!(edited_file.len(), original_file.len());
    std::fs::write(project.join("same.ts"), edited_file).expect("same-size source edit");
    let old_mcp = &mcp_first[0]["result"]["structuredContent"];
    let stale_mcp = source(
        &endpoint,
        &workspace,
        &project,
        &authority,
        json!({"coordinate":coordinates[0], "selected_symbol":selected[0],
            "cursor":old_mcp["nextCursor"].as_str().expect("old MCP cursor")}),
    );
    assert!(
        stale_mcp.get("error").is_some() || stale_mcp["result"]["isError"] == true,
        "MCP resumed changed source: {stale_mcp}"
    );
    let old_cli = &cli_first[0]["declaration_source_range"];
    let stale_cli = cli_selected_source_attempt(
        &endpoint,
        &workspace,
        &project,
        &authority,
        &coordinates[0],
        &cli_ids[0],
        Some(old_cli["nextCursor"].as_str().expect("old CLI cursor")),
    );
    assert!(
        !stale_cli.status.success(),
        "CLI resumed changed source: {}",
        String::from_utf8_lossy(&stale_cli.stdout)
    );
    for tool in ["backend.read", "backend.document", "backend.source"] {
        let reply = mcp(
            tool,
            &endpoint,
            &workspace,
            &project,
            &authority,
            json!({"coordinate":coordinates[0], "selected_symbol":selected[0]}),
        );
        let encoded = reply.to_string();
        assert!(
            !encoded.contains("const value_0"),
            "old source body appeared in {tool}: {reply}"
        );
        if tool == "backend.document" {
            assert!(
                reply["result"]["structuredContent"]["source"].is_null(),
                "stale document returned a source body: {reply}"
            );
        }
        assert!(
            reply.get("error").is_some()
                || reply["result"]["isError"] == true
                || encoded.contains("stale")
                || encoded.contains("source_fault"),
            "same-size edit appeared current for {tool}: {reply}"
        );
    }
    // Replacing the indexed file with a symlink outside the admitted project
    // must be refused even when the outside bytes match the indexed identity.
    // This checks confinement; it makes no claim about persistent inodes.
    let outside = fixture.root.join("outside.ts");
    std::fs::write(&outside, original_file).expect("outside source bytes");
    std::fs::remove_file(project.join("same.ts")).expect("remove edited source");
    std::os::unix::fs::symlink(&outside, project.join("same.ts")).expect("outside source symlink");
    let escaped = source(
        &endpoint,
        &workspace,
        &project,
        &authority,
        json!({"coordinate":coordinates[0], "selected_symbol":selected[0],
            "cursor":old_mcp["nextCursor"].as_str().expect("old MCP cursor")}),
    );
    assert!(
        escaped.get("error").is_some() || escaped["result"]["isError"] == true,
        "MCP followed source outside the admitted project: {escaped}"
    );
    let escaped_cli = cli_selected_source_attempt(
        &endpoint,
        &workspace,
        &project,
        &authority,
        &coordinates[0],
        &cli_ids[0],
        Some(old_cli["nextCursor"].as_str().expect("old CLI cursor")),
    );
    assert!(
        !escaped_cli.status.success(),
        "CLI followed source outside the admitted project: {}",
        String::from_utf8_lossy(&escaped_cli.stdout)
    );
    eprintln!(
        "same-file native owners=2 owner_bytes={} resumed_mcp_pages={mcp_resume_pages} resumed_mcp_ms={} resumed_cli_pages={cli_resume_pages} resumed_cli_ms={}",
        bodies[0].len(),
        mcp_resume_elapsed.as_millis(),
        cli_resume_elapsed.as_millis()
    );
}
