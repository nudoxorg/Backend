//! Development-mode public owner routes for native original-host Vue syntax.
use backend_client::Session;
use backend_library::{CommandReply, CompileExecutionIntent, IndexOperationKey, IndexOperationObservation, IndexOperationState, PackageReference, ProductText, SurfaceCommand, SurfaceReply};
use std::{error::Error, fs, time::{Duration, Instant}};

fn index(session: &mut Session, project: &std::path::Path, byte: u8) -> Result<(), Box<dyn Error>> {
    let key = IndexOperationKey::from_bytes([byte; 32]).map_err(|e| std::io::Error::other(e.to_string()))?;
    let package = PackageReference::parse(project.to_string_lossy().into_owned()).map_err(|e| std::io::Error::other(e.to_string()))?;
    let mut status = session.start_index_operation(key, package, CompileExecutionIntent::Interactive)?;
    let deadline = Instant::now() + Duration::from_secs(600);
    loop {
        let IndexOperationObservation::Known(known) = status else { return Err("unknown index status".into()) };
        match known.state { IndexOperationState::Published(_) => return Ok(()), IndexOperationState::Accepted | IndexOperationState::Active { .. } => {}, state => return Err(format!("index refused: {state:?}").into()) }
        if Instant::now() >= deadline { return Err("Vue index deadline".into()) }
        std::thread::sleep(Duration::from_millis(30));
        status = session.index_operation_status(key)?;
    }
}

fn verify(session: &mut Session, project: &std::path::Path) -> Result<(), Box<dyn Error>> {
    let CommandReply::Names(names) = session.names("mounted", 100)?.reply else { return Err("names reply".into()) };
    let row = names.root.rows().iter().find(|row| row.label.ends_with("::mounted") && row.source.captured().is_some_and(|s| s.path() == "App.vue")).ok_or("Vue declaration absent from public names search")?;
    assert_eq!(row.source.captured().unwrap().start_line(), 4);
    let coordinate = row.label.clone();
    let CommandReply::Document(document) = session.source(&coordinate)?.reply else { return Err("source reply".into()) };
    assert!(document.excerpt.text().is_some_and(|text| text.contains("return 'é漢'")), "{document:?}");
    assert!(!document.excerpt.text().unwrap().contains("<template>"));
    let reply = session.surface(SurfaceCommand::References { target: ProductText::new(&coordinate).map_err(|e| std::io::Error::other(e.to_string()))? })?;
    let SurfaceReply::References { references, coverage, .. } = reply else { return Err("references reply".into()) };
    assert_eq!(references.len(), 2, "two bound calls, excluding the shadowed parameter call");
    assert!(matches!(coverage, backend_library::ReferenceCoverage::Partial { .. }));
    for reference in references {
        assert_eq!(reference.evidence.confidence, backend_library::SemanticConfidence::Syntactic);
        let span = reference.evidence.source.ok_or("Vue reference span")?;
        let source = fs::read(project.join(span.file.as_str()))?;
        assert_eq!(&source[span.start as usize..span.end as usize], b"mounted");
    }
    Ok(())
}

#[test]
fn vue_scripts_answer_search_source_references_after_cold_owner_reopen() -> Result<(), Box<dyn Error>> {
    let fixture = tempfile::Builder::new().prefix("vue-owner-").tempdir_in("/tmp")?;
    let project = fixture.path().join("project"); fs::create_dir(&project)?;
    fs::write(project.join("App.vue"), "<template><button @click=\"mounted\">Run</button></template>\r\n<script setup lang=\"ts\">\r\n// é漢\r\nfunction mounted() { return 'é漢'; }\r\nmounted();\r\nfunction wrapper() { mounted(); }\r\nfunction shadow(mounted: () => void) { mounted(); }\r\n</script>\r\n<style>.button { color: red; }</style>\r\n")?;
    let paths = backend_runtime::WorkspacePaths::discover(Some(fixture.path().join("owner")), Some(fixture.path().join("state")), Some(fixture.path().join("owner.sock")))?;
    paths.initialize()?;
    let owner = super::native_ts_owner_journey_tests::start_owner(&paths)?;
    let mut session = Session::connect(owner.endpoint())?;
    index(&mut session, &project, 0x81)?;
    verify(&mut session, &project)?;
    drop(session); assert_eq!(owner.close()?.failures, 0);
    let owner = super::native_ts_owner_journey_tests::start_owner(&paths)?;
    let mut session = Session::connect(owner.endpoint())?;
    verify(&mut session, &project)?;
    fs::write(project.join("App.vue"), "<script setup>function replacement() { return 2; }</script>")?;
    index(&mut session, &project, 0x82)?;
    let CommandReply::Names(names) = session.names("mounted", 100)?.reply else { return Err("names reply".into()) };
    assert!(!names.root.rows().iter().any(|row| row.label.ends_with("::mounted")), "removed Vue declaration must retire");
    drop(session); assert_eq!(owner.close()?.failures, 0);
    Ok(())
}
