//! Fetches registry pages and archives through one bounded HTTP client.

use super::*;

impl HttpRegistryTransport {
    /// Constructs a transport after validating every resource and source policy.
    ///
    /// # Errors
    /// Returns invalid configuration when an endpoint or resource bound is unsafe.
    pub fn new(
        endpoint: RegistryEndpoint,
        authentication: Option<AuthenticationToken>,
        limits: AcquisitionLimits,
    ) -> Result<Self, AcquisitionError> {
        let limits = limits.validate()?;
        let config = ureq::Agent::config_builder()
            .timeout_connect(Some(limits.connect_timeout))
            .timeout_recv_response(Some(limits.read_timeout))
            .timeout_recv_body(Some(limits.read_timeout))
            // Respect the process proxy policy for real upstream verification;
            // ureq does not consult proxy environment variables implicitly.
            .proxy(ureq::Proxy::try_from_env())
            // Credentials and archive requests are bound to one configured
            // authority; a response cannot redirect either to another host.
            .max_redirects(0)
            .http_status_as_error(false)
            .build();
        let credentials = RegistryCredentialPolicy::new(&endpoint, authentication);
        Ok(Self {
            ecosystem: endpoint.ecosystem(),
            endpoint,
            credentials,
            limits,
            agent: config.new_agent(),
            mode: HttpFeedMode::Canonical,
            archive_handoffs: VecDeque::new(),
            archive_handoff_bytes: 0,
            trusted_followup_hosts: BTreeSet::new(),
        })
    }

    /// Constructs the bounded transport over one native ecosystem grammar.
    ///
    /// # Errors
    /// Returns invalid configuration when the source or resource policy is unsafe.
    pub fn for_native(
        adapter: EcosystemAdapter,
        authentication: Option<AuthenticationToken>,
        limits: AcquisitionLimits,
    ) -> Result<Self, AcquisitionError> {
        let endpoint = adapter.endpoint().clone();
        let mut transport = Self::new(endpoint, authentication, limits)?;
        transport.mode = HttpFeedMode::Native(adapter);
        Ok(transport)
    }

    /// Reads one bounded URL as an available, retry, or unmodified body.
    pub(super) fn get(
        &self,
        url: &str,
        maximum: usize,
    ) -> Result<TransportResult<Vec<u8>>, TransportFailure> {
        self.get_with_accept(url, maximum, None)
    }

    fn get_with_accept(
        &self,
        url: &str,
        maximum: usize,
        accept: Option<&str>,
    ) -> Result<TransportResult<Vec<u8>>, TransportFailure> {
        if !self.followup_url_is_admitted(url) {
            return Err(TransportFailure::Configuration);
        }
        let maximum_u64 = u64::try_from(maximum).map_err(|_| TransportFailure::Bounds)?;
        for attempt in 1..=self.limits.attempts.get() {
            let request = self.agent.get(url).header("accept-encoding", "identity");
            let request = match accept {
                Some(value) => request.header("accept", value),
                None => request,
            };
            let request = match self.credentials.authorization_for(url) {
                Some(value) => request.header("authorization", value),
                None => request,
            };
            let result = request.call();
            match result {
                Ok(mut response) => {
                    let status = response.status().as_u16();
                    if status == 304 {
                        return Ok(TransportResult::NotModified);
                    }
                    if status == 429 {
                        return Ok(TransportResult::RetryAfter(retry_after(&response)));
                    }
                    if matches!(status, 408 | 502 | 503 | 504) {
                        let delay = retry_after(&response);
                        if attempt == self.limits.attempts.get() {
                            return Ok(TransportResult::RetryAfter(delay));
                        }
                        continue;
                    }
                    if !(200..300).contains(&status) {
                        return Err(TransportFailure::Rejected(status));
                    }
                    let mut bytes = Vec::new();
                    response
                        .body_mut()
                        .as_reader()
                        .take(maximum_u64.saturating_add(1))
                        .read_to_end(&mut bytes)
                        .map_err(|_| TransportFailure::Protocol)?;
                    if bytes.len() > maximum {
                        return Err(TransportFailure::Overrun {
                            measured: u64::try_from(bytes.len())
                                .map_err(|_| TransportFailure::Bounds)?,
                            limit: u64::try_from(maximum).map_err(|_| TransportFailure::Bounds)?,
                        });
                    }
                    return Ok(TransportResult::Available(bytes));
                }
                Err(_) if attempt == self.limits.attempts.get() => {
                    return Ok(TransportResult::Unavailable);
                }
                Err(_) => {}
            }
        }
        Ok(TransportResult::Unavailable)
    }

    /// Reads one archive URL into a bounded artifact.
    pub(super) fn get_archive(
        &self,
        url: &str,
        maximum: usize,
    ) -> Result<TransportResult<ArchiveArtifact>, TransportFailure> {
        self.get_archive_full(url, maximum, None)
    }

    /// Resumes one archive transfer from a caller checkpoint.
    pub(super) fn get_archive_resumable(
        &self,
        url: &str,
        maximum: usize,
        checkpoint: TransferCheckpoint,
    ) -> Result<TransportResult<ArchiveArtifact>, TransportFailure> {
        if checkpoint.received == 0
            || checkpoint
                .validator
                .as_ref()
                .and_then(TransferValidator::if_range)
                .is_none()
        {
            return self.get_archive_full(url, maximum, None);
        }
        self.get_archive_range(url, maximum, checkpoint)
    }

    fn get_archive_full(
        &self,
        url: &str,
        maximum: usize,
        reset_reason: Option<TransferResetReason>,
    ) -> Result<TransportResult<ArchiveArtifact>, TransportFailure> {
        if !self.followup_url_is_admitted(url) {
            return Err(TransportFailure::Configuration);
        }
        for attempt in 1..=self.limits.attempts.get() {
            let request = self.agent.get(url).header("accept-encoding", "identity");
            let request = match self.credentials.authorization_for(url) {
                Some(value) => request.header("authorization", value),
                None => request,
            };
            match request.call() {
                Ok(mut response) => {
                    let status = response.status().as_u16();
                    if status == 304 {
                        return Ok(TransportResult::NotModified);
                    }
                    if status == 429 {
                        return Ok(TransportResult::RetryAfter(retry_after(&response)));
                    }
                    if matches!(status, 408 | 502 | 503 | 504) {
                        let delay = retry_after(&response);
                        if attempt == self.limits.attempts.get() {
                            return Ok(TransportResult::RetryAfter(delay));
                        }
                        continue;
                    }
                    if !(200..300).contains(&status) {
                        return Err(TransportFailure::Rejected(status));
                    }
                    if status != 200 {
                        return Err(TransportFailure::Protocol);
                    }
                    let declared = content_length(&response)?;
                    let validator = response_validator(&response);
                    let mut reader = response.body_mut().as_reader();
                    let (path, body_length) = stage_archive_body(&mut reader, maximum)?;
                    if declared.is_some_and(|length| length != body_length) {
                        let _ = fs::remove_file(&path);
                        return Err(TransportFailure::Protocol);
                    }
                    let total = declared.unwrap_or(body_length);
                    return Ok(TransportResult::Available(
                        ArchiveArtifact::from_range_file(
                            path,
                            0,
                            total,
                            body_length,
                            validator,
                            reset_reason,
                        ),
                    ));
                }
                Err(_) if attempt == self.limits.attempts.get() => {
                    return Ok(TransportResult::Unavailable);
                }
                Err(_) => {}
            }
        }
        Ok(TransportResult::Unavailable)
    }

