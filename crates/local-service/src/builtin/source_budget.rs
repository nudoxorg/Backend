//! Validated, identity-bearing limits for local source admission.
//!
//! Project admission, retained compiler text, and concurrent source bytes are
//! separate budgets. The source-byte budgets do not bound frontend/parser
//! allocation expansion or the aggregate in-memory source-record projection.
//! A larger project quota must not silently enlarge the number of concurrent
//! source reads.
//!
//! Operator overrides are decimal byte/item counts: `NUDOX_SOURCE_MAX_FILE_BYTES`
//! (8 MiB), `NUDOX_SOURCE_MAX_PROJECT_BYTES` (512 MiB),
//! `NUDOX_SOURCE_MAX_RETAINED_BYTES` (256 MiB),
//! `NUDOX_SOURCE_MAX_IN_FLIGHT_BYTES` (64 MiB),
//! `NUDOX_SOURCE_MAX_PROJECT_RECORD_BYTES` (128 MiB), and
//! `NUDOX_SOURCE_MAX_PROJECT_RECORDS` (500,000). Invalid values refuse the
//! scan before it starts; changing a valid value changes the warm-cache key.

use std::fmt;

const POLICY_VERSION: &[u8] = b"nudox.local-source-admission.v2\0";
const MAX_CONFIGURED_SOURCE_BYTES: usize = 4 * 1024 * 1024 * 1024;
const MAX_CONFIGURED_RECORDS: usize = 500_000;
const MAX_SCAN_WORKERS: usize = 8;

const MAX_FILE_ENV: &str = "NUDOX_SOURCE_MAX_FILE_BYTES";
const PROJECT_ENV: &str = "NUDOX_SOURCE_MAX_PROJECT_BYTES";
const RETAINED_ENV: &str = "NUDOX_SOURCE_MAX_RETAINED_BYTES";
const IN_FLIGHT_ENV: &str = "NUDOX_SOURCE_MAX_IN_FLIGHT_BYTES";
const RECORD_BYTES_ENV: &str = "NUDOX_SOURCE_MAX_PROJECT_RECORD_BYTES";
const RECORDS_ENV: &str = "NUDOX_SOURCE_MAX_PROJECT_RECORDS";

/// One scan's immutable resource limits. The environment can change between
/// scans, but a single scan always uses one validated policy and identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SourceAdmissionLimits {
    pub(super) max_file_source_bytes: usize,
    pub(super) max_project_source_bytes: usize,
    pub(super) max_retained_compiler_source_bytes: usize,
    pub(super) max_in_flight_source_bytes: usize,
    pub(super) max_project_record_bytes: usize,
    pub(super) max_project_records: usize,
    pub(super) max_encoded_record_bytes: usize,
}

impl Default for SourceAdmissionLimits {
    fn default() -> Self {
        Self {
            max_file_source_bytes: 8 * 1024 * 1024,
            max_project_source_bytes: 512 * 1024 * 1024,
            max_retained_compiler_source_bytes: 256 * 1024 * 1024,
            max_in_flight_source_bytes: 64 * 1024 * 1024,
            max_project_record_bytes: 128 * 1024 * 1024,
            max_project_records: MAX_CONFIGURED_RECORDS,
            // A persisted row cannot exceed this storage format's capacity.
            max_encoded_record_bytes: backend_engine::ProductSourceRecord::ROW_VALUE_CAPACITY,
        }
    }
}

/// Resource limits resolved for one scan, including their warm-cache identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct SourceAdmissionPolicy {
    limits: SourceAdmissionLimits,
    identity: [u8; 32],
}

