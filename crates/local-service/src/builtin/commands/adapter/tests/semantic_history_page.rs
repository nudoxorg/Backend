//! Genuine native Python publication and strict owner history reads. The
//! cursor is restored through a cold owner without another indexing request.
use super::*;
use backend_library::{
    CommandReply, IndexOperationState, PackageReference, SemanticHistoryPublicationStatus,
    SemanticVersionsCursor, SemanticVersionsPage, SemanticVersionsPageState, SurfaceCommand,
    SurfaceReply,
};

fn read(fixture: &mut AdapterFixture, id: u64, command: SurfaceCommand) -> SurfaceReply {
    let request = backend_library::CommandDto::new(id, Command::Surface(command));
    let bytes = serde_json::to_vec(&request).expect("normal versioned surface request");
    let (adapter, daemon) = fixture.parts();
    let Executed::Reply(bytes) = adapter
        .execute_or_defer(daemon, &bytes, id)
        .expect("actual owner history route")
    else {
        panic!("history read never admits an indexing operation");
    };
    let reply = backend_library::decode_reply_body(&bytes).expect("strict wire decoder");
    backend_library::admit_reply(&request, &reply).expect("exact command/page admission");
    assert_eq!(reply.request_id, id);
    let CommandReply::Surface(reply) = reply.reply else {
        panic!("successful retained-history response");
    };
    reply
}

fn page(
    fixture: &mut AdapterFixture,
    package: &PackageReference,
    cursor: Option<SemanticVersionsCursor>,
) -> SemanticVersionsPage {
    let SurfaceReply::SemanticVersionsPage(page) = read(
        fixture,
        890,
        SurfaceCommand::SemanticVersionsPage {
            package: package.clone(),
            limit: 8,
            cursor,
        },
    ) else {
        panic!("typed bounded history page");
    };
    page
}

