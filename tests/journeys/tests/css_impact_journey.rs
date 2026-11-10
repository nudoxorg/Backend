//! One CSS conversation through the real owner, a real Stylo worker process, the
//! public CLI and the public MCP server.
//!
//! The binaries come from `scripts/stage-css-journey.sh`, which builds the owner
//! against the worker's final bytes and lays both out the way the product does.
//! Run it with:
//!
//! ```text
//! tests/journeys/scripts/stage-css-journey.sh "$STAGE"
//! NUDOX_CSS_JOURNEY_BIN=<printed bin dir> cargo test -p backend-journeys \
//!     --features css-journey --test css_impact_journey -- --nocapture
//! ```
//!
//! Covered: index (real worker), selector / custom-property / at-rule queries,
//! exact source reads, references, impact with exact and candidate strengths,
//! ambiguity, honest not-found, an edit that makes the answer stale, re-index,
//! cold reopen of the owner, and a worker-less owner that reports a gap.
//! Debug builds skip the account gate, so this is development-mode evidence.
#![cfg(unix)]
#![deny(unsafe_code)]
#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

#[path = "../src/surface_matrix.rs"]
mod surface_matrix;

use backend_library::impact::{
    ImpactDomain, ImpactFreshness, ImpactReport, ImpactSeedState, ImpactStrength, ImpactTerminal,
};
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command as ProcessCommand, Output, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const CHILD_POLL: Duration = Duration::from_millis(20);
const INDEX_DEADLINE: Duration = Duration::from_secs(240);
static NEXT: AtomicU64 = AtomicU64::new(0);

fn staged_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("NUDOX_CSS_JOURNEY_BIN").unwrap_or_else(|| {
        panic!(
            "NUDOX_CSS_JOURNEY_BIN is not set: stage the product with tests/journeys/scripts/stage-css-journey.sh"
        )
    }))
}

struct Owner {
    child: Option<Child>,
    endpoint: PathBuf,
}
impl Owner {
    fn start(bin: &Path, workspace: &Path, authority: &Path, label: &str) -> Self {
        let serial = NEXT.fetch_add(1, Ordering::Relaxed);
        let endpoint = PathBuf::from(format!("/tmp/nudox-css-{label}-{}-{serial}.sock", std::process::id()));
        let _ = std::fs::remove_file(&endpoint);
        let args = surface_matrix::locald_args(&endpoint, workspace, authority, Some(0), true);
        let mut child = ProcessCommand::new(bin.join("backend-locald"))
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .env_remove("NUDOX_RUSTC")
            .spawn()
            .expect("spawn the real owner");
        let end = Instant::now() + surface_matrix::READY_DEADLINE;
        loop {
            assert!(child.try_wait().expect("poll owner").is_none(), "owner exited before readiness");
            if let Ok(meta) = std::fs::symlink_metadata(&endpoint)
                && meta.file_type().is_socket()
                && UnixStream::connect(&endpoint).is_ok()
            {
                break;
            }
            assert!(Instant::now() < end, "owner never listened on {}", endpoint.display());
            thread::sleep(CHILD_POLL);
        }
        Self { child: Some(child), endpoint }
    }
    fn stop(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_file(&self.endpoint);
    }
}
impl Drop for Owner {
    fn drop(&mut self) {
        self.stop();
    }
}

