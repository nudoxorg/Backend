//! Synthetic real-worker owner served to actual CLI/MCP child processes.
use super::*;
use crate::builtin::BuiltinModelError;
use crate::builtin::commands::index::{
    IndexScanFailure, PreparedSourceScan, capture_index_scan, prepare_source_scan, run_index_scan,
};
use crate::listener::{ListenerConfig, ListenerShutdown, UnixListenerService};
use crate::service::LocaldService;
use std::io::Write;
use std::process::{Command as Process, Output, Stdio};
use std::sync::Mutex;
use std::time::Instant;

struct ProcessListener {
    stop: ListenerShutdown,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl Drop for ProcessListener {
    fn drop(&mut self) {
        self.stop.request();
        if let Some(thread) = self.thread.take() {
            thread.join().expect("listener joined");
        }
    }
}
fn listen(
    fixture: &mut AdapterFixture,
    endpoint: &Path,
) -> Result<ProcessListener, Box<dyn std::error::Error>> {
    let commands = Arc::new(Mutex::new(fixture.adapter.take().ok_or("adapter missing")?));
    let command_owner = Arc::clone(&commands);
    let owner = fixture
        .daemon
        .take()
        .ok_or("daemon missing")?
        .into_owner(move |daemon: &mut ProductDaemon, body: &[u8]| {
            command_owner
                .lock()
                .expect("adapter lock")
                .execute(daemon, body)
                .map_err(|error| error.to_string())
        })
        .with_deferred_commands(Box::new(crate::builtin::DeferredBuiltinCommands(commands)));
    let mut config = ListenerConfig::new(endpoint);
    config.poll_interval = Duration::from_millis(1);
    config.owner_reply_timeout = Duration::from_secs(20);
    config.idle_timeout = None;
    let mut listener = UnixListenerService::bind(
        LocaldService::new(owner, crate::FrameLimits::default())?,
        config,
    )?;
    let stop = listener.shutdown_handle();
    let thread = std::thread::spawn(move || {
        listener.run().expect("real owner listener");
    });
    Ok(ProcessListener {
        stop,
        thread: Some(thread),
    })
}
fn child_output(
    binary: &Path,
    args: &[&str],
    input: Option<&str>,
) -> Result<Output, Box<dyn std::error::Error>> {
    let mut child = Process::new(binary)
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    if let Some(input) = input {
        child
            .stdin
            .take()
            .ok_or("missing stdin")?
            .write_all(input.as_bytes())?;
    }
    let started = Instant::now();
    while child.try_wait()?.is_none() {
        if started.elapsed() >= Duration::from_secs(30) {
            child.kill()?;
            let _ = child.wait();
            return Err("public fixture child timed out".into());
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Ok(child.wait_with_output()?)
}
fn checked_json(output: Output) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    if !output.status.success() {
        return Err(format!(
            "fixture child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into());
    }
    Ok(serde_json::from_slice(&output.stdout)?)
}

#[test]
#[ignore = "requires explicit NUDOX_TEST_STYLO_WORKER, NUDOX_TEST_CLI_BIN and NUDOX_TEST_MCP_BIN fixture artifacts"]
fn css_actual_cli_mcp_and_cold_owner_reopen() -> Result<(), Box<dyn std::error::Error>> {
    let cli = PathBuf::from(std::env::var_os("NUDOX_TEST_CLI_BIN").ok_or("CLI binary missing")?);
    let mcp = PathBuf::from(std::env::var_os("NUDOX_TEST_MCP_BIN").ok_or("MCP binary missing")?);
    let worker =
        PathBuf::from(std::env::var_os("NUDOX_TEST_STYLO_WORKER").ok_or("Stylo worker missing")?);
    let mut fixture = AdapterFixture::open(TempTree::new(), false);
    let (package, label) = fixture.add_target();
    fs::write(
        Path::new(&label).join("site.css"),
        ".hud-menu { --tone: red; } .hud-menu { color: blue }",
    )?;
    let artifact = backend_compile::ToolchainArtifact::from_path(&worker, Vec::new())?;
    let permit = Arc::new(
        crate::builtin::stylo_worker::StyloWorkerPermit::from_admitted(
            worker,
            artifact,
            backend_frontend_stylo::support_fingerprint(),
        )
        .map_err(|error| format!("{error:?}"))?,
    );
    let (adapter, daemon) = fixture.parts();
    let mut work = capture_index_scan(
        daemon,
        package,
        &label,
        Path::new(&label),
        None,
        1700,
        CompileExecutionIntent::Interactive,
        false,
        Arc::new(AtomicBool::new(false)),
        adapter.compiler.python_interpreter_admission(),
    )?;
    work.stylo_worker = Some(permit);
    let scan = run_index_scan(work).map_err(|error| match error {
        IndexScanFailure::Cancelled => BuiltinModelError("cancelled".into()),
        IndexScanFailure::Refused(error) => error,
    })?;
    let base = daemon.engine().daemon().owner().snapshot();
    let PreparedSourceScan::Ready(selection) = prepare_source_scan(
        &base,
        scan,
        &adapter.compiler,
        &mut adapter.semantic_authority,
        None,
        None,
        &mut || {},
    )?
    else {
        return Err("unexpected compiler capture".into());
    };
    super::super::commit_builtin_intent(
        daemon,
        1701,
        &selection.intent.ok_or("no native intent")?,
    )?;
    let workspace = fixture.root.0.join("workspace").canonicalize()?;
    let endpoint = crate::test_support::socket_path("stylo-public-process");
    let workspace = workspace
        .to_str()
        .ok_or("workspace is not UTF-8")?
        .to_owned();
    let endpoint_text = endpoint.to_str().ok_or("endpoint is not UTF-8")?.to_owned();
    let listener = listen(&mut fixture, &endpoint)?;
    let cli_args = [
        "--format",
        "json",
        "--detail",
        "full",
        "--project",
        &label,
        "--workspace",
        &workspace,
        "--endpoint",
        &endpoint_text,
        "stylo-query",
        &label,
        "--class",
        "hud-menu",
    ];
    let cli_answer = checked_json(child_output(&cli, &cli_args, None)?)?;
    let cli_page = &cli_answer["semantic_data"]["value"];
    let source = cli_page["hits"]
        .as_array()
        .ok_or("CLI native page missing")?
        .iter()
        .find_map(|hit| hit.get("source").and_then(serde_json::Value::as_str))
        .ok_or("CLI source action missing")?
        .to_owned();
    assert!(cli_page["hits"].as_array().unwrap().len() >= 2);
    let cli_default_args = [
        "--format",
        "json",
        "--project",
        &label,
        "--workspace",
        &workspace,
        "--endpoint",
        &endpoint_text,
        "stylo-query",
        &label,
        "--class",
        "hud-menu",
    ];
    let cli_default = checked_json(child_output(&cli, &cli_default_args, None)?)?;
    assert_eq!(cli_default["semantic_data"]["kind"], "stylo-query-summary");
    assert_eq!(cli_default["semantic_data"]["value"]["first_site"], source);
    assert_eq!(
        cli_default["semantic_data"]["value"]["basis"]["native_root"],
        cli_page["basis"]["native_root"]
    );
    assert!(
        serde_json::to_string(&cli_default)?
            .matches(source.as_str())
            .count()
            <= 2
    );
    let query = serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"backend.stylo_query","arguments":{"package":label.clone(),"class":"hud-menu","detail":"full"}}});
    let default_query = serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"backend.stylo_query","arguments":{"package":label.clone(),"class":"hud-menu"}}});
    let source_query = serde_json::json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"backend.stylo_read_site","arguments":{"reference":source.clone(),"detail":"full"}}});
    let input = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"initialize\",\"params\":{{\"protocolVersion\":\"2025-06-18\",\"capabilities\":{{}},\"clientInfo\":{{\"name\":\"css-fixture\",\"version\":\"1\"}}}}}}\n{{\"jsonrpc\":\"2.0\",\"method\":\"notifications/initialized\"}}\n{query}\n{default_query}\n{source_query}\n"
    );
    let mcp_args = [
        "--project",
        &label,
        "--workspace",
        &workspace,
        "--endpoint",
        &endpoint_text,
    ];
    let mcp_output = child_output(&mcp, &mcp_args, Some(&input))?;
    if !mcp_output.status.success() {
        return Err(format!(
            "MCP failed: {}",
            String::from_utf8_lossy(&mcp_output.stderr)
        )
        .into());
    }
    let messages = String::from_utf8(mcp_output.stdout)?
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let result = messages
        .iter()
        .find(|message| message["id"] == 2)
        .ok_or("missing MCP tool reply")?;
    let mcp_page = &result["result"]["structuredContent"]["semantic_data"]["value"];
    assert_eq!(
        mcp_page["basis"]["native_root"],
        cli_page["basis"]["native_root"]
    );
    assert_eq!(mcp_page["hits"], cli_page["hits"]);
    let default_result = messages
        .iter()
        .find(|message| message["id"] == 3)
        .ok_or("missing default MCP reply")?;
    let mcp_summary = &default_result["result"]["structuredContent"]["semantic_data"];
    assert_eq!(mcp_summary["kind"], "stylo-query-summary");
    assert_eq!(mcp_summary["value"]["first_site"], source);
    assert_eq!(
        mcp_summary["value"]["basis"]["native_root"],
        cli_page["basis"]["native_root"]
    );
    let mcp_source = messages
        .iter()
        .find(|message| message["id"] == 4)
        .ok_or("missing MCP source reply")?;
    assert_eq!(
        mcp_source["result"]["structuredContent"]["semantic_data"]["value"]["text"],
        "hud-menu"
    );
    drop(listener);
    fixture = fixture.reopen();
    let listener = listen(&mut fixture, &endpoint)?;
    let after = checked_json(child_output(&cli, &cli_args, None)?)?;
    assert_eq!(
        after["semantic_data"]["value"]["basis"]["native_root"],
        cli_page["basis"]["native_root"]
    );
    let read_args = [
        "--format",
        "json",
        "--detail",
        "full",
        "--project",
        &label,
        "--workspace",
        &workspace,
        "--endpoint",
        &endpoint_text,
        "stylo-read-site",
        &source,
    ];
    let read = checked_json(child_output(&cli, &read_args, None)?)?;
    assert_eq!(read["semantic_data"]["value"]["state"], "available");
    assert_eq!(read["semantic_data"]["value"]["text"], "hud-menu");
    let mcp_reopened = child_output(&mcp, &mcp_args, Some(&input))?;
    if !mcp_reopened.status.success() {
        return Err(format!(
            "MCP after reopen failed: {}",
            String::from_utf8_lossy(&mcp_reopened.stderr)
        )
        .into());
    }
    let reopened_messages = String::from_utf8(mcp_reopened.stdout)?
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .collect::<Result<Vec<_>, _>>()?;
    let reopened_source = reopened_messages
        .iter()
        .find(|message| message["id"] == 4)
        .ok_or("missing reopened MCP source reply")?;
    assert_eq!(
        reopened_source["result"]["structuredContent"]["semantic_data"]["value"]["text"],
        "hud-menu"
    );
    drop(listener);
    Ok(())
}
