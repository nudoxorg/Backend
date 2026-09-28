use super::*;

impl S3ClosurePublisher {
    /// Loads an all-or-nothing process configuration. No S3 variables means local-machine mode.
    pub(in crate::builtin) fn from_env(workspace: &Path) -> Result<Option<Self>, PublicationError> {
        let endpoint = read_env(ENDPOINT_ENV)?;
        let bucket = read_env(BUCKET_ENV)?;
        let region = read_env(REGION_ENV)?;
        let access_key = read_env(ACCESS_KEY_ENV)?;
        let secret_key = read_env(SECRET_KEY_ENV)?;
        let session_token = read_env(SESSION_TOKEN_ENV)?;
        let prefix = read_env(PREFIX_ENV)?.unwrap_or_default();
        let present = [
            endpoint.is_some(),
            bucket.is_some(),
            region.is_some(),
            access_key.is_some(),
            secret_key.is_some(),
        ];
        if !present.iter().any(|value| *value) {
            if session_token.is_some() || !prefix.is_empty() {
                return Err(PublicationError::Configuration);
            }
            return Ok(None);
        }
        if present.iter().any(|value| !value) {
            return Err(PublicationError::Configuration);
        }

        let endpoint = endpoint.ok_or(PublicationError::Configuration)?;
        #[cfg(any(test, feature = "cluster-process-journey-hooks"))]
        let endpoint_claim = if is_loopback_http(&endpoint) {
            S3Endpoint::loopback_http(&endpoint)
        } else {
            S3Endpoint::https(&endpoint)
        }
        .map_err(|_| PublicationError::Configuration)?;
        #[cfg(not(any(test, feature = "cluster-process-journey-hooks")))]
        let endpoint_claim =
            S3Endpoint::https(&endpoint).map_err(|_| PublicationError::Configuration)?;
        let bucket = bucket.ok_or(PublicationError::Configuration)?;
        let region = region.ok_or(PublicationError::Configuration)?;
        let access_key = access_key.ok_or(PublicationError::Configuration)?;
        let secret_key = secret_key.ok_or(PublicationError::Configuration)?;
        if !valid_bucket(&bucket)
            || !valid_token(&region)
            || !valid_token(&access_key)
            || secret_key.is_empty()
            || session_token.as_ref().is_some_and(String::is_empty)
            || !valid_prefix(&prefix)
        {
            return Err(PublicationError::Configuration);
        }
        let route_config = S3RouteConfig::new(
            [endpoint_claim],
            MAX_PACK_BYTES,
            3,
            Duration::from_millis(100),
        )
        .map_err(|_| PublicationError::Configuration)?;

        Ok(Some(Self {
            route: S3PackRoute::new(S3ObjectRoute::new(route_config)),
            endpoint,
            bucket,
            region,
            access_key,
            secret_key,
            session_token,
            prefix,
            receipt_root: workspace.join("remote-s3-receipts"),
            receipt_cache: Mutex::new(None),
        }))
    }
}

pub(super) fn origin_authority(origin: &str) -> Result<String, PublicationError> {
    let uri = origin
        .parse::<ureq::http::Uri>()
        .map_err(|_| PublicationError::Configuration)?;
    #[cfg(any(test, feature = "cluster-process-journey-hooks"))]
    let loopback_http = is_loopback_http_uri(&uri);
    #[cfg(not(any(test, feature = "cluster-process-journey-hooks")))]
    let loopback_http = false;
    if (uri.scheme_str() != Some("https") && !loopback_http)
        || uri
            .path_and_query()
            .is_some_and(|path| path.path() != "/" || path.query().is_some())
    {
        return Err(PublicationError::Configuration);
    }
    uri.authority()
        .map(|authority| authority.as_str().to_owned())
        .ok_or(PublicationError::Configuration)
}

#[cfg(any(test, feature = "cluster-process-journey-hooks"))]
fn is_loopback_http(origin: &str) -> bool {
    origin
        .parse::<ureq::http::Uri>()
        .is_ok_and(|uri| is_loopback_http_uri(&uri))
}

#[cfg(any(test, feature = "cluster-process-journey-hooks"))]
fn is_loopback_http_uri(uri: &ureq::http::Uri) -> bool {
    uri.scheme_str() == Some("http")
        && uri.authority().is_some_and(|authority| {
            let host = authority.host();
            host == "localhost"
                || host
                    .parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
        })
}

pub(super) fn valid_bucket(value: &str) -> bool {
    (3..=63).contains(&value.len())
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b".-".contains(&byte))
        && !value.starts_with('.')
        && !value.ends_with('.')
        && !value.contains("..")
}

pub(super) fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"-_".contains(&byte))
}

pub(super) fn valid_prefix(value: &str) -> bool {
    value.is_empty()
        || (value.ends_with('/')
            && value[..value.len() - 1].split('/').all(|component| {
                !component.is_empty()
                    && component != "."
                    && component != ".."
                    && component
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
            }))
}

pub(super) fn read_env(name: &str) -> Result<Option<String>, PublicationError> {
    env::var_os(name)
        .map(|value| {
            value
                .into_string()
                .map_err(|_| PublicationError::Configuration)
        })
        .transpose()
}
