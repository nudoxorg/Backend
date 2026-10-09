//! Finite, witnessed diagnostic inputs named by captured native configurations.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

use pyrefly::BaselineProcessor;
use pyrefly_config::config::ConfigFile;

use super::{
    CONFIG_BYTES, CapturedProjectLayout, CheckerError, FileWitness, FileWitnessRead,
    PythonProjectControl, checkpoint, hash_field, project_error, workspace_error,
};

#[derive(Debug)]
struct BaselineWitness {
    root: PathBuf,
    relative: PathBuf,
    digest: (blake3::Hash, u64),
}

/// Only explicitly configured, bounded regular files materialized in this mirror.
#[derive(Debug, Default)]
pub(in super::super) struct CapturedBaselines {
    files: BTreeMap<PathBuf, BaselineWitness>,
}

impl CapturedBaselines {
    pub(in super::super) fn capture(
        layout: &CapturedProjectLayout,
        original: &Path,
        configurations: &[PathBuf],
        mirror_witness: &mut Vec<FileWitness>,
        control: PythonProjectControl<'_>,
    ) -> Result<Self, CheckerError> {
        let mut result = Self::default();
        for configuration in configurations {
            checkpoint(control)?;
            if !ConfigFile::CONFIG_FILE_NAMES
                .iter()
                .chain(ConfigFile::ADDITIONAL_ROOT_FILE_NAMES)
                .any(|name| configuration.file_name().is_some_and(|leaf| leaf == *name))
            {
                continue;
            }
            // The native parser supplies the same config-relative path used by
            // State. It reads only an already captured configuration here.
            let (config, errors) = ConfigFile::from_file(configuration);
            if !errors.is_empty() {
                return Err(project_error(
                    &configuration.to_string_lossy(),
                    &format!(
                        "native captured configuration errors: {}",
                        errors
                            .iter()
                            .map(|error| error.get_message())
                            .collect::<Vec<_>>()
                            .join("; ")
                    ),
                ));
            }
            let Some(mut path) = config.baseline else {
                continue;
            };
            layout.rebase(&mut path, original)?;
            if result.files.contains_key(&path) {
                continue;
            }
            let relative = path
                .strip_prefix(layout.source_root())
                .map_err(|_| CheckerError::UncapturedDependency { path: path.clone() })?;
            let bytes = read_baseline(original, relative, control)?;
            let digest = (blake3::hash(&bytes), bytes.len() as u64);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(workspace_error)?;
            }
            std::fs::write(&path, &bytes).map_err(workspace_error)?;
            // The native parser must accept the exact captured bytes. The
            // baseline's listed paths are diagnostic keys, never source probes.
            BaselineProcessor::from_file(&path, layout.source_root()).map_err(|_| {
                project_error("", "captured baseline is not valid native diagnostic input")
            })?;
            let captured_bytes =
                rebase_absolute_baseline_keys(&bytes, original, layout.source_root())?;
            if let Some(ref captured_bytes) = captured_bytes {
                std::fs::write(&path, captured_bytes).map_err(workspace_error)?;
                BaselineProcessor::from_file(&path, layout.source_root()).map_err(|_| {
                    project_error("", "rebased baseline is not valid native diagnostic input")
                })?;
            }
            let mirror_bytes = captured_bytes.as_deref().unwrap_or(&bytes);
            mirror_witness.push(FileWitness {
                path: path.clone(),
                digest: Some((blake3::hash(mirror_bytes), mirror_bytes.len() as u64)),
                read: FileWitnessRead::Ordinary,
            });
            let captured = BaselineWitness {
                root: original.to_path_buf(),
                relative: relative.to_path_buf(),
                digest,
            };
            captured.validate_current(control)?;
            result.files.insert(path, captured);
        }
        Ok(result)
    }

    pub(in super::super) fn admit(
        &self,
        path: &mut PathBuf,
        layout: &CapturedProjectLayout,
        original: &Path,
    ) -> Result<(), CheckerError> {
        layout.rebase(path, original)?;
        if self.files.contains_key(path) {
            Ok(())
        } else {
            Err(CheckerError::UncapturedDependency { path: path.clone() })
        }
    }

    pub(in super::super) fn validate_current(
        &self,
        control: PythonProjectControl<'_>,
    ) -> Result<(), CheckerError> {
        for file in self.files.values() {
            file.validate_current(control)?;
        }
        checkpoint(control)
    }

    pub(in super::super) fn fingerprint(&self, identity: &mut blake3::Hasher) {
        for file in self.files.values() {
            identity.update(b"captured-diagnostic-baseline.v1;exact-original-root-key-rebase.v2;stable-open-file-read.v1\0");
            hash_field(identity, file.root.as_os_str().as_encoded_bytes());
            hash_field(identity, file.relative.as_os_str().as_encoded_bytes());
            identity.update(file.digest.0.as_bytes());
            identity.update(&file.digest.1.to_be_bytes());
        }
    }
}

impl BaselineWitness {
    fn validate_current(&self, control: PythonProjectControl<'_>) -> Result<(), CheckerError> {
        let bytes = read_baseline(&self.root, &self.relative, control)?;
        if self.digest != (blake3::hash(&bytes), bytes.len() as u64) {
            return Err(project_error("", "captured diagnostic baseline changed"));
        }
        Ok(())
    }
}