impl SourceAdmissionPolicy {
    /// Resolves bounded operator overrides once, then pins the result for the
    /// complete scan. Invalid configuration refuses admission instead of
    /// silently falling back to a different quota.
    pub(super) fn from_environment() -> Result<Self, String> {
        let mut limits = SourceAdmissionLimits::default();
        limits.max_file_source_bytes =
            configured_bytes(MAX_FILE_ENV, limits.max_file_source_bytes)?;
        limits.max_project_source_bytes =
            configured_bytes(PROJECT_ENV, limits.max_project_source_bytes)?;
        limits.max_retained_compiler_source_bytes =
            configured_bytes(RETAINED_ENV, limits.max_retained_compiler_source_bytes)?;
        limits.max_in_flight_source_bytes =
            configured_bytes(IN_FLIGHT_ENV, limits.max_in_flight_source_bytes)?;
        limits.max_project_record_bytes =
            configured_bytes(RECORD_BYTES_ENV, limits.max_project_record_bytes)?;
        limits.max_project_records = configured_count(RECORDS_ENV, limits.max_project_records)?;
        Self::new(limits).map_err(|refusal| refusal.to_string())
    }

    pub(super) fn new(limits: SourceAdmissionLimits) -> Result<Self, Refusal> {
        let max_file = limits.max_file_source_bytes;
        if max_file == 0 || max_file > MAX_CONFIGURED_SOURCE_BYTES {
            return Err(Refusal::new(
                "per-file source bytes",
                max_file,
                MAX_CONFIGURED_SOURCE_BYTES,
            ));
        }
        for (resource, value) in [
            ("project source bytes", limits.max_project_source_bytes),
            (
                "retained compiler source bytes",
                limits.max_retained_compiler_source_bytes,
            ),
            ("in-flight source bytes", limits.max_in_flight_source_bytes),
            ("project record bytes", limits.max_project_record_bytes),
        ] {
            if value > MAX_CONFIGURED_SOURCE_BYTES {
                return Err(Refusal::new(resource, value, MAX_CONFIGURED_SOURCE_BYTES));
            }
        }
        if max_file > limits.max_project_source_bytes {
            return Err(Refusal::new(
                "per-file source bytes versus project source bytes",
                max_file,
                limits.max_project_source_bytes,
            ));
        }
        let minimum_in_flight = max_file.checked_mul(2).unwrap_or(usize::MAX);
        if limits.max_in_flight_source_bytes < minimum_in_flight {
            return Err(Refusal::new(
                "in-flight source bytes (must admit one parser result and one queued result)",
                limits.max_in_flight_source_bytes,
                minimum_in_flight,
            ));
        }
        if limits.max_project_records == 0 || limits.max_project_records > MAX_CONFIGURED_RECORDS {
            return Err(Refusal::new(
                "project source records",
                limits.max_project_records,
                MAX_CONFIGURED_RECORDS,
            ));
        }
        if limits.max_encoded_record_bytes == 0
            || limits.max_encoded_record_bytes
                > backend_engine::ProductSourceRecord::ROW_VALUE_CAPACITY
        {
            return Err(Refusal::new(
                "encoded source record bytes",
                limits.max_encoded_record_bytes,
                backend_engine::ProductSourceRecord::ROW_VALUE_CAPACITY,
            ));
        }
        if limits.max_project_record_bytes < limits.max_encoded_record_bytes {
            return Err(Refusal::new(
                "project record bytes versus one encoded record",
                limits.max_project_record_bytes,
                limits.max_encoded_record_bytes,
            ));
        }

        let identity = identity_for(limits);
        Ok(Self { limits, identity })
    }

    pub(super) const fn limits(self) -> SourceAdmissionLimits {
        self.limits
    }

    /// Stable identity of every effective source quota. Warm scan reuse is
    /// valid only under this exact policy identity.
    pub(super) const fn identity(self) -> [u8; 32] {
        self.identity
    }

    /// Chooses a worker count from the configured source-byte allowance.
    /// This budgets two maximum source-file payloads per worker; it does not
    /// measure parser allocation expansion or aggregate record memory.
    pub(super) fn worker_count(self, available: usize, path_count: usize) -> usize {
        let limits = self.limits;
        let per_worker = limits
            .max_file_source_bytes
            .checked_mul(2)
            .unwrap_or(usize::MAX);
        let by_in_flight = limits.max_in_flight_source_bytes / per_worker.max(1);
        available
            .max(1)
            .min(MAX_SCAN_WORKERS)
            .min(by_in_flight.max(1))
            .min(path_count.max(1))
    }