    fn get_archive_range(
        &self,
        url: &str,
        maximum: usize,
        checkpoint: TransferCheckpoint,
    ) -> Result<TransportResult<ArchiveArtifact>, TransportFailure> {
        if !self.followup_url_is_admitted(url) {
            return Err(TransportFailure::Configuration);
        }
        let start = checkpoint.received;
        let if_range = checkpoint
            .validator
            .as_ref()
            .and_then(TransferValidator::if_range)
            .ok_or(TransportFailure::Protocol)?;
        for attempt in 1..=self.limits.attempts.get() {
            let request = self
                .agent
                .get(url)
                .header("accept-encoding", "identity")
                .header("range", format!("bytes={start}-"))
                .header("if-range", if_range);
            let request = match self.credentials.authorization_for(url) {
                Some(value) => request.header("authorization", value),
                None => request,
            };
            match request.call() {
                Ok(mut response) => {
                    let status = response.status().as_u16();
                    if status == 429 {
                        return Ok(TransportResult::RetryAfter(retry_after(&response)));
                    }
                    if matches!(status, 408 | 502 | 503 | 504) {
                        let delay = retry_after(&response);
                        if attempt == self.limits.attempts.get() {
                            return Ok(TransportResult::RetryAfter(delay));
                        }
                        continue;
                    }
                    let validator = response_validator(&response);
                    if status == 200 {
                        let reset = Some(if validator.as_ref() != checkpoint.validator.as_ref() {
                            TransferResetReason::ValidatorChanged
                        } else {
                            TransferResetReason::RangeIgnored
                        });
                        let declared = content_length(&response)?;
                        let mut reader = response.body_mut().as_reader();
                        let (path, body_length) = stage_archive_body(&mut reader, maximum)?;
                        if declared.is_some_and(|length| length != body_length) {
                            let _ = fs::remove_file(&path);
                            return Err(TransportFailure::Protocol);
                        }
                        let total = declared.unwrap_or(body_length);
                        return Ok(TransportResult::Available(
                            ArchiveArtifact::from_range_file(
                                path,
                                0,
                                total,
                                body_length,
                                validator,
                                reset,
                            ),
                        ));
                    }
                    if status == 416 {
                        let total = unsatisfied_range_total(&response)?;
                        if validator.as_ref() != checkpoint.validator.as_ref() {
                            drop(response);
                            return self.get_archive_full(
                                url,
                                maximum,
                                Some(TransferResetReason::ValidatorChanged),
                            );
                        }
                        if total != start {
                            return Err(TransportFailure::Protocol);
                        }
                        let (path, body_length) =
                            stage_archive_body(&mut Cursor::new([]), maximum)?;
                        return Ok(TransportResult::Available(
                            ArchiveArtifact::from_range_file(
                                path,
                                start,
                                total,
                                body_length,
                                validator,
                                None,
                            ),
                        ));
                    }
                    if status != 206 {
                        if !(200..300).contains(&status) {
                            return Err(TransportFailure::Rejected(status));
                        }
                        return Err(TransportFailure::Protocol);
                    }
                    let Some((range_start, range_end, total)) = content_range(&response)? else {
                        return Err(TransportFailure::Protocol);
                    };
                    if range_start != start || range_end < range_start || total <= range_end {
                        return Err(TransportFailure::Protocol);
                    }
                    if validator.as_ref() != checkpoint.validator.as_ref() {
                        drop(response);
                        return self.get_archive_full(
                            url,
                            maximum,
                            Some(TransferResetReason::ValidatorChanged),
                        );
                    }
                    let expected_body = range_end
                        .checked_sub(range_start)
                        .and_then(|length| length.checked_add(1))
                        .ok_or(TransportFailure::Bounds)?;
                    if usize::try_from(expected_body).map_err(|_| TransportFailure::Bounds)?
                        > maximum
                    {
                        return Err(TransportFailure::Overrun {
                            measured: total,
                            limit: u64::try_from(maximum).map_err(|_| TransportFailure::Bounds)?,
                        });
                    }
                    if content_length(&response)?.is_some_and(|length| length != expected_body) {
                        return Err(TransportFailure::Protocol);
                    }
                    let mut reader = response.body_mut().as_reader();
                    let (path, body_length) = stage_archive_body(
                        &mut reader,
                        usize::try_from(expected_body).map_err(|_| TransportFailure::Bounds)?,
                    )?;
                    if body_length != expected_body {
                        let _ = fs::remove_file(&path);
                        return Err(TransportFailure::Protocol);
                    }
                    return Ok(TransportResult::Available(
                        ArchiveArtifact::from_range_file(
                            path,
                            start,
                            total,
                            body_length,
                            validator,
                            None,
                        ),
                    ));
                }
                Err(_) if attempt == self.limits.attempts.get() => {
                    return Ok(TransportResult::Unavailable);
                }
                Err(_) => {}
            }
        }
        Ok(TransportResult::Unavailable)
    }

    /// Builds the canonical feed URL for one page request.
    pub(super) fn feed_url<S: FeedSchema>(&self, request: &FeedRequest<S>) -> String {
        format!(
            "{}/feed?schema={}&cursor={}&limit={}",
            self.endpoint.url(),
            S::VERSION,
            hex(&request.cursor.token()),
            request.max_items
        )
    }