fn rebase_absolute_baseline_keys(
    bytes: &[u8],
    original: &Path,
    mirror: &Path,
) -> Result<Option<Vec<u8>>, CheckerError> {
    // The actual native parser admitted the closed document first. Rebase only
    // exact original-root absolute keys and exclude unrelated absolute keys;
    // no coordinates or source reads result.
    let mut document: serde_json::Value = serde_json::from_slice(bytes)
        .map_err(|_| project_error("", "captured baseline is not valid native diagnostic input"))?;
    let mut changed = false;
    let rows = document
        .get_mut("errors")
        .and_then(serde_json::Value::as_array_mut)
        .ok_or_else(|| project_error("", "captured baseline has no native matching keys"))?;
    let mut admitted_rows = Vec::with_capacity(rows.len());
    for mut row in std::mem::take(rows) {
        let spelling = row
            .get("path")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| project_error("", "captured baseline key has no path"))?;
        let path = Path::new(spelling);
        if !path.is_absolute() {
            admitted_rows.push(row);
            continue;
        }
        // Unrelated absolute diagnostic keys are inert in the captured State,
        // even if one happens to name the private mirror. Keep their original
        // bytes in the input witness, but exclude them from its matching keys.
        let Ok(relative) = path.strip_prefix(original) else {
            changed = true;
            continue;
        };
        if path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
        {
            return Err(project_error(
                "",
                "non-normal absolute baseline diagnostic key is unsupported",
            ));
        }
        let target = mirror.join(relative);
        let target = target
            .to_str()
            .ok_or_else(|| project_error("", "captured baseline path is not UTF-8"))?;
        row["path"] = serde_json::Value::String(target.to_owned());
        admitted_rows.push(row);
        changed = true;
    }
    *rows = admitted_rows;
    if !changed {
        return Ok(None);
    }
    let bytes = serde_json::to_vec(&document)
        .map_err(|_| project_error("", "rebased baseline encoding failed"))?;
    if bytes.len() as u64 > CONFIG_BYTES {
        return Err(project_error("", "rebased baseline exceeds capture bound"));
    }
    Ok(Some(bytes))
}

fn read_baseline(
    root: &Path,
    relative: &Path,
    control: PythonProjectControl<'_>,
) -> Result<Vec<u8>, CheckerError> {
    read_captured_input(root, relative, Some(control))?.ok_or_else(|| {
        project_error(
            "",
            "configured baseline is absent from the captured source root",
        )
    })
}

pub(super) fn read_captured_input(
    root: &Path,
    relative: &Path,
    control: Option<PythonProjectControl<'_>>,
) -> Result<Option<Vec<u8>>, CheckerError> {
    // Each component is admitted when its handle is opened. These handles pin
    // the selected objects, not their continuously changing namespace position.
    // Publication separately reopens the original root-relative paths and
    // checks their current content witnesses.
    input_checkpoint(control)?;
    // O_NOFOLLOW applies only to the final component. Lexically remove root
    // trailing separators/dots so they cannot turn a final symlink into an
    // intermediate component; this does not resolve any filesystem alias.
    let root_entry = root.components().collect::<PathBuf>();
    let components = relative.components().collect::<Vec<_>>();
    if !root_entry.is_absolute()
        || root_entry
            .components()
            .any(|part| matches!(part, Component::ParentDir))
        || components.is_empty()
        || components.len() > 64
        || !components
            .iter()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err(CheckerError::UncapturedDependency {
            path: root.join(relative),
        });
    }
    read_regular_no_follow(&root_entry, relative, control)
}

fn input_checkpoint(control: Option<PythonProjectControl<'_>>) -> Result<(), CheckerError> {
    control.map_or(Ok(()), checkpoint)
}

#[cfg(unix)]
fn read_regular_no_follow(
    root: &Path,
    relative: &Path,
    control: Option<PythonProjectControl<'_>>,
) -> Result<Option<Vec<u8>>, CheckerError> {
    use rustix::fs::{Mode, OFlags, open, openat};

    let directory_flags =
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK;
    let mut directory = open(root, directory_flags, Mode::empty())
        .map_err(|error| workspace_error(error.into()))?;
    let mut components = relative.components().peekable();
    let file = loop {
        input_checkpoint(control)?;
        let Component::Normal(name) = components.next().expect("admitted captured-input path")
        else {
            unreachable!("admitted captured-input path")
        };
        let last = components.peek().is_none();
        let flags = if last {
            OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC | OFlags::NONBLOCK
        } else {
            directory_flags
        };
        let opened = match openat(&directory, name, flags, Mode::empty()) {
            Ok(file) => file,
            Err(error) if error == rustix::io::Errno::NOENT => return Ok(None),
            Err(error) => return Err(workspace_error(error.into())),
        };
        if last {
            break opened;
        }
        directory = opened;
    };
    read_bounded_file(std::fs::File::from(file), control).map(Some)
}

