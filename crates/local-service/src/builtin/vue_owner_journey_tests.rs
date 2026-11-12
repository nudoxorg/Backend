//! Development-mode public owner routes for native original-host Vue syntax.
use backend_client::Session;
use backend_library::{
    CommandReply, CompileExecutionIntent, IndexOperationKey, IndexOperationObservation,
    IndexOperationState, PackageReference, ProductText, SurfaceCommand, SurfaceReply,
};
use std::{
    error::Error,
    fs,
    time::{Duration, Instant},
};

fn index(session: &mut Session, project: &std::path::Path, byte: u8) -> Result<(), Box<dyn Error>> {
    let key = IndexOperationKey::from_bytes([byte; 32])
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let package = PackageReference::parse(project.to_string_lossy().into_owned())
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let mut status = session
        .start_index_operation(key, package.clone(), CompileExecutionIntent::Interactive)
        .map_err(|e| std::io::Error::other(format!("start operation: {e}")))?;
    let deadline = Instant::now() + Duration::from_secs(600);
    let mut request_digest = None;
    loop {
        let IndexOperationObservation::Known(known) = status else {
            return Err("unknown index status".into());
        };
        if known.operation_key != key
            || known.package != package
            || known.execution_intent != CompileExecutionIntent::Interactive
            || request_digest.is_some_and(|digest| digest != known.request_digest)
        {
            return Err("index status changed operation identity".into());
        }
        request_digest = Some(known.request_digest);
        match known.state {
            IndexOperationState::Published(_) => return Ok(()),
            IndexOperationState::Accepted | IndexOperationState::Active { .. } => {}
            state => return Err(format!("index refused: {state:?}").into()),
        }
        if Instant::now() >= deadline {
            return Err("Vue index deadline".into());
        }
        std::thread::sleep(Duration::from_millis(30));
        if session.connection_frames_remaining() == Some(0) {
            session.reconnect().map_err(|e| {
                std::io::Error::other(format!("rotate exhausted owner connection: {e}"))
            })?;
        }
        status = session
            .index_operation_status(key)
            .map_err(|e| std::io::Error::other(format!("poll operation: {e}")))?;
    }
}

#[test]
fn vue_index_rotates_small_frame_budget() -> Result<(), Box<dyn Error>> {
    const CHILD: &str = "NUDOX_VUE_FRAME_CONTROL_CHILD";
    if std::env::var_os(CHILD).is_none() {
        // Isolate the owner/client test hook from other tests and preserve the
        // caller's account-gate environment in the child process.
        let status = std::process::Command::new(std::env::current_exe()?)
            .env(CHILD, "1")
            .env(backend_client::CONNECTION_FRAME_BUDGET_ENV, "1")
            .args([
                "--exact",
                "builtin::vue_owner_journey_tests::vue_index_rotates_small_frame_budget",
                "--nocapture",
            ])
            .status()?;
        assert!(status.success(), "small-frame child failed: {status}");
        return Ok(());
    }
    let fixture = tempfile::Builder::new()
        .prefix("vue-frame-")
        .tempdir_in("/tmp")?;
    let project = fixture.path().join("project");
    fs::create_dir(&project)?;
    fs::write(
        project.join("App.vue"),
        "<script setup>function mounted() { return 1; }</script>",
    )?;
    let paths = backend_runtime::WorkspacePaths::discover(
        Some(fixture.path().join("owner")),
        Some(fixture.path().join("state")),
        Some(fixture.path().join("owner.sock")),
    )?;
    paths.initialize()?;
    let owner = super::native_ts_owner_journey_tests::start_owner(&paths)?;
    let mut session = Session::connect(owner.endpoint())?;
    assert_eq!(session.connection_frames_remaining(), Some(1));
    index(&mut session, &project, 0x83)?;
    drop(session);
    assert_eq!(owner.close()?.failures, 0);
    Ok(())
}