    /// Creates a typed admission token from the actual byte length returned by
    /// the confined read. Callers must use this after, not before, reading.
    pub(super) fn admit_actual_file(self, actual_bytes: usize) -> Result<AdmittedFile, Refusal> {
        if actual_bytes > self.limits.max_file_source_bytes {
            return Err(Refusal::new(
                "per-file source bytes",
                actual_bytes,
                self.limits.max_file_source_bytes,
            ));
        }
        Ok(AdmittedFile {
            actual_bytes,
            policy_identity: self.identity,
        })
    }

    pub(super) fn admit_retained_total(
        self,
        retained_bytes: usize,
        next_file_bytes: usize,
    ) -> Result<usize, Refusal> {
        let attempted = checked_sum(
            retained_bytes,
            next_file_bytes,
            "retained compiler source bytes",
        )?;
        check_limit(
            "retained compiler source bytes",
            attempted,
            self.limits.max_retained_compiler_source_bytes,
        )?;
        Ok(attempted)
    }

    pub(super) fn admit_preflight(
        self,
        source_bytes: usize,
        file_count: usize,
    ) -> Result<(), Refusal> {
        if source_bytes > self.limits.max_project_source_bytes {
            return Err(Refusal::new(
                "project source bytes",
                source_bytes,
                self.limits.max_project_source_bytes,
            ));
        }
        if file_count > self.limits.max_project_records {
            return Err(Refusal::new(
                "project source records",
                file_count,
                self.limits.max_project_records,
            ));
        }
        Ok(())
    }
}

/// Exact successful read length accepted under one source policy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct AdmittedFile {
    actual_bytes: usize,
    policy_identity: [u8; 32],
}

/// Deterministic project-wide source and persisted-row accounting.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct SourceAdmissionLedger {
    source_bytes: usize,
    retained_compiler_bytes: usize,
    encoded_record_bytes: usize,
    records: usize,
}

impl SourceAdmissionLedger {
    /// Charges one real row, including valid zero-byte files. The caller must
    /// feed files in normalized path order so a refusal always names the same
    /// boundary file regardless of worker completion order.
    pub(super) fn admit(
        &mut self,
        policy: SourceAdmissionPolicy,
        file: AdmittedFile,
        encoded_record_bytes: usize,
        retain_compiler_source: bool,
    ) -> Result<(), Refusal> {
        if file.policy_identity != policy.identity {
            return Err(Refusal::policy_mismatch());
        }
        if encoded_record_bytes > policy.limits.max_encoded_record_bytes {
            return Err(Refusal::new(
                "encoded source record bytes",
                encoded_record_bytes,
                policy.limits.max_encoded_record_bytes,
            ));
        }
        let source_bytes =
            checked_sum(self.source_bytes, file.actual_bytes, "project source bytes")?;
        let retained_compiler_bytes = checked_sum(
            self.retained_compiler_bytes,
            if retain_compiler_source {
                file.actual_bytes
            } else {
                0
            },
            "retained compiler source bytes",
        )?;
        let encoded_record_bytes_total = checked_sum(
            self.encoded_record_bytes,
            encoded_record_bytes,
            "project record bytes",
        )?;
        let records = checked_sum(self.records, 1, "project source records")?;

        check_limit(
            "project source bytes",
            source_bytes,
            policy.limits.max_project_source_bytes,
        )?;
        check_limit(
            "retained compiler source bytes",
            retained_compiler_bytes,
            policy.limits.max_retained_compiler_source_bytes,
        )?;
        check_limit(
            "project record bytes",
            encoded_record_bytes_total,
            policy.limits.max_project_record_bytes,
        )?;
        check_limit(
            "project source records",
            records,
            policy.limits.max_project_records,
        )?;

        self.source_bytes = source_bytes;
        self.retained_compiler_bytes = retained_compiler_bytes;
        self.encoded_record_bytes = encoded_record_bytes_total;
        self.records = records;
        Ok(())
    }

    pub(super) const fn source_bytes(self) -> usize {
        self.source_bytes
    }

