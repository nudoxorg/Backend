//! Direct controls on the real engine view-refresh capabilities. These use
//! admitted product fixtures and the original workspace lease and view sink.
use super::*;
use backend_engine::{PreparedReadHeadRefresh, ViewPersistence};

fn prepare_refresh(fixture: &mut AdapterFixture) -> PreparedReadHeadRefresh<BuiltinModel> {
    let (_, daemon) = fixture.parts();
    let owner = daemon.engine_mut().daemon_mut();
    let snapshot = owner.owner().snapshot();
    let view = owner.library().view().clone();
    let cursor = owner.library().cursor();
    let admission = crate::builtin::BuiltinViewAdmission {
        workspace_root: snapshot.root(),
        source_root: view.basis().root,
    };
    owner
        .reserve_read_head_writer()
        .expect("reserve original read-head writer")
        .prepare_refresh_view(view, cursor, &admission, None)
        .unwrap_or_else(|(_, error)| panic!("prepare admitted view refresh: {error}"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PersistedIdentity {
    workspace: [u8; 32],
    view: [u8; 32],
    cursor: backend_engine::Cursor,
}

struct ControlledSink {
    journal: crate::builtin::ViewJournal,
    calls: Arc<Mutex<Vec<PersistedIdentity>>>,
}

impl ViewPersistence for ControlledSink {
    fn persist(
        &mut self,
        workspace: backend_version::WorkspaceRoot,
        view: &backend_library::ViewRoot,
        cursor: backend_engine::Cursor,
        event: Option<&backend_library::CursorEvent>,
    ) -> Result<(), String> {
        let ordinal = {
            let mut calls = self.calls.lock().expect("sink identity observer");
            calls.push(PersistedIdentity {
                workspace: workspace.to_bytes(),
                view: view.root().to_bytes(),
                cursor,
            });
            calls.len()
        };
        match ordinal {
            1 => Err("injected view sink refusal".to_owned()),
            2 => panic!("injected view sink unwind"),
            _ => self.journal.persist(workspace, view, cursor, event),
        }
    }
}

#[test]
fn refresh_sink_error_and_unwind_retain_the_exact_capability_and_original_sink() {
    let mut fixture = AdapterFixture::new();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let journal = crate::builtin::ViewJournal::open(fixture.root.0.join("refresh-view.journal"))
        .expect("actual durable view journal");
    let before = {
        let (_, daemon) = fixture.parts();
        let owner = daemon.engine_mut().daemon_mut();
        owner
            .set_view_persistence(Box::new(ControlledSink {
                journal,
                calls: Arc::clone(&calls),
            }))
            .unwrap_or_else(|(_, error)| panic!("install original sink: {error}"));
        (
            owner.owner().snapshot(),
            owner.library().view().clone(),
            owner.library().cursor(),
        )
    };
    let refresh = prepare_refresh(&mut fixture);
    let refresh = match refresh.persist() {
        Err((refresh, _)) => refresh,
        Ok(_) => panic!("actual sink refusal must retain preparation"),
    };
    let refresh = match refresh.persist() {
        Err((refresh, _)) => refresh,
        Ok(_) => panic!("actual sink unwind must retain preparation"),
    };
    {
        let (_, daemon) = fixture.parts();
        let owner = daemon.engine_mut().daemon_mut();
        assert_eq!(owner.owner().head().root(), before.0.root());
        assert_eq!(owner.owner().head().sequence(), before.0.sequence());
        assert_eq!(owner.library().view().root(), before.1.root());
        assert_eq!(owner.library().cursor(), before.2);
        assert!(
            owner.reserve_read_head_writer().is_err(),
            "original lease remains reserved"
        );
    }
    let published = refresh
        .persist()
        .unwrap_or_else(|(_, error)| panic!("retry same retained preparation: {error}"));
    let (_, daemon) = fixture.parts();
    let owner = daemon.engine_mut().daemon_mut();
    let retired = owner
        .install_read_head_refresh(published)
        .unwrap_or_else(|(_, error)| panic!("install original durable refresh: {error}"));
    drop(retired);
    let after = owner.owner().snapshot();
    assert_eq!(after.commit().id(), before.0.commit().id());
    assert_eq!(after.root(), before.0.root());
    assert_eq!(after.sequence(), before.0.sequence());
    assert_eq!(owner.library().cursor(), before.2);
    let admission = crate::builtin::BuiltinViewAdmission {
        workspace_root: after.root(),
        source_root: before.1.basis().root,
    };
    owner
        .publish_view(before.1, before.2, &admission, None)
        .expect("original sink is returned with the lease");
    let calls = calls.lock().expect("same sink observer");
    assert_eq!(
        calls.len(),
        4,
        "one sink survives refusal, unwind, retry and return"
    );
    assert!(
        calls.iter().all(|call| call == &calls[0]),
        "every retry retains exact workspace/view/cursor"
    );
}

fn foreign_refresh_control(advance_foreign_cursor: bool) {
    // Unseeded independent owners admit identical genesis roots and views;
    // root equality cannot stand in for original reservation/epoch authority.
    let mut original = AdapterFixture::open(TempTree::new(), false);
    let mut foreign = AdapterFixture::open(TempTree::new(), false);
    let published = prepare_refresh(&mut original)
        .persist()
        .unwrap_or_else(|(_, error)| panic!("persist original view: {error}"));
    let (original_root, original_cursor) = {
        let (_, daemon) = original.parts();
        (
            daemon.engine().daemon().owner().head().root(),
            owner_cursor(daemon),
        )
    };
    let (_, daemon) = foreign.parts();
    let foreign_owner = daemon.engine_mut().daemon_mut();
    assert_eq!(foreign_owner.owner().head().root(), original_root);
    assert_eq!(foreign_owner.library().cursor(), original_cursor);
    if advance_foreign_cursor {
        let view = foreign_owner.library().view().clone();
        let cursor =
            backend_engine::Cursor::for_view_root_at(&view, original_cursor.sequence() + 1);
        let admission = crate::builtin::BuiltinViewAdmission {
            workspace_root: foreign_owner.owner().head().root(),
            source_root: view.basis().root,
        };
        foreign_owner
            .restore_view(view, cursor, &admission, Vec::new(), cursor.sequence())
            .expect("actual checked recovery admits a newer stream cursor");
    }
    let foreign_writer = foreign_owner
        .reserve_read_head_writer()
        .expect("independent foreign reservation");
    let (published, error) = match foreign_owner.install_read_head_refresh(published) {
        Err(refused) => refused,
        Ok(_) => panic!("foreign or stale-cursor refresh cannot install"),
    };
    if advance_foreign_cursor {
        assert!(
            matches!(error, backend_engine::DaemonError::CursorInvalid),
            "fixed-size cursor check rejects before original reservation can move"
        );
    }
    let foreign_retired = foreign_owner
        .return_unselected_read_head_writer(foreign_writer)
        .unwrap_or_else(|_| panic!("foreign original writer remains intact"));
    drop(foreign_retired);
    let (_, daemon) = original.parts();
    let owner = daemon.engine_mut().daemon_mut();
    let retired = owner
        .install_read_head_refresh(published)
        .unwrap_or_else(|(_, error)| panic!("refusal returns original refresh intact: {error}"));
    drop(retired);
    assert_eq!(owner.owner().head().root(), original_root);
    assert_eq!(owner.library().cursor(), original_cursor);
    let writer = owner
        .reserve_read_head_writer()
        .expect("original workspace lease is restored");
    drop(
        owner
            .return_unselected_read_head_writer(writer)
            .unwrap_or_else(|_| panic!("return original lease")),
    );
}

#[test]
fn refresh_rejects_foreign_reservation_despite_identical_root_and_cursor() {
    foreign_refresh_control(false);
}

#[test]
fn refresh_rejects_stale_cursor_and_returns_original_capability() {
    foreign_refresh_control(true);
}
