//! Adversarial integration tests for the `rusqdoltlite` engine binding.
//!
//! These exercise the full compile → open → commit → branch → merge → time-travel
//! lifecycle against the real vendored DoltLite engine, plus the error taxonomy
//! and byte-fidelity edge cases the catalog depends on.

use rusqdoltlite::{
	BranchName, Connection, DoltConnectionExtension, EngineError, MergeOutcome, Value,
};

/// Open an in-memory database and create a small versioned table.
fn open_with_table() -> Connection {
	let connection = Connection::open_in_memory().expect("open in-memory database");
	connection
		.execute(
			"CREATE TABLE item(id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
			&[],
		)
		.expect("create table");
	connection
}

#[test]
fn open_create_insert_commit_and_read_back() {
	let connection = open_with_table();
	let changed = connection
		.execute(
			"INSERT INTO item(id, name) VALUES (?, ?)",
			&[Value::from(1_i64), Value::from("alpha")],
		)
		.expect("insert row");
	assert_eq!(changed, 1);

	let commit = connection.dolt_commit("initial").expect("commit");
	assert!(!commit.as_str().is_empty());

	let names: Vec<String> = connection
		.query_rows("SELECT name FROM item ORDER BY id", &[], |row| row.get_text(0))
		.expect("query names");
	assert_eq!(names, vec!["alpha".to_string()]);
}

#[test]
fn head_advances_after_each_commit() {
	let connection = open_with_table();
	connection
		.execute("INSERT INTO item VALUES (1, 'a')", &[])
		.expect("insert");
	let first = connection.dolt_commit("first").expect("first commit");
	assert_eq!(connection.head().expect("head after first"), first);

	connection
		.execute("INSERT INTO item VALUES (2, 'b')", &[])
		.expect("insert");
	let second = connection.dolt_commit("second").expect("second commit");
	assert_eq!(connection.head().expect("head after second"), second);
	assert_ne!(first, second);
}

#[test]
fn fast_forward_merge_is_classified() {
	let connection = open_with_table();
	connection
		.execute("INSERT INTO item VALUES (1, 'a')", &[])
		.expect("insert");
	connection.dolt_commit("base").expect("base commit");

	let feature = BranchName::parse("feature").expect("valid branch");
	connection.dolt_branch_create(&feature).expect("branch");
	connection.dolt_checkout(&feature).expect("checkout feature");
	connection
		.execute("INSERT INTO item VALUES (2, 'b')", &[])
		.expect("insert on feature");
	let feature_head = connection.dolt_commit("add b").expect("feature commit");

	let main = BranchName::parse("main").expect("valid branch");
	connection.dolt_checkout(&main).expect("checkout main");

	// main has not diverged, so merging feature fast-forwards to its head.
	let outcome = connection.dolt_merge(&feature).expect("merge feature");
	match outcome {
		MergeOutcome::FastForward { new_head } => assert_eq!(new_head, feature_head),
		other => panic!("expected fast-forward, got {other:?}"),
	}
}

#[test]
fn true_three_way_merge_creates_merge_commit() {
	let connection = open_with_table();
	connection
		.execute("INSERT INTO item VALUES (1, 'a')", &[])
		.expect("insert");
	connection.dolt_commit("base").expect("base commit");

	let feature = BranchName::parse("feature").expect("valid branch");
	let main = BranchName::parse("main").expect("valid branch");

	// Diverge feature: add row 2.
	connection.dolt_branch_create(&feature).expect("branch");
	connection.dolt_checkout(&feature).expect("checkout feature");
	connection
		.execute("INSERT INTO item VALUES (2, 'b')", &[])
		.expect("insert on feature");
	connection.dolt_commit("feature b").expect("feature commit");

	// Diverge main independently: add row 3 (non-conflicting).
	connection.dolt_checkout(&main).expect("checkout main");
	connection
		.execute("INSERT INTO item VALUES (3, 'c')", &[])
		.expect("insert on main");
	let main_head = connection.dolt_commit("main c").expect("main commit");

	let outcome = connection.dolt_merge(&feature).expect("merge feature");
	match outcome {
		MergeOutcome::MergeCommit { new_head } => {
			assert_ne!(new_head, main_head, "merge commit is a fresh hash");
		}
		other => panic!("expected merge commit, got {other:?}"),
	}

	// All three rows are present after a clean three-way merge.
	let count: Vec<i64> = connection
		.query_rows("SELECT count(*) FROM item", &[], |row| row.get_integer(0))
		.expect("count rows");
	assert_eq!(count, vec![3]);
}

