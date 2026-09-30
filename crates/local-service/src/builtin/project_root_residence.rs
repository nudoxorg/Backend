//! Local project roots addressed by the source relation root that names them.
//!
//! Every product-state surface command (a package page issues four) refreshes
//! the local manifest facts of the workspace's local projects. The project
//! records live in the indexed source relation beside every file record, so
//! naming them means paging the whole relation and admitting each node:
//! O(indexed files) per command, measured at 68% of the owner thread while a
//! package page loaded. The relation root is content-addressed, so an equal
//! root names equal project records; the labels are read once per root.
//! Directory existence is still checked on every call, exactly as the
//! uncached path did, because it is a property of the disk, not the root.

use super::profile::BuiltinWorkspaceRelation;
use super::{BuiltinModelError, read_indexed_sources};
use backend_engine::WorkspaceSnapshot;
use backend_version::StateRoot;
use std::path::{Path, PathBuf};

/// Local (non-registry) project labels of one exact source relation root.
#[derive(Debug, Default)]
pub(crate) struct ProjectRootResidence {
    resident: Option<(StateRoot<BuiltinWorkspaceRelation>, Box<[String]>)>,
    reads: u64,
}

impl ProjectRootResidence {
    /// The workspace's local project directories that exist on disk now.
    ///
    /// # Errors
    ///
    /// The source relation cannot be opened, or (on a new root) paged and
    /// admitted.
    pub(crate) fn roots(
        &mut self,
        snapshot: &WorkspaceSnapshot,
    ) -> Result<Vec<PathBuf>, BuiltinModelError> {
        let root = snapshot
            .relation::<BuiltinWorkspaceRelation>()
            .map_err(|error| BuiltinModelError(format!("open indexed source relation: {error}")))?
            .root();
        let labels = match &self.resident {
            Some((resident, labels)) if *resident == root => labels,
            _ => {
                let indexed = read_indexed_sources(snapshot)?;
                self.reads = self.reads.saturating_add(1);
                let labels = indexed
                    .projects
                    .into_values()
                    .map(|project| project.label)
                    .filter(|label| !label.starts_with("pkg:"))
                    .collect();
                &self.resident.insert((root, labels)).1
            }
        };
        Ok(labels
            .iter()
            .map(Path::new)
            .filter(|path| path.is_dir())
            .map(Path::to_path_buf)
            .collect())
    }

    /// Full source-relation reads paid since this residence was created.
    #[cfg(test)]
    pub(crate) const fn reads(&self) -> u64 {
        self.reads
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use super::ProjectRootResidence;
    use crate::builtin::search_source_page::snapshot_of_projects;
    use std::collections::BTreeSet;
    use std::path::PathBuf;

    struct TempDirs(PathBuf);
    impl Drop for TempDirs {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn label(path: &std::path::Path) -> String {
        path.to_str().expect("utf-8 temp path").to_owned()
    }

    fn set(paths: &[PathBuf]) -> BTreeSet<PathBuf> {
        paths.iter().cloned().collect()
    }

    /// The relation root is the key: an unchanged root is served without
    /// paging, a new project changes the root and is seen, a registry
    /// package is never a local root, and a directory removed from disk
    /// leaves the answer even while its record stays resident.
    #[test]
    fn project_roots_follow_the_source_relation_root_and_the_disk() {
        let base = std::env::temp_dir().join(format!("nudox-project-roots-{}", std::process::id()));
        let _cleanup = TempDirs(base.clone());
        let (first, second) = (base.join("first"), base.join("second"));
        std::fs::create_dir_all(&first).expect("first project dir");
        std::fs::create_dir_all(&second).expect("second project dir");

        let one = snapshot_of_projects(&[label(&first), "pkg:cargo/serde@1.0.0".to_owned()])
            .expect("one local project");
        let two =
            snapshot_of_projects(&[label(&first), label(&second)]).expect("two local projects");

        let mut residence = ProjectRootResidence::default();
        assert_eq!(
            set(&residence.roots(&one).expect("roots")),
            set(&[first.clone()])
        );
        assert_eq!(residence.reads(), 1);
        assert_eq!(
            set(&residence.roots(&one).expect("roots")),
            set(&[first.clone()])
        );
        assert_eq!(
            residence.reads(),
            1,
            "an unchanged relation root pages nothing"
        );

        assert_eq!(
            set(&residence.roots(&two).expect("roots")),
            set(&[first.clone(), second.clone()]),
            "a new source relation root names its new project"
        );
        assert_eq!(residence.reads(), 2);

        std::fs::remove_dir_all(&second).expect("remove second project dir");
        assert_eq!(
            set(&residence.roots(&two).expect("roots")),
            set(&[first.clone()]),
            "a directory gone from disk is not a root, even while its record is resident"
        );
        assert_eq!(residence.reads(), 2);

        assert_eq!(set(&residence.roots(&one).expect("roots")), set(&[first]));
        assert_eq!(
            residence.reads(),
            3,
            "returning to an older root reads it again"
        );
    }
}