    pub(super) const fn retained_compiler_bytes(self) -> usize {
        self.retained_compiler_bytes
    }

    pub(super) const fn encoded_record_bytes(self) -> usize {
        self.encoded_record_bytes
    }

    pub(super) const fn records(self) -> usize {
        self.records
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Refusal {
    resource: &'static str,
    attempted: usize,
    limit: usize,
}

impl Refusal {
    const fn new(resource: &'static str, attempted: usize, limit: usize) -> Self {
        Self {
            resource,
            attempted,
            limit,
        }
    }

    fn policy_mismatch() -> Self {
        Self::new("source policy identity", 1, 0)
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "source admission refused: {} is {} bytes/items (configured limit {})",
            self.resource, self.attempted, self.limit
        )
    }
}

impl std::error::Error for Refusal {}

fn configured_bytes(name: &'static str, default: usize) -> Result<usize, String> {
    configured_number(name, default, MAX_CONFIGURED_SOURCE_BYTES)
}

fn configured_count(name: &'static str, default: usize) -> Result<usize, String> {
    configured_number(name, default, MAX_CONFIGURED_RECORDS)
}

fn configured_number(name: &'static str, default: usize, maximum: usize) -> Result<usize, String> {
    let Some(value) = std::env::var_os(name) else {
        return Ok(default);
    };
    let value = value
        .into_string()
        .map_err(|_| format!("{name} must be UTF-8 decimal digits"))?;
    let parsed = value
        .parse::<usize>()
        .map_err(|_| format!("{name} must be decimal bytes/items"))?;
    if parsed > maximum {
        return Err(format!(
            "source admission refused: {name} is {parsed} (configured maximum {maximum})"
        ));
    }
    Ok(parsed)
}

fn checked_sum(left: usize, right: usize, resource: &'static str) -> Result<usize, Refusal> {
    left.checked_add(right)
        .ok_or_else(|| Refusal::new(resource, usize::MAX, usize::MAX - 1))
}

fn check_limit(resource: &'static str, attempted: usize, limit: usize) -> Result<(), Refusal> {
    if attempted > limit {
        Err(Refusal::new(resource, attempted, limit))
    } else {
        Ok(())
    }
}

fn identity_for(limits: SourceAdmissionLimits) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    hasher.update(POLICY_VERSION);
    for value in [
        limits.max_file_source_bytes,
        limits.max_project_source_bytes,
        limits.max_retained_compiler_source_bytes,
        limits.max_in_flight_source_bytes,
        limits.max_project_record_bytes,
        limits.max_project_records,
        limits.max_encoded_record_bytes,
    ] {
        hasher.update(&u64::try_from(value).unwrap_or(u64::MAX).to_be_bytes());
    }
    *hasher.finalize().as_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_admit_large_project_and_generated_file_without_widening_in_flight() {
        let policy = SourceAdmissionPolicy::new(SourceAdmissionLimits::default())
            .expect("default policy is valid");
        let limits = policy.limits();
        assert!(limits.max_file_source_bytes > 512 * 1024);
        assert!(limits.max_project_source_bytes > 84 * 1024 * 1024);
        assert!(limits.max_in_flight_source_bytes < limits.max_project_source_bytes);
        assert!(
            policy.worker_count(32, 10_000) * limits.max_file_source_bytes * 2
                <= limits.max_in_flight_source_bytes
        );
    }

    #[test]
    fn policy_identity_tracks_every_effective_limit() {
        let first = SourceAdmissionPolicy::new(SourceAdmissionLimits::default())
            .expect("default policy is valid");
        let mut changed = SourceAdmissionLimits::default();
        changed.max_project_source_bytes -= 1;
        let second = SourceAdmissionPolicy::new(changed).expect("changed policy is valid");
        assert_ne!(first.identity(), second.identity());
        assert_eq!(
            first.identity(),
            SourceAdmissionPolicy::new(first.limits())
                .unwrap()
                .identity()
        );
    }

