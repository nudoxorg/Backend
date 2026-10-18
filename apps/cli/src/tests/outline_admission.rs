//! Real immutable catalog/window/DTO laws; no compiler or application credit.
use super::*;
use backend_library::{
    Basis, CommandReply, DeclarationKind, Frontier, Library, PageRequest, QueryLimit, QueryWindow,
    Row, RowId, ViewRoot, object_version, package_key, symbol_key, view_key, view_state_root,
};
use std::collections::BTreeSet;

const PROJECT: &str = "/cli-dense-outline";

fn catalog(count: usize, version: u8, wide: bool) -> Library {
    let basis = Basis::new(
        view_state_root(&[("outline".to_owned(), version.to_string())]),
        object_version(&[version]),
    );
    let rows = (0..count)
        .map(|n| {
            let name = if wide {
                "界".repeat(8192)
            } else {
                format!("Café_אב_{n:04}_{}", "member".repeat(8))
            };
            let coordinate = format!(
                "{PROJECT}::semantic::{}::{name}",
                backend_library::encode_id(symbol_key(&format!("{n}")).as_bytes())
            );
            Row::in_package(
                RowId::Symbol(symbol_key(&coordinate)),
                basis,
                package_key(PROJECT),
                coordinate,
            )
            .with_kind(DeclarationKind::Struct)
        })
        .chain(std::iter::once(Row::new(
            RowId::Package(package_key(PROJECT)),
            basis,
            PROJECT,
        )))
        .collect();
    let root = ViewRoot::new_incomplete(
        view_key(b"cli dense outline"),
        basis,
        Frontier::new(basis.branch, basis.log, basis.schema, basis.root, 0),
        rows,
        Vec::new(),
    )
    .expect("structural catalog; no compiler coverage");
    let cursor = backend_library::Cursor::for_view_root(&root);
    Library::from_view(root, cursor).expect("native catalog")
}

struct Outline {
    catalog: Library,
    windows: Vec<QueryWindow>,
    inputs: Vec<Option<PageContinuation>>,
    imported: Option<PageContinuation>,
    imports: usize,
    move_after_first: bool,
    native_ceiling_first: bool,
}
impl Outline {
    fn new(count: usize, wide: bool) -> Self {
        Self {
            catalog: catalog(count, 1, wide),
            windows: vec![],
            inputs: vec![],
            imported: None,
            imports: 0,
            move_after_first: false,
            native_ceiling_first: false,
        }
    }
}
impl Engine for Outline {
    fn revision(&mut self) -> Result<ViewStateRoot, ClientError> {
        unreachable!("page owns basis")
    }
    fn health(&mut self) -> Result<HealthReport, ClientError> {
        unreachable!("no extra health")
    }
    fn probe(&mut self, _: Probe<'_>) -> Result<ReplyDto, ClientError> {
        unreachable!("native windows only")
    }
    fn prepare_query_continuation(&mut self, token: &str) -> Result<PageContinuation, ClientError> {
        assert_eq!(token, "unchanged-owner-input");
        self.imports += 1;
        self.imported.ok_or(ClientError::StaleCursor)
    }
    fn probe_page_window(
        &mut self,
        probe: Probe<'_>,
        continuation: Option<PageContinuation>,
        window: QueryWindow,
    ) -> Result<ReplyDto, ClientError> {
        assert!(
            matches!(probe, Probe::OutlinePage { path: PROJECT, limit } if limit == window.issued().get())
        );
        if self.move_after_first && !self.windows.is_empty() {
            self.catalog = catalog(320, 2, false);
        }
        self.windows.push(window);
        self.inputs.push(continuation);
        if self.native_ceiling_first && self.windows.len() == 1 {
            return Err(ClientError::ResponseTooLarge {
                basis: Some(self.catalog.revision_root().into()),
            });
        }
        let request = PageRequest::new(self.catalog.revision_root(), window.issued())
            .with_window(window)
            .ok_or_else(|| ClientError::Protocol("window changed issued credit".to_owned()))?;
        let request = continuation.map_or(request, |cursor| request.with_continuation(cursor));
        let page = self
            .catalog
            .outline_page(package_key(PROJECT), request)
            .map_err(|error| ClientError::Protocol(error.to_string()))?;
        Ok(ReplyDto::new(1, CommandReply::ProjectionPage(page)))
    }
}
fn collect(node: &backend_present::OutlineEntry, keys: &mut BTreeSet<String>) {
    let coordinate = node
        .identity()
        .expect("full identity")
        .coordinate()
        .as_str();
    assert!(
        keys.insert(coordinate.to_owned()),
        "no clipped or repeated row"
    );
    for child in node.children() {
        collect(child, keys);
    }
}