    /// Reads one native ecosystem page through the admitted adapter.
    pub(super) fn fetch_native_page(
        &mut self,
        adapter: &EcosystemAdapter,
        request: FeedRequest,
    ) -> Result<TransportResult<FeedPage>, TransportFailure> {
        let (metadata, accept) = match adapter.ecosystem() {
            RegistryEcosystem::Pypi => (
                adapter.metadata_url(),
                Some("application/vnd.pypi.simple.v1+json"),
            ),
            RegistryEcosystem::Cargo => (adapter.metadata_url(), Some("application/json")),
            RegistryEcosystem::Npm => (
                adapter.metadata_url(),
                Some("application/vnd.npm.install-v1+json"),
            ),
            RegistryEcosystem::Maven => (
                adapter.metadata_url(),
                Some("application/xml, text/xml;q=0.9, */*;q=0.8"),
            ),
            RegistryEcosystem::Nuget | RegistryEcosystem::Golang | RegistryEcosystem::Cpp => {
                (adapter.metadata_url(), Some("application/json"))
            }
        };
        let bytes = match self.get_with_accept(&metadata, self.limits.max_feed_bytes, accept)? {
            TransportResult::Available(value) => value,
            TransportResult::Unavailable => return Ok(TransportResult::Unavailable),
            TransportResult::RetryAfter(delay) => return Ok(TransportResult::RetryAfter(delay)),
            TransportResult::NotModified => return Ok(TransportResult::NotModified),
        };
        let page = match adapter.ecosystem() {
            RegistryEcosystem::Cargo | RegistryEcosystem::Npm => {
                adapter.admit_page(&bytes, request)?
            }
            RegistryEcosystem::Pypi => self.python_page(adapter, bytes, request)?,
            RegistryEcosystem::Nuget => self.nuget_page(adapter, bytes, request)?,
            RegistryEcosystem::Maven => self.maven_page(adapter, bytes, request)?,
            RegistryEcosystem::Golang => self.go_page(adapter, bytes, request)?,
            RegistryEcosystem::Cpp => self.conan_page(adapter, bytes, request)?,
        };
        Ok(TransportResult::Available(page))
    }

    fn python_page(
        &mut self,
        adapter: &EcosystemAdapter,
        listing: Vec<u8>,
        request: FeedRequest,
    ) -> Result<FeedPage, TransportFailure> {
        let mut page = adapter.admit_page(&listing, request)?;
        for package in &mut page.packages {
            let url = adapter.pypi_json_url(package.coordinate.version());
            package.dependency_facts = match self.get(&url, self.limits.max_feed_bytes) {
                Ok(TransportResult::Available(body)) => {
                    adapter.pypi_requires_dist(&body, &package.coordinate, &body)?
                }
                Err(TransportFailure::Rejected(404)) => DependencyFacts::Unknown(
                    backend_library::ProductText::new("PyPI JSON API release is not published")
                        .map_err(|_| TransportFailure::Protocol)?,
                ),
                Ok(TransportResult::Unavailable | TransportResult::RetryAfter(_)) => {
                    DependencyFacts::Unavailable(
                        backend_library::ProductText::new("PyPI JSON API could not be fetched")
                            .map_err(|_| TransportFailure::Protocol)?,
                    )
                }
                Ok(TransportResult::NotModified) => return Err(TransportFailure::Protocol),
                Err(error) => return Err(error),
            };
        }
        Ok(page)
    }

    fn nuget_page(
        &mut self,
        adapter: &EcosystemAdapter,
        service_index: Vec<u8>,
        request: FeedRequest,
    ) -> Result<FeedPage, TransportFailure> {
        let root: serde_json::Value =
            serde_json::from_slice(&service_index).map_err(|_| TransportFailure::Protocol)?;
        let resources = root
            .get("resources")
            .and_then(serde_json::Value::as_array)
            .ok_or(TransportFailure::Protocol)?;
        let registration = nuget_resource(resources, "RegistrationsBaseUrl")
            .ok_or(TransportFailure::DownloadUnavailable)?;
        let package_base = nuget_resource(resources, "PackageBaseAddress");
        self.admit_resource_origin(registration)?;
        if let Some(package_base) = package_base {
            self.admit_resource_origin(package_base)?;
        }
        let url = format!(
            "{}/{}/index.json",
            registration.trim_end_matches('/'),
            adapter.package_name().to_ascii_lowercase()
        );
        let registration = match self.get(&url, self.limits.max_feed_bytes)? {
            TransportResult::Available(value) => value,
            TransportResult::Unavailable => return Err(TransportFailure::Protocol),
            TransportResult::RetryAfter(_) | TransportResult::NotModified => {
                return Err(TransportFailure::Protocol);
            }
        };
        let registration_root: serde_json::Value =
            serde_json::from_slice(&registration).map_err(|_| TransportFailure::Protocol)?;
        let mut leaves = Vec::new();
        let mut visited_pages = BTreeSet::new();
        // The root is already in hand. Marking it visited makes a malicious
        // self-referential page descriptor terminate without a second fetch.
        visited_pages.insert(url.clone());
        let mut snapshot = Vec::with_capacity(registration.len());
        self.collect_nuget_registration(
            &registration_root,
            &registration,
            &mut leaves,
            &mut visited_pages,
            &mut snapshot,
            0,
        )?;
        let leaf_refs = leaves.iter().collect::<Vec<_>>();
        let mut metadata = adapter.nuget_metadata_from_leaves(leaf_refs, package_base)?;
        if let Some(target) = adapter.target_version() {
            metadata.retain(|release| release.version == target);
        }
        let total = metadata.len();
        let (start, prefix) = adapter.page_start(&snapshot, request, total)?;
        let selected = metadata
            .into_iter()
            .skip(start)
            .take(request.max_items)
            .collect::<Vec<_>>();
        let mut releases = Vec::with_capacity(selected.len());
        for release in selected {
            let checksum = match release.checksum {
                Some(checksum) => checksum,
                None => {
                    let archive = match self
                        .get_archive(&release.archive_url, self.limits.max_archive_bytes)
                    {
                        Ok(TransportResult::Available(value)) => value,
                        Ok(TransportResult::Unavailable | TransportResult::RetryAfter(_)) => {
                            return Err(TransportFailure::DownloadUnavailable);
                        }
                        Ok(TransportResult::NotModified) => return Err(TransportFailure::Protocol),
                        Err(TransportFailure::Rejected(404)) => {
                            return Err(TransportFailure::DownloadUnavailable);
                        }
                        Err(error) => return Err(error),
                    };
                    let checksum = RegistryChecksum::sha512_hex(
                        &archive.digest_hex(ChecksumAlgorithm::Sha512)?,
                    )?;
                    self.cache_archive(checksum.cache_key(), archive)?;
                    checksum
                }
            };
            releases.push(adapter.release_from_checksum(
                &release.version,
                release.archive_url,
                checksum,
                &release.provenance,
                release.facts,
            )?);
        }
        adapter.admit_window(releases, request, start, prefix, total)
    }