#[cfg(windows)]
fn read_regular_no_follow(
    root: &Path,
    relative: &Path,
    control: Option<PythonProjectControl<'_>>,
) -> Result<Option<Vec<u8>>, CheckerError> {
    use backend_platform::win32::project_fs::{ProjectRoot, revision_for_file};

    // Reuse the platform's handle-relative reader. It pins every local-drive
    // ancestor and rejects reparses and special/hard-linked files; Python adds
    // no unsafe API or ambient path fallback here.
    let root = ProjectRoot::open(root).map_err(workspace_error)?;
    let parts = relative
        .components()
        .map(|part| {
            let Component::Normal(name) = part else {
                unreachable!("admitted captured-input path")
            };
            name.to_str()
                .ok_or_else(|| project_error("", "captured input path is not UTF-8"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    input_checkpoint(control)?;
    let file = match root.open_file_read(&parts) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(workspace_error(error)),
    };
    let before = revision_for_file(&file).map_err(workspace_error)?;
    let bytes = read_bounded_file(file.try_clone().map_err(workspace_error)?, control)?;
    let after = revision_for_file(&file).map_err(workspace_error)?;
    if before != after || before.length != bytes.len() as u64 {
        return Err(project_error(
            "",
            "captured compiler input changed while reading",
        ));
    }
    Ok(Some(bytes))
}

fn read_bounded_file(
    file: std::fs::File,
    control: Option<PythonProjectControl<'_>>,
) -> Result<Vec<u8>, CheckerError> {
    read_bounded_file_with(file, control, std::io::Read::read)
}

fn read_bounded_file_with(
    mut file: std::fs::File,
    control: Option<PythonProjectControl<'_>>,
    mut read: impl FnMut(&mut std::fs::File, &mut [u8]) -> std::io::Result<usize>,
) -> Result<Vec<u8>, CheckerError> {
    let before = file.metadata().map_err(workspace_error)?;
    if !before.is_file() || before.len() > CONFIG_BYTES {
        return Err(project_error(
            "",
            "compiler input is not a bounded regular captured file",
        ));
    }
    let mut output = Vec::new();
    let mut buffer = [0; 65536];
    loop {
        input_checkpoint(control)?;
        let count = read(&mut file, &mut buffer).map_err(workspace_error)?;
        if count == 0 {
            break;
        }
        if output.len() as u64 + count as u64 > CONFIG_BYTES {
            return Err(project_error(
                "",
                "compiler input changed beyond capture bound",
            ));
        }
        output.extend_from_slice(&buffer[..count]);
    }
    input_checkpoint(control)?;
    // These observations come from the same opened file, not a path lookup.
    // Checking the exact observed length also rejects an early EOF or growth
    // even when the platform's timestamp resolution cannot reveal the change.
    let after = file.metadata().map_err(workspace_error)?;
    if !after.is_file()
        || before.len() != output.len() as u64
        || after.len() != output.len() as u64
        || !same_open_file_revision(&before, &after)
    {
        return Err(project_error(
            "",
            "captured compiler input changed while reading",
        ));
    }
    Ok(output)
}

#[cfg(unix)]
fn same_open_file_revision(before: &std::fs::Metadata, after: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    before.dev() == after.dev()
        && before.ino() == after.ino()
        && before.size() == after.size()
        && before.mtime() == after.mtime()
        && before.mtime_nsec() == after.mtime_nsec()
        && before.ctime() == after.ctime()
        && before.ctime_nsec() == after.ctime_nsec()
}

#[cfg(not(unix))]
fn same_open_file_revision(_before: &std::fs::Metadata, _after: &std::fs::Metadata) -> bool {
    // Windows additionally compares the existing platform's same-handle
    // revision around this entire read in read_regular_no_follow.
    true
}

#[cfg(not(any(unix, windows)))]
fn read_regular_no_follow(
    root: &Path,
    relative: &Path,
    _control: Option<PythonProjectControl<'_>>,
) -> Result<Option<Vec<u8>>, CheckerError> {
    // Neither supported handle-relative platform reader exists on this target.
    Err(CheckerError::UncapturedDependency {
        path: root.join(relative),
    })
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;
    use std::time::{Duration, Instant};

    use super::super::super::Workspace;
    use super::super::PythonProjectSource;
    use super::*;

    fn control(cancelled: &AtomicBool) -> PythonProjectControl<'_> {
        PythonProjectControl {
            cancelled,
            deadline: Instant::now() + Duration::from_secs(30),
        }
    }

    fn captured_configuration_witness(
        root: &Path,
        relative: &Path,
    ) -> Result<FileWitness, CheckerError> {
        let bytes = read_captured_input(root, relative, None)?;
        Ok(FileWitness {
            path: root.join(relative),
            digest: bytes
                .as_ref()
                .map(|bytes| (blake3::hash(bytes), bytes.len() as u64)),
            read: FileWitnessRead::CapturedInput {
                root: root.to_path_buf(),
            },
        })
    }

    #[test]
    #[cfg(unix)]
    fn captured_configuration_revalidation_rejects_same_bytes_symlink_swaps()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let original = Workspace::create()?;
        let outside = Workspace::create()?;
        let bytes = b"[tool.pyrefly]\nsearch-path = [\"src\"]\n";
        std::fs::write(outside.path.join("pyproject.toml"), bytes)?;
        std::fs::write(original.path.join("pyproject.toml"), bytes)?;
        let alias = outside.path.join("root-alias");
        symlink(&original.path, &alias)?;
        for spelling in [alias.clone(), alias.join(""), alias.join(".")] {
            assert!(read_captured_input(&spelling, Path::new("pyproject.toml"), None).is_err());
        }
        let leaf = captured_configuration_witness(&original.path, Path::new("pyproject.toml"))?;
        leaf.validate_current()?;
        // Deterministically replace the previously captured regular leaf with
        // an external symlink carrying identical bytes. Digest equality alone
        // used to accept this configuration substitution.
        std::fs::remove_file(&leaf.path)?;
        symlink(outside.path.join("pyproject.toml"), &leaf.path)?;
        assert!(leaf.validate_current().is_err());

        std::fs::create_dir(original.path.join("nested"))?;
        std::fs::write(original.path.join("nested/pyproject.toml"), bytes)?;
        let intermediate =
            captured_configuration_witness(&original.path, Path::new("nested/pyproject.toml"))?;
        intermediate.validate_current()?;
        std::fs::rename(
            original.path.join("nested"),
            original.path.join("saved-nested"),
        )?;
        assert!(intermediate.validate_current().is_err());
        symlink(&outside.path, original.path.join("nested"))?;
        assert!(intermediate.validate_current().is_err());

        let absent =
            captured_configuration_witness(&original.path, Path::new("missing/pyproject.toml"))?;
        assert!(absent.digest.is_none());
        symlink(&outside.path, original.path.join("missing"))?;
        assert!(absent.validate_current().is_err());
        Ok(())
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn captured_configuration_revalidation_rejects_fifo_swaps_without_blocking()
    -> Result<(), Box<dyn std::error::Error>> {
        use rustix::fs::{CWD, Mode, OFlags, mkfifoat, open};
        use std::sync::mpsc;

        fn assert_nonblocking_refusal(witness: FileWitness, fifo: &Path) {
            let (sender, receiver) = mpsc::channel();
            let worker = std::thread::spawn(move || {
                sender
                    .send(witness.validate_current().is_err())
                    .expect("fixture receiver");
            });
            let outcome = receiver.recv_timeout(Duration::from_secs(2));
            // A regressed blocking open must fail the test rather than strand
            // the whole native runner. Opening both ends releases that open.
            if outcome.is_err() {
                let release = open(
                    fifo,
                    OFlags::RDWR | OFlags::NONBLOCK | OFlags::CLOEXEC,
                    Mode::empty(),
                );
                drop(release);
            }
            worker.join().expect("captured-input worker");
            assert_eq!(
                outcome.ok(),
                Some(true),
                "special input must refuse without a writer"
            );
        }

        let original = Workspace::create()?;
        let bytes = b"[tool.pyrefly]\n";
        let leaf = original.path.join("pyproject.toml");
        std::fs::write(&leaf, bytes)?;
        let witness = captured_configuration_witness(&original.path, Path::new("pyproject.toml"))?;
        std::fs::remove_file(&leaf)?;
        mkfifoat(CWD, &leaf, Mode::RUSR | Mode::WUSR)?;
        assert_nonblocking_refusal(witness, &leaf);

        let nested = original.path.join("nested");
        std::fs::create_dir(&nested)?;
        std::fs::write(nested.join("pyproject.toml"), bytes)?;
        let witness =
            captured_configuration_witness(&original.path, Path::new("nested/pyproject.toml"))?;
        std::fs::rename(&nested, original.path.join("saved-nested"))?;
        mkfifoat(CWD, &nested, Mode::RUSR | Mode::WUSR)?;
        assert_nonblocking_refusal(witness, &nested);
        Ok(())
    }

    #[test]
    fn temporary_workspaces_are_distinct_at_the_same_clock_tick()
    -> Result<(), Box<dyn std::error::Error>> {
        let workspaces = (0..32)
            .map(|_| Workspace::create_at(0))
            .collect::<Result<Vec<_>, _>>()?;
        let paths = workspaces
            .iter()
            .map(|workspace| &workspace.path)
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(paths.len(), workspaces.len());
        assert!(paths.iter().all(|path| path.is_dir()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for path in paths {
                assert_eq!(std::fs::metadata(path)?.permissions().mode() & 0o777, 0o700);
            }
        }
        Ok(())
    }

    #[test]
    fn captured_configuration_preserves_absence_bounds_cancellation_and_native_error_detail()
    -> Result<(), Box<dyn std::error::Error>> {
        let original = Workspace::create()?;
        let private = Workspace::create()?;
        let cancelled = AtomicBool::new(false);
        let missing = captured_configuration_witness(&original.path, Path::new("pyproject.toml"))?;
        assert!(missing.digest.is_none());
        missing.validate_current()?;
        std::fs::write(&missing.path, "[tool.pyrefly]\n")?;
        assert!(missing.validate_current().is_err());
        assert_eq!(
            read_captured_input(
                &original.path,
                Path::new("pyproject.toml"),
                Some(control(&cancelled))
            )?,
            Some(b"[tool.pyrefly]\n".to_vec())
        );
        let large = std::fs::File::create(original.path.join("large.toml"))?;
        large.set_len(CONFIG_BYTES + 1)?;
        assert!(
            read_captured_input(
                &original.path,
                Path::new("large.toml"),
                Some(control(&cancelled))
            )
            .is_err()
        );
        cancelled.store(true, std::sync::atomic::Ordering::Release);
        assert!(matches!(
            read_captured_input(
                &original.path,
                Path::new("pyproject.toml"),
                Some(control(&cancelled))
            ),
            Err(CheckerError::Cancelled { .. })
        ));
        cancelled.store(false, std::sync::atomic::Ordering::Release);
        assert!(matches!(
            read_captured_input(
                &original.path,
                Path::new("pyproject.toml"),
                Some(PythonProjectControl {
                    cancelled: &cancelled,
                    deadline: Instant::now()
                })
            ),
            Err(CheckerError::Deadline { .. })
        ));

        let selected = [PythonProjectSource {
            relative_path: "module.py",
            source: "",
        }];
        let layout = CapturedProjectLayout::new(&private.path, &original.path, &selected)?;
        std::fs::create_dir_all(layout.source_root())?;
        let config = layout.source_root().join("pyrefly.toml");
        std::fs::write(&config, "search-path = [\n")?;
        let (_, errors) = ConfigFile::from_file(&config);
        assert!(!errors.is_empty());
        let expected = format!(
            "native captured configuration errors: {}",
            errors
                .iter()
                .map(|error| error.get_message())
                .collect::<Vec<_>>()
                .join("; ")
        );
        match CapturedBaselines::capture(
            &layout,
            &original.path,
            &[config.clone()],
            &mut Vec::new(),
            control(&cancelled),
        ) {
            Err(CheckerError::ProjectReport { path, message }) => {
                assert_eq!(path, config);
                assert_eq!(message, expected);
            }
            other => panic!("expected exact native configuration error, got {other:?}"),
        }
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn captured_input_rejects_changes_during_same_open_file_read()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::io::{Read, Seek, SeekFrom, Write};

        let root = Workspace::create()?;
        let path = root.path.join("input.json");
        let original = vec![b'a'; 2 * 65536 + 17];
        std::fs::write(&path, &original)?;
        assert_eq!(
            read_bounded_file(std::fs::File::open(&path)?, None)?,
            original
        );

        for mutation in ["rewrite", "shrink", "grow", "early-eof"] {
            std::fs::write(&path, &original)?;
            let file = std::fs::File::open(&path)?;
            let mut writer = std::fs::OpenOptions::new().write(true).open(&path)?;
            let mut reads = 0;
            let result = read_bounded_file_with(file, None, |file, buffer| {
                reads += 1;
                if mutation == "early-eof" && reads == 2 {
                    return Ok(0);
                }
                let count = file.read(buffer)?;
                if reads == 1 {
                    match mutation {
                        "rewrite" => {
                            writer.seek(SeekFrom::Start(65536))?;
                            writer.write_all(&[b'b'; 65536])?;
                            // Make the modified-time difference deterministic
                            // even on filesystems with coarse clock ticks.
                            writer.set_times(std::fs::FileTimes::new().set_modified(
                                std::time::SystemTime::UNIX_EPOCH + Duration::from_secs(42),
                            ))?;
                        }
                        "shrink" => writer.set_len(65536)?,
                        "grow" => writer.set_len(3 * 65536)?,
                        "early-eof" => {}
                        _ => unreachable!("closed mutation fixture"),
                    }
                }
                Ok(count)
            });
            assert!(
                matches!(
                    result,
                    Err(CheckerError::ProjectReport { ref message, .. })
                        if message == "captured compiler input changed while reading"
                ),
                "same-open-file {mutation} must refuse, got {result:?}"
            );
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            std::fs::write(&path, &original)?;
            let file = std::fs::File::open(&path)?;
            let mut writer = std::fs::OpenOptions::new().write(true).open(&path)?;
            let before = file.metadata()?;
            let original_modified = before.modified()?;
            let mut reads = 0;
            let result = read_bounded_file_with(file, None, |file, buffer| {
                reads += 1;
                let count = file.read(buffer)?;
                if reads == 1 {
                    writer.seek(SeekFrom::Start(65536))?;
                    writer.write_all(&[b'c'; 65536])?;
                    writer.set_times(std::fs::FileTimes::new().set_modified(original_modified))?;
                    // Bound the fixture's wait for a distinct change-time tick;
                    // the production reader itself performs no waits.
                    let deadline = Instant::now() + Duration::from_secs(2);
                    while {
                        let after = writer.metadata()?;
                        (before.ctime(), before.ctime_nsec()) == (after.ctime(), after.ctime_nsec())
                    } {
                        assert!(Instant::now() < deadline, "change-time fixture unavailable");
                        std::thread::sleep(Duration::from_millis(1));
                        writer.set_permissions(before.permissions())?;
                    }
                    let after = writer.metadata()?;
                    assert_eq!(before.len(), after.len());
                    assert_eq!(before.modified()?, after.modified()?);
                }
                Ok(count)
            });
            assert!(matches!(
                result,
                Err(CheckerError::ProjectReport { ref message, .. })
                    if message == "captured compiler input changed while reading"
            ));
        }
        Ok(())
    }

    #[test]
    fn absolute_baseline_keys_rebase_only_exact_source_root()
    -> Result<(), Box<dyn std::error::Error>> {
        use pyrefly::NativeDiagnostic as Error;
        use pyrefly_config::error_kind::ErrorKind;
        use pyrefly_python::module::Module;
        use pyrefly_python::module_name::ModuleName;
        use pyrefly_python::module_path::ModulePath;
        use ruff_native_text_size::{TextRange, TextSize};
        use std::sync::Arc;

        let original = Workspace::create()?;
        let private = Workspace::create()?;
        let selected = [PythonProjectSource {
            relative_path: "api.py",
            source: "",
        }];
        let layout = CapturedProjectLayout::new(&private.path, &original.path, &selected)?;
        std::fs::create_dir_all(layout.source_root())?;
        let config = layout.source_root().join("pyrefly.toml");
        std::fs::write(&config, "baseline = \"baseline.json\"\n")?;
        let document = |path: &Path| {
            serde_json::to_vec(
                &serde_json::json!({"errors":[{"column":3,"path":path,"name":"bad-return","concise_description":"diagnostic","severity":"error"}]}),
            )
        };
        let bytes = document(&original.path.join("api.py"))?;
        let original_baseline = original.path.join("baseline.json");
        std::fs::write(&original_baseline, &bytes)?;
        let cancelled = AtomicBool::new(false);
        let mut mirrors = Vec::new();
        let inputs = CapturedBaselines::capture(
            &layout,
            &original.path,
            &[config.clone()],
            &mut mirrors,
            control(&cancelled),
        )?;
        let captured = layout.source_root().join("baseline.json");
        let captured_bytes = std::fs::read(&captured)?;
        let before: serde_json::Value = serde_json::from_slice(&bytes)?;
        let mut after: serde_json::Value = serde_json::from_slice(&captured_bytes)?;
        assert_eq!(
            after["errors"][0]["path"],
            serde_json::json!(layout.source_root().join("api.py"))
        );
        after["errors"][0]["path"] = before["errors"][0]["path"].clone();
        assert_eq!(after, before, "only the source-root path key may change");
        assert_eq!(std::fs::read(&original_baseline)?, bytes);
        let module = Module::new(
            ModuleName::from_str("api"),
            ModulePath::filesystem(layout.source_root().join("api.py")),
            Arc::new("0123456789".to_owned()),
        );
        let error = Error::new(
            module,
            TextRange::new(TextSize::new(2), TextSize::new(4)),
            "diagnostic".to_owned(),
            Vec::new(),
            ErrorKind::BadReturn,
        );
        assert!(
            BaselineProcessor::from_file(&captured, layout.source_root())?.matches_baseline(&error)
        );
        assert!(
            !BaselineProcessor::from_file(&original_baseline, layout.source_root())?
                .matches_baseline(&error)
        );
        inputs.validate_current(control(&cancelled))?;
        for mirror in mirrors {
            mirror.validate_current()?;
        }

        let relative = document(Path::new("api.py"))?;
        assert!(
            rebase_absolute_baseline_keys(&relative, &original.path, layout.source_root())?
                .is_none()
        );
        let outside = private.path.join("outside.py");
        let lookalike =
            PathBuf::from(format!("{}-lookalike", original.path.display())).join("api.py");
        // The third foreign key would match this exact private diagnostic if
        // outside-root keys were merely retained without filtering.
        let mirror_key = layout.source_root().join("api.py");
        for path in [outside, lookalike, mirror_key] {
            let original_bytes = document(&path)?;
            std::fs::write(&original_baseline, &original_bytes)?;
            let inputs = CapturedBaselines::capture(
                &layout,
                &original.path,
                &[config.clone()],
                &mut Vec::new(),
                control(&cancelled),
            )?;
            let admitted: serde_json::Value = serde_json::from_slice(&std::fs::read(&captured)?)?;
            assert_eq!(admitted, serde_json::json!({"errors":[]}));
            assert_eq!(std::fs::read(&original_baseline)?, original_bytes);
            assert!(
                !BaselineProcessor::from_file(&captured, layout.source_root())?
                    .matches_baseline(&error)
            );
            inputs.validate_current(control(&cancelled))?;
        }

        let mixed = serde_json::to_vec(&serde_json::json!({"errors":[
            {"column":3,"path":original.path.join("api.py"),"name":"bad-return"},
            {"column":4,"path":"other.py","name":"bad-return"},
            {"column":3,"path":layout.source_root().join("api.py"),"name":"bad-return"}
        ]}))?;
        std::fs::write(&original_baseline, &mixed)?;
        let inputs = CapturedBaselines::capture(
            &layout,
            &original.path,
            &[config],
            &mut Vec::new(),
            control(&cancelled),
        )?;
        let admitted: serde_json::Value = serde_json::from_slice(&std::fs::read(&captured)?)?;
        assert_eq!(
            admitted,
            serde_json::json!({"errors":[
                {"column":3,"path":layout.source_root().join("api.py"),"name":"bad-return"},
                {"column":4,"path":"other.py","name":"bad-return"}
            ]})
        );
        assert_eq!(std::fs::read(&original_baseline)?, mixed);
        assert!(
            BaselineProcessor::from_file(&captured, layout.source_root())?.matches_baseline(&error)
        );
        inputs.validate_current(control(&cancelled))?;

        let parent = original.path.join("nested/../api.py");
        assert!(matches!(
            rebase_absolute_baseline_keys(
                &document(&parent)?,
                &original.path,
                layout.source_root()
            ),
            Err(CheckerError::ProjectReport { .. })
        ));
        Ok(())
    }

    #[test]
    #[cfg(windows)]
    fn windows_captured_input_uses_existing_local_drive_boundary()
    -> Result<(), Box<dyn std::error::Error>> {
        let root = Workspace::create()?;
        std::fs::create_dir(root.path.join("nested"))?;
        std::fs::write(root.path.join("nested/pyproject.toml"), b"[tool.pyrefly]\n")?;
        assert_eq!(
            read_captured_input(&root.path, Path::new("nested/pyproject.toml"), None)?,
            Some(b"[tool.pyrefly]\n".to_vec())
        );
        for unsupported in [
            r"\\server\share\project",
            r"\\.\device\project",
            r"relative\project",
        ] {
            assert!(
                read_captured_input(Path::new(unsupported), Path::new("pyproject.toml"), None)
                    .is_err()
            );
        }
        // The platform reader accepts either Windows separator spelling while
        // the baseline mapping still compares exact path components.
        let key = root
            .path
            .join("api.py")
            .to_string_lossy()
            .replace('\\', "/");
        let input = serde_json::to_vec(
            &serde_json::json!({"errors":[{"path":key,"column":1,"name":"bad-return"}]}),
        )?;
        let mirror = root.path.join("mirror");
        let output = rebase_absolute_baseline_keys(&input, &root.path, &mirror)?
            .ok_or("expected absolute rebase")?;
        let output: serde_json::Value = serde_json::from_slice(&output)?;
        assert_eq!(
            output["errors"][0]["path"],
            serde_json::json!(mirror.join("api.py"))
        );

        let witness =
            captured_configuration_witness(&root.path, Path::new("nested/pyproject.toml"))?;
        let target = root.path.join("saved-nested");
        std::fs::rename(root.path.join("nested"), &target)?;
        // Match the existing platform's ordinary-user junction regression;
        // no Developer Mode or privileged symlink creation is assumed.
        let outcome = std::process::Command::new("cmd.exe")
            .args(["/C", "mklink", "/J"])
            .arg(root.path.join("nested"))
            .arg(&target)
            .output()?;
        assert!(
            outcome.status.success(),
            "junction fixture failed: {outcome:?}"
        );
        assert!(witness.validate_current().is_err());
        Ok(())
    }

    #[test]
    fn native_compact_and_legacy_baselines_have_identical_exact_matching_keys()
    -> Result<(), Box<dyn std::error::Error>> {
        use pyrefly::NativeDiagnostic as Error;
        use pyrefly_config::error_kind::ErrorKind;
        use pyrefly_python::module::Module;
        use pyrefly_python::module_name::ModuleName;
        use pyrefly_python::module_path::ModulePath;
        use ruff_native_text_size::{TextRange, TextSize};
        use std::sync::Arc;

        let root = Workspace::create()?;
        let path = root.path.join("baseline.json");
        let compact = r#"{"errors":[{"column":3,"path":"api.py","name":"bad-return","concise_description":"diagnostic","severity":"error"}]}"#;
        let legacy = r#"{"errors":[{"line":1,"column":3,"stop_line":1,"stop_column":5,"path":"api.py","code":-2,"name":"bad-return","description":"diagnostic","concise_description":"diagnostic"}]}"#;
        let compact_with_metadata = r#"{"errors":[{"column":3,"path":"api.py","name":"bad-return","unknown":true}],"writer":{"version":2}}"#;
        let legacy_with_metadata = r#"{"errors":[{"line":1,"column":3,"stop_line":1,"stop_column":5,"path":"api.py","code":-2,"name":"bad-return","description":"diagnostic","concise_description":"diagnostic","unknown":{"detail":true}}],"writer":42}"#;
        let module = Module::new(
            ModuleName::from_str("api"),
            ModulePath::filesystem(root.path.join("api.py")),
            Arc::new("0123456789".to_owned()),
        );
        let error = Error::new(
            module.clone(),
            TextRange::new(TextSize::new(2), TextSize::new(4)),
            "diagnostic".to_owned(),
            Vec::new(),
            ErrorKind::BadReturn,
        );
        let different_column = Error::new(
            module,
            TextRange::new(TextSize::new(3), TextSize::new(4)),
            "diagnostic".to_owned(),
            Vec::new(),
            ErrorKind::BadReturn,
        );
        for input in [compact, legacy, compact_with_metadata, legacy_with_metadata] {
            std::fs::write(&path, input)?;
            let processor = BaselineProcessor::from_file(&path, &root.path)?;
            assert!(processor.matches_baseline(&error));
            assert!(!processor.matches_baseline(&different_column));
            let mut ordinary = vec![error.clone(), different_column.clone()];
            let mut baseline = Vec::new();
            processor.process_errors(&mut ordinary, &mut baseline);
            assert_eq!(ordinary, vec![different_column.clone()]);
            assert_eq!(baseline, vec![error.clone()]);
        }
        for invalid in [
            "not JSON",
            r#"{"errors":[{"column":3,"path":"api.py"}]}"#,
            r#"{"errors":[{"column":3,"column":4,"path":"api.py","name":"bad-return"}]}"#,
            r#"{"errors":[{"column":3,"path":"api.py","name":"bad-return","line":"wrong"}]}"#,
            r#"{"errors":[{"column":0,"path":"api.py","name":"bad-return"}]}"#,
            r#"{"errors":[{"column":3,"path":"api.py","name":""}]}"#,
        ] {
            std::fs::write(&path, invalid)?;
            assert!(BaselineProcessor::from_file(&path, &root.path).is_err());
        }
        Ok(())
    }

    #[test]
    #[ignore = "requires the pinned Paperless v3.3.0 configuration/baseline input directory"]
    fn authentic_paperless_configuration_baseline_is_captured_without_policy_rewrite()
    -> Result<(), Box<dyn std::error::Error>> {
        let input = PathBuf::from(std::env::var("NUDOX_PAPERLESS_CONFIG_CONTROL")?);
        let config = std::fs::read(input.join("pyproject.toml"))?;
        let baseline = std::fs::read(input.join(".pyrefly-baseline.json"))?;
        assert_eq!(config.len(), 10849);
        assert_eq!(baseline.len(), 256741);
        assert_eq!(
            blake3::hash(&config).to_hex().as_str(),
            "d0e2775ec90016635a106410ca009f478b69d59d9818f7a6cfe62f64cc1a190d"
        );
        assert_eq!(
            blake3::hash(&baseline).to_hex().as_str(),
            "f3d5f806f1fc22088a628799277ad3963315f8d2c4fb80c98a9650287fcccd50"
        );
        let original = Workspace::create()?;
        let private = Workspace::create()?;
        let selected = [PythonProjectSource {
            relative_path: "src/module.py",
            source: "",
        }];
        let layout = CapturedProjectLayout::new(&private.path, &original.path, &selected)?;
        std::fs::create_dir_all(layout.source_root().join("src"))?;
        std::fs::write(original.path.join(".pyrefly-baseline.json"), &baseline)?;
        let config_path = layout.source_root().join("pyproject.toml");
        std::fs::write(&config_path, &config)?;
        let (before, errors) = ConfigFile::from_file(&config_path);
        assert!(errors.is_empty());
        assert_eq!(
            before.baseline,
            Some(layout.source_root().join(".pyrefly-baseline.json"))
        );
        assert_eq!(
            before.search_path_from_file,
            vec![layout.source_root().join("src")]
        );
        let cancelled = AtomicBool::new(false);
        let mut mirror_files = Vec::new();
        let inputs = CapturedBaselines::capture(
            &layout,
            &original.path,
            &[config_path.clone()],
            &mut mirror_files,
            control(&cancelled),
        )?;
        let (after, errors) = ConfigFile::from_file(&config_path);
        assert!(errors.is_empty());
        assert_eq!(before.baseline, after.baseline);
        assert_eq!(before.search_path_from_file, after.search_path_from_file);
        assert_eq!(std::fs::read(config_path)?, config);
        assert_eq!(
            std::fs::read(layout.source_root().join(".pyrefly-baseline.json"))?,
            baseline
        );
        inputs.validate_current(control(&cancelled))?;
        Ok(())
    }

    #[test]
    fn native_configuration_baseline_has_exact_mirror_admission_and_change_guard()
    -> Result<(), Box<dyn std::error::Error>> {
        let original = Workspace::create()?;
        let private = Workspace::create()?;
        let selected = [PythonProjectSource {
            relative_path: "module.py",
            source: "",
        }];
        let layout = CapturedProjectLayout::new(&private.path, &original.path, &selected)?;
        std::fs::create_dir_all(layout.source_root())?;
        let path = layout.source_root().join("pyproject.toml");
        // This is Paperless's actual diagnostic-source/search-root declaration.
        std::fs::write(
            &path,
            "[tool.pyrefly]\npython-platform = \"linux\"\nsearch-path = [\"src\"]\nbaseline = \".pyrefly-baseline.json\"\n",
        )?;
        std::fs::write(
            original.path.join(".pyrefly-baseline.json"),
            "{\"errors\":[]}",
        )?;
        let (before, errors) = ConfigFile::from_file(&path);
        assert!(errors.is_empty());
        let expected = layout.source_root().join(".pyrefly-baseline.json");
        assert_eq!(before.baseline, Some(expected.clone()));
        let cancelled = AtomicBool::new(false);
        let mut mirror_files = Vec::new();
        let inputs = CapturedBaselines::capture(
            &layout,
            &original.path,
            &[path.clone()],
            &mut mirror_files,
            control(&cancelled),
        )?;
        let (after, errors) = ConfigFile::from_file(&path);
        assert!(errors.is_empty());
        assert_eq!(before.baseline, after.baseline);
        assert_eq!(before.search_path_from_file, after.search_path_from_file);
        assert_eq!(std::fs::read(&expected)?, b"{\"errors\":[]}");
        inputs.admit(&mut expected.clone(), &layout, &original.path)?;
        assert!(
            inputs
                .admit(
                    &mut layout.source_root().join("other.json"),
                    &layout,
                    &original.path
                )
                .is_err()
        );
        inputs.validate_current(control(&cancelled))?;
        std::fs::write(
            original.path.join(".pyrefly-baseline.json"),
            "{\"errors\": []}",
        )?;
        assert!(inputs.validate_current(control(&cancelled)).is_err());

        // A pinned read does not lock directory names. Revalidation must
        // reopen the original path and refuse a baseline parent moved away.
        std::fs::create_dir(original.path.join("inputs"))?;
        std::fs::write(
            original.path.join("inputs/baseline.json"),
            "{\"errors\":[]}",
        )?;
        std::fs::write(
            &path,
            "[tool.pyrefly]\nbaseline = \"inputs/baseline.json\"\n",
        )?;
        let inputs = CapturedBaselines::capture(
            &layout,
            &original.path,
            &[path],
            &mut Vec::new(),
            control(&cancelled),
        )?;
        inputs.validate_current(control(&cancelled))?;
        std::fs::rename(
            original.path.join("inputs"),
            private.path.join("moved-inputs"),
        )?;
        assert!(inputs.validate_current(control(&cancelled)).is_err());
        std::fs::create_dir(original.path.join("inputs"))?;
        std::fs::write(
            original.path.join("inputs/baseline.json"),
            "{\"errors\": []}",
        )?;
        assert!(inputs.validate_current(control(&cancelled)).is_err());
        Ok(())
    }

    #[test]
    #[cfg(unix)]
    fn baseline_capture_refuses_outside_missing_malformed_symlink_bound_and_cancelled_inputs()
    -> Result<(), Box<dyn std::error::Error>> {
        use std::os::unix::fs::symlink;

        let original = Workspace::create()?;
        let private = Workspace::create()?;
        let outside = Workspace::create()?;
        let selected = [PythonProjectSource {
            relative_path: "module.py",
            source: "",
        }];
        let layout = CapturedProjectLayout::new(&private.path, &original.path, &selected)?;
        std::fs::create_dir_all(layout.source_root())?;
        let config = layout.source_root().join("pyrefly.toml");
        let cancelled = AtomicBool::new(false);
        let capture = || {
            CapturedBaselines::capture(
                &layout,
                &original.path,
                &[config.clone()],
                &mut Vec::new(),
                control(&cancelled),
            )
        };
        let external = outside.path.join("external.json");
        std::fs::write(&external, "{\"errors\":[]}")?;
        std::fs::write(
            &config,
            format!(
                "baseline = {:?}\n",
                external.to_str().ok_or("UTF-8 fixture")?
            ),
        )?;
        assert!(matches!(
            capture(),
            Err(CheckerError::UncapturedDependency { .. })
        ));
        std::fs::write(&config, "baseline = \"missing.json\"\n")?;
        assert!(capture().is_err());
        std::fs::write(&config, "baseline = \"bad.json\"\n")?;
        std::fs::write(original.path.join("bad.json"), "not JSON")?;
        assert!(matches!(capture(), Err(CheckerError::ProjectReport { .. })));
        std::fs::write(&config, "baseline = \"linked.json\"\n")?;
        symlink(&external, original.path.join("linked.json"))?;
        assert!(capture().is_err());
        symlink(&outside.path, original.path.join("linked-directory"))?;
        std::fs::write(&config, "baseline = \"linked-directory/external.json\"\n")?;
        assert!(capture().is_err());
        std::fs::write(&config, "baseline = \"large.json\"\n")?;
        let oversized = std::fs::File::create(original.path.join("large.json"))?;
        oversized.set_len(CONFIG_BYTES + 1)?;
        assert!(capture().is_err());
        cancelled.store(true, std::sync::atomic::Ordering::Release);
        assert!(matches!(capture(), Err(CheckerError::Cancelled { .. })));
        Ok(())
    }
}
