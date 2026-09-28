//! Durable code-forge acquisition gateway used by the local product surface.

use crate::process::ForgeConfig;
use backend_engine::forge::GitCommandTransport;
use backend_engine::{
    ForgeAcquisitionOutcome, ForgeAcquisitionService, ForgeCoordinate, ForgeProvider,
    ForgeRejectReason, ForgeSearchRecord, ForgeTransport, ForgeTransportError, HttpForgeTransport,
};
use backend_library::ForgePackageRecord;
use std::fmt;
use std::path::PathBuf;

#[path = "forge_gateway/package_view.rs"]
mod package_view;
pub(super) use package_view::{
    ForgePackageDetail, ForgePackagePin, ForgeRegistryEvidence, find_package_versions,
    project_package_details,
};

pub(super) struct ForgeGateway {
    service: ForgeAcquisitionService,
    config: ForgeConfig,
    transport_root: PathBuf,
}

impl fmt::Debug for ForgeGateway {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ForgeGateway")
            .field("service", &self.service)
            .field("policy", &self.config.policy)
            .field("limits", &self.config.limits)
            .field("transport_root", &self.transport_root)
            .finish_non_exhaustive()
    }
}

impl ForgeGateway {
    pub(super) fn open(root: impl Into<PathBuf>, config: ForgeConfig) -> Result<Self, String> {
        let root = root.into();
        let service = ForgeAcquisitionService::open(root.clone(), config.policy, config.limits)
            .map_err(|error| error.to_string())?;
        Ok(Self {
            service,
            config,
            transport_root: root.join("transport"),
        })
    }

    /// Acquires one source, returning the same exact cached result on repeated adds.
    pub(super) fn acquire(
        &mut self,
        raw_coordinate: &str,
    ) -> Result<ForgePackageRecord, ForgeGatewayError> {
        let coordinate = ForgeCoordinate::parse(raw_coordinate.to_owned())
            .map_err(ForgeGatewayError::Coordinate)?;
        let token = self
            .config
            .authentication
            .for_provider(coordinate.provider());
        match coordinate.provider() {
            ForgeProvider::GenericHttpsGit => {
                let mut transport = GitCommandTransport::new(&self.transport_root)
                    .map_err(ForgeGatewayError::Transport)?;
                if let Some(token) = token {
                    transport = transport.with_token(token);
                }
                self.acquire_with(&coordinate, &mut transport)
            }
            ForgeProvider::Github | ForgeProvider::Gitlab | ForgeProvider::Codeberg => {
                let mut transport = HttpForgeTransport::new(self.config.limits, token)
                    .map_err(ForgeGatewayError::Transport)?;
                self.acquire_with(&coordinate, &mut transport)
            }
        }
    }

    /// Reads only the durable forge journal and CAS; this path never creates a transport.
    pub(super) fn reference(
        &self,
        raw_coordinate: &str,
    ) -> Result<ForgePackageRecord, ForgeGatewayError> {
        let coordinate = ForgeCoordinate::parse(raw_coordinate.to_owned())
            .map_err(ForgeGatewayError::Coordinate)?;
        let result = self
            .service
            .reference(&coordinate)
            .map_err(|_| ForgeGatewayError::Corrupt)?
            .ok_or(ForgeGatewayError::NotCached)?;
        result
            .product_record()
            .map_err(|_| ForgeGatewayError::ProductProjection)
    }

    /// Returns the metadata-only records the package search projection needs.
    /// This reads the durable forge catalog but never opens an HTTP transport
    /// or rehydrates source archive bodies.
    pub(super) fn search_records(&self) -> Result<Vec<ForgeSearchRecord>, String> {
        self.service
            .search_records()
            .map_err(|error| format!("read forge search catalog: {error}"))
    }

    fn acquire_with<T: ForgeTransport>(
        &self,
        coordinate: &ForgeCoordinate,
        transport: &mut T,
    ) -> Result<ForgePackageRecord, ForgeGatewayError> {
        match self.service.acquire(coordinate, transport) {
            ForgeAcquisitionOutcome::Hit(result) => result
                .product_record()
                .map_err(|_| ForgeGatewayError::ProductProjection),
            ForgeAcquisitionOutcome::Offline => Err(ForgeGatewayError::Offline),
            ForgeAcquisitionOutcome::Unavailable => Err(ForgeGatewayError::Unavailable),
            ForgeAcquisitionOutcome::RetryAfter(millis) => {
                Err(ForgeGatewayError::RetryAfter(millis))
            }
            ForgeAcquisitionOutcome::Rejected(reason) => Err(ForgeGatewayError::Rejected(reason)),
            ForgeAcquisitionOutcome::Corrupt => Err(ForgeGatewayError::Corrupt),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ForgeGatewayError {
    Coordinate(backend_engine::ForgeCoordinateError),
    NotCached,
    Offline,
    Unavailable,
    RetryAfter(u64),
    Rejected(ForgeRejectReason),
    Corrupt,
    Transport(ForgeTransportError),
    ProductProjection,
}

impl fmt::Display for ForgeGatewayError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Coordinate(error) => write!(formatter, "forge coordinate rejected: {error}"),
            Self::NotCached => formatter.write_str("forge source is not in the local cache"),
            Self::Offline => {
                formatter.write_str("forge source is not cached and acquisition is offline")
            }
            Self::Unavailable => formatter.write_str("forge source is unavailable"),
            Self::RetryAfter(millis) => {
                write!(formatter, "forge source requested retry after {millis} ms")
            }
            Self::Rejected(reason) => write!(formatter, "forge acquisition rejected: {reason:?}"),
            Self::Corrupt => formatter.write_str("forge cache or journal is corrupt"),
            Self::Transport(error) => write!(formatter, "forge transport unavailable: {error:?}"),
            Self::ProductProjection => formatter.write_str("forge result could not be projected"),
        }
    }
}
