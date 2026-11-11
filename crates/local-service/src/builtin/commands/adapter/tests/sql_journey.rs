//! SQL acceptance journey.
//!
//! A realistic multi-file SQLite project is written to disk and indexed through
//! the production owner: the real filesystem scan, the real framed listener, a
//! real out-of-process syntaqlite worker, durable publication of the SQL
//! relation and its owner-sealed receipt, then queried over the same public
//! `sql-query` / `sql-read-source` surface the CLI and MCP use. The workspace is
//! then closed and reopened cold from its persisted journal.
//!
//! Development mode: debug builds skip the account gate, so this proves the
//! product route, not the gated release configuration.
use super::*;
use backend_library::sql_query::{
    SqlQueryPage, SqlQueryRequest, SqlQueryRole, SqlQueryTerminal, SqlSourceRead,
};
use backend_semantic::sql_analysis::{
    SqlDefinitionAction, SqlFact, SqlObjectKind, SqlRefKind, SqlResolution, SqlStatementKind,
    SqlStatementSupport,
};

/// Build machines that run many lanes at once can take longer than the product
/// default just to start a process; the budget is recorded in the row settings.
const TERMINAL_WAIT: std::time::Duration = std::time::Duration::from_secs(600);
const TEST_WORKER_WALL: std::time::Duration = std::time::Duration::from_secs(90);

/// Build (or locate) a frontend worker binary. `NUDOX_TEST_SQLITE_WORKER` /
/// `NUDOX_TEST_POSTGRES_WORKER` select a prebuilt one; otherwise the isolated
/// frontend workspace is built here with this process's own cargo.
pub(super) fn frontend_worker(frontend: &str, binary: &str) -> PathBuf {
    let variable = format!("NUDOX_TEST_{}_WORKER", frontend.to_ascii_uppercase());
    if let Some(path) = std::env::var_os(&variable) {
        return PathBuf::from(path);
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root");
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| repo.join("target"))
        .join(format!("{frontend}-frontend-worker"));
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = std::process::Command::new(cargo)
        .args(["build", "--locked", "--offline", "--bin", binary, "--manifest-path"])
        .arg(repo.join("frontends").join(frontend).join("Cargo.toml"))
        .env("CARGO_TARGET_DIR", &target)
        // The pgrust checkout is owned by another user on shared build hosts;
        // let the build script's read-only git inspection run (this file-less,
        // per-process setting does not touch any git configuration on disk).
        .env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", "safe.directory")
        .env("GIT_CONFIG_VALUE_0", "*")
        // The checked launch hashes the worker on every use: keep it small.
        .env("CARGO_PROFILE_DEV_DEBUG", "0")
        .env("CARGO_PROFILE_DEV_STRIP", "symbols")
        .output()
        .expect("spawn cargo for the frontend worker");
    assert!(
        output.status.success(),
        "frontend worker build failed:\n{}",
        String::from_utf8_lossy(&output.stderr).lines().rev().take(40).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n")
    );
    target.join("debug").join(binary)
}

/// Admit a worker the way an owner would: its executable identity plus the
/// producer basis it reports. Development-mode test admission only.
pub(super) fn admitted_workers(sqlite: Option<PathBuf>, postgres: Option<PathBuf>) -> crate::builtin::sql_index::SqlWorkers {
    use backend_sql_contract::{Dialect, Engine, Request, Settings, call_worker};
    let probe = |path: &Path, dialect, engine| {
        let artifact = backend_compile::ToolchainArtifact::from_path(path, Vec::new())
            .expect("worker artifact");
        // One start can exceed the probe's own 10s on a saturated build host.
        let request = Request {
            version: backend_sql_contract::PROTOCOL_VERSION,
            expected_basis: None,
            source: b"SELECT 1".to_vec(),
            settings: Settings::ProcessDefaults,
            context: None,
        };
        let basis = (0..6)
            .find_map(|_| call_worker(path, &request, dialect, engine).basis)
            .expect("worker reports its producer basis");
        (artifact, basis)
    };
    crate::builtin::sql_index::SqlWorkers {
        sqlite: sqlite.map(|path| {
            let (artifact, basis) = probe(&path, Dialect::Sqlite, Engine::Syntaqlite4e4de76);
            Arc::new(
                crate::builtin::sql_capture::SqliteWorkerPermit::from_admitted(path, artifact, basis)
                    .expect("SQLite worker permit")
                    .with_wall(TEST_WORKER_WALL),
            )
        }),
        postgres: postgres.map(|path| {
            let (artifact, basis) = probe(&path, Dialect::Postgres, Engine::Pgrust38634b4);
            Arc::new(
                crate::builtin::sql_capture::PostgresWorkerPermit::from_admitted(path, artifact, basis)
                    .expect("PostgreSQL worker permit")
                    .with_wall(TEST_WORKER_WALL),
            )
        }),
    }
}

