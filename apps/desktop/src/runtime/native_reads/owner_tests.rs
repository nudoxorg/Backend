//! The SQL, CSS and Impact reads against a real owner: the framed listener,
//! the public `impact`, `sql-query` and `stylo-query` commands, and the
//! reply admission, on a project of SQL, CSS and TSX files.
//!
//! The project is never indexed, so the owner holds no analysis of it. What this
//! proves is the route and the honesty: the pages read the real replies (and
//! their admission), and a project the owner has not analysed is reported as
//! unavailable, never as having no such table, class or impact. (An index of a
//! project of SQL and CSS only, in this build, which installs neither worker,
//! was also tried: the owner answered after 15 minutes and the client had
//! given up. Not a test; recorded in the lane notes.)

#![allow(clippy::expect_used, clippy::panic)]

use super::*;
use crate::host::lease::DesktopHost;
use crate::model::browse::BrowseKey;
use crate::model::native::Verdict;
use crate::runtime::reads::{OutlineCache, PageReader, ReadContext, ReadRequest, SessionReader};
use backend_runtime::WorkspacePaths;
use std::path::Path;

fn project(tag: &str) -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos();
    let root = crate::host::scratch_base().join(format!("nx-native-{tag}-{}-{nonce}", std::process::id()));
    crate::host::private_dir(&root).expect("private root");
    let project = root.join("shop");
    for dir in ["db", "queries", "src"] {
        std::fs::create_dir_all(project.join(dir)).expect("dir");
    }
    std::fs::write(project.join("db/001_init.sql"), "CREATE TABLE orders (id INTEGER PRIMARY KEY, total INTEGER NOT NULL);\n").expect("sql");
    std::fs::write(project.join("queries/recent.sql"), "SELECT id, total FROM orders ORDER BY id DESC;\n").expect("query");
    std::fs::write(project.join("src/card.css"), ".card { color: var(--brand); }\n:root { --brand: #336699; }\n").expect("css");
    std::fs::write(project.join("src/Card.tsx"), "import './card.css';\nexport function Card() { return <div className=\"card\" />; }\n").expect("tsx");
    project.canonicalize().expect("canonical project")
}

fn read(endpoint: &Path, key: BrowseKey) -> Result<crate::model::browse::BrowseValue, ReadFailure> {
    let mut reader = SessionReader::connect(endpoint);
    let cancel = CancellationToken::new();
    let outlines = OutlineCache::default();
    let context = ReadContext { worker: 0, cancel: &cancel, outlines: &outlines, progress: None };
    match reader.read(&ReadRequest::Browse(key), &context)? {
        PageValue::Browse(value) => Ok(value),
        other => panic!("a browse read answered {other:?}"),
    }
}

#[test]
fn a_real_owner_answers_the_three_pages_and_never_calls_an_unanalysed_project_empty() {
    let root = project("journey").parent().expect("scratch root").to_path_buf();
    let project_dir = root.join("shop").canonicalize().expect("project");
    let paths = WorkspacePaths::discover(Some(project_dir.clone()), Some(root.join("data")), Some(root.with_extension("sock"))).expect("paths");
    let host = DesktopHost::start_with_paths(paths).expect("the owner starts");
    let local = crate::core::LocalProjectId::from_path(&project_dir).expect("project id");
    let table = NativeSubject::new(local.clone(), NativeKind::SqlTable, "orders", None).expect("table");
    let class = NativeSubject::new(local.clone(), NativeKind::CssClass, "card", None).expect("class");

    let Ok(crate::model::browse::BrowseValue::Sql(sql)) = read(host.endpoint(), BrowseKey::Sql(table.clone())) else { panic!("the SQL page did not read") };
    assert!(!sql.coverage.analysed && !sql.coverage.complete, "{:?}", sql.coverage);
    assert!(matches!(&sql.object, SqlObject::Unavailable(why) if why.contains("not read") || why.contains("not been analysed")), "an unanalysed project is unavailable, not absent: {:?}", sql.object);
    assert!(sql.definitions.is_empty() && sql.uses.is_empty());
    assert!(matches!(sql.users.state, UsersState::Unavailable(_)));

    let Ok(crate::model::browse::BrowseValue::Css(css)) = read(host.endpoint(), BrowseKey::Css(class.clone())) else { panic!("the CSS page did not read") };
    assert!(matches!(&css.state, CssState::Unavailable(_)), "{:?}", css.state);
    assert!(css.rules.is_empty() && !css.coverage.analysed);

    for subject in [table, class] {
        let Ok(crate::model::browse::BrowseValue::Impact(impact)) = read(host.endpoint(), BrowseKey::Impact(ImpactRoute::new(subject.clone()))) else { panic!("the impact page did not read") };
        assert!(matches!(impact.seed, SeedView::Unavailable(_) | SeedView::NotFound(_)), "{:?}", impact.seed);
        assert!(impact.rows.is_empty());
        assert!(!impact.proves_no_impact);
        assert!(
            matches!(impact.verdict(), Verdict::NothingSearched(_) | Verdict::NameNotFound(_)),
            "an unanalysed project never reads as no impact: {:?}",
            impact.verdict()
        );
        assert!(!impact.verdict().sentence(&subject).contains("Nothing depends"));
    }
    drop(host);
    let _ = std::fs::remove_dir_all(&root);
}