#[test]
fn conflicting_merge_reports_conflicts_with_table_list() {
	let connection = open_with_table();
	connection
		.execute("INSERT INTO item VALUES (1, 'original')", &[])
		.expect("insert");
	connection.dolt_commit("base").expect("base commit");

	let feature = BranchName::parse("feature").expect("valid branch");
	let main = BranchName::parse("main").expect("valid branch");

	// feature rewrites row 1.
	connection.dolt_branch_create(&feature).expect("branch");
	connection.dolt_checkout(&feature).expect("checkout feature");
	connection
		.execute("UPDATE item SET name = 'feature-value' WHERE id = 1", &[])
		.expect("update on feature");
	connection.dolt_commit("feature edit").expect("feature commit");

	// main rewrites the same row differently → row-level conflict.
	connection.dolt_checkout(&main).expect("checkout main");
	connection
		.execute("UPDATE item SET name = 'main-value' WHERE id = 1", &[])
		.expect("update on main");
	connection.dolt_commit("main edit").expect("main commit");

	let outcome = connection.dolt_merge(&feature).expect("merge should not hard-error");
	match outcome {
		MergeOutcome::Conflicts { conflicted_tables } => {
			assert!(
				conflicted_tables.iter().any(|table| table == "item"),
				"expected `item` in conflicted tables, got {conflicted_tables:?}"
			);
		}
		other => panic!("expected conflicts, got {other:?}"),
	}
}

#[test]
fn as_of_time_returns_old_head_and_none_before_history() {
	let connection = open_with_table();
	connection
		.execute("INSERT INTO item VALUES (1, 'a')", &[])
		.expect("insert");
	let first = connection.dolt_commit("first").expect("first commit");

	connection
		.execute("INSERT INTO item VALUES (2, 'b')", &[])
		.expect("insert");
	connection.dolt_commit("second").expect("second commit");

	// Far-future instant resolves to the current (newest) head.
	let far_future_ms = 32_503_680_000_000_i64; // year ~3000
	let resolved = connection
		.resolve_as_of_time(far_future_ms)
		.expect("resolve future");
	assert!(resolved.is_some(), "a commit exists before the far future");

	// Instant before all history resolves to nothing.
	let before_history = connection
		.resolve_as_of_time(0)
		.expect("resolve epoch");
	assert!(before_history.is_none(), "no commit predates the Unix epoch");

	// `first` must still be reachable in the log (sanity on history retention).
	let log_hashes: Vec<String> = connection
		.query_rows("SELECT commit_hash FROM dolt_log", &[], |row| row.get_text(0))
		.expect("read log");
	assert!(log_hashes.iter().any(|hash| hash == first.as_str()));
}

#[test]
fn as_of_read_returns_old_rows_via_time_travel_table() {
	// DoltLite exposes point-in-time reads through `dolt_at_<table>(ref)`.
	let connection = open_with_table();
	connection
		.execute("INSERT INTO item VALUES (1, 'a')", &[])
		.expect("insert");
	connection.dolt_commit("one row").expect("commit one");

	connection
		.execute("INSERT INTO item VALUES (2, 'b')", &[])
		.expect("insert");
	connection.dolt_commit("two rows").expect("commit two");

	// Present state has two rows.
	let now: Vec<i64> = connection
		.query_rows("SELECT count(*) FROM item", &[], |row| row.get_integer(0))
		.expect("count now");
	assert_eq!(now, vec![2]);

	// One commit back had exactly one row.
	let past: Vec<i64> = connection
		.query_rows(
			"SELECT count(*) FROM dolt_at_item('HEAD~1')",
			&[],
			|row| row.get_integer(0),
		)
		.expect("count past");
	assert_eq!(past, vec![1]);
}

#[test]
fn malformed_sql_lands_in_sql_error_variant() {
	let connection = Connection::open_in_memory().expect("open");
	let error = connection
		.execute("SELCT nonsense FROM nowhere", &[])
		.expect_err("malformed SQL must error");
	match error {
		EngineError::Sql { sql, .. } => {
			assert!(sql.contains("SELCT"), "error carries the offending SQL");
		}
		other => panic!("expected Sql variant, got {other:?}"),
	}
}

#[test]
fn constraint_violation_is_typed() {
	let connection = open_with_table();
	connection
		.execute("INSERT INTO item VALUES (1, 'a')", &[])
		.expect("first insert");
	let error = connection
		.execute("INSERT INTO item VALUES (1, 'duplicate')", &[])
		.expect_err("primary-key clash must error");
	assert!(
		matches!(error, EngineError::Constraint { .. }),
		"expected Constraint, got {error:?}"
	);
}

#[test]
fn commit_with_nothing_staged_is_a_dolt_error() {
	let connection = open_with_table();
	connection
		.execute("INSERT INTO item VALUES (1, 'a')", &[])
		.expect("insert");
	connection.dolt_commit("initial").expect("first commit");

	// Nothing changed since the last commit.
	let error = connection
		.dolt_commit("empty")
		.expect_err("empty commit must error");
	assert!(
		matches!(error, EngineError::Dolt { .. }),
		"expected Dolt error, got {error:?}"
	);
}

#[test]
fn text_parameter_with_interior_nul_round_trips() {
	// Value::Text binds with an explicit byte length, so an interior NUL is a
	// legitimate character rather than a string terminator.
	let connection = open_with_table();
	let payload = String::from("before\u{0}after");
	connection
		.execute(
			"INSERT INTO item(id, name) VALUES (1, ?)",
			&[Value::from(payload.clone())],
		)
		.expect("insert NUL-bearing text");

	let read_back: Vec<String> = connection
		.query_rows("SELECT name FROM item WHERE id = 1", &[], |row| row.get_text(0))
		.expect("read back");
	assert_eq!(read_back, vec![payload]);
}