#[test]
fn cli_outline_real_full_frame_pages_preserve_original_credit_cursor_and_complete_union() {
    let mut engine = Outline::new(320, false);
    let expected: BTreeSet<_> = engine
        .catalog
        .view()
        .rows()
        .iter()
        .filter(|row| matches!(row.id, RowId::Symbol(_)))
        .map(|row| row.label.clone())
        .collect();
    let options = crate::options::split(&["--json", "--detail", "full"].map(str::to_owned))
        .expect("normal full CLI options")
        .0;
    let mut union = BTreeSet::new();
    let mut pages = 0;
    loop {
        let token = engine.imported.map(|_| "unchanged-owner-input");
        let prior = engine.windows.len();
        let answer = outline_with_output_admission(
            &mut engine,
            PROJECT,
            200,
            token,
            || Ok(()),
            |_, answer| {
                // Charge the entire continuation in the actual CLI encoding, including escaping.
                let cursor = answer
                    .continuation()
                    .map(|_| "owner-proof-אב-\"-\\".repeat(64));
                crate::render::admit_answer_with_cursor(answer, &options, cursor.as_deref())
            },
        )
        .expect("complete bounded native page");
        let Answer::Outline(tree) = &answer else {
            panic!("outline")
        };
        let encoded = backend_present::encode_answer(
            &answer,
            backend_present::Detail::Full,
            None,
            backend_present::DEFAULT_RESPONSE_BUDGET_BYTES,
        )
        .expect("full typed frame");
        let dto: serde_json::Value = serde_json::from_slice(&encoded.bytes).expect("complete JSON");
        assert_eq!(dto["page"]["limit"], 200, "issued credit never shrinks");
        for node in tree.roots() {
            collect(node, &mut union);
        }
        assert!(
            engine.windows[prior..]
                .iter()
                .all(|window| window.issued().get() == 200)
        );
        assert!(
            engine.inputs[prior..]
                .iter()
                .all(|input| *input == engine.imported)
        );
        if token.is_some() {
            assert!(engine.imports >= engine.windows.len() - prior);
        }
        engine.imported = answer.continuation();
        pages += 1;
        assert!(pages < 100);
        if engine.imported.is_none() {
            break;
        }
    }
    assert!(pages > 1);
    assert_eq!(union, expected);
    assert!(
        engine
            .windows
            .iter()
            .any(|window| window.delivery().get() < 200)
    );
}

#[test]
fn cli_outline_source_movement_and_original_scope_refuse_before_another_admission() {
    for native_ceiling in [false, true] {
        let mut engine = Outline::new(320, false);
        engine.move_after_first = true;
        engine.native_ceiling_first = native_ceiling;
        let result = outline_with_output_admission(
            &mut engine,
            PROJECT,
            200,
            None,
            || Ok(()),
            |_, answer| {
                backend_present::encode_answer(answer, backend_present::Detail::Full, None, 1024)
                    .map(|_| ())
                    .map_err(backend_present::oversized_fault)
            },
        );
        assert!(result.is_err());
        assert_eq!(
            engine.windows.len(),
            2,
            "source movement cannot restart discovery"
        );
    }
    for after_reply in [false, true] {
        let mut engine = Outline::new(320, false);
        let mut checks = 0;
        let result = outline_with_output_admission(
            &mut engine,
            PROJECT,
            200,
            None,
            || {
                checks += 1;
                if !after_reply || checks > 1 {
                    Err(Fault::usage(
                        "scope",
                        "cancelled or expired original request",
                    ))
                } else {
                    Ok(())
                }
            },
            |_, _| panic!("expired scope must not admit output"),
        );
        assert!(result.is_err());
        assert_eq!(engine.windows.len(), usize::from(after_reply));
    }
}

#[test]
fn cli_outline_one_row_oversize_is_atomic_and_exported_cursor_is_part_of_budget() {
    let mut engine = Outline::new(1, true);
    let options = crate::options::split(&["--json", "--detail", "full"].map(str::to_owned))
        .expect("normal full CLI options")
        .0;
    let fault = outline_with_output_admission(
        &mut engine,
        PROJECT,
        1,
        None,
        || Ok(()),
        |_, answer| crate::render::admit_answer_with_cursor(answer, &options, None),
    )
    .expect_err("one complete row cannot fit; never truncate");
    assert_eq!(fault.cause().slug(), backend_present::CauseSlug::Oversized);
    assert_eq!(engine.windows.len(), 1);
    let mut engine = Outline::new(2, false);
    let answer =
        outline_with_output_admission(&mut engine, PROJECT, 1, None, || Ok(()), |_, _| Ok(()))
            .expect("tiny real page");
    crate::render::admit_answer_with_cursor(&answer, &options, Some("small-owner-proof"))
        .expect("small exact cursor fits");
    let fault =
        crate::render::admit_answer_with_cursor(&answer, &options, Some(&"x".repeat(49152)))
            .expect_err("same fields plus real emitted token no longer fit");
    assert_eq!(fault.cause().slug(), backend_present::CauseSlug::Oversized);
}

#[test]
fn cli_outline_invalid_credit_and_unissued_cursor_never_probe_or_reset() {
    for (credit, cursor) in [(0, None), (200, Some("unchanged-owner-input"))] {
        let mut engine = Outline::new(320, false);
        let result = outline_with_output_admission(
            &mut engine,
            PROJECT,
            credit,
            cursor,
            || Ok(()),
            |_, _| panic!("invalid input must never reach output admission"),
        );
        assert!(result.is_err());
        assert!(
            engine.windows.is_empty(),
            "no fresh first page replaces an invalid cursor"
        );
    }
}