    fn collect_nuget_registration(
        &self,
        value: &serde_json::Value,
        raw: &[u8],
        leaves: &mut Vec<serde_json::Value>,
        visited_pages: &mut BTreeSet<String>,
        snapshot: &mut Vec<u8>,
        depth: usize,
    ) -> Result<(), TransportFailure> {
        if depth > MAX_NUGET_REGISTRATION_DEPTH {
            return Err(TransportFailure::Bounds);
        }
        snapshot
            .len()
            .checked_add(raw.len())
            .filter(|length| *length <= MAX_NUGET_REGISTRATION_SNAPSHOT_BYTES)
            .ok_or(TransportFailure::Overrun {
                measured: u64::try_from(snapshot.len().saturating_add(raw.len()))
                    .map_err(|_| TransportFailure::Bounds)?,
                limit: u64::try_from(MAX_NUGET_REGISTRATION_SNAPSHOT_BYTES)
                    .map_err(|_| TransportFailure::Bounds)?,
            })?;
        snapshot.extend_from_slice(raw);
        let object = value.as_object().ok_or(TransportFailure::Protocol)?;
        if object.contains_key("catalogEntry") {
            if leaves.len() >= MAX_NUGET_REGISTRATION_LEAVES {
                return Err(TransportFailure::Bounds);
            }
            leaves.push(value.clone());
            return Ok(());
        }
        let Some(items) = object.get("items").and_then(serde_json::Value::as_array) else {
            let page_url = object
                .get("@id")
                .and_then(serde_json::Value::as_str)
                .ok_or(TransportFailure::Protocol)?;
            if !visited_pages.insert(page_url.to_owned()) {
                return Ok(());
            }
            if visited_pages.len() > MAX_NUGET_REGISTRATION_PAGES {
                return Err(TransportFailure::Bounds);
            }
            let page = match self.get(page_url, self.limits.max_feed_bytes)? {
                TransportResult::Available(value) => value,
                TransportResult::Unavailable | TransportResult::RetryAfter(_) => {
                    return Err(TransportFailure::DownloadUnavailable);
                }
                TransportResult::NotModified => return Err(TransportFailure::Protocol),
            };
            let page_value: serde_json::Value =
                serde_json::from_slice(&page).map_err(|_| TransportFailure::Protocol)?;
            return self.collect_nuget_registration(
                &page_value,
                &page,
                leaves,
                visited_pages,
                snapshot,
                depth.saturating_add(1),
            );
        };
        for item in items {
            if item
                .as_object()
                .is_some_and(|object| object.contains_key("catalogEntry"))
            {
                if leaves.len() >= MAX_NUGET_REGISTRATION_LEAVES {
                    return Err(TransportFailure::Bounds);
                }
                leaves.push(item.clone());
                continue;
            }
            if item
                .as_object()
                .is_some_and(|object| object.contains_key("items"))
            {
                // Some private feeds inline a page object inside the root
                // document. Recurse into it directly rather than requiring
                // an otherwise unnecessary network URL.
                self.collect_nuget_registration(
                    item,
                    raw,
                    leaves,
                    visited_pages,
                    snapshot,
                    depth.saturating_add(1),
                )?;
                continue;
            }
            let Some(page_url) = item.get("@id").and_then(serde_json::Value::as_str) else {
                return Err(TransportFailure::Protocol);
            };
            if !visited_pages.insert(page_url.to_owned()) {
                continue;
            }
            if visited_pages.len() > MAX_NUGET_REGISTRATION_PAGES {
                return Err(TransportFailure::Bounds);
            }
            let page = match self.get(page_url, self.limits.max_feed_bytes)? {
                TransportResult::Available(value) => value,
                TransportResult::Unavailable | TransportResult::RetryAfter(_) => {
                    return Err(TransportFailure::DownloadUnavailable);
                }
                TransportResult::NotModified => return Err(TransportFailure::Protocol),
            };
            let page_value: serde_json::Value =
                serde_json::from_slice(&page).map_err(|_| TransportFailure::Protocol)?;
            self.collect_nuget_registration(
                &page_value,
                &page,
                leaves,
                visited_pages,
                snapshot,
                depth.saturating_add(1),
            )?;
        }
        Ok(())
    }

