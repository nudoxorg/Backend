use super::super::*;
use super::ForgeTransport;

/// Safe process-backed transport for a generic HTTPS Git source.
pub struct GitCommandTransport {
    root: PathBuf,
}

struct GitArchiveReader {
    child: Child,
    stdout: ChildStdout,
    finished: bool,
}

impl Read for GitArchiveReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let read = self.stdout.read(bytes)?;
        if read == 0 && !self.finished {
            self.finished = true;
            let status = self.child.wait()?;
            if !status.success() {
                return Err(io::Error::other("git archive failed"));
            }
        }
        Ok(read)
    }
}

impl Drop for GitArchiveReader {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

impl GitCommandTransport {
    /// Creates a transport whose temporary repositories live below `root`.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, ForgeTransportError> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(|_| ForgeTransportError::Unavailable)?;
        Ok(Self { root })
    }
}

impl ForgeTransport for GitCommandTransport {
    fn resolve(
        &mut self,
        coordinate: &ForgeCoordinate,
    ) -> Result<ForgeResolution, ForgeTransportError> {
        let repo = self.root.join("repo");
        if repo.exists() {
            let _ = fs::remove_dir_all(&repo);
        }
        let repo_url = coordinate.repository_url();
        let repo_url_arg = repo_url.to_owned();
        let target = repo.to_string_lossy().to_string();
        let status = Command::new("git")
            .args(["init", "--quiet", &target])
            .current_dir(&self.root)
            .stdin(Stdio::null())
            .status()
            .map_err(|_| ForgeTransportError::Unavailable)?;
        if !status.success() {
            return Err(ForgeTransportError::Protocol);
        }
        let revision_token = coordinate_revision_token(coordinate.revision());
        let status = Command::new("git")
            .args([
                "-C",
                &target,
                "fetch",
                "--quiet",
                "--no-tags",
                &repo_url_arg,
                &revision_token,
            ])
            .stdin(Stdio::null())
            .status()
            .map_err(|_| ForgeTransportError::Unavailable)?;
        if !status.success() {
            return Err(ForgeTransportError::NotFound);
        }
        let commit = self.git_output(&target, &["rev-parse", "FETCH_HEAD"])?;
        let tree = self.git_output(&target, &["show", "-s", "--format=%T", "FETCH_HEAD"])?;
        ForgeResolution::for_coordinate(
            coordinate,
            ForgeObjectId::parse(&commit).map_err(|_| ForgeTransportError::Protocol)?,
            Some(ForgeObjectId::parse(&tree).map_err(|_| ForgeTransportError::Protocol)?),
            commit,
        )
        .map_err(|_| ForgeTransportError::Integrity)
    }

    fn fetch_archive(
        &mut self,
        _coordinate: &ForgeCoordinate,
        resolution: &ForgeResolution,
    ) -> Result<ForgeArchive, ForgeTransportError> {
        let target = self.root.join("repo").to_string_lossy().to_string();
        let commit = resolution.commit.as_hex();
        let mut child = Command::new("git")
            .args(["-C", &target, "archive", "--format=tar", &commit])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .map_err(|_| ForgeTransportError::Unavailable)?;
        let stdout = child.stdout.take().ok_or(ForgeTransportError::Protocol)?;
        Ok(ForgeArchive::from_reader(
            GitArchiveReader {
                child,
                stdout,
                finished: false,
            },
            ForgeArchiveFormat::Tar,
            None::<String>,
        ))
    }
}

impl GitCommandTransport {
    fn git_output(&self, target: &str, args: &[&str]) -> Result<String, ForgeTransportError> {
        let output = Command::new("git")
            .args(["-C", target])
            .args(args)
            .stdin(Stdio::null())
            .output()
            .map_err(|_| ForgeTransportError::Unavailable)?;
        if !output.status.success() {
            return Err(ForgeTransportError::Protocol);
        }
        String::from_utf8(output.stdout)
            .map(|value| value.trim().to_owned())
            .map_err(|_| ForgeTransportError::Protocol)
    }
}

fn coordinate_revision_token(revision: &ForgeRevision) -> String {
    match revision {
        ForgeRevision::Commit(commit) => commit.as_hex(),
        ForgeRevision::Tag(tag) => tag.as_str().to_owned(),
        ForgeRevision::Branch(branch) => branch.as_str().to_owned(),
    }
}