fn verify(session: &mut Session, project: &std::path::Path) -> Result<(), Box<dyn Error>> {
    let CommandReply::Names(names) = session
        .names("mounted", 100)
        .map_err(|e| std::io::Error::other(format!("names: {e}")))?
        .reply
    else {
        return Err("names reply".into());
    };
    let row = names
        .root
        .rows()
        .iter()
        .find(|row| {
            row.label.ends_with("::mounted")
                && row.source.captured().is_some_and(|s| s.path() == "App.vue")
        })
        .ok_or("Vue declaration absent from public names search")?;
    assert_eq!(row.source.captured().unwrap().start_line(), 4);
    let coordinate = row.label.clone();
    let selected = row.id;
    // Published-name fallback is useful but does not establish lexical
    // coverage. Wait for the actual lexical reply at the selected view.
    let deadline = Instant::now() + Duration::from_secs(600);
    let search = loop {
        if session
            .connection_frames_remaining()
            .is_some_and(|remaining| remaining < 2)
        {
            session.reconnect()?;
        }
        match session
            .search("mounted", 100)
            .map_err(|e| std::io::Error::other(format!("lexical search: {e}")))?
            .reply
        {
            CommandReply::Search(search) => break search,
            CommandReply::PublishedNameSearch(_) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            other => return Err(format!("Vue lexical search reply: {other:?}").into()),
        }
    };
    assert!(
        search.root.rows().iter().any(|row| row.id == selected),
        "ordinary lexical search retains the Vue declaration"
    );
    let CommandReply::Names(modules) = session
        .names("App.vue", 100)
        .map_err(|e| std::io::Error::other(format!("component names: {e}")))?
        .reply
    else {
        return Err("component names reply".into());
    };
    let module = modules
        .root
        .rows()
        .iter()
        .find(|row| {
            row.kind == Some(backend_compile::DeclarationKind::Module)
                && row
                    .source
                    .captured()
                    .is_some_and(|source| source.path() == "App.vue")
        })
        .ok_or("published Vue component module missing")?
        .label
        .clone();
    assert_eq!(module, format!("{}::App.vue", project.display()));
    let CommandReply::Document(component) = session
        .source(&module)
        .map_err(|e| std::io::Error::other(format!("component source {module}: {e}")))?
        .reply
    else {
        return Err("component source reply".into());
    };
    assert!(
        component
            .excerpt
            .text()
            .is_some_and(|text| text.starts_with("<template>") && text.contains("<script setup")),
        "full verified Vue host source"
    );

    let CommandReply::Document(document) = session
        .source(&coordinate)
        .map_err(|e| std::io::Error::other(format!("declaration source: {e}")))?
        .reply
    else {
        return Err("source reply".into());
    };
    assert!(
        document
            .excerpt
            .text()
            .is_some_and(|text| text.contains("return 'é漢'")),
        "{document:?}"
    );
    assert!(!document.excerpt.text().unwrap().contains("<template>"));
    let reply = session
        .surface(SurfaceCommand::References {
            target: ProductText::new(&coordinate)
                .map_err(|e| std::io::Error::other(e.to_string()))?,
        })
        .map_err(|e| std::io::Error::other(format!("references: {e}")))?;
    let SurfaceReply::References {
        references,
        coverage,
        ..
    } = reply
    else {
        return Err("references reply".into());
    };
    assert_eq!(
        references.len(),
        2,
        "two bound calls, excluding the shadowed parameter call"
    );
    assert!(matches!(
        coverage,
        backend_library::ReferenceCoverage::Partial { .. }
    ));
    for reference in references {
        assert_eq!(
            reference.evidence.confidence,
            backend_library::SemanticConfidence::Syntactic
        );
        let span = reference.evidence.source.ok_or("Vue reference span")?;
        let source = fs::read(project.join(span.file.as_str()))?;
        assert_eq!(&source[span.start as usize..span.end as usize], b"mounted");
    }
    let package = PackageReference::parse("pkg:npm/vue@3.5.44")
        .map_err(|e| std::io::Error::other(e.to_string()))?;
    let installed = session.surface(SurfaceCommand::Package {
        package: package.clone(),
    })?;
    let SurfaceReply::PackageInstalled(facts) = installed else {
        return Err("installed package owner reply".into());
    };
    assert!(
        facts
            .files
            .iter()
            .any(|file| file.excerpt.as_str().contains("createApp"))
    );
    assert!(
        facts
            .engines
            .iter()
            .any(|engine| engine.as_str() == "node >=22")
    );
    assert!(matches!(
        session.surface(SurfaceCommand::Dependencies {
            package: package.clone()
        })?,
        SurfaceReply::DependenciesInstalled(_)
    ));
    assert!(matches!(
        session.surface(SurfaceCommand::PackageVersions { package })?,
        SurfaceReply::PackageVersionsInstalled(_)
    ));
    Ok(())
}