    fn maven_page(
        &mut self,
        adapter: &EcosystemAdapter,
        metadata: Vec<u8>,
        request: FeedRequest,
    ) -> Result<FeedPage, TransportFailure> {
        let parsed = adapter.maven_metadata(&metadata)?;
        let mut versions = parsed.versions;
        if let Some(target) = adapter.target_version() {
            versions.retain(|version| version.version == target);
        }
        let (start, prefix) = adapter.page_start(&metadata, request, versions.len())?;
        let selected = versions
            .iter()
            .skip(start)
            .take(request.max_items)
            .collect::<Vec<_>>();
        let mut releases = Vec::with_capacity(selected.len());
        for version in selected {
            // Maven's primary JAR is bytecode. The source classifier is the
            // real archive that can pass through the shared source ingester,
            // semantic pipeline, and desktop code-search journey.
            let source_url = adapter.maven_source_archive_url_for(
                &version.version,
                version.timestamped_sources.as_deref(),
            );
            let (archive_url, checksum, mut provenance, signature, artifact_kind) = match self
                .maven_artifact_evidence(&source_url)
            {
                Ok((checksum, body, signature)) => {
                    let mut provenance = metadata.clone();
                    provenance.extend_from_slice(&body);
                    if let backend_library::RegistryNativeObservation::Recorded(signature) =
                        &signature
                    {
                        provenance.extend_from_slice(signature);
                    }
                    (
                        source_url,
                        checksum,
                        provenance,
                        signature,
                        super::super::NativeArtifactKind::MavenSources,
                    )
                }
                Err(TransportFailure::DownloadUnavailable) => {
                    let main_url = adapter.maven_archive_url_with_timestamped(
                        &version.version,
                        version.timestamped_jar.as_deref(),
                    );
                    let (checksum, body, signature) = self.maven_artifact_evidence(&main_url)?;
                    let mut provenance = metadata.clone();
                    provenance.extend_from_slice(&body);
                    if let backend_library::RegistryNativeObservation::Recorded(signature) =
                        &signature
                    {
                        provenance.extend_from_slice(signature);
                    }
                    (
                        main_url,
                        checksum,
                        provenance,
                        signature,
                        super::super::NativeArtifactKind::MavenJar,
                    )
                }
                Err(error) => return Err(error),
            };
            let pom_url =
                adapter.maven_pom_url_for(&version.version, version.timestamped_pom.as_deref());
            let (dependency_facts, pom_observation) = match self
                .get(&pom_url, self.limits.max_feed_bytes)
            {
                Ok(TransportResult::Available(pom)) => {
                    provenance.extend_from_slice(&pom);
                    let coordinate = adapter.coordinate_for_version(&version.version)?;
                    let dependencies =
                        adapter.maven_dependencies(&pom, &coordinate, &provenance)?;
                    let claim = backend_library::RegistryNativeEvidenceClaim {
                        url: pom_url.clone(),
                        digest: *blake3::hash(&pom).as_bytes(),
                        bytes: u64::try_from(pom.len()).map_err(|_| TransportFailure::Bounds)?,
                    };
                    (
                        dependencies,
                        backend_library::RegistryNativeObservation::Recorded(claim),
                    )
                }
                Err(TransportFailure::Rejected(404)) => (
                    DependencyFacts::Unknown(
                        backend_library::ProductText::new("Maven POM is not published")
                            .map_err(|_| TransportFailure::Protocol)?,
                    ),
                    backend_library::RegistryNativeObservation::NotRecorded(
                        "Maven POM is not published".to_owned(),
                    ),
                ),
                Ok(TransportResult::Unavailable | TransportResult::RetryAfter(_)) => (
                    DependencyFacts::Unavailable(
                        backend_library::ProductText::new("Maven POM could not be fetched")
                            .map_err(|_| TransportFailure::Protocol)?,
                    ),
                    backend_library::RegistryNativeObservation::Unavailable(
                        "Maven POM could not be fetched".to_owned(),
                    ),
                ),
                Ok(TransportResult::NotModified) => return Err(TransportFailure::Protocol),
                Err(error) => return Err(error),
            };
            let mut release = adapter.maven_release_with_dependencies(
                &version.version,
                archive_url,
                checksum,
                &provenance,
                dependency_facts,
            )?;
            let archive_url = release.archive_url.clone();
            let filename = archive_url
                .rsplit('/')
                .next()
                .filter(|value| !value.is_empty())
                .ok_or(TransportFailure::Protocol)?;
            release.set_artifacts(vec![super::super::NativeArtifact {
                filename: std::sync::Arc::from(filename),
                url: std::sync::Arc::from(archive_url.as_str()),
                checksum: release.checksum.clone(),
                kind: artifact_kind,
                requires_python: None,
                size: None,
                yanked: false,
                yanked_reason: None,
            }]);
            let signature = match signature {
                backend_library::RegistryNativeObservation::Recorded(bytes) => {
                    backend_library::RegistryNativeObservation::Recorded(
                        backend_library::RegistryNativeEvidenceClaim {
                            url: format!("{archive_url}.asc"),
                            digest: *blake3::hash(&bytes).as_bytes(),
                            bytes: u64::try_from(bytes.len())
                                .map_err(|_| TransportFailure::Bounds)?,
                        },
                    )
                }
                backend_library::RegistryNativeObservation::NotRecorded(reason) => {
                    backend_library::RegistryNativeObservation::NotRecorded(reason)
                }
                backend_library::RegistryNativeObservation::Unavailable(reason) => {
                    backend_library::RegistryNativeObservation::Unavailable(reason)
                }
            };
            let checksum_suffix = match release.checksum.algorithm() {
                ChecksumAlgorithm::Sha1 => ".sha1",
                ChecksumAlgorithm::Sha256 => ".sha256",
                ChecksumAlgorithm::Sha512 => ".sha512",
                ChecksumAlgorithm::GoModule => return Err(TransportFailure::Protocol),
            };
            let dependency_observation = match &pom_observation {
                backend_library::RegistryNativeObservation::Recorded(_) => {
                    backend_library::RegistryNativeObservation::Recorded(
                        release.dependency_facts.clone(),
                    )
                }
                backend_library::RegistryNativeObservation::NotRecorded(reason) => {
                    backend_library::RegistryNativeObservation::NotRecorded(reason.clone())
                }
                backend_library::RegistryNativeObservation::Unavailable(reason) => {
                    backend_library::RegistryNativeObservation::Unavailable(reason.clone())
                }
            };
            release.record_maven_native_metadata(
                format!("{archive_url}{checksum_suffix}"),
                signature,
                pom_observation,
                dependency_observation,
            )?;
            releases.push(release);
        }
        adapter.admit_window(releases, request, start, prefix, versions.len())
    }

    fn maven_checksum(
        &self,
        archive_url: &str,
    ) -> Result<(RegistryChecksum, Vec<u8>), TransportFailure> {
        self.maven_artifact_evidence(archive_url)
            .map(|(checksum, body, _signature)| (checksum, body))
    }

    fn maven_artifact_evidence(
        &self,
        archive_url: &str,
    ) -> Result<
        (
            RegistryChecksum,
            Vec<u8>,
            backend_library::RegistryNativeObservation<Vec<u8>>,
        ),
        TransportFailure,
    > {
        for (suffix, parser) in [
            (".sha256", ChecksumAlgorithm::Sha256),
            (".sha512", ChecksumAlgorithm::Sha512),
            (".sha1", ChecksumAlgorithm::Sha1),
        ] {
            let response = self.get(&format!("{archive_url}{suffix}"), 512);
            let body = match response {
                Ok(TransportResult::Available(body)) => body,
                Ok(TransportResult::Unavailable | TransportResult::RetryAfter(_)) => {
                    return Err(TransportFailure::DownloadUnavailable);
                }
                Ok(TransportResult::NotModified) => return Err(TransportFailure::Protocol),
                Err(TransportFailure::Rejected(404)) => continue,
                Err(error) => return Err(error),
            };
            let text = std::str::from_utf8(&body).map_err(|_| TransportFailure::Protocol)?;
            let token = text
                .split_ascii_whitespace()
                .next()
                .ok_or(TransportFailure::Protocol)?;
            let checksum = match parser {
                ChecksumAlgorithm::Sha1 => RegistryChecksum::sha1_hex(token)?,
                ChecksumAlgorithm::Sha256 => RegistryChecksum::sha256_hex(token)?,
                ChecksumAlgorithm::Sha512 => RegistryChecksum::sha512_hex(token)?,
                ChecksumAlgorithm::GoModule => return Err(TransportFailure::Protocol),
            };
            // Signature sidecars are evidence rather than a trust decision:
            // Maven Central publishes OpenPGP signatures, but key validation
            // belongs to the configured advisory/signature authority. Fetch
            // and retain the bounded bytes when available, while keeping a
            // missing/temporarily unavailable signature a typed absence.
            let signature = match self.get(&format!("{archive_url}.asc"), 128 * 1024) {
                Ok(TransportResult::Available(signature)) => {
                    backend_library::RegistryNativeObservation::Recorded(signature)
                }
                Ok(TransportResult::Unavailable | TransportResult::RetryAfter(_)) => {
                    backend_library::RegistryNativeObservation::Unavailable(
                        "Maven signature sidecar could not be fetched".to_owned(),
                    )
                }
                Err(TransportFailure::Rejected(404)) => {
                    backend_library::RegistryNativeObservation::NotRecorded(
                        "Maven signature sidecar is not published".to_owned(),
                    )
                }
                Ok(TransportResult::NotModified) => return Err(TransportFailure::Protocol),
                Err(error) => return Err(error),
            };
            return Ok((checksum, body, signature));
        }
        Err(TransportFailure::DownloadUnavailable)
    }

