//! Protocol v4 syntactic outline for SQLite: fixtures, exact renderings, tamper checks and the
//! real worker binary.
mod common;

use backend_sql_contract::{
    Binding, DefinitionAction, Gap, ObjectKind, Outline, OutlineCoverage, RefKind, RefRole,
    Reference, ScopeKind, StatementKind, StatementOutline, StatementSupport, Syntax,
};
use common::{analyze, outline, render, request, supports, text};
use std::path::Path;

const FIXTURES: &[&str] = &[
    "schema",
    "m001_add_column",
    "m002_rename_column",
    "m003_drop_column",
    "m004_rename_table",
    "views",
    "triggers",
    "indexes",
    "queries",
    "quoted",
    "unmodeled",
];

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/outline")
        .join(format!("{name}.sql"));
    std::fs::read_to_string(path).unwrap()
}

/// Independent decoder for a written name token: strips quoting, undoes doubled-quote escapes
/// and lowercases ASCII only.
fn decode_written(written: &str) -> String {
    let bytes = written.as_bytes();
    let (inner, quote) = match bytes[0] {
        b'"' | b'`' | b'\'' => (&written[1..written.len() - 1], Some(bytes[0] as char)),
        b'[' => (&written[1..written.len() - 1], None),
        _ => (written, None),
    };
    let mut out = match quote {
        Some(q) => inner.replace(&format!("{q}{q}"), &q.to_string()),
        None => inner.to_owned(),
    };
    out.make_ascii_lowercase();
    out
}

fn statement_text<'s>(src: &'s str, s: &StatementOutline) -> &'s str {
    text(src, &s.extent)
}

fn first<'o>(o: &'o Outline, src: &str, starts: &str) -> &'o StatementOutline {
    o.statements
        .iter()
        .find(|s| statement_text(src, s).starts_with(starts))
        .unwrap_or_else(|| panic!("no statement starting with {starts:?}"))
}

fn refs<'o>(
    s: &'o StatementOutline,
    kind: RefKind,
    role: RefRole,
) -> Vec<(&'o Option<String>, &'o str)> {
    s.references
        .iter()
        .filter(|r| r.kind == kind && r.role == role)
        .map(|r| (&r.qualifier, r.name.as_str()))
        .collect()
}

fn col<'o>(s: &'o StatementOutline, role: RefRole) -> Vec<String> {
    refs(s, RefKind::Column, role)
        .into_iter()
        .map(|(q, n)| match q {
            Some(q) => format!("{q}.{n}"),
            None => n.to_owned(),
        })
        .collect()
}

#[test]
fn every_fixture_validates_and_every_name_extent_is_the_written_spelling() {
    for name in FIXTURES {
        let src = fixture(name);
        let o = outline(&src);
        let expect_complete = *name != "unmodeled";
        assert_eq!(
            o.coverage == OutlineCoverage::Complete,
            expect_complete,
            "{name}"
        );
        let mut last_end = 0;
        for s in &o.statements {
            assert!(s.extent.start >= last_end, "{name}: statements ascend");
            last_end = s.extent.end;
            let st = statement_text(&src, s);
            assert!(!st.ends_with(';'), "{name}: no trailing semicolon");
            assert!(!st.starts_with(char::is_whitespace), "{name}");
            for d in &s.definitions {
                assert_eq!(
                    decode_written(text(&src, &d.name_extent)),
                    d.name,
                    "{name}: definition {}",
                    d.name
                );
                if let DefinitionAction::Rename { to, to_extent } = &d.action {
                    assert_eq!(&decode_written(text(&src, to_extent)), to, "{name}");
                }
            }
            for r in &s.references {
                if r.name == "*" {
                    assert_eq!(text(&src, &r.extent), "*");
                } else {
                    assert_eq!(
                        decode_written(text(&src, &r.extent)),
                        r.name,
                        "{name}: reference {}",
                        r.name
                    );
                }
                assert!(r.full_extent.start <= r.extent.start && r.extent.end == r.full_extent.end);
            }
            for e in &s.scope {
                assert!(e.region.start >= s.extent.start && e.region.end <= s.extent.end);
            }
        }
    }
}

#[test]
fn crlf_line_endings_keep_every_byte_extent_exact() {
    for name in FIXTURES {
        let lf = fixture(name);
        let crlf = lf.replace('\n', "\r\n");
        assert!(crlf.len() > lf.len() || !lf.contains('\n'));
        let a = outline(&lf);
        let b = outline(&crlf);
        // The rendering shows each extent as the text it covers; CRLF only adds the CR.
        assert_eq!(
            render(&lf, &a),
            render(&crlf, &b).replace("\\r", ""),
            "{name}: CRLF changes only the line endings inside multi-line extents"
        );
        assert_eq!(a.statements.len(), b.statements.len());
        // Names never contain a line break; their extents are byte-for-byte equal to the
        // spelling.
        for s in &b.statements {
            for r in &s.references {
                if r.name != "*" {
                    assert_eq!(decode_written(text(&crlf, &r.extent)), r.name);
                }
            }
        }
    }
}

