//! Module language selection from the bounded, confined configuration read set.

use backend_semantic::vocabulary::GoVersion;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

/// A missing directive remains an explicit historical 1.25 default; it is not
/// promoted from a suggested toolchain or the currently advertised profile.
#[derive(Clone, Default)]
pub(crate) struct GoProjectCapture {
    modules: BTreeMap<PathBuf, (Option<GoVersion>, [u8; 32])>,
    absent: BTreeSet<PathBuf>,
}

impl GoProjectCapture {
    pub(super) fn absent(&mut self, directory: PathBuf) {
        self.absent.insert(directory);
    }

    pub(super) fn validate_source(&self, relative: &Path) -> Result<(), String> {
        let mut directory = relative.parent();
        while let Some(path) = directory {
            if self.modules.contains_key(path) {
                return Ok(());
            }
            if !self.absent.contains(path) {
                return Err(format!(
                    "Go source {} has an unobserved module boundary",
                    relative.display()
                ));
            }
            directory = path.parent();
        }
        Ok(())
    }
    pub(super) fn admit(
        &mut self,
        path: &Path,
        bytes: &[u8],
        content: [u8; 32],
    ) -> Result<(), String> {
        let profile = super::super::local_manifest::captured_go_language_profile(bytes)
            .map_err(|error| format!("compiler configuration {}: {error}", path.display()))?;
        let directory = path
            .parent()
            .ok_or("go.mod has no captured module directory")?;
        if self
            .modules
            .insert(directory.to_path_buf(), (profile, content))
            .is_some()
        {
            return Err("duplicate captured Go module directory".to_owned());
        }
        Ok(())
    }

    pub(super) fn literal_version(&self, relative: &Path) -> Option<GoVersion> {
        let mut directory = relative.parent();
        while let Some(path) = directory {
            if let Some((version, _)) = self.modules.get(path) {
                return *version;
            }
            directory = path.parent();
        }
        None
    }

    /// Only the file's observed negative chain and nearest owning module
    /// participate. The whole capture remains a mutation fence, but another
    /// module's content cannot invalidate this file's analysis.
    pub(super) fn analysis_identity(&self, base: [u8; 32], relative: &Path) -> [u8; 32] {
        if self.modules.is_empty() && self.absent.is_empty() {
            return base;
        }
        let mut hash = blake3::Hasher::new();
        hash.update(b"nudox.go.captured-module-language.v1\0");
        hash.update(&base);
        let mut directory = relative.parent();
        while let Some(parent) = directory {
            let path = parent
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            hash.update(&(path.len() as u64).to_be_bytes());
            hash.update(path.as_bytes());
            if let Some((version, content)) = self.modules.get(parent) {
                hash.update(&[1, version.map_or(0, |version| 1 + version as u8)]);
                hash.update(content);
                break;
            }
            hash.update(&[0]);
            directory = parent.parent();
        }
        *hash.finalize().as_bytes()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captured_module_boundaries_and_content_change_identity_without_filesystem_reads()
    -> Result<(), String> {
        let mut capture = GoProjectCapture::default();
        capture.admit(
            Path::new("go.mod"),
            b"module example.com/root\ngo 1.26.2\n",
            [1; 32],
        )?;
        capture.admit(
            Path::new("nested/go.mod"),
            b"module example.com/nested\ntoolchain go1.26.4\n",
            [2; 32],
        )?;
        assert_eq!(
            capture.literal_version(Path::new("root.go")),
            Some(GoVersion::Go126)
        );
        assert_eq!(
            capture.literal_version(Path::new("nested/child.go")),
            None,
            "nearest module without a directive keeps the historical default"
        );
        assert_eq!(
            capture.literal_version(Path::new("nested-other/child.go")),
            Some(GoVersion::Go126)
        );
        let before = capture.analysis_identity([9; 32], Path::new("root.go"));
        let mut changed = capture.clone();
        changed
            .modules
            .get_mut(Path::new(""))
            .ok_or("root module missing")?
            .1 = [3; 32];
        assert_ne!(
            before,
            changed.analysis_identity([9; 32], Path::new("root.go")),
            "even same-version module content changes invalidate reuse"
        );
        changed.modules.remove(Path::new("nested"));
        assert_eq!(
            changed.literal_version(Path::new("nested/child.go")),
            Some(GoVersion::Go126)
        );
        assert_ne!(
            before,
            changed.analysis_identity([9; 32], Path::new("root.go"))
        );
        let unrelated_before = capture.analysis_identity([9; 32], Path::new("root.go"));
        capture.admit(
            Path::new("unrelated/go.mod"),
            b"module example.com/unrelated\ngo 1.24.3\n",
            [8; 32],
        )?;
        assert_eq!(
            unrelated_before,
            capture.analysis_identity([9; 32], Path::new("root.go"))
        );
        assert!(
            capture
                .admit(
                    Path::new("future/go.mod"),
                    b"module example.com/future\ngo 1.27.0\n",
                    [4; 32]
                )
                .is_err()
        );
        Ok(())
    }
    #[test]
    fn module_creation_and_deletion_only_change_the_owning_frontier() -> Result<(), String> {
        let mut capture = GoProjectCapture::default();
        capture.admit(
            Path::new("go.mod"),
            b"module example.com/root\ngo 1.25.0\n",
            [1; 32],
        )?;
        let nested = Path::new("nested/child.go");
        let parent = Path::new("main.go");
        assert!(
            capture.validate_source(nested).is_err(),
            "missing observations cannot become a legacy default"
        );
        capture.absent(PathBuf::from("nested"));
        capture.validate_source(nested)?;
        let parent_before = capture.analysis_identity([9; 32], parent);
        let nested_before = capture.analysis_identity([9; 32], nested);
        capture.admit(
            Path::new("nested/go.mod"),
            b"module example.com/nested\ngo 1.26.2\n",
            [2; 32],
        )?;
        assert_eq!(parent_before, capture.analysis_identity([9; 32], parent));
        assert_ne!(nested_before, capture.analysis_identity([9; 32], nested));
        assert_eq!(capture.literal_version(nested), Some(GoVersion::Go126));
        capture.modules.remove(Path::new("nested"));
        capture.validate_source(nested)?;
        assert_eq!(nested_before, capture.analysis_identity([9; 32], nested));
        assert_eq!(parent_before, capture.analysis_identity([9; 32], parent));
        Ok(())
    }
}