    fn go_page(
        &self,
        adapter: &EcosystemAdapter,
        listing: Vec<u8>,
        request: FeedRequest,
    ) -> Result<FeedPage, TransportFailure> {
        let mut versions = adapter.go_versions(&listing)?;
        if let Some(target) = adapter.target_version() {
            versions.retain(|version| version == target);
        }
        let (start, prefix) = adapter.page_start(&listing, request, versions.len())?;
        let selected = versions
            .iter()
            .skip(start)
            .take(request.max_items)
            .collect::<Vec<_>>();
        let mut releases = Vec::with_capacity(selected.len());
        for version in selected {
            let info = self.required(&adapter.go_info_url(version))?;
            let module = self.required(&adapter.go_mod_url(version))?;
            let sum = self.required(&adapter.go_sum_lookup_url(version))?;
            let info = adapter.go_info(&info, version)?;
            let module = adapter.go_mod(&module, version)?;
            let checksum = go_checksum(&sum, &adapter.go_module_path(), version)?;
            let mut provenance = Vec::with_capacity(
                listing.len() + info.provenance.len() + module.provenance.len() + sum.len(),
            );
            provenance.extend_from_slice(&listing);
            provenance.extend_from_slice(&info.provenance);
            provenance.extend_from_slice(&module.provenance);
            provenance.extend_from_slice(&sum);
            let standing = if module.retracts.iter().any(|range| range.contains(version)) {
                super::super::ReleaseFacts::new(
                    super::super::ReleaseStanding::Retracted,
                    super::super::DownloadCount::NotReported(
                        super::super::DownloadCountGap::Unsupported,
                    ),
                    super::super::SecurityStanding::Unassessed,
                )
            } else {
                super::super::ReleaseFacts::default()
            };
            let mut release = adapter.go_release(version, checksum, &provenance, standing)?;
            release.dependency_facts =
                adapter.go_dependencies(&release.coordinate, &module, &provenance)?;
            let archive_url = release.archive_url.clone();
            let filename = archive_url
                .rsplit('/')
                .next()
                .filter(|value| !value.is_empty())
                .ok_or(TransportFailure::Protocol)?;
            release.set_artifacts(vec![super::super::NativeArtifact {
                filename: std::sync::Arc::from(filename),
                url: std::sync::Arc::from(archive_url.as_str()),
                checksum: release.checksum.clone(),
                kind: super::super::NativeArtifactKind::GoSource,
                requires_python: None,
                size: None,
                yanked: false,
                yanked_reason: None,
            }]);
            let source = backend_library::RegistryGoSourceFacts {
                module: module.module.clone(),
                version: version.clone(),
                info: backend_library::RegistryNativeEvidenceClaim {
                    url: adapter.go_info_url(version),
                    digest: *blake3::hash(&info.provenance).as_bytes(),
                    bytes: u64::try_from(info.provenance.len())
                        .map_err(|_| TransportFailure::Bounds)?,
                },
                module_file: backend_library::RegistryNativeEvidenceClaim {
                    url: adapter.go_mod_url(version),
                    digest: *blake3::hash(&module.provenance).as_bytes(),
                    bytes: u64::try_from(module.provenance.len())
                        .map_err(|_| TransportFailure::Bounds)?,
                },
                checksum: backend_library::RegistryNativeEvidenceClaim {
                    url: adapter.go_sum_lookup_url(version),
                    digest: *blake3::hash(&sum).as_bytes(),
                    bytes: u64::try_from(sum.len()).map_err(|_| TransportFailure::Bounds)?,
                },
            };
            release.set_native_metadata(backend_library::RegistryNativeMetadata {
                version: backend_library::REGISTRY_NATIVE_METADATA_VERSION,
                availability: backend_library::RegistryNativeAvailability::Recorded,
                provenance: backend_library::RegistryNativeProvenance::SourceDigest(
                    release.provenance.as_bytes(),
                ),
                details: backend_library::RegistryNativeDetails::Golang(
                    backend_library::RegistryGoMetadata {
                        artifacts: release
                            .artifacts
                            .iter()
                            .map(|artifact| backend_library::RegistryNativeArtifact {
                                filename: artifact.filename().to_owned(),
                                url: artifact.url().to_owned(),
                                checksum: super::super::ecosystem::native_checksum(
                                    artifact.checksum(),
                                ),
                                kind: backend_library::RegistryNativeArtifactKind::GoSource,
                                requires_python: None,
                                size: artifact.size(),
                                yanked: None,
                                yanked_reason: artifact.yanked_reason().map(ToOwned::to_owned),
                            })
                            .collect::<Vec<_>>()
                            .into_boxed_slice(),
                        retracts: module
                            .retracts
                            .iter()
                            .map(|range| backend_library::RegistryGoRetract {
                                lower: range.lower.clone(),
                                upper: range.upper.clone(),
                            })
                            .collect::<Vec<_>>()
                            .into_boxed_slice(),
                        source: backend_library::RegistryNativeObservation::Recorded(source),
                    },
                ),
            })?;
            releases.push(release);
        }
        adapter.admit_window(releases, request, start, prefix, versions.len())
    }