struct Session<'a> {
    bin: &'a Path,
    owner: &'a Owner,
    workspace: &'a Path,
    project: &'a Path,
    authority: &'a Path,
}
impl Session<'_> {
    fn cli_raw(&self, words: &[&str]) -> Output {
        let mut command = ProcessCommand::new(self.bin.join("backend-cli"));
        command
            .arg("--endpoint").arg(&self.owner.endpoint)
            .arg("--workspace").arg(self.workspace)
            .arg("--project").arg(self.project)
            .arg("--format").arg("json")
            .arg("--detail").arg("full")
            .args(words)
            .env("NO_COLOR", "1")
            .env("BACKEND_LOCALD_AUTHORITY_SECRET_FILE", self.authority);
        surface_matrix::run_bounded(command, &format!("CLI {words:?}"), None)
    }
    fn cli(&self, words: &[&str]) -> Value {
        let output = self.cli_raw(words);
        assert!(
            output.status.success(),
            "CLI {words:?} failed ({}): stdout={} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("CLI {words:?} printed no JSON ({error}): {}", String::from_utf8_lossy(&output.stdout)))
    }
    fn mcp(&self, tool: &str, arguments: Value) -> Value {
        let lines = [
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"css-impact-journey","version":"1"}}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized","params":{}}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":tool,"arguments":arguments}}),
        ];
        let mut input = lines.iter().map(Value::to_string).collect::<Vec<_>>().join("\n").into_bytes();
        input.push(b'\n');
        let mut command = ProcessCommand::new(self.bin.join("backend-mcp"));
        command
            .arg("--endpoint").arg(&self.owner.endpoint)
            .arg("--workspace").arg(self.workspace)
            .arg("--project").arg(self.project)
            .env("BACKEND_LOCALD_AUTHORITY_SECRET_FILE", self.authority);
        let output = surface_matrix::run_bounded(command, &format!("MCP {tool}"), Some(input));
        assert!(output.status.success(), "MCP failed: {}", String::from_utf8_lossy(&output.stderr));
        let reply = String::from_utf8(output.stdout)
            .expect("MCP stdout UTF-8")
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .find(|value| value["id"] == 2)
            .unwrap_or_else(|| panic!("MCP {tool} had no reply"));
        assert!(reply.get("error").is_none(), "MCP {tool} protocol error: {reply}");
        reply
    }
    fn project_arg(&self) -> String {
        self.project.to_string_lossy().into_owned()
    }
    fn impact(&self, extra: &[&str]) -> ImpactReport {
        let project = self.project_arg();
        let mut words = vec!["impact", project.as_str()];
        words.extend_from_slice(extra);
        report_in(&self.cli(&words))
    }
}

/// Finds a JSON object by one of its keys anywhere in a reply.
fn find_key<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    match value {
        Value::Object(map) => map.get(key).or_else(|| map.values().find_map(|inner| find_key(inner, key))),
        Value::Array(items) => items.iter().find_map(|inner| find_key(inner, key)),
        _ => None,
    }
}
fn report_in(value: &Value) -> ImpactReport {
    let semantic = find_key(value, "semantic_data").unwrap_or_else(|| panic!("reply has no semantic_data: {value}"));
    assert_eq!(semantic["kind"], "impact", "semantic data is not an impact report: {semantic}");
    serde_json::from_value(semantic["value"].clone())
        .unwrap_or_else(|error| panic!("impact report did not deserialize ({error}): {}", semantic["value"]))
}
fn all_strings<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::String(text) => out.push(text),
        Value::Array(items) => items.iter().for_each(|item| all_strings(item, out)),
        Value::Object(map) => map.values().for_each(|item| all_strings(item, out)),
        _ => {}
    }
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("create directory");
    for entry in std::fs::read_dir(from).expect("read fixture directory") {
        let entry = entry.expect("fixture entry");
        let target = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy fixture file");
        }
    }
}

/// Owner state directories must be private to the current user.
fn private_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path).expect("create private directory");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).expect("make directory private");
}

fn names(report: &ImpactReport, kind: &str) -> BTreeSet<String> {
    report.rows.iter().filter(|row| row.node.kind == kind).map(|row| row.node.name.clone()).collect()
}
fn strength_of(report: &ImpactReport, kind: &str, name: &str) -> Option<ImpactStrength> {
    report.rows.iter().filter(|row| row.node.kind == kind && row.node.name == name).map(|row| row.strength).min()
}

/// Follows `--cursor` continuations of a CSS query until the scan completes.
fn stylo_pages(session: &Session<'_>, project: &str, selector: &[&str]) -> Vec<Value> {
    let mut pages = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..40 {
        let mut words = vec!["stylo-query", project];
        words.extend_from_slice(selector);
        if let Some(cursor) = cursor.as_deref() {
            words.extend(["--cursor", cursor]);
        }
        let reply = session.cli(&words);
        let next = find_key(&reply, "terminal")
            .filter(|terminal| terminal["state"] == "more")
            .and_then(|terminal| terminal["continuation"].as_str())
            .map(str::to_owned);
        pages.push(reply);
        match next {
            Some(next) => cursor = Some(next),
            None => return pages,
        }
    }
    panic!("CSS scan never completed");
}
fn page_hits(pages: &[Value]) -> Vec<&Value> {
    pages
        .iter()
        .filter_map(|page| find_key(page, "hits").and_then(Value::as_array))
        .flatten()
        .collect()
}