    #[test]
    fn validated_policy_rejects_in_flight_budget_smaller_than_two_files() {
        let mut limits = SourceAdmissionLimits::default();
        limits.max_file_source_bytes = 8 * 1024 * 1024;
        limits.max_in_flight_source_bytes = 15 * 1024 * 1024;
        let error = SourceAdmissionPolicy::new(limits).unwrap_err();
        assert!(error.to_string().contains("in-flight source bytes"));
    }

    #[test]
    fn actual_read_length_and_policy_bound_tokens_and_zero_byte_records() {
        let mut limits = SourceAdmissionLimits::default();
        limits.max_file_source_bytes = 64;
        limits.max_project_source_bytes = 128;
        limits.max_retained_compiler_source_bytes = 128;
        limits.max_in_flight_source_bytes = 128;
        limits.max_project_record_bytes = 4096;
        limits.max_encoded_record_bytes = 4096;
        let policy = SourceAdmissionPolicy::new(limits).unwrap();
        let mut ledger = SourceAdmissionLedger::default();
        let zero = policy.admit_actual_file(0).unwrap();
        ledger.admit(policy, zero, 48, true).unwrap();
        assert_eq!(ledger.source_bytes(), 0);
        assert_eq!(ledger.retained_compiler_bytes(), 0);
        assert_eq!(ledger.records(), 1);
        assert_eq!(ledger.encoded_record_bytes(), 48);
        assert!(policy.admit_actual_file(65).is_err());
    }

    #[test]
    fn project_record_and_retention_budgets_are_independent() {
        let mut limits = SourceAdmissionLimits::default();
        limits.max_file_source_bytes = 64;
        limits.max_project_source_bytes = 128;
        limits.max_retained_compiler_source_bytes = 32;
        limits.max_in_flight_source_bytes = 128;
        limits.max_project_record_bytes = 4096;
        limits.max_encoded_record_bytes = 4096;
        let policy = SourceAdmissionPolicy::new(limits).unwrap();
        let mut ledger = SourceAdmissionLedger::default();
        let source = policy.admit_actual_file(40).unwrap();
        let error = ledger.admit(policy, source, 48, true).unwrap_err();
        assert!(error.to_string().contains("retained compiler source bytes"));
        assert_eq!(ledger.records(), 0, "rejected files are not charged");
        ledger
            .admit(policy, policy.admit_actual_file(40).unwrap(), 48, false)
            .unwrap();
        assert_eq!(ledger.source_bytes(), 40);
        assert_eq!(ledger.retained_compiler_bytes(), 0);
    }

    #[test]
    fn configured_preflight_reports_exact_boundary_and_record_count() {
        let policy = SourceAdmissionPolicy::new(SourceAdmissionLimits::default()).unwrap();
        let limit = policy.limits().max_project_source_bytes;
        assert!(policy.admit_preflight(limit, 1).is_ok());
        let error = policy.admit_preflight(limit + 1, 1).unwrap_err();
        assert!(error.to_string().contains("project source bytes"));
        let error = policy
            .admit_preflight(0, policy.limits().max_project_records + 1)
            .unwrap_err();
        assert!(error.to_string().contains("project source records"));
    }

    #[test]
    fn project_preflight_does_not_charge_the_compiler_subset_budget() {
        let mut limits = SourceAdmissionLimits::default();
        limits.max_project_source_bytes = 128;
        limits.max_retained_compiler_source_bytes = 32;
        limits.max_in_flight_source_bytes = 128;
        limits.max_file_source_bytes = 64;
        let policy = SourceAdmissionPolicy::new(limits).unwrap();

        assert!(policy.admit_preflight(96, 2).is_ok());
        let mut ledger = SourceAdmissionLedger::default();
        ledger
            .admit(policy, policy.admit_actual_file(40).unwrap(), 48, false)
            .unwrap();
        assert_eq!(ledger.source_bytes(), 40);
        let refusal = ledger
            .admit(policy, policy.admit_actual_file(40).unwrap(), 48, true)
            .unwrap_err();
        assert!(
            refusal
                .to_string()
                .contains("retained compiler source bytes")
        );
        assert_eq!(ledger.source_bytes(), 40, "a refused row is not charged");
    }
}