    fn conan_page(
        &mut self,
        adapter: &EcosystemAdapter,
        revisions: Vec<u8>,
        request: FeedRequest,
    ) -> Result<FeedPage, TransportFailure> {
        let revision = adapter.conan_revision(&revisions)?;
        let files = self.required(&adapter.conan_files_url(&revision))?;
        let manifest = adapter.conan_file_manifest(&files)?;
        let source_availability = manifest.source_availability();
        let archive_name = manifest.preferred_archive_name()?;
        let archive_url = adapter.conan_archive_url(&revision, &archive_name);
        let source_entry = manifest.entry("conan_sources.tgz");
        let export_entry = manifest.entry("conan_export.tgz");
        let (checksum, archive, source_url) = if let Some(entry) = source_entry {
            // A Conan source archive is already content-addressed by the
            // recipe file manifest. Prefer it over the recipe export so the
            // shared source ingester sees the actual project tree and does not
            // have to execute Python just to discover a mirror.
            let fetched = match self.get_archive(&archive_url, self.limits.max_archive_bytes)? {
                TransportResult::Available(value) => value,
                TransportResult::Unavailable | TransportResult::RetryAfter(_) => {
                    return Err(TransportFailure::DownloadUnavailable);
                }
                TransportResult::NotModified => return Err(TransportFailure::Protocol),
            };
            let checksum = if let Some(expected) = &entry.sha256 {
                let bytes = fetched.into_bytes(self.limits.max_archive_bytes)?;
                if entry
                    .size
                    .is_some_and(|size| size != u64::try_from(bytes.len()).unwrap_or(u64::MAX))
                    || !expected.verifies(&bytes)
                {
                    return Err(TransportFailure::Integrity);
                }
                self.cache_archive(expected.cache_key(), ArchiveArtifact::from_bytes(bytes))?;
                expected.clone()
            } else {
                let checksum =
                    RegistryChecksum::sha256_hex(&fetched.digest_hex(ChecksumAlgorithm::Sha256)?)?;
                self.cache_archive(checksum.cache_key(), fetched)?;
                checksum
            };
            (checksum, archive_url.clone(), Some(archive_url.clone()))
        } else {
            let Some(export_entry) = export_entry else {
                // A package binary without source is useful to a compiler
                // resolver but is not source material. Keep that distinction
                // explicit instead of silently indexing headers as a project.
                return Err(TransportFailure::DownloadUnavailable);
            };
            let recipe = match self.get_archive(&archive_url, self.limits.max_archive_bytes)? {
                TransportResult::Available(value) => value,
                TransportResult::Unavailable | TransportResult::RetryAfter(_) => {
                    return Err(TransportFailure::DownloadUnavailable);
                }
                TransportResult::NotModified => return Err(TransportFailure::Protocol),
            };
            let recipe = recipe.into_bytes(self.limits.max_archive_bytes)?;
            if export_entry
                .size
                .is_some_and(|size| size != u64::try_from(recipe.len()).unwrap_or(u64::MAX))
            {
                return Err(TransportFailure::Integrity);
            }
            if let Some(expected) = &export_entry.sha256 {
                if !expected.verifies(&recipe) {
                    return Err(TransportFailure::Integrity);
                }
            }
            let source = Self::conan_source_spec(
                &recipe,
                adapter.conan_recipe_version(),
                self.limits.max_archive_bytes,
            )?;
            if let Some(source) = source {
                let mut integrity_failure = false;
                let mut source_archive = None;
                for url in source.urls {
                    // Conan's authenticated export is the authority that names
                    // this source mirror. Keep the mirror policy closed: a recipe
                    // cannot turn this package add into arbitrary HTTPS egress.
                    // Admit the host for this one bounded handoff before asking
                    // the shared archive transport to read it; redirects and
                    // credentials remain forbidden.
                    if !conan_source_mirror_allowed(&url) {
                        return Err(TransportFailure::Configuration);
                    }
                    self.admit_resource_origin(&url)?;
                    let fetched = match self.get_archive(&url, self.limits.max_archive_bytes)? {
                        TransportResult::Available(value) => value,
                        TransportResult::Unavailable | TransportResult::RetryAfter(_) => continue,
                        TransportResult::NotModified => return Err(TransportFailure::Protocol),
                    };
                    let bytes = fetched.into_bytes(self.limits.max_archive_bytes)?;
                    if source.checksum.verifies(&bytes) {
                        source_archive = Some((url, bytes));
                        break;
                    }
                    integrity_failure = true;
                }
                let Some((source_url, bytes)) = source_archive else {
                    return Err(if integrity_failure {
                        TransportFailure::Integrity
                    } else {
                        TransportFailure::DownloadUnavailable
                    });
                };
                let checksum = source.checksum;
                self.cache_archive(checksum.cache_key(), ArchiveArtifact::from_bytes(bytes))?;
                // The source mirror is the verified content origin, but the
                // release descriptor must retain Conan as its authoritative
                // archive authority. `fetch_archive` consumes the staged handoff
                // keyed by this checksum, so this does not download the recipe
                // a second time.
                (checksum, archive_url.clone(), Some(source_url))
            } else {
                let checksum = if let Some(expected) = &export_entry.sha256 {
                    expected.clone()
                } else {
                    RegistryChecksum::sha256_hex(&hex_digest(Sha256::digest(&recipe).as_slice()))?
                };
                self.cache_archive(checksum.cache_key(), ArchiveArtifact::from_bytes(recipe))?;
                (checksum, archive_url.clone(), None)
            }
        };
        let mut provenance = Vec::with_capacity(revisions.len() + files.len());
        provenance.extend_from_slice(&revisions);
        provenance.extend_from_slice(&files);
        provenance.extend_from_slice(archive.as_bytes());
        if let Some(source_url) = source_url.as_ref() {
            provenance.extend_from_slice(source_url.as_bytes());
        }
        let mut release = adapter.release_from_checksum(
            adapter.conan_recipe_version(),
            archive,
            checksum,
            &provenance,
            ReleaseFacts::default(),
        )?;
        let archive_url = release.archive_url.clone();
        let artifact_kind = match source_availability {
            super::super::ecosystem::ConanSourceAvailability::Archive => {
                super::super::NativeArtifactKind::ConanSource
            }
            super::super::ecosystem::ConanSourceAvailability::RecipeOnly => {
                super::super::NativeArtifactKind::ConanRecipe
            }
            super::super::ecosystem::ConanSourceAvailability::Unavailable => {
                super::super::NativeArtifactKind::Other
            }
        };
        release.set_artifacts(vec![super::super::NativeArtifact {
            filename: std::sync::Arc::from(archive_name),
            url: std::sync::Arc::from(archive_url.as_str()),
            checksum: release.checksum.clone(),
            kind: artifact_kind,
            requires_python: None,
            size: None,
            yanked: false,
            yanked_reason: None,
        }]);
        let source = match source_availability {
            super::super::ecosystem::ConanSourceAvailability::Archive => {
                backend_library::RegistryConanSourceAvailability::Archive
            }
            super::super::ecosystem::ConanSourceAvailability::RecipeOnly => {
                backend_library::RegistryConanSourceAvailability::RecipeOnly
            }
            super::super::ecosystem::ConanSourceAvailability::Unavailable => {
                backend_library::RegistryConanSourceAvailability::Unavailable
            }
        };
        release.set_native_metadata(backend_library::RegistryNativeMetadata {
            version: backend_library::REGISTRY_NATIVE_METADATA_VERSION,
            availability: backend_library::RegistryNativeAvailability::Recorded,
            provenance: backend_library::RegistryNativeProvenance::SourceDigest(
                release.provenance.as_bytes(),
            ),
            details: backend_library::RegistryNativeDetails::Cpp(
                backend_library::RegistryConanMetadata {
                    artifacts: release
                        .artifacts
                        .iter()
                        .map(|artifact| backend_library::RegistryNativeArtifact {
                            filename: artifact.filename().to_owned(),
                            url: artifact.url().to_owned(),
                            checksum: super::super::ecosystem::native_checksum(artifact.checksum()),
                            kind: match source {
                                backend_library::RegistryConanSourceAvailability::Archive => {
                                    backend_library::RegistryNativeArtifactKind::ConanSource
                                }
                                backend_library::RegistryConanSourceAvailability::RecipeOnly => {
                                    backend_library::RegistryNativeArtifactKind::ConanRecipe
                                }
                                backend_library::RegistryConanSourceAvailability::Unavailable => {
                                    backend_library::RegistryNativeArtifactKind::Other
                                }
                            },
                            requires_python: None,
                            size: artifact.size(),
                            yanked: None,
                            yanked_reason: artifact.yanked_reason().map(ToOwned::to_owned),
                        })
                        .collect::<Vec<_>>()
                        .into_boxed_slice(),
                    source,
                    source_url,
                },
            ),
        })?;
        let (start, prefix) = adapter.page_start(&provenance, request, 1)?;
        adapter.admit_window(vec![release], request, start, prefix, 1)
    }