fn wait_for_css(session: &Session<'_>, want_fresh_name: &str) -> Value {
    let project = session.project_arg();
    let end = Instant::now() + INDEX_DEADLINE;
    loop {
        let pages = stylo_pages(session, &project, &["--custom-property", want_fresh_name]);
        let attempt = find_key(&pages[0], "attempt").map(|attempt| attempt.to_string()).unwrap_or_default();
        if !page_hits(&pages).is_empty() && attempt.contains("latest_observed") {
            return pages[0].clone();
        }
        let reply = pages[0].clone();
        assert!(Instant::now() < end, "CSS never published {want_fresh_name}: {reply}");
        thread::sleep(Duration::from_millis(500));
    }
}

#[test]
#[allow(clippy::too_many_lines, reason = "one conversation covers the whole journey")]
fn css_is_indexed_queried_traced_edited_and_reopened_through_the_real_product() {
    let bin = staged_bin();
    assert!(bin.join("backend-stylo-worker").is_file(), "the staged product has no Stylo worker");
    let serial = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("nudox-css-journey-{}-{serial}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    private_dir(&root);
    let root = root.canonicalize().expect("canonical root");
    let project = root.join("storefront");
    copy_tree(&Path::new(env!("CARGO_MANIFEST_DIR")).join("fixtures/storefront"), &project);
    let project = project.canonicalize().expect("canonical project");
    let workspace = root.join("workspace");
    private_dir(&workspace);
    let authority = surface_matrix::write_authority(&root);

    // 1. Index through the public CLI; the real worker process analyzes the stylesheets.
    let mut owner = Owner::start(&bin, &workspace, &authority, "main");
    {
        let session = Session { bin: &bin, owner: &owner, workspace: &workspace, project: &project, authority: &authority };
        let added = session.cli(&["add", &project.to_string_lossy()]);
        assert_eq!(added["answer"], "product", "{added}");
        let brand = wait_for_css(&session, "--brand");
        // Native TypeScript evidence is published in a second phase; wait until
        // the code domain has read the components before asking about them.
        let end = Instant::now() + INDEX_DEADLINE;
        loop {
            let probe = session.impact(&["--custom-property", "--brand"]);
            let code_files = probe.domains.iter().find(|d| d.domain == ImpactDomain::Code).map_or(0, |d| d.files);
            if code_files >= 7 {
                break;
            }
            assert!(Instant::now() < end, "TypeScript evidence never reached the impact route: {probe:?}");
            thread::sleep(Duration::from_millis(500));
        }
        let capability = find_key(&brand, "worker_capability").expect("worker capability in basis").to_string();
        assert!(capability.contains("admitted"), "the real worker was not admitted: {capability}");

        // 2. Selectors, custom properties and exact source through CLI and MCP.
        let pages = stylo_pages(&session, &project.to_string_lossy(), &["--class", "btn"]);
        let class = Value::Array(pages.clone());
        let mut strings = Vec::new();
        all_strings(&class, &mut strings);
        assert!(strings.iter().any(|s| s.contains(".btn") && !s.contains(".btn-")), "no .btn selector in {class}");
        // The compact operand of an authored-site row is its exact-source reference.
        let site = pages
            .iter()
            .filter_map(|page| page["records"].as_array())
            .flatten()
            .find(|record| record["tags"].as_array().is_some_and(|tags| tags.iter().any(|t| t == "class selector name")))
            .and_then(|record| record["operand"].as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| panic!("no authored site reference in {class}"));
        let source = session.cli(&["stylo-read-site", &site]);
        let mut texts = Vec::new();
        all_strings(&source, &mut texts);
        assert!(texts.iter().any(|t| *t == "btn" || t.contains("btn")), "exact source read did not return the class token: {source}");
        let mcp_class = session.mcp("backend.stylo_query", json!({"package": project.to_string_lossy(), "class": "btn", "detail": "full"}));
        assert_eq!(mcp_class["result"]["isError"], false, "{mcp_class}");

        // A handle into the stylesheet which step 7 edits.
        let theme_pages = stylo_pages(&session, &project.to_string_lossy(), &["--custom-property", "--brand"]);
        let theme_site = theme_pages
            .iter()
            .filter_map(|page| page["records"].as_array())
            .flatten()
            .find(|record| record["tags"].as_array().is_some_and(|tags| tags.iter().any(|t| t == "custom-property declaration name")))
            .and_then(|record| record["operand"].as_str())
            .map(str::to_owned)
            .unwrap_or_else(|| panic!("no --brand declaration site in {theme_pages:?}"));
        let before_edit = session.cli(&["stylo-read-site", &theme_site]);
        assert_eq!(find_key(&before_edit, "text").and_then(Value::as_str), Some("--brand"), "{before_edit}");

        // 3. Impact of a custom property: every hop through the cascade is a candidate.
        let brand_impact = session.impact(&["--custom-property", "--brand"]);
        assert!(matches!(brand_impact.seed_state, ImpactSeedState::Resolved), "{brand_impact:?}");
        assert!(names(&brand_impact, "custom-property").contains("--accent"), "{brand_impact:?}");
        assert!(names(&brand_impact, "rule").iter().any(|n| n.contains(".btn")), "{brand_impact:?}");
        assert_eq!(strength_of(&brand_impact, "rule", ".btn"), Some(ImpactStrength::Candidate));
        assert_eq!(brand_impact.totals.exact, 0, "cascade hops must not be exact: {brand_impact:?}");
        assert!(brand_impact.gaps.iter().any(|g| g.facet == "css.cascade"));
        assert!(matches!(brand_impact.freshness, ImpactFreshness::Current { .. }), "{:?}", brand_impact.freshness);
        assert!(!brand_impact.proves_no_impact());
        // The same question over MCP gives the same typed answer.
        let mcp_brand = session.mcp("backend.impact", json!({"package": project.to_string_lossy(), "custom_property": "--brand", "detail": "full"}));
        assert_eq!(mcp_brand["result"]["isError"], false, "{mcp_brand}");
        let mcp_report = report_in(&mcp_brand["result"]["structuredContent"]);
        assert_eq!(mcp_report.totals, brand_impact.totals);

        // 4. A class reaches the components which use it, and their renderers.
        let btn = session.impact(&["--class", "btn", "--depth", "4"]);
        assert!(matches!(btn.seed_state, ImpactSeedState::Resolved));
        assert!(names(&btn, "component").contains("Button"), "{btn:?}");
        assert!(names(&btn, "component").contains("ProductCard"), "rendered Button => ProductCard affected: {btn:?}");
        assert_eq!(strength_of(&btn, "component", "Button"), Some(ImpactStrength::Candidate), "literal class token is a candidate");

        // 5. CSS-module classes are exact; same-named module classes are ambiguous until narrowed.
        let ambiguous = session.impact(&["--class", "title"]);
        assert!(matches!(ambiguous.seed_state, ImpactSeedState::Ambiguous), "{ambiguous:?}");
        assert!(ambiguous.rows.is_empty());
        let header_title = session.impact(&["--class", "title", "--in", "src/components/Header.module.css"]);
        assert!(matches!(header_title.seed_state, ImpactSeedState::Resolved), "{header_title:?}");
        assert_eq!(strength_of(&header_title, "component", "Header"), Some(ImpactStrength::Exact));
        assert!(!names(&header_title, "component").contains("ProductCard"));
        let card = session.impact(&["--class", "card"]);
        assert_eq!(strength_of(&card, "component", "ProductCard"), Some(ImpactStrength::Exact));
        assert_eq!(strength_of(&card, "component", "ProductGrid"), Some(ImpactStrength::Exact), "ProductGrid renders ProductCard");

        // 6. At-rules, components, handles, filters and honest absence.
        let media = session.impact(&["--at-rule", "media"]);
        assert!(matches!(media.seed_state, ImpactSeedState::Resolved), "{media:?}");
        assert!(media.rows.iter().any(|row| row.relation == "css.conditions-rule" && row.strength == ImpactStrength::Exact), "{media:?}");
        let supports = session.impact(&["--at-rule", "supports"]);
        assert!(matches!(supports.seed_state, ImpactSeedState::Resolved), "{supports:?}");
        assert!(supports.seeds.iter().any(|seed| seed.name.starts_with("@supports") && seed.name.contains("display")), "the @supports prelude is projected: {supports:?}");
        let grid = supports.rows.iter().find(|row| row.node.kind == "rule" && row.node.name == ".grid").expect("rule nested in @supports");
        assert_eq!((grid.relation.as_str(), grid.strength), ("css.conditions-rule", ImpactStrength::Exact));
        assert_eq!(strength_of(&supports, "component", "ProductGrid"), Some(ImpactStrength::Candidate), "{supports:?}");
        let keyframes = session.impact(&["--at-rule", "keyframes"]);
        assert!(matches!(keyframes.seed_state, ImpactSeedState::Resolved), "{keyframes:?}");
        assert!(keyframes.seeds.iter().any(|seed| seed.name.contains("fade-in")), "{keyframes:?}");
        assert!(!keyframes.proves_no_impact(), "no dependents found is not a proof while gaps remain");
        let layer = session.impact(&["--at-rule", "layer"]);
        assert!(matches!(layer.seed_state, ImpactSeedState::Resolved), "{layer:?}");
        let button = session.impact(&["--component", "Button", "--depth", "4"]);
        for name in ["ProductCard", "ThemeToggle", "ProductGrid", "App"] {
            assert_eq!(strength_of(&button, "component", name), Some(ImpactStrength::Exact), "{name}: {button:?}");
        }
        let exact_only = session.impact(&["--custom-property", "--brand", "--exact"]);
        assert!(exact_only.rows.is_empty() && exact_only.totals.candidate > 0, "{exact_only:?}");
        assert!(exact_only.gaps.iter().any(|g| g.facet == "impact.hidden-candidates"));
        let missing = session.impact(&["--class", "no-such-class"]);
        assert!(matches!(missing.seed_state, ImpactSeedState::NotFound { .. }), "{missing:?}");
        assert!(!missing.proves_no_impact());
        let followed = brand_impact.rows.iter().find_map(|row| row.node.handle.as_ref()).expect("a row handle");
        let by_handle = session.impact(&["--handle", followed.as_str()]);
        assert!(matches!(by_handle.seed_state, ImpactSeedState::Resolved), "{by_handle:?}");
        let sql = session.impact(&["--sql-table", "orders"]);
        assert!(matches!(sql.seed_state, ImpactSeedState::Unavailable { .. }), "SQL is not installed: {sql:?}");
        assert!(sql.rows.is_empty() && !sql.proves_no_impact());
        // Paging resumes the same ordered list.
        let first = session.impact(&["--custom-property", "--brand", "--limit", "2"]);
        if let ImpactTerminal::More { cursor } = &first.terminal {
            let second = session.impact(&["--custom-property", "--brand", "--limit", "2", "--cursor", cursor.as_str()]);
            assert_eq!(second.rows.first().map(|row| row.n), first.rows.last().map(|row| row.n + 1));
        } else {
            panic!("expected a second page: {first:?}");
        }

        // 7. Edit the stylesheet without re-indexing: the answer says it is stale.
        let theme = project.join("src/styles/theme.css");
        let mut text = std::fs::read_to_string(&theme).expect("theme.css");
        text.push_str("\n.btn:hover {\n  background: var(--brand-hover);\n}\n:root {\n  --brand-hover: var(--brand);\n}\n");
        std::fs::write(&theme, text).expect("edit theme.css");
        let stale = session.impact(&["--custom-property", "--brand"]);
        match &stale.freshness {
            ImpactFreshness::Stale { changed, .. } => assert!(changed.iter().any(|path| path == "src/styles/theme.css"), "{changed:?}"),
            other => panic!("an edited stylesheet must be reported stale, got {other:?}"),
        }
        let old_read = session.cli_raw(&["stylo-read-site", &theme_site]);
        let old_text = String::from_utf8_lossy(&old_read.stdout).into_owned();
        assert!(old_read.status.success(), "stylo-read-site failed outright: {old_text}");
        assert!(
            ["Source changed", "source_changed", "Selection changed", "expired"].iter().any(|word| old_text.contains(word)),
            "an old site handle must not read changed bytes: {old_text}"
        );

        // 8. Re-index; the new derived property is traced and freshness recovers.
        let again = session.cli(&["add", &project.to_string_lossy()]);
        assert_eq!(again["answer"], "product", "{again}");
        eprintln!("re-index said: {again}");
        let end = Instant::now() + Duration::from_secs(120);
        let refreshed = loop {
            // A saved operation which already reached a terminal state is acknowledged
            // by the next `add`; the one after that starts the fresh operation.
            let again = session.cli(&["add", &project.to_string_lossy()]);
            assert_eq!(again["answer"], "product", "{again}");
            let report = session.impact(&["--custom-property", "--brand"]);
            if names(&report, "custom-property").contains("--brand-hover") && matches!(report.freshness, ImpactFreshness::Current { .. }) {
                break report;
            }
            if Instant::now() >= end {
                let hover = session.cli(&["stylo-query", &project.to_string_lossy(), "--custom-property", "--brand-hover"]);
                let health = session.cli(&["health"]);
                panic!("re-index never made --brand-hover visible.\nquery: {hover}\nhealth: {health}\nreport freshness: {:?} gaps: {:?}", report.freshness, report.gaps);
            }
            thread::sleep(Duration::from_millis(500));
        };
        assert_eq!(refreshed.seed_state, ImpactSeedState::Resolved);
    }

    // 9. Cold reopen: a new owner process on the same workspace answers from durable state.
    let before = {
        let session = Session { bin: &bin, owner: &owner, workspace: &workspace, project: &project, authority: &authority };
        session.impact(&["--class", "btn", "--depth", "4"])
    };
    owner.stop();
    let reopened = Owner::start(&bin, &workspace, &authority, "reopen");
    {
        let session = Session { bin: &bin, owner: &reopened, workspace: &workspace, project: &project, authority: &authority };
        let after = session.impact(&["--class", "btn", "--depth", "4"]);
        assert_eq!(after.totals, before.totals, "cold reopen changed the answer");
        assert_eq!(
            after.rows.iter().map(|r| (r.node.kind.clone(), r.node.name.clone(), r.strength)).collect::<Vec<_>>(),
            before.rows.iter().map(|r| (r.node.kind.clone(), r.node.name.clone(), r.strength)).collect::<Vec<_>>()
        );
        assert!(matches!(after.freshness, ImpactFreshness::Current { .. }));
        let query = session.cli(&["stylo-query", &project.to_string_lossy(), "--custom-property", "--brand-hover"]);
        assert!(find_key(&query, "hits").and_then(Value::as_array).is_some_and(|hits| !hits.is_empty()), "{query}");
    }
    drop(reopened);

    // 10. An owner whose worker is missing must say so instead of answering "no impact".
    let bare = root.join("no-worker");
    std::fs::create_dir_all(&bare).expect("bare bin");
    for name in ["backend-locald", "backend-cli", "backend-mcp"] {
        std::fs::copy(bin.join(name), bare.join(name)).expect("copy product binary");
    }
    let bare_workspace = root.join("workspace-no-worker");
    private_dir(&bare_workspace);
    let bare_owner = Owner::start(&bare, &bare_workspace, &authority, "bare");
    let session = Session { bin: &bare, owner: &bare_owner, workspace: &bare_workspace, project: &project, authority: &authority };
    let added = session.cli(&["add", &project.to_string_lossy()]);
    assert_eq!(added["answer"], "product", "{added}");
    let end = Instant::now() + INDEX_DEADLINE;
    loop {
        let report = session.impact(&["--class", "btn"]);
        if matches!(report.seed_state, ImpactSeedState::Unavailable { .. }) && !report.proves_no_impact() {
            assert!(report.rows.is_empty());
            break;
        }
        assert!(Instant::now() < end, "a worker-less owner never reported CSS as unavailable: {report:?}");
        thread::sleep(Duration::from_millis(500));
    }
    let _ = OsString::new();
}
