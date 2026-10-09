fn lock_command(state: &mut ProductState, command: SurfaceCommand) -> Result<SurfaceReply, String> {
    state.execute(
        command,
        &view(),
        &[],
        &CatalogLookupIndex::from_catalog(&[]),
        &indexed_graph(Vec::new()),
        None,
    )
}
fn create_locked(path: &Path, name: &str) -> SurfaceCommand {
    SurfaceCommand::ProjectCreate {
        name: ProjectName::new(name).unwrap(),
        lockfile: Some(ProductText::new(path.to_str().unwrap()).unwrap()),
    }
}
fn sync_locked(id: ProjectId) -> SurfaceCommand {
    SurfaceCommand::ProjectSync {
        project: ProjectSelector::Id(id),
    }
}
fn npm_lock(version: &str) -> String {
    serde_json::json!({"lockfileVersion":3,"packages":{"":{},"node_modules/unindexed":{"version":version,"integrity":"sha512-genuine-padding==","resolved":format!("https://registry.npmjs.org/unindexed/-/unindexed-{version}.tgz")}}}).to_string()
}

#[test]
fn lockfile_create_validates_before_identity_or_commit_and_sync_rereads_atomically() {
    let root = fixture("lockfile-atomic");
    let state_path = root.join("product-state.json");
    let lockfile = root.join("package-lock.json");
    let mut state = ProductState::open(state_path.clone()).unwrap();
    let first_identity = state.state.next_project;
    fs::write(&lockfile, "invalid JSON").unwrap();
    assert!(lock_command(&mut state, create_locked(&lockfile, "App")).is_err());
    assert_eq!(state.state.next_project, first_identity);
    assert_eq!(state.state.epoch, 0);
    assert!(state.state.projects.is_empty());
    assert!(!state_path.exists());
    fs::write(&lockfile, npm_lock("1.0.0")).unwrap();
    let created = match lock_command(&mut state, create_locked(&lockfile, "App")).unwrap() {
        SurfaceReply::ProjectCreated(project) => project,
        other => panic!("{other:?}"),
    };
    assert!(created.members.is_empty());
    assert!(created.lockfile_membership.is_none());
    let synced = match lock_command(&mut state, sync_locked(created.id)).unwrap() {
        SurfaceReply::ProjectSynced(project) => project,
        other => panic!("{other:?}"),
    };
    assert_eq!(synced.members[0].as_str(), "pkg:npm/unindexed@1.0.0");
    assert_eq!(synced.member_manifest_names[0].as_str(), "unindexed");
    assert_eq!(
        synced.lockfile_membership,
        Some(backend_library::ProjectLockfileMembership::Complete)
    );
    let committed = fs::read(&state_path).unwrap();
    let epoch = state.state.epoch;
    fs::write(&lockfile, npm_lock("not-a-version")).unwrap();
    assert!(lock_command(&mut state, sync_locked(created.id)).is_err());
    assert_eq!(state.state.epoch, epoch);
    assert_eq!(fs::read(&state_path).unwrap(), committed);
    assert_eq!(state.state.projects[0].members, synced.members);
    fs::write(&lockfile, npm_lock("2.0.0")).unwrap();
    lock_command(&mut state, sync_locked(created.id)).unwrap();
    drop(state);
    let mut reopened = ProductState::open(state_path).unwrap();
    assert_eq!(
        reopened.state.projects[0].members[0].as_str(),
        "pkg:npm/unindexed@2.0.0"
    );
    match lock_command(&mut reopened, SurfaceCommand::Projects).unwrap() {
        SurfaceReply::Projects(projects) => {
            assert_eq!(projects[0].member_manifest_names[0].as_str(), "unindexed")
        }
        _ => unreachable!(),
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn lockfile_partial_workspace_rows_persist_without_refusing_registry_members() {
    let root = fixture("lockfile-workspace");
    let lockfile = root.join("package-lock.json");
    fs::create_dir(root.join("versionless")).unwrap();
    fs::write(
        root.join("versionless/package.json"),
        r#"{"name":"versionless"}"#,
    )
    .unwrap();
    fs::create_dir(root.join("pinned")).unwrap();
    fs::write(
        root.join("pinned/package.json"),
        r#"{"name":"pinned-local","version":"0.2.0"}"#,
    )
    .unwrap();
    fs::write(
        &lockfile,
        serde_json::json!({"lockfileVersion":3,"packages":{
            "":{},"versionless":{"name":"versionless"},"absent":{"name":"absent"},
            "pinned":{"name":"pinned-local","version":"0.2.0"},
            "node_modules/unindexed":{"version":"1.0.0","resolved":"https://registry.npmjs.org/unindexed/-/unindexed-1.0.0.tgz"}
        }})
        .to_string(),
    )
    .unwrap();
    let state_path = root.join("product-state.json");
    let mut state = ProductState::open(state_path.clone()).unwrap();
    let created = match lock_command(&mut state, create_locked(&lockfile, "App")).unwrap() {
        SurfaceReply::ProjectCreated(project) => project,
        _ => unreachable!(),
    };
    let synced = match lock_command(&mut state, sync_locked(created.id)).unwrap() {
        SurfaceReply::ProjectSynced(project) => project,
        _ => unreachable!(),
    };
    assert_eq!(synced.members.len(), 2);
    assert!(
        synced
            .members
            .iter()
            .any(|member| matches!(member, PackageReference::Local(_)))
    );
    assert!(
        synced
            .member_manifest_names
            .iter()
            .any(|name| name.as_str() == "pinned-local")
    );
    match &synced.lockfile_membership {
        Some(backend_library::ProjectLockfileMembership::Partial { unresolved }) => {
            assert_eq!(unresolved.len(), 2);
            assert!(
                unresolved
                    .iter()
                    .any(|row| row.name.as_str() == "versionless")
            );
            assert!(unresolved.iter().any(|row| row.name.as_str() == "absent"));
        }
        other => panic!("{other:?}"),
    }
    drop(state);
    let mut reopened = ProductState::open(state_path).unwrap();
    match lock_command(&mut reopened, SurfaceCommand::Projects).unwrap() {
        SurfaceReply::Projects(projects) => assert_eq!(projects[0], synced),
        _ => unreachable!(),
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn lockfile_ordinary_large_npm_project_syncs_all_members_with_bounded_reply() {
    let root = fixture("lockfile-large");
    let lockfile = root.join("package-lock.json");
    let mut packages = serde_json::Map::new();
    for index in 0..1444 {
        packages.insert(
            format!("node_modules/p{index}"),
            serde_json::json!({"version":"1.0.0","resolved":format!("https://registry.npmjs.org/p{index}/-/p{index}-1.0.0.tgz")}),
        );
    }
    fs::write(
        &lockfile,
        serde_json::json!({"lockfileVersion":3,"packages":packages}).to_string(),
    )
    .unwrap();
    let mut state = ProductState::open(root.join("product-state.json")).unwrap();
    let id = match lock_command(&mut state, create_locked(&lockfile, "Large")).unwrap() {
        SurfaceReply::ProjectCreated(project) => project.id,
        _ => unreachable!(),
    };
    let reply = lock_command(&mut state, sync_locked(id)).unwrap();
    reply.admit(reply.id()).unwrap();
    assert!(serde_json::to_vec(&reply).unwrap().len() < 4 * 1024 * 1024);
    match reply {
        SurfaceReply::ProjectSynced(project) => assert_eq!(project.members.len(), 1444),
        _ => unreachable!(),
    }
    fs::remove_dir_all(root).unwrap();
}

#[cfg(unix)]
#[test]
fn lockfile_workspace_symlink_escape_fails_before_project_commit() {
    let root = fixture("lockfile-escape");
    let foreign = fixture("lockfile-foreign");
    fs::write(
        foreign.join("package.json"),
        r#"{"name":"foreign","version":"1.0.0"}"#,
    )
    .unwrap();
    std::os::unix::fs::symlink(&foreign, root.join("escape")).unwrap();
    let lockfile = root.join("package-lock.json");
    fs::write(
        &lockfile,
        r#"{"lockfileVersion":3,"packages":{"escape":{"name":"foreign","version":"1.0.0"}}}"#,
    )
    .unwrap();
    let mut state = ProductState::open(root.join("product-state.json")).unwrap();
    assert!(lock_command(&mut state, create_locked(&lockfile, "Escape")).is_err());
    assert_eq!(state.state.epoch, 0);
    assert!(state.state.projects.is_empty());
    fs::remove_dir_all(root).unwrap();
    fs::remove_dir_all(foreign).unwrap();
}

#[test]
fn lockfile_legacy_project_state_is_read_only_until_a_successful_version_two_commit() {
    let root = fixture("lockfile-state-version");
    let path = root.join("product-state.json");
    let mut original = StoredState::default();
    original.version = 1;
    let bytes = serde_json::to_vec(&original).unwrap();
    fs::write(&path, &bytes).unwrap();
    let mut state = ProductState::open(path.clone()).unwrap();
    assert_eq!(state.state.version, 1);
    lock_command(&mut state, SurfaceCommand::Projects).unwrap();
    assert_eq!(
        fs::read(&path).unwrap(),
        bytes,
        "read does not migrate durable state"
    );
    lock_command(&mut state, create("New")).unwrap();
    assert_eq!(state.state.version, 2);
    assert_eq!(ProductState::open(path.clone()).unwrap().state.version, 2);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn lockfile_parent_replacement_cannot_rebind_old_workspace_inventory() {
    let root = fixture("lockfile-parent-generation");
    let parent = root.join("selected");
    fs::create_dir(&parent).unwrap();
    let cap =
        backend_platform::directory::DirectoryCapability::open_read_only_source(&parent).unwrap();
    let inventory = backend_engine::project_lockfile::LockfileFormat::Npm
        .parse(r#"{"lockfileVersion":3,"packages":{"local":{"name":"local"}}}"#)
        .unwrap();
    fs::rename(&parent, root.join("retired")).unwrap();
    fs::create_dir(&parent).unwrap();
    fs::create_dir(parent.join("local")).unwrap();
    fs::write(
        parent.join("local/package.json"),
        r#"{"name":"foreign","version":"9.0.0"}"#,
    )
    .unwrap();
    assert!(project_lockfile::resolve(inventory, &parent, &cap).is_err());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn lockfile_unimplemented_formats_are_refused_at_create_without_empty_project() {
    let root = fixture("lockfile-unsupported");
    let path = root.join("pnpm-lock.yaml");
    fs::write(&path, "lockfileVersion: '9.0'\n").unwrap();
    let mut state = ProductState::open(root.join("product-state.json")).unwrap();
    let error = lock_command(&mut state, create_locked(&path, "App")).unwrap_err();
    assert!(error.contains("unsupported project lockfile format"));
    assert!(state.state.projects.is_empty());
    assert_eq!(state.state.epoch, 0);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn lockfile_sync_then_manual_membership_changes_reopen_with_parallel_names() {
    let root = fixture("lockfile-member-change");
    let lockfile = root.join("package-lock.json");
    fs::write(&lockfile, npm_lock("1.0.0")).unwrap();
    let state_path = root.join("product-state.json");
    let mut state = ProductState::open(state_path.clone()).unwrap();
    let id = match lock_command(&mut state, create_locked(&lockfile, "App")).unwrap() {
        SurfaceReply::ProjectCreated(project) => project.id,
        _ => unreachable!(),
    };
    lock_command(&mut state, sync_locked(id)).unwrap();
    // Previously saved builds can retain exact display names. A membership
    // edit must clear that snapshot in pending state, not only enrich its reply.
    state.state.projects[0].member_manifest_names =
        vec![ProductText::from_static("unindexed")].into_boxed_slice();
    for add in [true, false] {
        let package = PackageReference::parse("pkg:npm/manual@2.0.0").unwrap();
        let command = if add {
            SurfaceCommand::ProjectAdd {
                project: ProjectSelector::Id(id),
                package,
            }
        } else {
            SurfaceCommand::ProjectRemove {
                project: ProjectSelector::Id(id),
                package,
            }
        };
        let reply = lock_command(&mut state, command).unwrap();
        reply.admit(reply.id()).unwrap();
        drop(state);
        state = ProductState::open(state_path.clone()).unwrap();
        assert_eq!(
            state.state.projects[0].members.len(),
            if add { 2 } else { 1 }
        );
        assert!(state.state.projects[0].member_manifest_names.is_empty());
        assert!(state.state.projects[0].lockfile_membership.is_none());
    }
    fs::remove_dir_all(root).unwrap();
}
