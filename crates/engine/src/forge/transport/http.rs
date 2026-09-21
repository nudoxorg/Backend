use super::super::*;
use super::ForgeTransport;

/// Bounded HTTP forge transport for the public provider APIs.
pub struct HttpForgeTransport {
    limits: ForgeAcquisitionLimits,
    agent: ureq::Agent,
    token: Option<ForgeAuthToken>,
}

/// Process-local forge token; it never participates in coordinate or journal identity.
#[derive(Clone, Eq, PartialEq)]
pub struct ForgeAuthToken(Arc<str>);

impl ForgeAuthToken {
    /// Admits a token without control bytes.
    pub fn new(value: impl Into<String>) -> Result<Self, ForgeTransportError> {
        let value = value.into();
        if value.is_empty() || value.contains(['\r', '\n']) {
            return Err(ForgeTransportError::Policy);
        }
        Ok(Self(Arc::from(value)))
    }
}

impl fmt::Debug for ForgeAuthToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("ForgeAuthToken([REDACTED])")
    }
}

impl HttpForgeTransport {
    /// Creates a bounded provider transport. Credentials are process-local.
    pub fn new(
        limits: ForgeAcquisitionLimits,
        token: Option<ForgeAuthToken>,
    ) -> Result<Self, ForgeTransportError> {
        let limits = limits.validate().map_err(|_| ForgeTransportError::Bounds)?;
        let config = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(10)))
            .timeout_recv_response(Some(Duration::from_secs(30)))
            .timeout_recv_body(Some(Duration::from_secs(30)))
            .max_redirects(0)
            .http_status_as_error(false)
            .build();
        Ok(Self {
            limits,
            agent: config.new_agent(),
            token,
        })
    }

    fn get(&self, url: &str, maximum: usize) -> Result<Vec<u8>, ForgeTransportError> {
        let mut request = self.agent.get(url).header("accept-encoding", "identity");
        if let Some(token) = &self.token {
            request = request.header("authorization", &format!("Bearer {}", token.0));
        }
        let mut response = request
            .call()
            .map_err(|_| ForgeTransportError::Unavailable)?;
        let status = response.status().as_u16();
        if status == 404 {
            return Err(ForgeTransportError::NotFound);
        }
        if status == 429 {
            return Err(ForgeTransportError::RetryAfter(1_000));
        }
        if !(200..300).contains(&status) {
            return Err(ForgeTransportError::Protocol);
        }
        let mut bytes = Vec::new();
        response
            .body_mut()
            .as_reader()
            .take(u64::try_from(maximum).unwrap_or(u64::MAX).saturating_add(1))
            .read_to_end(&mut bytes)
            .map_err(|_| ForgeTransportError::Protocol)?;
        if bytes.len() > maximum {
            return Err(ForgeTransportError::Bounds);
        }
        Ok(bytes)
    }

    fn get_reader(&self, url: &str) -> Result<Box<dyn Read>, ForgeTransportError> {
        let mut request = self.agent.get(url).header("accept-encoding", "identity");
        if let Some(token) = &self.token {
            request = request.header("authorization", &format!("Bearer {}", token.0));
        }
        let response = request
            .call()
            .map_err(|_| ForgeTransportError::Unavailable)?;
        let status = response.status().as_u16();
        if status == 404 {
            return Err(ForgeTransportError::NotFound);
        }
        if status == 429 {
            return Err(ForgeTransportError::RetryAfter(1_000));
        }
        if !(200..300).contains(&status) {
            return Err(ForgeTransportError::Protocol);
        }
        Ok(Box::new(response.into_body().into_reader()))
    }
}

