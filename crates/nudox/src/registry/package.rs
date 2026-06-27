use jiff::Timestamp;
use lang_types::Language;
use semver::Version;
use serde::{Deserialize, Deserializer, Serialize};
use url::Url;

use crate::sync_progress::{PackageSyncPhase, PackageSyncStatus};

fn deserialize_lenient_version<'de, D: Deserializer<'de>>(d: D) -> Result<Version, D::Error> {
	let s = String::deserialize(d)?;
	if let Ok(v) = Version::parse(&s) {
		return Ok(v);
	}
	let padded = match s.matches('.').count() {
		0 => format!("{s}.0.0"),
		1 => format!("{s}.0"),
		_ => s.clone(),
	};
	Version::parse(&padded)
		.map_err(|e| serde::de::Error::custom(format!("invalid version `{s}`: {e}")))
}

fn deserialize_language<'de, D: Deserializer<'de>>(d: D) -> Result<Language, D::Error> {
	let s = String::deserialize(d)?;
	s.parse::<Language>().map_err(serde::de::Error::custom)
}

#[derive(Debug, Clone, Deserialize)]
pub struct NewPackageRequest {
	#[serde(deserialize_with = "deserialize_language")]
	pub language:    Language,
	pub name:        String,
	#[serde(deserialize_with = "deserialize_lenient_version")]
	pub version:     Version,
	#[serde(default)]
	pub source:      Option<String>,
	#[serde(default)]
	pub entry_point: Option<String>,
	#[serde(default)]
	pub branch:      Option<String>,
}

#[derive(Debug, Clone)]
pub enum AddPackageOutcome {
	Created(PackageSnapshot),
	Existing(PackageSnapshot),
}

#[derive(Debug, Clone, Serialize)]
pub struct PackageSnapshot {
	pub id:       u64,
	pub language: Language,
	pub name:     String,
	pub slug:     String,
	pub source:   Url,
	pub version:  Version,
	pub branch:   String,
	pub state:    PackageStateSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageStateSnapshot {
	pub health:                  PackageHealth,
	#[serde(default)]
	pub sync_status:             PackageSyncStatus,
	#[serde(default)]
	pub sync_phase:              Option<PackageSyncPhase>,
	#[serde(default)]
	pub sync_detail:             Option<String>,
	pub last_checked_at:         Option<Timestamp>,
	pub last_synced_at:          Option<Timestamp>,
	pub tracked_version_commit:  Option<String>,
	pub latest_remote_commit:    Option<String>,
	pub remote_update_available: bool,
	pub entry_count:             usize,
	pub document_count:          usize,
	pub vector_count:            usize,
	pub last_error:              Option<String>,
}

impl PackageStateSnapshot {
	pub(super) fn new() -> Self {
		Self {
			health:                  PackageHealth::Pending,
			sync_status:             PackageSyncStatus::Idle,
			sync_phase:              None,
			sync_detail:             None,
			last_checked_at:         None,
			last_synced_at:          None,
			tracked_version_commit:  None,
			latest_remote_commit:    None,
			remote_update_available: false,
			entry_count:             0,
			document_count:          0,
			vector_count:            0,
			last_error:              None,
		}
	}

	pub(super) fn normalize_rehydrated(mut self) -> Self {
		if !matches!(self.sync_status, PackageSyncStatus::Idle) {
			self.sync_status = PackageSyncStatus::Idle;
			self.sync_phase = None;
			self.sync_detail = None;
		}
		self
	}
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PackageHealth {
	Pending,
	Healthy,
	Degraded,
}
