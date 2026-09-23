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
        let fetched = Command::new("git")
            .args([
                "-C",
                &target,
                "fetch",
                "--quiet",
                "--no-tags",
                &repo_url_arg,
                &revision_token,
            ])
            // Classification reads git's diagnostics, so pin their language.
            .env("LC_ALL", "C")
            .env("LANGUAGE", "C")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|_| ForgeTransportError::Unavailable)?;
        if !fetched.status.success() {
            return Err(classify_fetch_failure(&fetched.stderr));
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

/// Classifies a failed `git fetch` from its C-locale diagnostics.
///
/// Only an answer from the forge that the repository or revision does not
/// exist is `NotFound`; the service records that as a permanent tombstone.
/// Every other failure (a dropped connection, DNS, TLS, authentication, a
/// server error) is transient and must stay retryable.
fn classify_fetch_failure(stderr: &[u8]) -> ForgeTransportError {
    let diagnostics = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    let missing = [
        "couldn't find remote ref",
        "repository not found",
        "does not appear to be a git repository",
        "remote ref does not exist",
    ];
    if missing.iter().any(|marker| diagnostics.contains(marker)) {
        ForgeTransportError::NotFound
    } else {
        ForgeTransportError::Unavailable
    }
}

#[cfg(test)]
mod classification_tests {
    use super::{ForgeTransportError, classify_fetch_failure};

    #[test]
    fn a_missing_revision_or_repository_is_not_found() {
        for stderr in [
            "fatal: couldn't find remote ref refs/heads/nope\n",
            "remote: Repository not found.\nfatal: repository 'https://x/y/' not found\n",
            "fatal: '/tmp/x' does not appear to be a git repository\n",
        ] {
            assert_eq!(
                classify_fetch_failure(stderr.as_bytes()),
                ForgeTransportError::NotFound,
                "{stderr}"
            );
        }
    }

    #[test]
    fn a_transport_failure_stays_retryable() {
        for stderr in [
            "error: Empty reply from server (curl_result = 52, http_code = 0)\n",
            "fatal: unable to access 'https://x/': Could not resolve host: x\n",
            "fatal: unable to access 'https://x/': Failed to connect to x port 443\n",
            "error: RPC failed; HTTP 503 curl 22\n",
            "",
        ] {
            assert_eq!(
                classify_fetch_failure(stderr.as_bytes()),
                ForgeTransportError::Unavailable,
                "{stderr}"
            );
        }
    }
}