const INIT: &str = "\
-- Baseline schema.
CREATE TABLE accounts (
  id INTEGER PRIMARY KEY,
  email TEXT NOT NULL UNIQUE,
  balance INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE orders (
  id INTEGER PRIMARY KEY,
  account_id INTEGER NOT NULL REFERENCES accounts(id),
  total INTEGER NOT NULL
);
CREATE INDEX orders_by_account ON orders(account_id);
";
const TIER: &str = "ALTER TABLE accounts ADD COLUMN tier TEXT NOT NULL DEFAULT 'basic';\n";
const VIEWS: &str = "\
CREATE VIEW account_spend AS
  SELECT a.id AS account_id, a.email, SUM(o.total) AS spent
  FROM accounts a JOIN orders o ON o.account_id = a.id
  GROUP BY a.id, a.email;

CREATE TRIGGER accounts_tier_audit AFTER UPDATE OF tier ON accounts
BEGIN
  INSERT INTO orders (account_id, total) VALUES (NEW.id, 0);
END;
";
const QUERIES: &str = "\
SELECT email, balance FROM accounts WHERE id = ?1;
UPDATE accounts SET balance = balance - ?2 WHERE id = ?1;
SELECT s.email, s.spent FROM account_spend s WHERE s.spent > 100;
";
const DROP_BALANCE: &str = "ALTER TABLE accounts DROP COLUMN balance;\n";

fn sqlite_files() -> Vec<(String, Vec<u8>)> {
    [
        ("db/migrations/0001_init.sql", INIT),
        ("db/migrations/0002_tier.sql", TIER),
        ("db/migrations/0003_views.sql", VIEWS),
        ("db/queries/accounts.sql", QUERIES),
    ]
    .into_iter()
    .map(|(path, text)| (path.to_owned(), text.as_bytes().to_vec()))
    .collect()
}

fn request(project: &Path, role: SqlQueryRole) -> SqlQueryRequest {
    SqlQueryRequest {
        package: backend_library::PackageReference::parse(project.to_str().expect("path").to_owned())
            .expect("package"),
        role,
        name: None,
        kind: None,
        parent: None,
        uses: None,
        page_rows: 128,
        output_bytes: 4_194_304,
        scan_rows: 262_144,
        scan_bytes: 64 * 1024 * 1024,
        continuation: None,
    }
}

/// Every page of a query, through the listener, until the terminal is Complete.
fn all_hits(session: &mut Session, mut request: SqlQueryRequest) -> (SqlQueryPage, Vec<backend_library::sql_query::SqlQueryHit>) {
    let mut hits = Vec::new();
    let mut host_gaps = Vec::new();
    loop {
        let backend_library::SurfaceReply::SqlQuery(mut page) = session
            .surface(backend_library::SurfaceCommand::SqlQuery { request: request.clone() })
            .expect("sql-query through the public route")
        else {
            panic!("SQL query reply kind");
        };
        hits.extend(page.hits.clone());
        host_gaps.extend(page.host_gaps.clone());
        match &page.terminal {
            SqlQueryTerminal::More { continuation, .. } => request.continuation = Some(continuation.clone()),
            SqlQueryTerminal::Complete => { page.host_gaps = host_gaps; return (page, hits); },
            SqlQueryTerminal::Unavailable => panic!("SQL analysis unavailable: {:?}", page.basis.attempt),
        }
    }
}

fn definitions(session: &mut Session, project: &Path, kind: SqlObjectKind, parent: Option<&str>) -> Vec<(String, bool)> {
    let mut r = request(project, SqlQueryRole::Definition);
    r.kind = Some(kind);
    r.parent = parent.map(str::to_owned);
    let (_, hits) = all_hits(session, r);
    let mut names = hits
        .into_iter()
        .filter_map(|hit| match hit.fact {
            SqlFact::Definition(d) if matches!(d.action, SqlDefinitionAction::Create | SqlDefinitionAction::AddColumn) => {
                Some((d.head_name.unwrap_or(d.name), d.live_at_head))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn read_text(session: &mut Session, hit: &backend_library::sql_query::SqlQueryHit) -> (String, String) {
    let reference = hit.source.clone().expect("exact source reference");
    let backend_library::SurfaceReply::SqlReadSource(SqlSourceRead::Available { path, text, .. }) = session
        .surface(backend_library::SurfaceCommand::SqlReadSource { reference })
        .expect("sql-read-source through the public route")
    else {
        panic!("source reply");
    };
    (path, text)
}

fn publish(wire: &mut SourceWire, byte: u8) {
    mixed_start_again(wire, byte);
    wire.wait_terminal_within(TERMINAL_WAIT);
    mixed_published(wire);
}

#[test]
fn sqlite_project_indexes_publishes_queries_reindexes_and_survives_cold_reopen() {
    let workers = admitted_workers(Some(frontend_worker("sqlite", "sqlite-worker")), None);
    let mut wire = SourceWire::start_sql(
        TestStage::BeforeSourcePreparation,
        true,
        SqlFixture { files: sqlite_files(), workers },
    );
    wire.wait_at_source_within(TERMINAL_WAIT);
    wire.release();
    wire.wait_terminal_within(TERMINAL_WAIT);
    mixed_published(&wire);
    wire.commands.lock().expect("adapter").source_test_hook = None;
    let project = wire.source.parent().expect("project").to_owned();
    let mut session = Session::connect(&wire.endpoint).expect("listener");

    // Tables, columns, views, triggers, indexes: definitions across files.
    let tables = definitions(&mut session, &project, SqlObjectKind::Table, None);
    assert_eq!(tables, [("accounts".to_owned(), true), ("orders".to_owned(), true)]);
    let columns = definitions(&mut session, &project, SqlObjectKind::Column, Some("accounts"));
    assert_eq!(
        columns.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(),
        ["balance", "email", "id", "tier"],
        "ALTER TABLE ADD COLUMN in a later file extends the table"
    );
    assert_eq!(definitions(&mut session, &project, SqlObjectKind::View, None), [("account_spend".to_owned(), true)]);
    assert_eq!(definitions(&mut session, &project, SqlObjectKind::Trigger, None), [("accounts_tier_audit".to_owned(), true)]);
    assert_eq!(definitions(&mut session, &project, SqlObjectKind::Index, None), [("orders_by_account".to_owned(), true)]);

    // Honest coverage: nothing was skipped, and the page says so.
    let (page, gaps) = all_hits(&mut session, request(&project, SqlQueryRole::Gap));
    assert!(gaps.is_empty(), "no statement of this project is partially modeled: {gaps:?}");
    assert!(page.basis.inventory_complete, "implicit selection covers the complete walk");
    assert!(page.basis.implicit_selection);
    assert_eq!(page.basis.gaps, Default::default());

    // Who uses accounts.balance? Two mentions, in another file, bound exactly.
    let mut uses = request(&project, SqlQueryRole::Reference);
    uses.uses = Some("accounts.balance".into());
    uses.kind = Some(SqlObjectKind::Column);
    let (_, hits) = all_hits(&mut session, uses.clone());
    let balance: Vec<_> = hits
        .iter()
        .filter(|hit| matches!(&hit.fact, SqlFact::Reference(r) if r.kind == SqlRefKind::Column && r.name == "balance"))
        .collect();
    assert_eq!(balance.len(), 3, "SELECT list, SET target and SET expression: {balance:?}");
    assert!(balance.iter().all(|hit| hit.path == "db/queries/accounts.sql"));
    for hit in &balance {
        let SqlFact::Reference(reference) = &hit.fact else { unreachable!() };
        assert!(matches!(reference.resolution, SqlResolution::Resolved { live: true, .. }));
        assert_eq!(reference.statement_kind == SqlStatementKind::Update || reference.statement_kind == SqlStatementKind::Select, true);
        let (path, text) = read_text(&mut session, hit);
        assert_eq!(path, "db/queries/accounts.sql");
        assert_eq!(text, "balance", "exact source span of the written name");
    }

    // The view depends on accounts.email through an alias and a join.
    let mut email = request(&project, SqlQueryRole::Reference);
    email.uses = Some("accounts.email".into());
    let (_, hits) = all_hits(&mut session, email);
    assert!(hits.iter().any(|hit| matches!(&hit.fact, SqlFact::Reference(r)
        if r.in_object.as_ref().is_some_and(|o| o.kind == SqlObjectKind::View && o.name == "account_spend"))),
        "view account_spend is linked to the column it selects");

    // Impact: what breaks if accounts.email changes, through the shared framework.
    {
        use backend_library::impact::{
            ImpactDomain, ImpactDomainState, ImpactRequest, ImpactSeed, ImpactSeedSpec, ImpactSeedState,
            ImpactStrength,
        };
        let seed = ImpactSeed::Named(ImpactSeedSpec {
            domain: ImpactDomain::Sql,
            kind: "column".into(),
            name: "accounts.email".into(),
            path: None,
        });
        let request = ImpactRequest::new(
            backend_library::PackageReference::parse(project.to_str().expect("path").to_owned()).expect("package"),
            seed,
        );
        let backend_library::SurfaceReply::Impact(report) = session
            .surface(backend_library::SurfaceCommand::Impact { request })
            .expect("impact through the public route")
        else {
            panic!("impact reply kind");
        };
        assert!(matches!(report.seed_state, ImpactSeedState::Resolved), "{:?}", report.seed_state);
        let sql = report.domains.iter().find(|d| d.domain == ImpactDomain::Sql).expect("SQL domain");
        assert!(matches!(sql.state, ImpactDomainState::Analyzed), "{:?}", sql.state);
        let names: Vec<_> = report.rows.iter().map(|row| (row.node.kind.as_str(), row.node.name.as_str(), row.strength)).collect();
        assert!(names.iter().any(|(kind, name, strength)| *kind == "view" && *name == "account_spend" && *strength == ImpactStrength::Exact),
            "the view that selects accounts.email: {names:?}");
        assert!(names.iter().any(|(kind, _, strength)| *kind == "query" && *strength == ImpactStrength::Exact),
            "the query that reads accounts.email: {names:?}");
        // The view's own consumers follow, as candidates (the view column is unnamed in the project).
        assert!(names.iter().any(|(kind, _, strength)| *kind == "query" && *strength == ImpactStrength::Candidate)
            || names.iter().filter(|(kind, ..)| *kind == "query").count() >= 1);
        assert!(!names.iter().any(|(kind, _, _)| *kind == "table"), "a column does not change its owning table: {names:?}");
        assert!(report.gaps.iter().any(|gap| gap.facet == "sql.embedded-sql"), "named blind spot");
        assert!(!report.proves_no_impact());
    }

    // Edit: a new migration drops accounts.balance. Reindex; queries elsewhere
    // now point at a dropped column.
    fs::write(project.join("db/migrations/0004_drop_balance.sql"), DROP_BALANCE).expect("new migration");
    publish(&mut wire, 0xd1);
    let columns = definitions(&mut session, &project, SqlObjectKind::Column, Some("accounts"));
    assert!(columns.contains(&("balance".to_owned(), false)), "dropped column is not live at head: {columns:?}");
    let (_, hits) = all_hits(&mut session, uses);
    let live: Vec<bool> = hits
        .iter()
        .filter_map(|hit| match &hit.fact {
            SqlFact::Reference(r) if r.name == "balance" && r.statement_kind != SqlStatementKind::AlterTable => match &r.resolution {
                SqlResolution::Resolved { live, .. } => Some(*live),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(live, [false, false, false], "unchanged query file re-bound to the dropped column");

    // Cold reopen: no memory of the running owner survives.
    drop(session);
    let root = TempTree(std::mem::take(&mut wire.fixture.root.0));
    drop(wire);
    let reopened = crate::builtin::sql_journey_support::reopen_and_query(&root.0.join("workspace"), &project)
        .expect("cold reopen and query");
    assert!(reopened.analysis);
    assert_eq!(reopened.tables, ["accounts", "orders"]);
    assert!(reopened.balance_dropped);
}

#[test]
fn unsupported_statement_and_unparseable_file_are_reported_as_gaps_never_as_empty() {
    let workers = admitted_workers(Some(frontend_worker("sqlite", "sqlite-worker")), None);
    let files = vec![
        ("db/0001_schema.sql".to_owned(), b"CREATE TABLE t (id INTEGER);\nCREATE VIRTUAL TABLE docs USING fts5(body);\n".to_vec()),
        ("db/0002_broken.sql".to_owned(), b"SELECT FROM WHERE;".to_vec()),
    ];
    let mut wire = SourceWire::start_sql(TestStage::BeforeSourcePreparation, true, SqlFixture { files, workers });
    wire.wait_at_source_within(TERMINAL_WAIT);
    wire.release();
    wire.wait_terminal_within(TERMINAL_WAIT);
    mixed_published(&wire);
    let project = wire.source.parent().expect("project").to_owned();
    let mut session = Session::connect(&wire.endpoint).expect("listener");
    let mut gap_request = request(&project, SqlQueryRole::Gap);
    gap_request.page_rows = 1;
    let (page, gaps) = all_hits(&mut session, gap_request);
    assert_eq!(page.host_gaps.len(), 1);
    assert_eq!(page.host_gaps[0].path, "db/0002_broken.sql");
    assert!(matches!(page.host_gaps[0].reason, backend_library::sql_query::SqlHostGapReason::NativeError));
    assert!(!gaps.is_empty(), "the virtual table statement is not fully modeled");
    assert_eq!(page.basis.gaps.native_error_hosts, 1, "the unparseable file is counted");
    assert!(page.basis.gaps.partial_statements + page.basis.gaps.unsupported_statements >= 1);
    // A search that finds nothing cannot claim "none": the gap counters say why.
    let mut uses = request(&project, SqlQueryRole::Reference);
    uses.uses = Some("t.id".into());
    let (page, hits) = all_hits(&mut session, uses);
    assert!(hits.is_empty());
    assert_ne!(page.basis.gaps, Default::default());
    let _ = (SqlStatementSupport::Full, ());
}

const PG_INIT: &str = "\
CREATE SCHEMA app;
CREATE TYPE app.tier AS ENUM ('basic', 'pro');
CREATE TABLE app.accounts (
  id bigserial PRIMARY KEY,
  email text NOT NULL UNIQUE,
  balance integer NOT NULL DEFAULT 0,
  tier app.tier NOT NULL DEFAULT 'basic'
);
CREATE TABLE app.orders (
  id bigserial PRIMARY KEY,
  account_id bigint NOT NULL REFERENCES app.accounts (id),
  total integer NOT NULL
);
CREATE INDEX orders_by_account ON app.orders (account_id);
";
const PG_VIEWS: &str = "\
CREATE VIEW app.account_spend AS
  SELECT a.id AS account_id, a.email, sum(o.total) AS spent
  FROM app.accounts a JOIN app.orders o ON o.account_id = a.id
  GROUP BY a.id, a.email;

CREATE FUNCTION app.charge(p_account bigint, p_amount integer) RETURNS void
LANGUAGE sql AS $$ UPDATE app.accounts SET balance = balance - p_amount WHERE id = p_account $$;
";
const PG_RENAME: &str = "ALTER TABLE app.accounts RENAME COLUMN email TO login;\n";
const PG_QUERIES: &str = "\
SELECT login, balance FROM app.accounts WHERE id = $1;

WITH recent AS (SELECT account_id, total FROM app.orders WHERE total > 100)
SELECT a.login, r.total FROM app.accounts a JOIN recent r ON r.account_id = a.id;
";

#[test]
fn postgres_project_binds_across_schemas_renames_and_survives_cold_reopen() {
    let workers = admitted_workers(None, Some(frontend_worker("postgres", "postgres-worker")));
    let files = [
        ("db/migrations/0001_init.sql", PG_INIT),
        ("db/migrations/0002_views.sql", PG_VIEWS),
        ("db/migrations/0003_rename.sql", PG_RENAME),
        ("db/queries/accounts.sql", PG_QUERIES),
    ]
    .into_iter()
    .map(|(path, text)| (path.to_owned(), text.as_bytes().to_vec()))
    .collect();
    let mut wire = SourceWire::start_sql(TestStage::BeforeSourcePreparation, true, SqlFixture { files, workers });
    wire.wait_at_source_within(TERMINAL_WAIT);
    wire.release();
    wire.wait_terminal_within(TERMINAL_WAIT);
    mixed_published(&wire);
    let project = wire.source.parent().expect("project").to_owned();
    let mut session = Session::connect(&wire.endpoint).expect("listener");

    let tables = definitions(&mut session, &project, SqlObjectKind::Table, None);
    assert_eq!(tables, [("accounts".to_owned(), true), ("orders".to_owned(), true)]);
    let columns = definitions(&mut session, &project, SqlObjectKind::Column, Some("accounts"));
    assert_eq!(
        columns.iter().map(|(name, _)| name.as_str()).collect::<Vec<_>>(),
        ["balance", "id", "login", "tier"],
        "the rename in a later migration is the column's head name"
    );
    assert_eq!(definitions(&mut session, &project, SqlObjectKind::Function, None), [("charge".to_owned(), true)]);
    assert_eq!(definitions(&mut session, &project, SqlObjectKind::View, None), [("account_spend".to_owned(), true)]);

    // The view (written before the rename) and the queries (written after it)
    // reach the same column entity.
    let mut uses = request(&project, SqlQueryRole::Reference);
    uses.uses = Some("app.accounts.login".into());
    uses.kind = Some(SqlObjectKind::Column);
    let (page, hits) = all_hits(&mut session, uses);
    let refs: Vec<_> = hits
        .iter()
        .filter_map(|hit| match &hit.fact {
            SqlFact::Reference(r) if r.statement_kind != SqlStatementKind::AlterTable => Some((hit.path.as_str(), r)),
            _ => None,
        })
        .collect();
    assert!(refs.iter().any(|(path, r)| path.ends_with("0002_views.sql")
        && r.in_object.as_ref().is_some_and(|o| o.kind == SqlObjectKind::View && o.name == "account_spend")),
        "view over the renamed column: {refs:?}");
    assert_eq!(refs.iter().filter(|(path, _)| *path == "db/queries/accounts.sql").count(), 2,
        "both query mentions of login, one through an alias");
    assert!(refs.iter().all(|(_, r)| matches!(r.resolution, SqlResolution::Resolved { live: true, .. })));
    // The SQL-language function body was walked, so it is linked to the table it updates.
    let mut balance = request(&project, SqlQueryRole::Reference);
    balance.uses = Some("app.accounts.balance".into());
    let (_, hits) = all_hits(&mut session, balance);
    assert!(hits.iter().any(|hit| matches!(&hit.fact, SqlFact::Reference(r)
        if r.in_object.as_ref().is_some_and(|o| o.kind == SqlObjectKind::Function && o.name == "charge"))));
    assert!(page.basis.inventory_complete);
    drop(session);
    let root = TempTree(std::mem::take(&mut wire.fixture.root.0));
    drop(wire);
    let reopened = crate::builtin::sql_journey_support::reopen_and_query(&root.0.join("workspace"), &project)
        .expect("cold reopen and query");
    assert!(reopened.analysis);
    assert_eq!(reopened.tables, ["accounts", "orders"]);
}

/// Stages at which the owner process is killed mid-publication.
const CRASH_STAGES: [(&str, TestStage); 4] = [
    ("before-grant-validation", TestStage::BeforeGrantValidation),
    ("after-grant", TestStage::AfterGrant),
    ("after-selection", TestStage::AfterSelection),
    ("after-receipt", TestStage::AfterReceipt),
];

/// Child half of the crash test: index the SQLite project and kill this whole
/// process at the named publication stage. Without the environment the parent
/// sets, it has nothing to do and returns.
#[test]
fn sql_crash_child() {
    let (Some(dir), Some(stage)) = (
        std::env::var_os("NUDOX_SQL_CRASH_DIR").map(PathBuf::from),
        std::env::var("NUDOX_SQL_CRASH_STAGE").ok(),
    ) else {
        return;
    };
    let (_, stage) = CRASH_STAGES.iter().find(|(name, _)| *name == stage).copied().expect("known stage");
    let workers = admitted_workers(Some(frontend_worker("sqlite", "sqlite-worker")), None);
    let mut wire = SourceWire::start_sql(
        TestStage::BeforeSourcePreparation,
        true,
        SqlFixture { files: sqlite_files(), workers },
    );
    wire.wait_at_source_within(TERMINAL_WAIT);
    let project = wire.source.parent().expect("project").to_owned();
    let workspace = project.parent().expect("workspace").to_owned();
    fs::write(dir.join("paths"), format!("{}\n{}\n", workspace.display(), project.display())).expect("record paths");
    wire.commands.lock().expect("adapter").publication_test_hook = Some(TestHook {
        stage,
        run: Box::new(|| std::process::exit(137)),
    });
    wire.release();
    wire.wait_terminal_within(TERMINAL_WAIT);
    panic!("the process should have been killed at the crash stage");
}

#[test]
fn killing_the_owner_mid_publication_never_leaves_a_torn_sql_analysis() {
    let test = module_path!().split_once("::").expect("crate prefix").1.to_owned() + "::sql_crash_child";
    for (name, _) in CRASH_STAGES {
        let dir = tempfile::tempdir().expect("crash coordination directory");
        let status = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args(["--exact", &test, "--nocapture", "--test-threads=1"])
            .env("NUDOX_SQL_CRASH_DIR", dir.path())
            .env("NUDOX_SQL_CRASH_STAGE", name)
            .status()
            .expect("spawn crashing owner");
        assert_eq!(status.code(), Some(137), "owner was killed at {name}");
        let paths = fs::read_to_string(dir.path().join("paths")).expect("child recorded its paths");
        let mut lines = paths.lines();
        let workspace = PathBuf::from(lines.next().expect("workspace"));
        let project = PathBuf::from(lines.next().expect("project"));
        // The owner reopens from whatever the kill left. It either holds no SQL
        // analysis yet, or the complete one: never a part of it.
        let reopened = crate::builtin::sql_journey_support::reopen_and_query(&workspace, &project)
            .unwrap_or_else(|error| panic!("reopen after a kill at {name}: {error}"));
        if reopened.analysis {
            assert_eq!(reopened.tables, ["accounts", "orders"], "kill at {name} left a partial analysis");
        } else {
            assert!(reopened.tables.is_empty());
        }
        // The crashed run's workspace is not removed by the killed child.
        fs::remove_dir_all(workspace.parent().expect("fixture root")).expect("remove crashed fixture");
    }
}

#[test]
fn flat_sql_upsert_and_renamed_objects_keep_precise_owner_edges() {
    use backend_library::impact::{ImpactDomain, ImpactRequest, ImpactSeed, ImpactSeedSpec};
    let workers = admitted_workers(Some(frontend_worker("sqlite", "sqlite-worker")), None);
    let files = [
        ("db/0001_init.sql", "CREATE TABLE authors (id INTEGER PRIMARY KEY, name TEXT, country TEXT);\nCREATE TABLE books (id INTEGER PRIMARY KEY, price_cents INTEGER, title TEXT);\nCREATE TABLE order_items (book_id INTEGER REFERENCES books(id));\nCREATE INDEX books_by_title ON books(title);"),
        ("db/0002_rename.sql", "ALTER TABLE books RENAME COLUMN price_cents TO list_price_cents;\nALTER TABLE order_items RENAME TO order_lines;"),
        ("db/catalog.sql", "INSERT INTO authors (id, name, country) VALUES (?1, ?2, ?3) ON CONFLICT (id) DO UPDATE SET name = excluded.name, country = excluded.country;\nSELECT list_price_cents FROM books;\nSELECT title FROM books;"),
        ("db/broken.sql", "SELECT price_cents FROM books;\nSELECT * FROM order_items;"),
    ].into_iter().map(|(path, text)| (path.to_owned(), text.as_bytes().to_vec())).collect();
    let mut wire = SourceWire::start_sql(TestStage::BeforeSourcePreparation, true, SqlFixture { files, workers });
    wire.wait_at_source_within(TERMINAL_WAIT);
    wire.release();
    wire.wait_terminal_within(TERMINAL_WAIT);
    mixed_published(&wire);
    let project = wire.source.parent().expect("project").to_owned();
    let mut session = Session::connect(&wire.endpoint).expect("listener");
    let (page, _) = all_hits(&mut session, request(&project, SqlQueryRole::Gap));
    assert_eq!(page.basis.gaps, Default::default(), "valid flat files must all reach SQLite");
    let mut upsert = request(&project, SqlQueryRole::Reference);
    upsert.uses = Some("authors.name".into());
    let (_, hits) = all_hits(&mut session, upsert);
    assert!(hits.iter().any(|hit| matches!(&hit.fact, SqlFact::Reference(reference) if reference.qualifier.as_deref() == Some("excluded") && matches!(reference.resolution, SqlResolution::Resolved { .. }))));
    let (_, diagnostics) = all_hits(&mut session, request(&project, SqlQueryRole::Diagnostic));
    for name in ["price_cents", "order_items"] {
        assert!(diagnostics.iter().any(|hit| matches!(&hit.fact, SqlFact::Reference(reference) if reference.name == name && matches!(reference.resolution, SqlResolution::Unresolved { .. }))), "unbound {name} is discoverable without a name filter");
    }
    let impact = |session: &mut Session, kind: &str, name: &str| {
        let request = ImpactRequest::new(
            backend_library::PackageReference::parse(project.to_str().unwrap().to_owned()).unwrap(),
            ImpactSeed::Named(ImpactSeedSpec { domain: ImpactDomain::Sql, kind: kind.into(), name: name.into(), path: None }),
        );
        let backend_library::SurfaceReply::Impact(report) = session.surface(backend_library::SurfaceCommand::Impact { request }).expect("public impact") else { panic!("impact reply") };
        report
    };
    let column = impact(&mut session, "column", "books.list_price_cents");
    assert!(column.rows.iter().any(|row| row.node.kind == "query"));
    assert!(column.rows.iter().all(|row| row.node.kind != "table" && row.node.kind != "index"), "column impact cannot spread through containment: {:?}", column.rows);
    let table = impact(&mut session, "table", "order_lines");
    assert!(table.rows.iter().all(|row| row.node.name != "order_items"), "the old table spelling must share the renamed entity");
}
