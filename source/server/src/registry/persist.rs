use std::{collections::HashMap, num::NonZeroU64, sync::Arc};

use tracing::warn;

use crate::{storage::StorageLayout, sync_progress::PackageSyncStatus};

use super::{PackageHandle, PackageId, PackageKey, PackageSpec, TrackedPackage};
use super::package::PackageStateSnapshot;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct PersistedRegistry {
	pub packages: Vec<PersistedPackage>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub(super) struct PersistedPackage {
	pub id:     u64,
	pub spec:   PackageSpec,
	pub handle: PackageHandle,
	pub state:  PackageStateSnapshot,
}

pub(super) fn load_persisted_registry(storage: &StorageLayout) -> Option<PersistedRegistry> {
	let path = storage.packages_file();
	if !path.is_file() {
		return None;
	}

	let bytes = match std::fs::read(path) {
		Ok(bytes) => bytes,
		Err(error) => {
			warn!(path = %path.display(), error = %error, "failed to read persisted package registry");
			return None;
		}
	};

	match serde_json::from_slice::<PersistedRegistry>(&bytes) {
		Ok(registry) => Some(registry),
		Err(error) => {
			warn!(path = %path.display(), error = %error, "failed to parse persisted package registry");
			None
		}
	}
}

pub(super) fn rehydrate_registry(
	registry: PersistedRegistry,
) -> (HashMap<PackageId, Arc<TrackedPackage>>, HashMap<PackageKey, PackageId>) {
	let mut packages = HashMap::new();
	let mut keys = HashMap::new();

	for package in registry.packages {
		let Some(id) = NonZeroU64::new(package.id).map(PackageId) else {
			continue;
		};
		let key = PackageKey {
			language: package.spec.language,
			name:     package.spec.name.clone(),
			version:  package.spec.version.clone(),
			branch:   package.spec.branch.clone(),
		};
		let mut state = package.state;
		if !matches!(state.sync_status, PackageSyncStatus::Idle) {
			state.sync_status = PackageSyncStatus::Idle;
			state.sync_phase = None;
			state.sync_detail = None;
		}
		let tracked = Arc::new(TrackedPackage::rehydrated(id, package.spec, package.handle, state));
		keys.insert(key, id);
		packages.insert(id, tracked);
	}

	(packages, keys)
}
