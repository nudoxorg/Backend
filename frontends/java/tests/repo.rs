use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use backend_frontend_java::legacy::purl::MavenCoordinates;
use backend_frontend_java::legacy::repo::{LocateError, Repository};

struct TempRepo(PathBuf);

static NEXT_REPOSITORY: AtomicUsize = AtomicUsize::new(0);

impl TempRepo {
    fn new() -> Self {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock before epoch")
            .as_nanos();
        let ordinal = NEXT_REPOSITORY.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("nudox-java-repo-{suffix}-{ordinal}"));
        fs::create_dir(&path).expect("create unique repository root");
        Self(path)
    }
}

impl Drop for TempRepo {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0) {
            if !std::thread::panicking() {
                panic!("remove temporary repository: {error}");
            }
        }
    }
}

#[test]
fn locates_jar_and_sources_in_dotted_group_layout() {
    let repo = TempRepo::new();
    let coordinates = MavenCoordinates::parse("maven:org.example:tool@1.0").expect("valid PURL");
    let directory = repo.0.join("org/example/tool/1.0");
    fs::create_dir_all(&directory).expect("create Maven layout");
    let jar = directory.join("tool-1.0.jar");
    let sources = directory.join("tool-1.0-sources.jar");
    fs::write(&jar, []).expect("write jar marker");
    fs::write(&sources, []).expect("write sources marker");
    let repository = Repository::new(&repo.0);
    assert_eq!(repository.jar(&coordinates).expect("jar exists"), jar);
    assert_eq!(
        repository
            .sources_jar(&coordinates)
            .expect("sources exists"),
        sources
    );
}

#[test]
fn missing_artifacts_retain_exact_paths_and_error_kind() {
    let repo = TempRepo::new();
    let coordinates = MavenCoordinates::parse("pkg:maven/a/b@v").expect("valid PURL");
    let repository = Repository::new(Path::new(&repo.0));
    let jar = repo.0.join("a/b/v/b-v.jar");
    let sources = repo.0.join("a/b/v/b-v-sources.jar");
    assert_eq!(
        repository.jar(&coordinates),
        Err(LocateError::Missing { path: jar.clone() })
    );
    assert_eq!(
        repository.sources_jar(&coordinates),
        Err(LocateError::MissingSources { path: sources })
    );
    fs::create_dir_all(jar.parent().expect("jar parent")).expect("create layout");
    fs::write(&jar, []).expect("write jar marker");
    assert_eq!(repository.jar(&coordinates).expect("jar exists"), jar);
    assert!(matches!(
        repository.sources_jar(&coordinates),
        Err(LocateError::MissingSources { .. })
    ));
}