#[test]
fn unicode_identifiers_and_crlf_have_exact_utf8_byte_offsets() {
    let src = "-- naïve café ☕\r\nCREATE TABLE CAFÉ (id INTEGER PRIMARY KEY, \"名前\" TEXT, [naïve col] TEXT);\r\n\
INSERT INTO café (\"名前\") VALUES ('日本語');\r\n\
SELECT \"名前\", [naïve col], café.id FROM café WHERE \"名前\" = '☕' AND CAFÉ.id > 0;\r\n";
    let o = outline(src);
    assert_eq!(o.coverage, OutlineCoverage::Complete);
    assert_eq!(o.statements.len(), 3);
    let create = &o.statements[0];
    let table = &create.definitions[0];
    assert_eq!(table.kind, ObjectKind::Table);
    // ASCII lowercase only: the non-ASCII capital stays as written.
    assert_eq!(table.name, "cafÉ");
    assert_eq!(text(src, &table.name_extent), "CAFÉ");
    assert_eq!(table.name_extent.start, src.find("CAFÉ").unwrap());
    assert_eq!(table.name_extent.end - table.name_extent.start, "CAFÉ".len());
    let cols: Vec<_> = create
        .definitions
        .iter()
        .filter(|d| d.kind == ObjectKind::Column)
        .map(|d| (d.name.as_str(), text(src, &d.name_extent), text(src, &d.extent)))
        .collect();
    assert_eq!(
        cols,
        vec![
            ("id", "id", "id INTEGER PRIMARY KEY"),
            ("名前", "\"名前\"", "\"名前\" TEXT"),
            ("naïve col", "[naïve col]", "[naïve col] TEXT"),
        ]
    );
    let insert = &o.statements[1];
    assert_eq!(insert.kind, StatementKind::Insert);
    let write_cols: Vec<_> = insert
        .references
        .iter()
        .filter(|r| r.role == RefRole::Write && r.kind == RefKind::Column)
        .map(|r| (r.name.as_str(), text(src, &r.extent)))
        .collect();
    assert_eq!(write_cols, vec![("名前", "\"名前\"")]);
    let select = &o.statements[2];
    let cols: Vec<_> = select
        .references
        .iter()
        .filter(|r| r.kind == RefKind::Column)
        .map(|r| (r.qualifier.clone(), r.name.as_str(), text(src, &r.full_extent)))
        .collect();
    assert_eq!(
        cols,
        vec![
            (None, "名前", "\"名前\""),
            (None, "naïve col", "[naïve col]"),
            (Some("café".to_owned()), "id", "café.id"),
            (None, "名前", "\"名前\""),
            (Some("cafÉ".to_owned()), "id", "CAFÉ.id"),
        ]
    );
    // Every extent lands on a character boundary and the whole outline validates (also
    // asserted by `outline`).
    assert!(o.validate(src.as_bytes()));
}

#[test]
fn quoted_identifiers_with_escapes_and_comments_between_tokens() {
    let src = fixture("quoted");
    let o = outline(&src);
    let create = &o.statements[0];
    let names: Vec<_> = create
        .definitions
        .iter()
        .map(|d| (d.name.as_str(), text(&src, &d.name_extent)))
        .collect();
    assert_eq!(
        names,
        vec![
            ("mixed case", "\"Mixed Case\""),
            ("id", "\"Id\""),
            ("back`tick", "`back``tick`"),
            ("br acket", "[br acket]"),
            ("dq\"x", "\"dq\"\"x\""),
        ]
    );
    let select = &o.statements[1];
    let cols: Vec<_> = select
        .references
        .iter()
        .filter(|r| r.kind == RefKind::Column)
        .map(|r| (r.qualifier.as_deref(), r.name.as_str(), text(&src, &r.full_extent)))
        .collect();
    assert_eq!(
        cols,
        vec![
            (Some("mixed case"), "id", "\"Mixed Case\".\"Id\""),
            (Some("mc"), "back`tick", "mc.`back``tick`"),
            (None, "dq\"x", "\"dq\"\"x\""),
            (
                Some("br acket"),
                "y",
                "[br acket] /* c3 */ . /* c4 */ y"
            ),
        ]
    );
    // A comment or line comment between a qualifier and its column is inside the full extent.
    let split = &o.statements[3];
    assert_eq!(
        text(&src, &split.references[0].full_extent),
        "t -- trailing\n  . /* mid */ col"
    );
    assert_eq!(text(&src, &split.references[0].extent), "col");
    assert_eq!(split.references[0].qualifier.as_deref(), Some("t"));
}

#[test]
fn double_quoted_bare_name_is_an_unqualified_column_reference_with_its_spelling() {
    // Documented DQS choice: emitted as a Column reference; the extent starts with a double
    // quote so the owner can apply SQLite's string fallback when no column resolves.
    let src = "SELECT \"not_a_column\", 'text', \"t\".\"c\" FROM t";
    let o = outline(src);
    let s = &o.statements[0];
    let cols: Vec<_> = s
        .references
        .iter()
        .filter(|r| r.kind == RefKind::Column)
        .map(|r| (r.qualifier.as_deref(), r.name.as_str(), text(src, &r.full_extent)))
        .collect();
    assert_eq!(
        cols,
        vec![
            (None, "not_a_column", "\"not_a_column\""),
            (Some("t"), "c", "\"t\".\"c\""),
        ]
    );
    // Single-quoted text is a literal.
    assert!(!s.references.iter().any(|r| r.name == "text"));
}

#[test]
fn schema_tables_columns_constraints_and_foreign_keys() {
    let src = fixture("schema");
    let o = outline(&src);
    assert_eq!(
        o.statements.iter().map(|s| s.kind).collect::<Vec<_>>(),
        vec![
            StatementKind::Other,
            StatementKind::CreateTable,
            StatementKind::CreateTable,
            StatementKind::CreateTable,
            StatementKind::CreateTable,
            StatementKind::CreateTable
        ]
    );
    let users = first(&o, &src, "CREATE TABLE users");
    let columns: Vec<_> = users
        .definitions
        .iter()
        .filter(|d| d.kind == ObjectKind::Column)
        .map(|d| (d.name.as_str(), d.detail.as_deref(), d.parent.as_deref()))
        .collect();
    assert_eq!(
        columns,
        vec![
            ("id", Some("INTEGER"), Some("users")),
            ("email", Some("TEXT"), Some("users")),
            ("display_name", Some("VARCHAR(80)"), Some("users")),
            ("legacy_flag", Some("INTEGER"), Some("users")),
            ("created_at", Some("TEXT"), Some("users")),
        ]
    );
    let email = users.definitions.iter().find(|d| d.name == "email").unwrap();
    assert_eq!(
        text(&src, &email.extent),
        "email TEXT NOT NULL UNIQUE COLLATE NOCASE"
    );
    // CHECK expressions: schema-role column and function references.
    assert_eq!(col(users, RefRole::Schema), vec!["legacy_flag", "email"]);
    assert_eq!(
        refs(users, RefKind::Function, RefRole::Schema)
            .into_iter()
            .map(|(_, n)| n)
            .collect::<Vec<_>>(),
        vec!["length"]
    );
    // The table itself is in scope for unqualified names in its constraints.
    assert!(users.scope.iter().any(|e| e.kind == ScopeKind::Relation
        && e.visible_as == "users"
        && e.relation.as_deref() == Some("users")));

    let products = first(&o, &src, "CREATE TABLE IF NOT EXISTS products");
    assert!(products.definitions[0].conditional);
    // GENERATED and CHECK expressions read price_cents.
    assert_eq!(
        col(products, RefRole::Schema),
        vec!["price_cents", "price_cents"]
    );

    let orders = first(&o, &src, "CREATE TABLE orders");
    // Column-level REFERENCES users (id): relation + column qualified by the parent.
    assert_eq!(
        refs(orders, RefKind::Relation, RefRole::Schema)
            .into_iter()
            .map(|(_, n)| n)
            .collect::<Vec<_>>(),
        vec!["users"]
    );
    assert!(col(orders, RefRole::Schema).contains(&"users.id".to_owned()));
    // DEFAULT (datetime('now')) is a function, not a column.
    assert!(refs(orders, RefKind::Function, RefRole::Schema)
        .iter()
        .any(|(_, n)| *n == "datetime"));

    let items = first(&o, &src, "CREATE TABLE order_items");
    // PRIMARY KEY / FOREIGN KEY lists are qualified by the table itself, referenced columns by
    // the parent table.
    assert_eq!(
        col(items, RefRole::Schema),
        vec![
            "order_items.order_id",
            "order_items.sku",
            "order_items.order_id",
            "orders.id",
            "order_items.sku",
            "products.sku",
        ]
    );
    assert_eq!(
        refs(items, RefKind::Relation, RefRole::Schema)
            .into_iter()
            .map(|(_, n)| n)
            .collect::<Vec<_>>(),
        vec!["orders", "products"]
    );
}