#[test]
fn blob_round_trips_byte_for_byte() {
	let connection = Connection::open_in_memory().expect("open");
	connection
		.execute("CREATE TABLE b(id INTEGER PRIMARY KEY, data BLOB)", &[])
		.expect("create blob table");
	let bytes: Vec<u8> = (0u8..=255u8).collect();
	connection
		.execute(
			"INSERT INTO b VALUES (1, ?)",
			&[Value::from(bytes.clone())],
		)
		.expect("insert blob");

	let read_back: Vec<Vec<u8>> = connection
		.query_rows("SELECT data FROM b WHERE id = 1", &[], |row| row.get_blob(0))
		.expect("read blob");
	assert_eq!(read_back, vec![bytes]);
}

#[test]
fn typed_getter_rejects_wrong_type() {
	let connection = Connection::open_in_memory().expect("open");
	let error = connection
		.query_rows("SELECT 'text-not-int'", &[], |row| row.get_integer(0))
		.expect_err("integer getter on text must error");
	assert!(
		matches!(error, EngineError::TypeMismatch { .. }),
		"expected TypeMismatch, got {error:?}"
	);
}

#[test]
fn null_column_maps_to_none_for_optional_getter() {
	let connection = Connection::open_in_memory().expect("open");
	let values: Vec<Option<i64>> = connection
		.query_rows("SELECT NULL", &[], |row| row.get_optional_integer(0))
		.expect("optional getter");
	assert_eq!(values, vec![None]);
}

#[test]
fn transaction_rolls_back_on_drop() {
	let mut connection = open_with_table();
	{
		let transaction = connection.transaction().expect("begin");
		transaction
			.execute("INSERT INTO item VALUES (1, 'a')", &[])
			.expect("insert in txn");
		// Drop without commit → rollback.
	}
	let count: Vec<i64> = connection
		.query_rows("SELECT count(*) FROM item", &[], |row| row.get_integer(0))
		.expect("count");
	assert_eq!(count, vec![0], "rolled-back insert is not visible");
}

#[test]
fn transaction_commit_persists() {
	let mut connection = open_with_table();
	{
		let transaction = connection.transaction().expect("begin");
		transaction
			.execute("INSERT INTO item VALUES (1, 'a')", &[])
			.expect("insert in txn");
		transaction.commit().expect("commit txn");
	}
	let count: Vec<i64> = connection
		.query_rows("SELECT count(*) FROM item", &[], |row| row.get_integer(0))
		.expect("count");
	assert_eq!(count, vec![1]);
}

#[test]
fn garbage_collection_succeeds_and_preserves_reachable_history() {
	let connection = open_with_table();
	connection
		.execute("INSERT INTO item VALUES (1, 'a')", &[])
		.expect("insert");
	connection.dolt_commit("keep me").expect("commit");

	connection.dolt_gc().expect("gc must succeed");

	// Reachable data survives collection.
	let count: Vec<i64> = connection
		.query_rows("SELECT count(*) FROM item", &[], |row| row.get_integer(0))
		.expect("count after gc");
	assert_eq!(count, vec![1]);
}

#[test]
fn invalid_branch_name_is_rejected_before_touching_the_engine() {
	assert!(BranchName::parse("-dangerous").is_err());
	assert!(BranchName::parse("has space").is_err());
	assert!(BranchName::parse("a..b").is_err());
	assert!(BranchName::parse("main").is_ok());
	assert!(BranchName::parse("local/device-1").is_ok());
}

#[test]
fn two_connections_to_the_same_file_each_open() {
	// Documents DoltLite's multi-connection behaviour for the catalog's
	// single-writer discipline: two connections to the same on-disk database can
	// both open and read. Writes are serialized by the engine's file locking; the
	// catalog layer above this crate keeps a single advertised writer, so this
	// test only asserts that a second reader connection is viable and sees
	// committed data.
	let directory = std::env::temp_dir().join(format!(
		"rusqdoltlite-concurrent-{}",
		std::process::id()
	));
	let _ = std::fs::create_dir_all(&directory);
	let path = directory.join("catalog.dolt");
	let path_string = path.to_string_lossy().into_owned();
	let _ = std::fs::remove_file(&path);

	{
		let writer = Connection::open(&path_string).expect("open writer");
		writer
			.execute("CREATE TABLE item(id INTEGER PRIMARY KEY, name TEXT)", &[])
			.expect("create");
		writer
			.execute("INSERT INTO item VALUES (1, 'committed')", &[])
			.expect("insert");
		writer.dolt_commit("seed").expect("commit");
	}

	// A fresh reader connection observes the committed row.
	let reader = Connection::open(&path_string).expect("open reader");
	let names: Vec<String> = reader
		.query_rows("SELECT name FROM item ORDER BY id", &[], |row| row.get_text(0))
		.expect("read");
	assert_eq!(names, vec!["committed".to_string()]);

	let _ = std::fs::remove_file(&path);
}