#[test]
fn vue_scripts_answer_search_source_references_after_cold_owner_reopen()
-> Result<(), Box<dyn Error>> {
    let fixture = tempfile::Builder::new()
        .prefix("vue-owner-")
        .tempdir_in("/tmp")?;
    let project = fixture.path().join("project");
    fs::create_dir(&project)?;
    fs::create_dir_all(project.join("frontend/node_modules/vue"))?;
    fs::write(
        project.join("frontend/node_modules/vue/package.json"),
        r#"{"name":"vue","version":"3.5.44","types":"index.d.ts","engines":{"node":">=22"},"dependencies":{"@vue/runtime-dom":"3.5.44"}}"#,
    )?;
    fs::write(
        project.join("frontend/node_modules/vue/index.d.ts"),
        "export declare function createApp(component: unknown): unknown;\n",
    )?;
    fs::write(
        project.join("frontend/node_modules/vue/README.md"),
        "# Installed Vue API documentation\n",
    )?;

    fs::write(
        project.join("App.vue"),
        "<template><button @click=\"mounted\">Run</button></template>\r\n<script setup lang=\"ts\">\r\n// é漢\r\nfunction mounted() { return 'é漢'; }\r\nmounted();\r\nfunction wrapper() { mounted(); }\r\nfunction shadow(mounted: () => void) { mounted(); }\r\n</script>\r\n<style>.button { color: red; }</style>\r\n",
    )?;
    let paths = backend_runtime::WorkspacePaths::discover(
        Some(fixture.path().join("owner")),
        Some(fixture.path().join("state")),
        Some(fixture.path().join("owner.sock")),
    )?;
    paths.initialize()?;
    let owner = super::native_ts_owner_journey_tests::start_owner(&paths)
        .map_err(|e| std::io::Error::other(format!("initial owner start: {e}")))?;
    let mut session = Session::connect(owner.endpoint())
        .map_err(|e| std::io::Error::other(format!("initial owner connect: {e}")))?;
    index(&mut session, &project, 0x81)
        .map_err(|e| std::io::Error::other(format!("initial index: {e}")))?;
    verify(&mut session, &project)
        .map_err(|e| std::io::Error::other(format!("initial verification: {e}")))?;
    drop(session);
    assert_eq!(owner.close()?.failures, 0);
    let owner = super::native_ts_owner_journey_tests::start_owner(&paths)
        .map_err(|e| std::io::Error::other(format!("cold owner start: {e}")))?;
    let mut session = Session::connect(owner.endpoint())
        .map_err(|e| std::io::Error::other(format!("cold owner connect: {e}")))?;
    verify(&mut session, &project)
        .map_err(|e| std::io::Error::other(format!("cold verification: {e}")))?;
    fs::write(
        project.join("App.vue"),
        "<script setup>function replacement() { return 2; }</script>",
    )?;
    index(&mut session, &project, 0x82)
        .map_err(|e| std::io::Error::other(format!("edited index: {e}")))?;
    let CommandReply::Names(names) = session.names("mounted", 100)?.reply else {
        return Err("names reply".into());
    };
    assert!(
        !names
            .root
            .rows()
            .iter()
            .any(|row| row.label.ends_with("::mounted")),
        "removed Vue declaration must retire"
    );
    drop(session);
    assert_eq!(owner.close()?.failures, 0);
    Ok(())
}