impl ForgeTransport for HttpForgeTransport {
    fn resolve(
        &mut self,
        coordinate: &ForgeCoordinate,
    ) -> Result<ForgeResolution, ForgeTransportError> {
        let (url, tree_json) = match coordinate.provider() {
            ForgeProvider::Github => (
                format!(
                    "https://api.github.com/repos/{}/{}/commits/{}",
                    coordinate.owner(),
                    coordinate.repository(),
                    revision_path(coordinate.revision())
                ),
                format!(
                    "https://api.github.com/repos/{}/{}/git/trees/{}",
                    coordinate.owner(),
                    coordinate.repository(),
                    revision_path(coordinate.revision())
                ),
            ),
            ForgeProvider::Gitlab => (
                format!(
                    "https://gitlab.com/api/v4/projects/{}/repository/commits/{}",
                    gitlab_project_path(coordinate),
                    revision_path(coordinate.revision())
                ),
                String::new(),
            ),
            ForgeProvider::Codeberg => (
                format!(
                    "{}/api/v1/repos/{}/{}/commits/{}",
                    host_root(coordinate.repository_url()),
                    coordinate.owner(),
                    coordinate.repository(),
                    revision_path(coordinate.revision())
                ),
                format!(
                    "{}/api/v1/repos/{}/{}/git/commits/{}",
                    host_root(coordinate.repository_url()),
                    coordinate.owner(),
                    coordinate.repository(),
                    revision_path(coordinate.revision())
                ),
            ),
            ForgeProvider::GenericHttpsGit => return Err(ForgeTransportError::Policy),
        };
        let bytes = self.get(&url, self.limits.max_metadata_bytes)?;
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| ForgeTransportError::Protocol)?;
        let commit = value
            .get("sha")
            .or_else(|| value.get("id"))
            .and_then(Value::as_str)
            .ok_or(ForgeTransportError::Protocol)?;
        let tree = value
            .get("commit")
            .and_then(|commit| commit.get("tree"))
            .and_then(|tree| tree.get("sha"))
            .and_then(Value::as_str)
            .or_else(|| value.get("tree_id").and_then(Value::as_str));
        let tree = tree
            .map(ForgeObjectId::parse)
            .transpose()
            .map_err(|_| ForgeTransportError::Protocol)?;
        let _ = tree_json;
        ForgeResolution::for_coordinate(
            coordinate,
            ForgeObjectId::parse(commit).map_err(|_| ForgeTransportError::Protocol)?,
            tree,
            commit,
        )
        .map_err(|_| ForgeTransportError::Integrity)
    }

    fn fetch_archive(
        &mut self,
        coordinate: &ForgeCoordinate,
        resolution: &ForgeResolution,
    ) -> Result<ForgeArchive, ForgeTransportError> {
        let commit = resolution.commit.as_hex();
        let (url, root_prefix) = match coordinate.provider() {
            ForgeProvider::Github => (
                format!("{}/archive/{}.tar.gz", coordinate.repository_url(), commit),
                Some(format!("{}-{}", coordinate.repository(), commit)),
            ),
            ForgeProvider::Gitlab => (
                format!(
                    "{}/repository/archive.tar.gz?sha={}",
                    coordinate.repository_url(),
                    commit
                ),
                Some(format!("{}-{}", coordinate.repository(), commit)),
            ),
            ForgeProvider::Codeberg => (
                format!("{}/archive/{}.tar.gz", coordinate.repository_url(), commit),
                Some(format!("{}-{}", coordinate.repository(), commit)),
            ),
            ForgeProvider::GenericHttpsGit => return Err(ForgeTransportError::Policy),
        };
        Ok(ForgeArchive::from_reader(
            self.get_reader(&url)?,
            ForgeArchiveFormat::TarGzip,
            root_prefix,
        ))
    }

    fn fetch_metadata(
        &mut self,
        coordinate: &ForgeCoordinate,
        _resolution: &ForgeResolution,
    ) -> Result<ForgeRepositoryMetadata, ForgeTransportError> {
        let url = match coordinate.provider() {
            ForgeProvider::Github => format!(
                "https://api.github.com/repos/{}/{}",
                coordinate.owner(),
                coordinate.repository()
            ),
            ForgeProvider::Gitlab => format!(
                "https://gitlab.com/api/v4/projects/{}",
                gitlab_project_path(coordinate)
            ),
            ForgeProvider::Codeberg => format!(
                "{}/api/v1/repos/{}/{}",
                host_root(coordinate.repository_url()),
                coordinate.owner(),
                coordinate.repository()
            ),
            ForgeProvider::GenericHttpsGit => {
                return Ok(ForgeRepositoryMetadata::unavailable(
                    coordinate.owner(),
                    ForgeUnavailableReason::Unsupported,
                ));
            }
        };
        let bytes = self.get(&url, self.limits.max_metadata_bytes)?;
        let value: Value =
            serde_json::from_slice(&bytes).map_err(|_| ForgeTransportError::Protocol)?;
        let mut metadata = ForgeRepositoryMetadata::from_json(coordinate.owner(), &value);
        metadata.readme = ForgeFact::Unavailable(ForgeUnavailableReason::Unsupported);
        Ok(metadata)
    }
}

fn revision_path(revision: &ForgeRevision) -> String {
    match revision {
        ForgeRevision::Commit(commit) => commit.as_hex(),
        ForgeRevision::Tag(tag) => tag.as_str().to_owned(),
        ForgeRevision::Branch(branch) => branch.as_str().to_owned(),
    }
}

fn gitlab_project_path(coordinate: &ForgeCoordinate) -> String {
    format!(
        "{}%2F{}",
        coordinate.owner().replace('/', "%2F"),
        coordinate.repository()
    )
}

fn host_root(url: &str) -> &str {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url;
    };
    rest.find('/')
        .map_or(url, |slash| &url[..scheme.len() + 3 + slash])
}