    fn conan_source_spec(
        recipe: &[u8],
        version: &str,
        maximum: usize,
    ) -> Result<Option<ConanSourceSpec>, TransportFailure> {
        let Some(metadata) = conan_export_file(recipe, maximum)? else {
            return Ok(None);
        };
        let text = std::str::from_utf8(&metadata).map_err(|_| TransportFailure::Protocol)?;
        let mut in_sources = false;
        let mut sources_indent = 0usize;
        let mut in_target = false;
        let mut target_indent = 0usize;
        let mut in_urls = false;
        let mut urls = Vec::new();
        let mut checksum = None;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }
            let indent = line.len() - line.trim_start_matches(' ').len();
            if !in_sources {
                if trimmed == "sources:" {
                    in_sources = true;
                    sources_indent = indent;
                }
                continue;
            }
            if !in_target {
                if indent <= sources_indent {
                    break;
                }
                if trimmed == format!("{version}:") {
                    in_target = true;
                    target_indent = indent;
                }
                continue;
            }
            if indent <= target_indent {
                break;
            }
            if let Some(value) = trimmed.strip_prefix("sha256:") {
                checksum = Some(RegistryChecksum::sha256_hex(value.trim())?);
                in_urls = false;
            } else if trimmed == "url:" {
                in_urls = true;
            } else if in_urls && indent > target_indent {
                if let Some(value) = trimmed.strip_prefix("-") {
                    let value = value.trim().trim_matches(['\'', '"']);
                    if !value.is_empty() {
                        urls.push(value.to_owned());
                    }
                } else {
                    in_urls = false;
                }
            } else {
                in_urls = false;
            }
        }
        let Some(checksum) = checksum else {
            return Err(TransportFailure::Protocol);
        };
        if urls.is_empty() {
            return Err(TransportFailure::DownloadUnavailable);
        }
        Ok(Some(ConanSourceSpec { urls, checksum }))
    }

    fn required(&self, url: &str) -> Result<Vec<u8>, TransportFailure> {
        match self.get(url, self.limits.max_feed_bytes)? {
            TransportResult::Available(value) => Ok(value),
            TransportResult::Unavailable => Err(TransportFailure::DownloadUnavailable),
            TransportResult::RetryAfter(_) => Err(TransportFailure::DownloadUnavailable),
            TransportResult::NotModified => Err(TransportFailure::Protocol),
        }
    }

    fn followup_url_is_admitted(&self, url: &str) -> bool {
        if same_authority(self.endpoint.url(), url) {
            return true;
        }
        let Ok(uri) = url.parse::<ureq::http::Uri>() else {
            return false;
        };
        let Some(authority) = uri.authority() else {
            return false;
        };
        let loopback = authority
            .host()
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
            .unwrap_or(authority.host())
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
        if !(uri.scheme_str() == Some("https") || (uri.scheme_str() == Some("http") && loopback))
            || authority.as_str().contains('@')
            || uri.query().is_some()
            || url.contains('#')
        {
            return false;
        }
        let host = authority.host();
        let host = host.to_ascii_lowercase();
        if self.trusted_followup_hosts.contains(&host) {
            return true;
        }
        official_followup_host(self.endpoint.url(), self.ecosystem, &host)
    }

    fn admit_resource_origin(&mut self, resource: &str) -> Result<(), TransportFailure> {
        let uri = resource
            .parse::<ureq::http::Uri>()
            .map_err(|_| TransportFailure::Protocol)?;
        let Some(authority) = uri.authority() else {
            return Err(TransportFailure::Protocol);
        };
        let loopback = authority
            .host()
            .strip_prefix('[')
            .and_then(|value| value.strip_suffix(']'))
            .unwrap_or(authority.host())
            .parse::<IpAddr>()
            .is_ok_and(|address| address.is_loopback());
        if !(uri.scheme_str() == Some("https") || (uri.scheme_str() == Some("http") && loopback))
            || authority.as_str().contains('@')
            || uri.query().is_some()
            || resource.contains('#')
        {
            return Err(TransportFailure::Configuration);
        }
        self.trusted_followup_hosts
            .insert(authority.host().to_ascii_lowercase());
        Ok(())
    }

    fn cache_archive(
        &mut self,
        key: [u8; 32],
        artifact: ArchiveArtifact,
    ) -> Result<(), TransportFailure> {
        if self
            .archive_handoffs
            .iter()
            .any(|handoff| handoff.key == key)
        {
            return Ok(());
        }
        let length = usize::try_from(artifact.length()).map_err(|_| TransportFailure::Bounds)?;
        let next = self
            .archive_handoff_bytes
            .checked_add(length)
            .ok_or(TransportFailure::Bounds)?;
        if next > self.limits.max_page_archive_bytes {
            return Err(TransportFailure::Overrun {
                measured: u64::try_from(next).map_err(|_| TransportFailure::Bounds)?,
                limit: u64::try_from(self.limits.max_page_archive_bytes)
                    .map_err(|_| TransportFailure::Bounds)?,
            });
        }
        self.archive_handoff_bytes = next;
        self.archive_handoffs
            .push_back(ArchiveHandoff { key, artifact });
        Ok(())
    }
}