#[test]
fn migrations_add_rename_drop_column_and_rename_table() {
    let add_src = fixture("m001_add_column");
    let add = outline(&add_src);
    let d = &add.statements[0].definitions[0];
    assert_eq!(add.statements[0].kind, StatementKind::AlterTable);
    assert_eq!(
        (d.kind, &d.action, d.name.as_str(), d.parent.as_deref(), d.detail.as_deref()),
        (
            ObjectKind::Column,
            &DefinitionAction::AddColumn,
            "nickname",
            Some("users"),
            Some("TEXT")
        )
    );
    assert_eq!(text(&add_src, &d.extent), "nickname TEXT NOT NULL DEFAULT ''");
    assert_eq!(
        refs(&add.statements[0], RefKind::Relation, RefRole::Schema)
            .into_iter()
            .map(|(_, n)| n)
            .collect::<Vec<_>>(),
        vec!["users"]
    );

    let rc_src = fixture("m002_rename_column");
    let rc = outline(&rc_src);
    let d = &rc.statements[0].definitions[0];
    assert_eq!((d.kind, d.name.as_str()), (ObjectKind::Column, "nickname"));
    assert_eq!(d.parent.as_deref(), Some("users"));
    match &d.action {
        DefinitionAction::Rename { to, to_extent } => {
            assert_eq!(to, "handle");
            assert_eq!(text(&rc_src, to_extent), "handle");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(text(&rc_src, &d.name_extent), "nickname");
    assert_eq!(col(&rc.statements[0], RefRole::Schema), vec!["users.nickname"]);

    let dc_src = fixture("m003_drop_column");
    let dc = outline(&dc_src);
    let d = &dc.statements[0].definitions[0];
    assert_eq!(d.action, DefinitionAction::DropColumn);
    assert_eq!((d.name.as_str(), d.parent.as_deref()), ("legacy_flag", Some("users")));
    assert_eq!(col(&dc.statements[0], RefRole::Schema), vec!["users.legacy_flag"]);

    let rt_src = fixture("m004_rename_table");
    let rt = outline(&rt_src);
    let d = &rt.statements[0].definitions[0];
    assert_eq!((d.kind, d.name.as_str()), (ObjectKind::Table, "order_items"));
    match &d.action {
        DefinitionAction::Rename { to, to_extent } => {
            assert_eq!(to, "line_items");
            assert_eq!(text(&rt_src, to_extent), "line_items");
        }
        other => panic!("{other:?}"),
    }
    for o in [&add, &rc, &dc, &rt] {
        assert_eq!(o.coverage, OutlineCoverage::Complete);
    }
}

#[test]
fn view_over_join_with_aliases_and_a_cte() {
    let src = fixture("views");
    let o = outline(&src);
    let v = first(&o, &src, "CREATE VIEW order_totals");
    assert_eq!(v.kind, StatementKind::CreateView);
    assert_eq!(v.support, StatementSupport::Full);
    assert_eq!(v.definitions[0].kind, ObjectKind::View);
    // The whole body is a schema dependency.
    assert!(v.references.iter().all(|r| r.role == RefRole::Schema));
    let relations: Vec<_> = refs(v, RefKind::Relation, RefRole::Schema)
        .into_iter()
        .map(|(_, n)| n)
        .collect();
    assert_eq!(
        relations,
        vec!["order_items", "products", "orders", "users", "item_totals"]
    );
    let cte = v.scope.iter().find(|e| e.kind == ScopeKind::Cte).unwrap();
    assert_eq!(cte.visible_as, "item_totals");
    assert!(text(&src, &cte.region).starts_with("WITH item_totals AS ("));
    // Alias beats table name for the visible name; the relation is still recorded.
    let entries: Vec<_> = v
        .scope
        .iter()
        .filter(|e| e.kind == ScopeKind::Relation)
        .map(|e| (e.visible_as.as_str(), e.relation.as_deref().unwrap()))
        .collect();
    assert_eq!(
        entries,
        vec![
            ("oi", "order_items"),
            ("p", "products"),
            ("o", "orders"),
            ("u", "users"),
            ("t", "item_totals")
        ]
    );
    // The CTE's own FROM items are visible only inside its SELECT, the outer ones only in the
    // outer SELECT.
    let oi = v.scope.iter().find(|e| e.visible_as == "oi").unwrap();
    let o_entry = v.scope.iter().find(|e| e.visible_as == "o").unwrap();
    assert!(text(&src, &oi.region).starts_with("SELECT oi.order_id AS order_id"));
    assert!(text(&src, &o_entry.region).starts_with("SELECT o.id, u.email"));
    assert!(oi.region.end <= o_entry.region.start);
    // SUM is a function reference.
    assert!(refs(v, RefKind::Function, RefRole::Schema)
        .iter()
        .any(|(_, n)| *n == "sum"));

    // A view with an explicit column list defines those columns.
    let ue = first(&o, &src, "CREATE VIEW IF NOT EXISTS user_emails");
    assert!(ue.definitions[0].conditional);
    assert_eq!(ue.definitions[0].detail.as_deref(), Some("uid, address"));
    let cols: Vec<_> = ue
        .definitions
        .iter()
        .filter(|d| d.kind == ObjectKind::Column)
        .map(|d| (d.name.as_str(), d.parent.as_deref().unwrap()))
        .collect();
    assert_eq!(cols, vec![("uid", "user_emails"), ("address", "user_emails")]);
}

#[test]
fn triggers_scope_new_and_old_and_walk_their_bodies() {
    let src = fixture("triggers");
    let o = outline(&src);
    let t = first(&o, &src, "CREATE TRIGGER orders_touch");
    assert_eq!(t.kind, StatementKind::CreateTrigger);
    assert_eq!(t.support, StatementSupport::Full);
    let d = &t.definitions[0];
    assert_eq!(
        (d.kind, d.name.as_str(), d.parent.as_deref()),
        (ObjectKind::Trigger, "orders_touch", Some("orders"))
    );
    let rows: Vec<_> = t
        .scope
        .iter()
        .filter(|e| e.kind == ScopeKind::TriggerRow)
        .map(|e| e.visible_as.as_str())
        .collect();
    assert_eq!(rows, vec!["new", "old"]);
    // UPDATE OF status: a column of the trigger's table.
    assert!(col(t, RefRole::Schema).contains(&"orders.status".to_owned()));
    // WHEN reads NEW/OLD.
    let when: Vec<_> = t
        .references
        .iter()
        .filter(|r| r.qualifier.as_deref() == Some("new") && r.name == "status")
        .collect();
    assert!(!when.is_empty());
    assert_eq!(text(&src, &when[0].full_extent), "NEW.status");
    // Body: INSERT writes audit_log, UPDATE writes orders.
    let writes: Vec<_> = refs(t, RefKind::Relation, RefRole::Write)
        .into_iter()
        .map(|(_, n)| n)
        .collect();
    assert_eq!(writes, vec!["audit_log", "orders"]);
    // The UPDATE's own target is visible only inside that statement.
    let inner = t
        .scope
        .iter()
        .find(|e| e.kind == ScopeKind::Relation && e.visible_as == "orders")
        .unwrap();
    assert!(text(&src, &inner.region).starts_with("UPDATE orders SET"));

    // INSERT-only / DELETE-only events expose one pseudo row.
    let c = first(&o, &src, "CREATE TRIGGER IF NOT EXISTS orders_cleanup");
    assert!(c.definitions[0].conditional);
    let rows: Vec<_> = c
        .scope
        .iter()
        .filter(|e| e.kind == ScopeKind::TriggerRow)
        .map(|e| e.visible_as.as_str())
        .collect();
    assert_eq!(rows, vec!["old"]);
    assert_eq!(refs(c, RefKind::Relation, RefRole::Write)[0].1, "order_items");
}

#[test]
fn indexes_and_drops() {
    let src = fixture("indexes");
    let o = outline(&src);
    let i1 = first(&o, &src, "CREATE INDEX idx_orders_user");
    let d = &i1.definitions[0];
    assert_eq!(
        (d.kind, d.name.as_str(), d.parent.as_deref(), d.detail.as_deref()),
        (
            ObjectKind::Index,
            "idx_orders_user",
            Some("orders"),
            Some("user_id, placed_at DESC")
        )
    );
    assert_eq!(col(i1, RefRole::Schema), vec!["orders.user_id", "orders.placed_at"]);
    let i2 = first(&o, &src, "CREATE UNIQUE INDEX IF NOT EXISTS idx_users_email_lower");
    assert!(i2.definitions[0].conditional);
    // Expression key: the function and the column inside it (resolved through the scope entry).
    assert_eq!(col(i2, RefRole::Schema), vec!["email"]);
    assert_eq!(refs(i2, RefKind::Function, RefRole::Schema)[0].1, "lower");
    assert_eq!(i2.scope[0].relation.as_deref(), Some("users"));
    // Partial index: key columns bound, WHERE columns unqualified.
    let i3 = first(&o, &src, "CREATE INDEX idx_orders_open");
    assert_eq!(col(i3, RefRole::Schema), vec!["orders.status", "status"]);

    let drops: Vec<_> = o
        .statements
        .iter()
        .filter(|s| s.kind == StatementKind::Drop)
        .map(|s| {
            let d = &s.definitions[0];
            (d.kind, d.action.clone(), d.conditional, d.schema.clone(), d.name.clone())
        })
        .collect();
    assert_eq!(
        drops,
        vec![
            (ObjectKind::Index, DefinitionAction::Drop, true, None, "idx_old".to_owned()),
            (
                ObjectKind::Trigger,
                DefinitionAction::Drop,
                true,
                Some("main".to_owned()),
                "orders_old_trigger".to_owned()
            ),
            (ObjectKind::View, DefinitionAction::Drop, false, None, "v_gone".to_owned()),
            (ObjectKind::Table, DefinitionAction::Drop, true, None, "temp_stuff".to_owned()),
        ]
    );
    let kinds: Vec<_> = o
        .statements
        .iter()
        .filter(|s| s.kind == StatementKind::Drop)
        .map(|s| (s.references[0].kind, s.references[0].role))
        .collect();
    assert_eq!(
        kinds,
        vec![
            (RefKind::Index, RefRole::Schema),
            (RefKind::Trigger, RefRole::Schema),
            (RefKind::Relation, RefRole::Schema),
            (RefKind::Relation, RefRole::Schema)
        ]
    );
}

#[test]
fn queries_select_insert_update_delete_returning() {
    let src = fixture("queries");
    let o = outline(&src);
    assert_eq!(o.coverage, OutlineCoverage::Complete);
    let kinds: Vec<_> = o.statements.iter().map(|s| s.kind).collect();
    use StatementKind as K;
    assert_eq!(
        kinds,
        vec![
            K::Select,
            K::Select,
            K::Insert,
            K::Insert,
            K::Update,
            K::Update,
            K::Delete,
            K::Insert,
            K::Select,
            K::Select
        ]
    );
    // Aggregates are functions, the result alias is not a column, and HAVING/ORDER BY on it
    // adds nothing.
    let s0 = &o.statements[0];
    assert_eq!(refs(s0, RefKind::Function, RefRole::Read)[0].1, "count");
    assert!(!s0.references.iter().any(|r| r.name == "n_orders"));
    assert_eq!(
        col(s0, RefRole::Read),
        vec!["u.id", "u.email", "o.id", "o.user_id", "u.id", "u.legacy_flag", "u.id", "u.email"]
    );
    // Parameters are not references; star is a Column named "*".
    let s1 = &o.statements[1];
    assert_eq!(col(s1, RefRole::Read), vec!["*", "id", "email"]);
    assert_eq!(text(&src, &s1.references[0].extent), "*");

    // INSERT ... RETURNING
    let ins = &o.statements[2];
    assert_eq!(refs(ins, RefKind::Relation, RefRole::Write)[0].1, "users");
    assert_eq!(col(ins, RefRole::Write), vec!["users.email", "users.display_name"]);
    assert_eq!(col(ins, RefRole::Read), vec!["id", "created_at"]);

    // ON CONFLICT DO UPDATE: targets are Write, `excluded` is its own scope entry.
    let up = &o.statements[3];
    assert_eq!(
        col(up, RefRole::Write),
        vec![
            "products.sku",
            "products.name",
            "products.price_cents",
            "products.name",
            "products.price_cents"
        ]
    );
    let excluded = up.scope.iter().find(|e| e.visible_as == "excluded").unwrap();
    assert_eq!(excluded.relation.as_deref(), Some("products"));
    assert!(text(&src, &excluded.region).starts_with("ON CONFLICT (sku)"));
    assert!(col(up, RefRole::Read).contains(&"excluded.name".to_owned()));
    assert!(col(up, RefRole::Read).contains(&"products.price_cents".to_owned()));

    // UPDATE with a subquery and RETURNING.
    let upd = &o.statements[4];
    assert_eq!(col(upd, RefRole::Write), vec!["orders.status"]);
    assert_eq!(
        col(upd, RefRole::Read),
        vec!["user_id", "id", "legacy_flag", "id", "status"]
    );
    let sub_users = upd.scope.iter().find(|e| e.visible_as == "users").unwrap();
    assert_eq!(text(&src, &sub_users.region), "SELECT id FROM users WHERE legacy_flag = 1");

    // UPDATE ... FROM
    let upf = &o.statements[5];
    assert_eq!(col(upf, RefRole::Write), vec!["order_items.quantity"]);
    assert!(col(upf, RefRole::Read).contains(&"p.sku".to_owned()));
    assert!(col(upf, RefRole::Read).contains(&"order_items.sku".to_owned()));
    let vis: Vec<_> = upf.scope.iter().map(|e| e.visible_as.as_str()).collect();
    assert_eq!(vis, vec!["order_items", "p"]);

    // DELETE with a correlated subquery: the outer target is visible inside it.
    let del = &o.statements[6];
    assert_eq!(refs(del, RefKind::Relation, RefRole::Write)[0].1, "orders");
    let oi = del.scope.iter().find(|e| e.visible_as == "oi").unwrap();
    let outer = del.scope.iter().find(|e| e.visible_as == "orders").unwrap();
    assert!(outer.region.start <= oi.region.start && oi.region.end <= outer.region.end);
    assert!(oi.region.end - oi.region.start < outer.region.end - outer.region.start);
    assert!(col(del, RefRole::Read).contains(&"orders.id".to_owned()));

    // REPLACE is an INSERT with a SELECT source.
    let rep = &o.statements[7];
    assert_eq!(col(rep, RefRole::Write).len(), 3);
    assert_eq!(refs(rep, RefKind::Relation, RefRole::Read)[0].1, "products");

    // Table-valued function in FROM.
    let tvf = &o.statements[8];
    assert!(tvf.scope.iter().any(|e| e.kind == ScopeKind::Function && e.visible_as == "jv"));
    assert!(refs(tvf, RefKind::Function, RefRole::Read)
        .iter()
        .any(|(_, n)| *n == "json_each"));
    // Compound select with scalar subquery and a positional ORDER BY.
    let cmp = &o.statements[9];
    assert_eq!(refs(cmp, RefKind::Function, RefRole::Read)[0].1, "max");
    assert!(cmp.scope.iter().any(|e| e.visible_as == "oi"));
}

#[test]
fn star_forms_and_correlated_blocks() {
    let src = "SELECT t.*, u.*, *, count(*) FROM t, u";
    let o = outline(src);
    let stars: Vec<_> = o.statements[0]
        .references
        .iter()
        .filter(|r| r.name == "*")
        .map(|r| (r.schema.as_deref(), r.qualifier.as_deref(), text(src, &r.full_extent)))
        .collect();
    assert_eq!(
        stars,
        vec![
            (None, Some("t"), "t.*"),
            (None, Some("u"), "u.*"),
            (None, None, "*"),
        ]
    );
    // count(*) is not a star expansion.
    assert_eq!(stars.len(), 3);

    let src = "SELECT a.x FROM a WHERE EXISTS (SELECT 1 FROM b WHERE b.y = a.x)";
    let o = outline(src);
    let s = &o.statements[0];
    let a = s.scope.iter().find(|e| e.visible_as == "a").unwrap();
    let b = s.scope.iter().find(|e| e.visible_as == "b").unwrap();
    assert_eq!(text(src, &a.region), src);
    assert_eq!(text(src, &b.region), "SELECT 1 FROM b WHERE b.y = a.x");
}

#[test]
fn derived_tables_ctes_with_columns_and_compound_selects() {
    let src = "WITH RECURSIVE c(n, m) AS (SELECT 1, 2 UNION ALL SELECT n + 1, m FROM c WHERE n < 5), \
d AS (SELECT * FROM e) \
SELECT x.n, y.k FROM (SELECT n FROM c) AS x JOIN d AS y ON y.k = x.n \
UNION SELECT 1, 2 ORDER BY 1";
    let o = outline(src);
    let s = &o.statements[0];
    assert_eq!(s.kind, StatementKind::Select);
    assert_eq!(s.support, StatementSupport::Full);
    let c = s.scope.iter().find(|e| e.kind == ScopeKind::Cte && e.visible_as == "c").unwrap();
    assert_eq!(c.columns.as_deref(), Some(&["n".to_owned(), "m".to_owned()][..]));
    let d = s.scope.iter().find(|e| e.kind == ScopeKind::Cte && e.visible_as == "d").unwrap();
    assert_eq!(d.columns, None);
    let x = s.scope.iter().find(|e| e.visible_as == "x").unwrap();
    assert_eq!(x.kind, ScopeKind::Subquery);
    assert_eq!(x.relation, None);
    // CTEs are visible in the whole WITH statement.
    assert_eq!(text(src, &c.region), src);
    assert_eq!(text(src, &d.region), src);
    // JOIN ... ON reads both sides; ORDER BY 1 adds nothing.
    assert!(col(s, RefRole::Read).contains(&"y.k".to_owned()));
}

#[test]
fn unmodeled_constructs_are_partial_with_the_right_gap() {
    use Gap::*;
    use StatementSupport::*;
    let src = fixture("unmodeled");
    let o = outline(&src);
    assert_eq!(o.coverage, OutlineCoverage::Partial);
    let by_text: Vec<_> = o
        .statements
        .iter()
        .map(|s| (statement_text(&src, s).to_owned(), s.kind, s.support))
        .collect();
    let expect = [
        ("CREATE VIRTUAL TABLE docs", StatementKind::CreateTable, Partial(UnmodeledConstruct)),
        ("CREATE TABLE copy AS SELECT", StatementKind::CreateTable, Partial(UnmodeledConstruct)),
        ("ATTACH DATABASE", StatementKind::Other, Partial(UnmodeledStatement)),
        ("DETACH DATABASE", StatementKind::Other, Partial(UnmodeledStatement)),
        ("EXPLAIN QUERY PLAN", StatementKind::Other, Partial(UnmodeledStatement)),
        ("PRAGMA table_info(users)", StatementKind::Other, Partial(UnmodeledConstruct)),
        ("PRAGMA user_version = 3", StatementKind::Other, Full),
        ("REINDEX users", StatementKind::Other, Partial(UnmodeledConstruct)),
        ("ANALYZE", StatementKind::Other, Full),
        ("VACUUM", StatementKind::Other, Full),
        ("BEGIN", StatementKind::Other, Full),
        ("SAVEPOINT s1", StatementKind::Other, Full),
        ("RELEASE s1", StatementKind::Other, Full),
        ("COMMIT", StatementKind::Other, Full),
    ];
    assert_eq!(by_text.len(), expect.len());
    for ((got_text, kind, support), (prefix, want_kind, want_support)) in
        by_text.iter().zip(expect)
    {
        assert!(got_text.starts_with(prefix), "{got_text:?} vs {prefix}");
        assert_eq!((*kind, *support), (want_kind, want_support), "{got_text}");
    }
    // A statement with nothing to extract carries nothing.
    let begin = first(&o, &src, "BEGIN");
    assert!(begin.definitions.is_empty() && begin.references.is_empty() && begin.scope.is_empty());
    // CREATE TABLE AS SELECT still reports the table and the SELECT's references.
    let ctas = first(&o, &src, "CREATE TABLE copy");
    assert_eq!(ctas.definitions[0].name, "copy");
    assert!(ctas.definitions.iter().all(|d| d.kind != ObjectKind::Column));
    assert_eq!(refs(ctas, RefKind::Relation, RefRole::Read)[0].1, "users");
}

#[test]
fn one_partial_statement_makes_the_file_partial_and_all_full_makes_it_complete() {
    let full = outline("SELECT 1; SELECT 2");
    assert_eq!(full.coverage, OutlineCoverage::Complete);
    assert_eq!(
        supports(&full),
        vec![StatementSupport::Full, StatementSupport::Full]
    );
    let mixed = outline("SELECT 1; ATTACH 'x' AS y; SELECT 2");
    assert_eq!(mixed.coverage, OutlineCoverage::Partial);
    assert_eq!(
        supports(&mixed),
        vec![
            StatementSupport::Full,
            StatementSupport::Partial(Gap::UnmodeledStatement),
            StatementSupport::Full
        ]
    );
}

#[test]
fn outline_is_present_without_context_and_for_non_select_and_multi_statement_files() {
    let src = "CREATE TABLE a (x); INSERT INTO a VALUES (1); SELECT x FROM a";
    let response = analyze(src);
    assert_eq!(response.binding, Binding::MissingContext);
    assert!(matches!(response.syntax, Syntax::Parsed { statements: 3, .. }));
    assert_eq!(response.outline.as_ref().unwrap().statements.len(), 3);
    // Comment-only and empty files parse to an empty, complete outline.
    for src in ["", "-- nothing\n", "/* a */ ;"] {
        let response = analyze(src);
        assert!(matches!(response.syntax, Syntax::Parsed { .. }), "{src:?}");
        let o = response.outline.unwrap();
        assert!(o.statements.is_empty());
        assert_eq!(o.coverage, OutlineCoverage::Complete);
        assert!(o.validate(src.as_bytes()));
    }
}

#[test]
fn parse_errors_and_other_failures_carry_no_outline() {
    let response = analyze("SELECT FROM WHERE");
    assert_eq!(response.syntax, Syntax::NativeError);
    assert!(response.outline.is_none());
    let mut req = request("SELECT 1");
    req.version = 3;
    let response = backend_frontend_sqlite_experimental::analyze_native(&req);
    assert!(response.outline.is_none());
    assert!(!matches!(response.syntax, Syntax::Parsed { .. }));
    let response = analyze("SELECT 1 \u{0} ");
    assert!(response.outline.is_none());
}

#[test]
fn legacy_single_select_analysis_still_runs_next_to_the_outline() {
    use backend_frontend_sqlite_experimental::{ProposedCatalog, ProposedTable};
    use backend_sql_contract::SemanticContext;
    let payload = ProposedCatalog {
        epoch: 1,
        tables: vec![ProposedTable {
            name: "t".into(),
            columns: vec!["id".into()],
        }],
    }
    .encode()
    .unwrap();
    let mut req = request("SELECT id FROM t");
    req.context = Some(SemanticContext::Proposed {
        dialect: backend_sql_contract::Dialect::Sqlite,
        payload,
    });
    let response = backend_frontend_sqlite_experimental::analyze_native(&req);
    assert!(matches!(response.binding, Binding::ProposedAnalysis { .. }));
    let o = response.outline.unwrap();
    assert!(o.validate(&req.source));
    assert_eq!(col(&o.statements[0], RefRole::Read), vec!["id"]);
}

#[test]
fn validate_accepts_every_fixture_and_rejects_tampered_extents() {
    for name in FIXTURES {
        let src = fixture(name);
        let o = outline(&src);
        assert!(o.validate(src.as_bytes()), "{name}");
        // Different bytes of the same length may still validate structurally, but a source that
        // is not the one the outline came from at the edges must not.
        assert!(!o.validate(&src.as_bytes()[..src.len() / 2]) || o.statements.is_empty());
    }
    let src = fixture("schema");
    let good = outline(&src);

    let mut tampered = |f: &dyn Fn(&mut Outline)| {
        let mut o = good.clone();
        f(&mut o);
        assert!(!o.validate(src.as_bytes()));
    };
    tampered(&|o| o.statements[1].extent.end = src.len() + 1);
    tampered(&|o| o.statements[1].extent.start = o.statements[1].extent.end);
    tampered(&|o| o.statements[2].extent.start = o.statements[1].extent.start);
    tampered(&|o| {
        let before = o.statements[1].extent.start - 1;
        o.statements[1].definitions[1].name_extent.start = before;
    });
    tampered(&|o| {
        let r = &mut o.statements[2].references[0];
        r.extent.end += 1_000_000;
    });
    tampered(&|o| {
        let r = &mut o.statements[2].references[0];
        r.full_extent.start = r.extent.start + 1;
    });
    tampered(&|o| o.statements[1].scope[0].region.end = src.len() + 5);
    tampered(&|o| o.coverage = OutlineCoverage::Partial);
    tampered(&|o| o.statements[1].support = StatementSupport::Partial(Gap::Limit));
    tampered(&|o| o.statements[1].definitions[0].name.clear());
    tampered(&|o| {
        // A Column definition must have a parent.
        o.statements[1].definitions[1].parent = None;
    });
    tampered(&|o| {
        // A star reference whose extent is not "*".
        let s = &mut o.statements[2];
        s.references[0].name = "*".into();
    });
    // Multibyte: an extent that splits a character is not valid text.
    let uni = "SELECT \"名前\" FROM t";
    let mut o = outline(uni);
    assert!(o.validate(uni.as_bytes()));
    // The opening quote is one byte; two bytes in splits the three-byte character.
    o.statements[0].references[0].extent.start += 2;
    o.statements[0].references[0].full_extent.start += 2;
    assert!(!o.validate(uni.as_bytes()));
    // And the outline of one source is not valid against a source with different boundaries.
    let a = outline("SELECT \"名前\" FROM t");
    assert!(!a.validate("SELECT 'x' FROM t".as_bytes()));
}

#[test]
fn limits_are_reported_not_hidden() {
    // More statements than the contract carries: the last kept statement says so.
    let many = "SELECT 1;".repeat(backend_sql_contract::MAX_OUTLINE_STATEMENTS + 5);
    let o = outline(&many);
    assert_eq!(o.statements.len(), backend_sql_contract::MAX_OUTLINE_STATEMENTS);
    assert_eq!(o.coverage, OutlineCoverage::Partial);
    assert_eq!(
        o.statements.last().unwrap().support,
        StatementSupport::Partial(Gap::Limit)
    );

    // More items than the contract carries.
    let columns = vec!["a"; backend_sql_contract::MAX_OUTLINE_ITEMS + 100].join(",");
    let src = format!("SELECT {columns} FROM t; SELECT 1");
    let o = outline(&src);
    let s = &o.statements[0];
    assert_eq!(s.support, StatementSupport::Partial(Gap::Limit));
    assert_eq!(o.coverage, OutlineCoverage::Partial);
    // Once the budget is spent later statements are recorded without content.
    let later = &o.statements[1];
    assert_eq!(later.support, StatementSupport::Unsupported(Gap::Limit));
    assert!(later.definitions.is_empty() && later.references.is_empty() && later.scope.is_empty());
    assert!(
        s.definitions.len() + s.references.len() + s.scope.len()
            <= backend_sql_contract::MAX_OUTLINE_ITEMS
    );

    // An identifier longer than the contract allows is dropped and flagged.
    let long = "x".repeat(backend_sql_contract::MAX_OUTLINE_TEXT + 1);
    let src = format!("SELECT {long}, ok FROM t");
    let o = outline(&src);
    assert_eq!(o.statements[0].support, StatementSupport::Partial(Gap::Limit));
    assert_eq!(col(&o.statements[0], RefRole::Read), vec!["ok"]);

    // Very deep nesting never crashes the worker: it is either rejected by the parser or
    // reported as a limit.
    let depth = 5000;
    let src = format!("SELECT {}1{}", "(".repeat(depth), ")".repeat(depth));
    let response = analyze(&src);
    match response.syntax {
        Syntax::Parsed { .. } => {
            let o = response.outline.unwrap();
            assert!(o.validate(src.as_bytes()));
        }
        _ => assert!(response.outline.is_none()),
    }
    let src = format!("SELECT {}x{} FROM t", "(".repeat(depth), ")".repeat(depth));
    let response = analyze(&src);
    if let Syntax::Parsed { .. } = response.syntax {
        let o = response.outline.unwrap();
        assert!(o.validate(src.as_bytes()));
        assert_eq!(o.statements[0].support, StatementSupport::Partial(Gap::Limit));
    }
}

fn worker() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_sqlite-worker"))
}

/// The checked path runs the shared process supervisor, which signals the processes it
/// believes belong to its run; a worker spawned by a concurrent test in this binary can be
/// caught by it (observed as SIGKILL). Worker-spawning tests therefore take turns.
static WORKER_TURN: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn worker_turn() -> std::sync::MutexGuard<'static, ()> {
    WORKER_TURN.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[test]
fn real_worker_returns_the_same_validated_outline_for_every_fixture() {
    let _turn = worker_turn();
    for name in FIXTURES {
        let src = fixture(name);
        let req = request(&src);
        let in_process = backend_frontend_sqlite_experimental::analyze_native(&req);
        let from_worker = backend_frontend_sqlite_experimental::analyze(worker(), &req);
        assert!(
            matches!(from_worker.syntax, Syntax::Parsed { .. }),
            "{name}: {:?}",
            from_worker.syntax
        );
        let o = from_worker.outline.as_ref().expect("outline from worker");
        assert!(o.validate(&req.source), "{name}");
        assert_eq!(from_worker.outline, in_process.outline, "{name}");
    }
}

#[test]
fn checked_worker_path_verifies_every_outline_extent_against_host_bytes() {
    let _turn = worker_turn();
    use backend_sql_contract::{
        Cancellation, CheckedLaunch, ProcessEnvironment, ProcessLimits, ToolchainArtifact,
        VerifiedSourceText,
    };
    use std::time::Duration;
    let artifact = ToolchainArtifact::from_path(worker(), vec![]).unwrap();
    let cwd = std::env::current_dir().unwrap();
    let limits = ProcessLimits::new(
        16 * 1024 * 1024,
        1024 * 1024,
        Duration::from_secs(20),
        17 * 1024 * 1024,
    )
    .unwrap();
    let (cancel, _) = Cancellation::new();
    for name in FIXTURES {
        let src = fixture(name);
        let host = src.as_bytes();
        let verified = VerifiedSourceText::capture(host).unwrap();
        let req = backend_sql_contract::Request {
            version: backend_sql_contract::PROTOCOL_VERSION,
            expected_basis: Some(backend_frontend_sqlite_experimental::observed_basis()),
            source: host.to_vec(),
            settings: backend_sql_contract::Settings::ProcessDefaults,
            context: None,
        };
        let checked = backend_frontend_sqlite_experimental::analyze_checked(
            &req,
            host,
            verified.content(),
            verified.extent(0..host.len()).unwrap().source(),
            CheckedLaunch {
                program: worker(),
                workspace: &cwd,
                artifact: artifact.clone(),
                environment: ProcessEnvironment::new(vec![]).unwrap(),
                limits,
                cancellation: &cancel,
            },
        )
        .unwrap_or_else(|e| panic!("{name}: {e:?}"));
        let response = checked.response();
        let o = response.outline.as_ref().unwrap();
        // One verified host extent per claimed range, each matching the host bytes.
        assert_eq!(checked.extents().len(), o.extents().count(), "{name}");
        for (verified, claimed) in checked.extents().iter().zip(o.extents()) {
            assert_eq!(verified.text(), &src[claimed.start..claimed.end], "{name}");
        }
    }
}

#[test]
fn reference_helper_sanity() {
    // Guards the helpers the tests above lean on.
    let r = Reference {
        kind: RefKind::Column,
        role: RefRole::Read,
        schema: None,
        qualifier: None,
        name: "x".into(),
        extent: backend_sql_contract::Extent { start: 0, end: 1 },
        full_extent: backend_sql_contract::Extent { start: 0, end: 1 },
    };
    assert_eq!(decode_written("[A b]"), "a b");
    assert_eq!(decode_written("\"A\"\"B\""), "a\"b");
    assert_eq!(decode_written("`x``Y`"), "x`y");
    assert_eq!(r.name, "x");
}

#[test]
fn window_functions_and_named_windows_are_walked() {
    let src = "SELECT row_number() OVER (PARTITION BY a ORDER BY b ROWS BETWEEN 1 PRECEDING AND c FOLLOWING) AS r, \
sum(x) FILTER (WHERE y > 0) OVER w FROM t WINDOW w AS (PARTITION BY z)";
    let o = outline(src);
    let s = &o.statements[0];
    assert_eq!(s.support, StatementSupport::Full);
    let funcs: Vec<_> = refs(s, RefKind::Function, RefRole::Read)
        .into_iter()
        .map(|(_, n)| n)
        .collect();
    assert_eq!(funcs, vec!["row_number", "sum"]);
    // The window name `w` is not a column.
    assert_eq!(col(s, RefRole::Read), vec!["a", "b", "c", "x", "y", "z"]);
}

#[test]
fn temp_objects_are_recorded_in_the_temp_schema() {
    let src = "CREATE TEMP TABLE scratch (a); CREATE TEMPORARY VIEW v AS SELECT a FROM scratch; \
CREATE TEMP TRIGGER tg AFTER INSERT ON scratch BEGIN SELECT 1; END; CREATE TABLE main.real (a)";
    let o = outline(src);
    let schemas: Vec<_> = o
        .statements
        .iter()
        .map(|s| (s.definitions[0].name.as_str(), s.definitions[0].schema.as_deref()))
        .collect();
    assert_eq!(
        schemas,
        vec![
            ("scratch", Some("temp")),
            ("v", Some("temp")),
            ("tg", Some("temp")),
            ("real", Some("main"))
        ]
    );
    assert_eq!(o.coverage, OutlineCoverage::Complete);
}

/// Truncated and word-deleted variants of the fixtures reach odd parser states. Whatever the
/// parser makes of them, the worker neither panics nor returns an outline that fails
/// validation.
#[test]
fn damaged_inputs_never_panic_and_never_yield_an_invalid_outline() {
    let mut checked = 0;
    let mut parsed = 0;
    for name in FIXTURES {
        let src = fixture(name);
        let check = |variant: &str| {
            let response = analyze(variant);
            match response.syntax {
                Syntax::Parsed { .. } => {
                    let o = response
                        .outline
                        .as_ref()
                        .unwrap_or_else(|| panic!("parsed without outline: {variant:?}"));
                    assert!(o.validate(variant.as_bytes()), "invalid for {variant:?}");
                    true
                }
                _ => {
                    assert!(response.outline.is_none());
                    false
                }
            }
        };
        for end in (0..=src.len()).filter(|&i| src.is_char_boundary(i)) {
            checked += 1;
            parsed += usize::from(check(&src[..end]));
        }
        // Delete each whitespace-separated word in turn.
        let words: Vec<&str> = src.split(' ').collect();
        for skip in 0..words.len() {
            let variant: Vec<&str> = words
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != skip)
                .map(|(_, w)| *w)
                .collect();
            checked += 1;
            parsed += usize::from(check(&variant.join(" ")));
        }
    }
    assert!(checked > 5000 && parsed > 500, "{checked} {parsed}");
}