fn published_page(
    fixture: &mut AdapterFixture,
    package: &PackageReference,
    cursor: Option<SemanticVersionsCursor>,
) -> SemanticVersionsPage {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    loop {
        let page = page(fixture, package, cursor.clone());
        let published = page.entries.iter().any(|entry| {
            matches!(
                entry.version.history_status,
                SemanticHistoryPublicationStatus::PublishedCatalog { .. }
            )
        });
        if published {
            return page;
        }
        if cursor.is_some() {
            assert_eq!(page.state, SemanticVersionsPageState::Preparing);
            assert!(page.entries.is_empty());
            assert_eq!(page.next_cursor, cursor, "cold wait never advances");
        }
        assert!(
            std::time::Instant::now() < deadline,
            "genuine selected native history did not publish within 30s: {page:?}"
        );
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn native_large_history_pages_resume_exact_saved_cursor_after_owner_reopen() {
    let mut fixture = AdapterFixture::new();
    let (_, label) = fixture.add_target();
    fs::write(
        Path::new(&label).join("pyproject.toml"),
        "[project]\nname=\"paged_native_history\"\nversion=\"1.0.0\"\n",
    )
    .expect("real project manifest");
    for ordinal in 0..133 {
        fs::write(
            Path::new(&label).join(format!("source_{ordinal:03}.py")),
            format!(
                "def exact_value_{ordinal}(value: int) -> int:\n    return value + {ordinal}\n"
            ),
        )
        .expect("distinct genuine compiler input");
    }
    install_mixed_native_compiler(&mut fixture);
    let operation = run_mixed_native_operation(&mut fixture, &label, 0xc1);
    assert!(matches!(
        operation.state,
        IndexOperationState::Published { .. }
    ));
    let package = PackageReference::parse(label).expect("actual project operand");
    let first = published_page(&mut fixture, &package, None);
    assert_eq!(first.state, SemanticVersionsPageState::Ready);
    assert_eq!(first.version_count, 1);
    assert_eq!(first.entries[0].version.artifacts, 133);
    assert_eq!(
        first.entries[0]
            .version
            .selected_source_frontier
            .as_ref()
            .expect("genuine selected source")
            .file_count,
        133
    );
    let basis = first.basis.clone();
    let (adapter, daemon) = fixture.parts();
    let snapshot = daemon.engine().daemon().owner().snapshot();
    assert_eq!(
        basis.semantic_relation_root,
        *snapshot
            .relation::<BuiltinSemanticRelation>()
            .expect("semantic root")
            .root()
            .as_bytes()
    );
    assert_eq!(
        basis.source_relation_root,
        *snapshot
            .relation::<crate::builtin::BuiltinWorkspaceRelation>()
            .expect("source root")
            .root()
            .as_bytes()
    );
    assert!(adapter.indexing.is_none());
    let SurfaceReply::SemanticVersions(original) = read(
        &mut fixture,
        891,
        SurfaceCommand::SemanticVersions {
            package: package.clone(),
        },
    ) else {
        panic!("original full owner history remains readable internally");
    };
    assert!(
        serde_json::to_vec(&original)
            .expect("original complete history")
            .len()
            > 49_152
    );
    let SemanticHistoryPublicationStatus::Published { proof, .. } = &original[0].history_status
    else {
        panic!("genuine complete proof");
    };
    let original = proof.clone();
    let mut images = first
        .entries
        .iter()
        .flat_map(|entry| entry.images.iter().cloned())
        .collect::<Vec<_>>();
    let SemanticHistoryPublicationStatus::PublishedCatalog { catalog, .. } =
        &first.entries[0].version.history_status
    else {
        panic!("whole catalog header");
    };
    let catalog = catalog.clone();
    let saved = first
        .next_cursor
        .clone()
        .expect("large catalog must continue");
    let hot_work = fixture
        .parts()
        .0
        .semantic_authority
        .semantic_versions_catalog_work();
    let repeated = page(&mut fixture, &package, Some(saved.clone()));
    assert_eq!(repeated.entries[0].start_ordinal, 8);
    let reused_work = fixture
        .parts()
        .0
        .semantic_authority
        .semantic_versions_catalog_work();
    assert_eq!(reused_work.builds, hot_work.builds);
    assert_eq!(reused_work.admitted_images, hot_work.admitted_images);
    assert_eq!(reused_work.hashed_bytes, hot_work.hashed_bytes);
    assert_eq!(
        reused_work.hits,
        hot_work.hits + 1,
        "real owner route reuses the exact observation"
    );
    fixture = fixture.reopen();
    let mut cursor = Some(saved);
    let mut pages = 1;
    let mut cold_ready_work: Option<
        crate::builtin::semantic_authority::SemanticVersionsCatalogWork,
    > = None;
    loop {
        let page = published_page(&mut fixture, &package, cursor.clone());
        let current_work = fixture
            .parts()
            .0
            .semantic_authority
            .semantic_versions_catalog_work();
        if let Some(first_ready) = cold_ready_work {
            assert_eq!(current_work.builds, first_ready.builds);
            assert_eq!(current_work.admitted_images, first_ready.admitted_images);
            assert_eq!(current_work.hashed_bytes, first_ready.hashed_bytes);
        } else {
            assert_eq!(
                current_work.admitted_images, 133,
                "cold admitted catalog is built once"
            );
            assert!(current_work.hashed_bytes > 49_152);
            cold_ready_work = Some(current_work);
        }
        assert_eq!(
            page.basis, basis,
            "cold root/catalog identity remains exact"
        );
        assert_eq!(page.entries[0].start_ordinal as usize, images.len());
        assert!(!page.entries[0].first_segment);
        assert!(
            serde_json::to_vec(&page)
                .expect("complete typed page")
                .len()
                <= backend_library::MAX_SEMANTIC_HISTORY_PAGE_BYTES
        );
        for entry in &page.entries {
            assert_eq!(
                entry.version.generation,
                first.entries[0].version.generation
            );
            assert_eq!(
                entry.version.selected_source_frontier,
                first.entries[0].version.selected_source_frontier
            );
            images.extend_from_slice(&entry.images);
        }
        pages += 1;
        assert!(pages <= 133, "no duplicate/nonadvancing ready page");
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(
        images.as_slice(),
        original.images.as_ref(),
        "no gap, duplicate or changed proof"
    );
    assert_eq!(
        catalog
            .assemble(images.into_boxed_slice())
            .expect("complete proof reassembled"),
        original
    );
    let (adapter, daemon) = fixture.parts();
    assert!(
        adapter.indexing.is_none(),
        "cold history observation never reindexes"
    );
    assert_eq!(
        adapter
            .resolve_index_operation(daemon, operation.operation_key, None)
            .expect("retained operation"),
        backend_library::IndexOperationObservation::Known(operation)
    );
}
