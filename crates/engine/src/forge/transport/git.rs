use super::super::*;
use super::ForgeTransport;

/// Safe process-backed transport for a generic HTTPS Git source.
///
/// The fetch is a shallow partial clone (`--depth=1 --filter=blob:none`) when
/// the remote can serve one. Historical blobs stay on the remote. A remote
/// that only speaks dumb HTTP falls back to a full fetch of the same ref.
/// Ambient credential helpers are disabled; an optional token is attached
/// only as a process-local Authorization header.
pub struct GitCommandTransport {
    root: PathBuf,
    token: Option<ForgeAuthToken>,
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
        Ok(Self { root, token: None })
    }

    /// Attaches a process-local bearer token. The token is not written into
    /// the repository URL or the on-disk git config.
    #[must_use]
    pub fn with_token(mut self, token: ForgeAuthToken) -> Self {
        self.token = Some(token);
        self
    }

    fn command(&self) -> Command {
        let mut command = Command::new("git");
        command
            .env("LC_ALL", "C")
            .env("LANGUAGE", "C")
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_CONFIG_COUNT", "1")
            .env("GIT_CONFIG_KEY_0", "credential.helper")
            .env("GIT_CONFIG_VALUE_0", "")
            .stdin(Stdio::null());
        if let Some(token) = &self.token {
            command
                .env("GIT_CONFIG_COUNT", "2")
                .env("GIT_CONFIG_KEY_1", "http.extraheader")
                .env(
                    "GIT_CONFIG_VALUE_1",
                    format!("Authorization: {}", token.authorization_header()),
                );
        }
        command
    }

    fn prepare_repo(&self) -> Result<String, ForgeTransportError> {
        let repo = self.root.join("repo");
        if repo.exists() {
            fs::remove_dir_all(&repo).map_err(|_| ForgeTransportError::Unavailable)?;
        }
        let target = repo.to_string_lossy().into_owned();
        let status = self
            .command()
            .args(["init", "--quiet", &target])
            .current_dir(&self.root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|_| ForgeTransportError::Unavailable)?;
        if !status.success() {
            return Err(ForgeTransportError::Protocol);
        }
        Ok(target)
    }

    fn configure_partial_remote(
        &self,
        target: &str,
        url: &str,
    ) -> Result<(), ForgeTransportError> {
        self.git_quiet(&["-C", target, "remote", "add", "origin", url])?;
        self.git_quiet(&["-C", target, "config", "remote.origin.promisor", "true"])?;
        self.git_quiet(&[
            "-C",
            target,
            "config",
            "remote.origin.partialclonefilter",
            "blob:none",
        ])
    }

    fn git_quiet(&self, args: &[&str]) -> Result<(), ForgeTransportError> {
        let status = self
            .command()
            .args(args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|_| ForgeTransportError::Unavailable)?;
        if status.success() {
            Ok(())
        } else {
            Err(ForgeTransportError::Protocol)
        }
    }

    fn fetch_revision(&self, url: &str, revision: &str) -> Result<(), ForgeTransportError> {
        let target = self.prepare_repo()?;
        if self.configure_partial_remote(&target, url).is_ok() {
            let partial = self
                .command()
                .args([
                    "-C",
                    &target,
                    "fetch",
                    "--quiet",
                    "--no-tags",
                    "--depth=1",
                    "--filter=blob:none",
                    "origin",
                    revision,
                ])
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .output()
                .map_err(|_| ForgeTransportError::Unavailable)?;
            if partial.status.success() {
                return Ok(());
            }
            let failure = classify_fetch_failure(&partial.stderr);
            if failure == ForgeTransportError::NotFound
                || !remote_rejects_partial_clone(&partial.stderr)
            {
                return Err(failure);
            }
        }
        let target = self.prepare_repo()?;
        let full = self
            .command()
            .args([
                "-C",
                &target,
                "fetch",
                "--quiet",
                "--no-tags",
                url,
                revision,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .map_err(|_| ForgeTransportError::Unavailable)?;
        if full.status.success() {
            Ok(())
        } else {
            Err(classify_fetch_failure(&full.stderr))
        }
    }
}

impl ForgeTransport for GitCommandTransport {
    fn resolve(
        &mut self,
        coordinate: &ForgeCoordinate,
    ) -> Result<ForgeResolution, ForgeTransportError> {
        let revision_token = coordinate_revision_token(coordinate.revision());
        self.fetch_revision(coordinate.repository_url(), &revision_token)?;
        let target = self.root.join("repo").to_string_lossy().into_owned();
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
        let target = self.root.join("repo").to_string_lossy().into_owned();
        let commit = resolution.commit.as_hex();
        let mut child = self
            .command()
            .args(["-C", &target, "archive", "--format=tar", &commit])
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
        let output = self
            .command()
            .args(["-C", target])
            .args(args)
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

fn remote_rejects_partial_clone(stderr: &[u8]) -> bool {
    let diagnostics = String::from_utf8_lossy(stderr).to_ascii_lowercase();
    diagnostics.contains("does not support shallow")
        || diagnostics.contains("does not support filter")
        || diagnostics.contains("filtering not recognized")
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
    use super::{ForgeTransportError, classify_fetch_failure, remote_rejects_partial_clone};

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
    fn a_dumb_http_remote_is_eligible_for_the_full_fetch_fallback() {
        assert!(remote_rejects_partial_clone(
            b"fatal: dumb http transport does not support shallow capabilities\n"
        ));
        assert!(!remote_rejects_partial_clone(
            b"fatal: unable to access 'https://x/': Could not resolve host: x\n"
        ));
        assert!(!remote_rejects_partial_clone(
            b"fatal: couldn't find remote ref refs/heads/nope\n"
        ));
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
