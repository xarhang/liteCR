//! Migration staging/application boundary.
//!
//! Exact v1 manifest/stream commitment verification is implemented in
//! [`package`]; package preparation and row mapping remain in the operator
//! tooling. Actual source mutations must go through an implementation of
//! `MigrationExecutor` that enlists the existing Registry use case and this
//! journal in one DB transaction. This module intentionally provides no
//! direct SQL mutation path.

use std::collections::BTreeSet;

use async_trait::async_trait;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::access::{AccessContext, AccessDenied, Permission, ResourceScope, authorize};

pub mod package;

pub const PACKAGE_FORMAT: &str = "litecr-migration/v1";
pub const MAX_BATCH_RECORDS: usize = 500;
pub const MAX_BATCH_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_RECORD_BYTES: usize = 1024 * 1024;
pub const MAX_API_BATCH_BYTES: usize = 8 * 1024 * 1024;

/// Explicit deployment policy for retaining temporary exact migration bytes.
/// The string references the protection configured outside this application;
/// liteCR does not assume a particular encryption mechanism.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationPackagePolicy {
    pub global_quota_bytes: u64,
    pub max_active_uploads: u32,
    pub retention_seconds: u64,
    pub storage_protection_reference: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationPackageV2Receipt {
    pub upload_id: String,
    pub manifest_sha256: [u8; 32],
    pub stream_sha256: [u8; 32],
    pub state: String,
    pub staged_batch_count: u64,
    pub staged_bytes: u64,
    pub record_count: u64,
    pub committed_batch_sequence: u64,
    pub committed_record_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationRun {
    /// SHA-256 over the exact manifest bytes. It binds every checkpoint/retry.
    pub manifest_sha256: [u8; 32],
    /// SHA-256 over the exact NDJSON stream bytes.
    pub stream_sha256: [u8; 32],
    pub snapshot_id: String,
    pub source_system_id: String,
    pub registry_id: String,
    pub organization_id: String,
    pub expected_record_count: u64,
}

/// One source row as it appears in the staged package. `raw_ndjson` includes
/// the original LF and is retained so adapters can preserve unsupported data
/// rather than silently dropping it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationRecord {
    pub source_system_id: String,
    pub source_record_key: String,
    pub source_version: String,
    /// Stable canonical JSON checksum from the validator.
    pub payload_sha256: [u8; 32],
    /// Includes original source bytes and the terminating LF.
    pub raw_ndjson: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationBatch {
    pub run: MigrationRun,
    pub sequence: u64,
    pub first_input_line: u64,
    pub last_input_line: u64,
    pub batch_sha256: [u8; 32],
    /// Exact concatenated input rows for this batch.
    pub exact_bytes: Vec<u8>,
    pub records: Vec<MigrationRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedSource {
    pub source_system_id: String,
    pub source_record_key: String,
    pub source_version: String,
    pub payload_sha256: [u8; 32],
    pub public_id: String,
    pub source_record_id: String,
    pub receive_version: i64,
    /// The exact current payload hash/version needed to prove safe reversal.
    pub expected_current_sha256: [u8; 32],
    pub expected_current_receive_version: i64,
    pub source_record_created: bool,
    pub person_created: bool,
    pub previous_receive_version: Option<i64>,
    pub introduced_alias_ids: Vec<String>,
    pub introduced_identifier_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationCheckpoint {
    pub manifest_sha256: [u8; 32],
    pub committed_batch_sequence: u64,
    pub committed_record_count: u64,
    pub state: MigrationRunState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MigrationRunState {
    Validated,
    Importing,
    Completed,
    ReverseBlocked,
    Reversed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BatchCommit {
    pub checkpoint: MigrationCheckpoint,
    pub applied: Vec<AppliedSource>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReverseManifest {
    pub manifest_sha256: [u8; 32],
    pub registry_id: String,
    pub organization_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReversePreflightRequest {
    pub manifest_sha256: [u8; 32],
    pub registry_id: String,
    pub organization_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReverseReceipt {
    pub manifest_sha256: [u8; 32],
    pub withdrawn_record_count: u64,
    pub detached_identifier_count: u64,
    pub retained_alias_count: u64,
    pub state: MigrationRunState,
}

/// Auditable authorization to resubmit corrected rows from a quarantined
/// source package. The request contains hashes and line metadata only; source
/// bytes remain in the operator's protected package directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantineDisposition {
    pub action: QuarantineDispositionAction,
    pub parent_manifest_sha256: [u8; 32],
    pub replacement_manifest_sha256: Option<[u8; 32]>,
    pub registry_id: String,
    pub organization_id: String,
    pub reason: String,
    pub idempotency_key: String,
    pub corrections: Vec<QuarantineCorrection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantineCorrection {
    pub input_line: u64,
    pub duplicate_of_line: Option<u64>,
    pub quarantine_code: String,
    pub original_payload_sha256: [u8; 32],
    pub corrected_payload_sha256: Option<[u8; 32]>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarantineDispositionAction {
    CorrectedResubmission,
    Deferred,
    DuplicateAcknowledged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QuarantineDispositionReceipt {
    pub disposition_id: String,
    pub action: QuarantineDispositionAction,
    pub replacement_manifest_sha256: Option<[u8; 32]>,
    pub correction_count: u64,
    pub replayed: bool,
}

/// Aggregate, privacy-preserving result of an advisory reverse preflight.
/// Counts are grouped by stable blocker code; source keys and IDs are never
/// returned. This is only a snapshot and cannot authorize a later reverse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReversePreflight {
    pub eligible: bool,
    pub blockers: Vec<ReverseBlockerCount>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReverseBlockerCount {
    pub code: ReverseBlockerCode,
    pub count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ReverseBlockerCode {
    InvalidCheckpoint,
    AlreadyReversed,
    JournalMismatch,
    MissingCurrentSource,
    SourceAlreadyWithdrawn,
    SourceChanged,
    LaterSourceVersion,
    IdentityDependency,
    MissingPerson,
}

impl ReverseBlockerCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidCheckpoint => "invalid_checkpoint",
            Self::AlreadyReversed => "already_reversed",
            Self::JournalMismatch => "journal_mismatch",
            Self::MissingCurrentSource => "missing_current_source",
            Self::SourceAlreadyWithdrawn => "source_already_withdrawn",
            Self::SourceChanged => "source_changed",
            Self::LaterSourceVersion => "later_source_version",
            Self::IdentityDependency => "identity_dependency",
            Self::MissingPerson => "missing_person",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentSourceState {
    pub source_system_id: String,
    pub source_record_key: String,
    pub payload_sha256: [u8; 32],
    pub receive_version: i64,
    pub has_later_identity_dependency: bool,
    pub relationship_representable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReverseRefusal {
    pub source_system_id: String,
    pub source_record_key: String,
    pub reason: ReverseRefusalReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReverseRefusalReason {
    MissingCurrentRecord,
    LaterSourceChange,
    ImportedVersionNotCurrent,
    LaterIdentityDependency,
    RelationshipNotRepresentable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReverseReplayCheck {
    pub representable: bool,
    pub refusals: Vec<ReverseRefusal>,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum MigrationError {
    #[error("access denied")]
    AccessDenied(#[from] AccessDenied),
    #[error("migration batch is invalid")]
    InvalidBatch,
    #[error("migration batch checksum does not match")]
    ChecksumMismatch,
    #[error("migration record exceeds configured bound")]
    RecordTooLarge,
    #[error("migration record key or version is duplicated in this batch")]
    DuplicateRecord,
    #[error("migration batch could not be committed")]
    CommitFailed,
    #[error("migration checkpoint/package does not match")]
    CheckpointConflict,
    #[error("server-verified migration uploads are not configured")]
    PackageV2Disabled,
    #[error("migration upload was not found")]
    PackageNotFound,
    #[error("migration upload state or exact bytes conflict with this request")]
    PackageConflict,
    #[error("migration upload staging quota is exhausted")]
    PackageQuotaExceeded,
    #[error("migration upload package verification failed")]
    PackageVerificationFailed,
    #[error("migration row contains a field not safely representable by the Registry schema")]
    UnsupportedRecord,
    #[error("migration identity or alias conflicts with an existing scoped identity")]
    IdentityCollision,
    #[error("migration manifest cannot be safely withdrawn because later changes depend on it")]
    ReverseBlocked,
    #[error("quarantine disposition request is invalid or contains a fail-closed conflict")]
    InvalidDisposition,
}

/// The concrete adapter must call only authorized Registry migration
/// operations. It must persist source mutations, idempotency, audit/outbox,
/// batch checkpoint and journal atomically, so a killed process can never
/// leave an unjournaled import. Returning success without these effects is a
/// contract violation.
#[async_trait]
pub trait MigrationExecutor: Send + Sync + 'static {
    /// Performs a bounded no-write validation pass. Concrete adapters should
    /// validate every transformation that would otherwise be attempted during
    /// commit, while preserving the same scope checks as a write operation.
    async fn validate_batch(
        &self,
        _context: &AccessContext,
        batch: &MigrationBatch,
    ) -> Result<(), MigrationError> {
        crate::migration::validate_batch(batch)
    }

    async fn commit_batch_atomically(
        &self,
        context: &AccessContext,
        batch: &MigrationBatch,
    ) -> Result<BatchCommit, MigrationError>;

    async fn checkpoint(
        &self,
        context: &AccessContext,
        run: &MigrationRun,
    ) -> Result<Option<MigrationCheckpoint>, MigrationError>;

    async fn reverse_manifest_atomically(
        &self,
        context: &AccessContext,
        request: &ReverseManifest,
    ) -> Result<ReverseReceipt, MigrationError>;

    /// Read-only advisory evaluation using the same authoritative eligibility
    /// rules as reverse. The reverse transaction must always recheck.
    async fn reverse_preflight(
        &self,
        context: &AccessContext,
        request: &ReversePreflightRequest,
    ) -> Result<ReversePreflight, MigrationError>;

    /// Record a reviewed, hash-bound corrected resubmission package. This
    /// operation never writes source records or removes quarantine evidence.
    async fn record_quarantine_disposition(
        &self,
        context: &AccessContext,
        request: &QuarantineDisposition,
        idempotency_hash: [u8; 32],
        request_fingerprint: [u8; 32],
    ) -> Result<QuarantineDispositionReceipt, MigrationError>;

    async fn create_package_v2(
        &self,
        _context: &AccessContext,
        _manifest_bytes: &[u8],
        _policy: &MigrationPackagePolicy,
    ) -> Result<MigrationPackageV2Receipt, MigrationError> {
        Err(MigrationError::PackageV2Disabled)
    }

    async fn stage_package_v2_batch(
        &self,
        _context: &AccessContext,
        _upload_id: &str,
        _sequence: u64,
        _exact_ndjson: &[u8],
        _policy: &MigrationPackagePolicy,
    ) -> Result<MigrationPackageV2Receipt, MigrationError> {
        Err(MigrationError::PackageV2Disabled)
    }

    async fn finalize_package_v2(
        &self,
        _context: &AccessContext,
        _upload_id: &str,
        _policy: &MigrationPackagePolicy,
    ) -> Result<MigrationPackageV2Receipt, MigrationError> {
        Err(MigrationError::PackageV2Disabled)
    }

    async fn inspect_package_v2(
        &self,
        _context: &AccessContext,
        _upload_id: &str,
    ) -> Result<MigrationPackageV2Receipt, MigrationError> {
        Err(MigrationError::PackageV2Disabled)
    }

    async fn apply_package_v2_batch(
        &self,
        _context: &AccessContext,
        _upload_id: &str,
    ) -> Result<MigrationPackageV2Receipt, MigrationError> {
        Err(MigrationError::PackageV2Disabled)
    }
}

pub struct MigrationService {
    executor: Box<dyn MigrationExecutor>,
}

impl MigrationService {
    pub fn new(executor: Box<dyn MigrationExecutor>) -> Self {
        Self { executor }
    }

    pub async fn validate_batch(
        &self,
        context: Option<&AccessContext>,
        batch: &MigrationBatch,
    ) -> Result<(), MigrationError> {
        validate_batch(batch)?;
        let scope = ResourceScope {
            registry_id: batch.run.registry_id.clone(),
            organization_id: batch.run.organization_id.clone(),
            record_id: None,
        };
        authorize(context, Permission::MigrationRead, &scope)?;
        let context = context.ok_or(MigrationError::AccessDenied(AccessDenied::Unauthenticated))?;
        self.executor.validate_batch(context, batch).await
    }

    pub async fn commit_batch(
        &self,
        context: Option<&AccessContext>,
        batch: MigrationBatch,
    ) -> Result<BatchCommit, MigrationError> {
        validate_batch(&batch)?;
        let scope = ResourceScope {
            registry_id: batch.run.registry_id.clone(),
            organization_id: batch.run.organization_id.clone(),
            record_id: None,
        };
        authorize(context, Permission::MigrationWrite, &scope)?;
        let context = context.ok_or(MigrationError::AccessDenied(AccessDenied::Unauthenticated))?;
        self.executor.commit_batch_atomically(context, &batch).await
    }

    pub async fn create_package_v2(
        &self,
        context: Option<&AccessContext>,
        manifest_bytes: &[u8],
        policy: &MigrationPackagePolicy,
    ) -> Result<MigrationPackageV2Receipt, MigrationError> {
        let manifest = package::parse_manifest(manifest_bytes)
            .map_err(|_| MigrationError::PackageVerificationFailed)?;
        let scope = ResourceScope {
            registry_id: manifest.run.registry_id.clone(),
            organization_id: manifest.run.organization_id.clone(),
            record_id: None,
        };
        authorize(context, Permission::MigrationWrite, &scope)?;
        let context = context.ok_or(MigrationError::AccessDenied(AccessDenied::Unauthenticated))?;
        self.executor
            .create_package_v2(context, manifest_bytes, policy)
            .await
    }

    pub async fn stage_package_v2_batch(
        &self,
        context: Option<&AccessContext>,
        upload_id: &str,
        sequence: u64,
        exact_ndjson: &[u8],
        policy: &MigrationPackagePolicy,
    ) -> Result<MigrationPackageV2Receipt, MigrationError> {
        if exact_ndjson.is_empty() || exact_ndjson.len() > MAX_API_BATCH_BYTES {
            return Err(MigrationError::InvalidBatch);
        }
        let context = context.ok_or(MigrationError::AccessDenied(AccessDenied::Unauthenticated))?;
        self.executor
            .stage_package_v2_batch(context, upload_id, sequence, exact_ndjson, policy)
            .await
    }

    pub async fn finalize_package_v2(
        &self,
        context: Option<&AccessContext>,
        upload_id: &str,
        policy: &MigrationPackagePolicy,
    ) -> Result<MigrationPackageV2Receipt, MigrationError> {
        let context = context.ok_or(MigrationError::AccessDenied(AccessDenied::Unauthenticated))?;
        self.executor
            .finalize_package_v2(context, upload_id, policy)
            .await
    }

    pub async fn inspect_package_v2(
        &self,
        context: Option<&AccessContext>,
        upload_id: &str,
    ) -> Result<MigrationPackageV2Receipt, MigrationError> {
        let context = context.ok_or(MigrationError::AccessDenied(AccessDenied::Unauthenticated))?;
        self.executor.inspect_package_v2(context, upload_id).await
    }

    pub async fn apply_package_v2_batch(
        &self,
        context: Option<&AccessContext>,
        upload_id: &str,
    ) -> Result<MigrationPackageV2Receipt, MigrationError> {
        let context = context.ok_or(MigrationError::AccessDenied(AccessDenied::Unauthenticated))?;
        self.executor
            .apply_package_v2_batch(context, upload_id)
            .await
    }

    pub async fn checkpoint(
        &self,
        context: Option<&AccessContext>,
        run: &MigrationRun,
    ) -> Result<Option<MigrationCheckpoint>, MigrationError> {
        let scope = ResourceScope {
            registry_id: run.registry_id.clone(),
            organization_id: run.organization_id.clone(),
            record_id: None,
        };
        authorize(context, Permission::MigrationRead, &scope)?;
        let context = context.ok_or(MigrationError::AccessDenied(AccessDenied::Unauthenticated))?;
        self.executor.checkpoint(context, run).await
    }

    pub async fn reverse_manifest(
        &self,
        context: Option<&AccessContext>,
        request: ReverseManifest,
    ) -> Result<ReverseReceipt, MigrationError> {
        if request.registry_id.trim().is_empty()
            || request.organization_id.trim().is_empty()
            || request.reason.trim().is_empty()
            || request.reason.len() > 2000
        {
            return Err(MigrationError::InvalidBatch);
        }
        let scope = ResourceScope {
            registry_id: request.registry_id.clone(),
            organization_id: request.organization_id.clone(),
            record_id: None,
        };
        authorize(context, Permission::MigrationWrite, &scope)?;
        let context = context.ok_or(MigrationError::AccessDenied(AccessDenied::Unauthenticated))?;
        self.executor
            .reverse_manifest_atomically(context, &request)
            .await
    }

    pub async fn reverse_preflight(
        &self,
        context: Option<&AccessContext>,
        request: &ReversePreflightRequest,
    ) -> Result<ReversePreflight, MigrationError> {
        if request.registry_id.trim().is_empty() || request.organization_id.trim().is_empty() {
            return Err(MigrationError::InvalidBatch);
        }
        let scope = ResourceScope {
            registry_id: request.registry_id.clone(),
            organization_id: request.organization_id.clone(),
            record_id: None,
        };
        authorize(context, Permission::MigrationRead, &scope)?;
        let context = context.ok_or(MigrationError::AccessDenied(AccessDenied::Unauthenticated))?;
        self.executor.reverse_preflight(context, request).await
    }

    pub async fn record_quarantine_disposition(
        &self,
        context: Option<&AccessContext>,
        request: QuarantineDisposition,
    ) -> Result<QuarantineDispositionReceipt, MigrationError> {
        if request.replacement_manifest_sha256 == Some(request.parent_manifest_sha256)
            || request.registry_id.trim().is_empty()
            || request.organization_id.trim().is_empty()
            || request.reason.trim().is_empty()
            || request.reason.len() > 2000
            || request.reason.chars().any(char::is_control)
            || request.idempotency_key.trim().is_empty()
            || request.idempotency_key.len() > 256
            || request.idempotency_key.chars().any(char::is_control)
            || request.corrections.is_empty()
            || request.corrections.len() > 1000
        {
            return Err(MigrationError::InvalidDisposition);
        }
        if (request.action == QuarantineDispositionAction::Deferred)
            != request.replacement_manifest_sha256.is_none()
        {
            return Err(MigrationError::InvalidDisposition);
        }
        let mut previous_line = 0;
        for correction in &request.corrections {
            if correction.input_line == 0
                || correction.input_line <= previous_line
                || correction.quarantine_code.is_empty()
                || correction.quarantine_code.len() > 64
                || !correction
                    .quarantine_code
                    .bytes()
                    .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
                || !action_matches_code(request.action, correction)
                || (request.action == QuarantineDispositionAction::DuplicateAcknowledged
                    && correction
                        .duplicate_of_line
                        .is_none_or(|line| line == 0 || line >= correction.input_line))
                || (request.action == QuarantineDispositionAction::CorrectedResubmission
                    && correction.quarantine_code == "EXACT_REPLAY_DUPLICATE"
                    && correction
                        .duplicate_of_line
                        .is_none_or(|line| line == 0 || line >= correction.input_line))
                || (request.action == QuarantineDispositionAction::CorrectedResubmission
                    && correction.quarantine_code != "EXACT_REPLAY_DUPLICATE"
                    && correction.duplicate_of_line.is_some())
                || (request.action == QuarantineDispositionAction::Deferred
                    && correction.duplicate_of_line.is_some())
            {
                return Err(MigrationError::InvalidDisposition);
            }
            previous_line = correction.input_line;
        }
        let scope = ResourceScope {
            registry_id: request.registry_id.clone(),
            organization_id: request.organization_id.clone(),
            record_id: None,
        };
        authorize(context, Permission::MigrationWrite, &scope)?;
        let context = context.ok_or(MigrationError::AccessDenied(AccessDenied::Unauthenticated))?;
        let idempotency_hash = sha256(request.idempotency_key.as_bytes());
        let mut fingerprint = Sha256::new();
        fingerprint.update(request.parent_manifest_sha256);
        match request.replacement_manifest_sha256 {
            Some(digest) => {
                fingerprint.update([1]);
                fingerprint.update(digest);
            }
            None => fingerprint.update([0]),
        }
        fingerprint.update([match request.action {
            QuarantineDispositionAction::CorrectedResubmission => 1,
            QuarantineDispositionAction::Deferred => 2,
            QuarantineDispositionAction::DuplicateAcknowledged => 3,
        }]);
        hash_part(&mut fingerprint, request.registry_id.as_bytes());
        hash_part(&mut fingerprint, request.organization_id.as_bytes());
        hash_part(&mut fingerprint, request.reason.as_bytes());
        for correction in &request.corrections {
            fingerprint.update(correction.input_line.to_be_bytes());
            match correction.duplicate_of_line {
                Some(line) => {
                    fingerprint.update([1]);
                    fingerprint.update(line.to_be_bytes());
                }
                None => fingerprint.update([0]),
            }
            hash_part(&mut fingerprint, correction.quarantine_code.as_bytes());
            fingerprint.update(correction.original_payload_sha256);
            match correction.corrected_payload_sha256 {
                Some(digest) => {
                    fingerprint.update([1]);
                    fingerprint.update(digest);
                }
                None => fingerprint.update([0]),
            }
        }
        let request_fingerprint: [u8; 32] = fingerprint.finalize().into();
        self.executor
            .record_quarantine_disposition(context, &request, idempotency_hash, request_fingerprint)
            .await
    }
}

fn action_matches_code(
    action: QuarantineDispositionAction,
    correction: &QuarantineCorrection,
) -> bool {
    let code = correction.quarantine_code.as_str();
    match action {
        QuarantineDispositionAction::CorrectedResubmission => {
            (matches!(
                code,
                "ROW_INVALID_UTF8_OR_JSON"
                    | "ROW_MISSING_LINE_FEED"
                    | "ROW_TOO_LARGE"
                    | "NON_CANONICAL_NUMBER"
            ) && correction
                .corrected_payload_sha256
                .is_some_and(|digest| digest != correction.original_payload_sha256)
                && correction.duplicate_of_line.is_none())
                || (code == "EXACT_REPLAY_DUPLICATE"
                    && correction.duplicate_of_line.is_some()
                    && correction.corrected_payload_sha256
                        == Some(correction.original_payload_sha256))
        }
        QuarantineDispositionAction::Deferred => {
            correction.corrected_payload_sha256.is_none() && correction.duplicate_of_line.is_none()
        }
        QuarantineDispositionAction::DuplicateAcknowledged => {
            code == "EXACT_REPLAY_DUPLICATE"
                && correction.corrected_payload_sha256 == Some(correction.original_payload_sha256)
        }
    }
}

fn hash_part(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

pub fn validate_batch(batch: &MigrationBatch) -> Result<(), MigrationError> {
    if batch.run.snapshot_id.trim().is_empty()
        || batch.run.source_system_id.trim().is_empty()
        || batch.run.registry_id.trim().is_empty()
        || batch.run.organization_id.trim().is_empty()
        || batch.sequence == 0
        || batch.first_input_line == 0
        || batch.last_input_line < batch.first_input_line
        || batch.records.is_empty()
        || batch.records.len() > MAX_BATCH_RECORDS
        || batch.exact_bytes.len() > MAX_BATCH_BYTES
        || batch.sequence > i64::MAX as u64
    {
        return Err(MigrationError::InvalidBatch);
    }
    if sha256(&batch.exact_bytes) != batch.batch_sha256 {
        return Err(MigrationError::ChecksumMismatch);
    }
    let mut keys = BTreeSet::new();
    let mut concatenated = Vec::new();
    for record in &batch.records {
        if record.raw_ndjson.len() > MAX_RECORD_BYTES || !record.raw_ndjson.ends_with(b"\n") {
            return Err(MigrationError::RecordTooLarge);
        }
        if record.source_system_id != batch.run.source_system_id
            || record.source_record_key.trim().is_empty()
            || record.source_version.trim().is_empty()
        {
            return Err(MigrationError::InvalidBatch);
        }
        if !keys.insert((
            record.source_system_id.as_str(),
            record.source_record_key.as_str(),
            record.source_version.as_str(),
        )) {
            return Err(MigrationError::DuplicateRecord);
        }
        let row_bytes = record
            .raw_ndjson
            .strip_suffix(b"\n")
            .unwrap_or(&record.raw_ndjson);
        let row: serde_json::Value =
            serde_json::from_slice(row_bytes).map_err(|_| MigrationError::InvalidBatch)?;
        if !canonical_numbers_supported(&row) {
            return Err(MigrationError::InvalidBatch);
        }
        let object = row.as_object().ok_or(MigrationError::InvalidBatch)?;
        if object
            .get("source_system_id")
            .and_then(serde_json::Value::as_str)
            != Some(record.source_system_id.as_str())
            || object
                .get("source_record_key")
                .and_then(serde_json::Value::as_str)
                != Some(record.source_record_key.as_str())
            || object
                .get("source_version")
                .and_then(serde_json::Value::as_str)
                != Some(record.source_version.as_str())
        {
            return Err(MigrationError::InvalidBatch);
        }
        let canonical = serde_json::to_vec(&row).map_err(|_| MigrationError::InvalidBatch)?;
        if sha256(&canonical) != record.payload_sha256 {
            return Err(MigrationError::ChecksumMismatch);
        }
        concatenated.extend_from_slice(&record.raw_ndjson);
    }
    if concatenated != batch.exact_bytes {
        return Err(MigrationError::ChecksumMismatch);
    }
    Ok(())
}

/// Reverse replay is a compensating operation. Any changed source version,
/// later identity dependency, missing row, or unrepresentable relationship
/// blocks the whole planned replay before mutation begins.
pub fn check_reverse_replay(
    journal: &[AppliedSource],
    current: &[CurrentSourceState],
) -> ReverseReplayCheck {
    let current_by_key = current
        .iter()
        .map(|row| {
            (
                (
                    row.source_system_id.as_str(),
                    row.source_record_key.as_str(),
                ),
                row,
            )
        })
        .collect::<std::collections::HashMap<_, _>>();
    let mut refusals = Vec::new();
    for imported in journal {
        if imported.expected_current_receive_version != imported.receive_version {
            refusals.push(ReverseRefusal {
                source_system_id: imported.source_system_id.clone(),
                source_record_key: imported.source_record_key.clone(),
                reason: ReverseRefusalReason::ImportedVersionNotCurrent,
            });
        }
        let key = (
            imported.source_system_id.as_str(),
            imported.source_record_key.as_str(),
        );
        match current_by_key.get(&key) {
            None => refusals.push(ReverseRefusal {
                source_system_id: imported.source_system_id.clone(),
                source_record_key: imported.source_record_key.clone(),
                reason: ReverseRefusalReason::MissingCurrentRecord,
            }),
            Some(now) => {
                if now.receive_version != imported.expected_current_receive_version
                    || now.payload_sha256 != imported.expected_current_sha256
                {
                    refusals.push(ReverseRefusal {
                        source_system_id: imported.source_system_id.clone(),
                        source_record_key: imported.source_record_key.clone(),
                        reason: ReverseRefusalReason::LaterSourceChange,
                    });
                }
                if now.has_later_identity_dependency {
                    refusals.push(ReverseRefusal {
                        source_system_id: imported.source_system_id.clone(),
                        source_record_key: imported.source_record_key.clone(),
                        reason: ReverseRefusalReason::LaterIdentityDependency,
                    });
                }
                if !now.relationship_representable {
                    refusals.push(ReverseRefusal {
                        source_system_id: imported.source_system_id.clone(),
                        source_record_key: imported.source_record_key.clone(),
                        reason: ReverseRefusalReason::RelationshipNotRepresentable,
                    });
                }
            }
        }
    }
    ReverseReplayCheck {
        representable: refusals.is_empty(),
        refusals,
    }
}

fn sha256(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn canonical_numbers_supported(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Number(number) => number
            .as_i64()
            .map(|n| n.unsigned_abs() < (1_u64 << 53))
            .or_else(|| number.as_u64().map(|n| n < (1_u64 << 53)))
            .unwrap_or(false),
        serde_json::Value::Array(values) => values.iter().all(canonical_numbers_supported),
        serde_json::Value::Object(values) => values.values().all(canonical_numbers_supported),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::access::{ActorKind, ActorRef, Role, ScopeGrant};
    use std::collections::BTreeSet;

    fn sample_batch(records: Vec<MigrationRecord>) -> MigrationBatch {
        let exact_bytes = records
            .iter()
            .flat_map(|record| record.raw_ndjson.clone())
            .collect::<Vec<_>>();
        MigrationBatch {
            run: MigrationRun {
                manifest_sha256: [1; 32],
                stream_sha256: [2; 32],
                snapshot_id: "synthetic-snapshot".into(),
                source_system_id: "synthetic-system".into(),
                registry_id: "synthetic-registry".into(),
                organization_id: "synthetic-organization".into(),
                expected_record_count: records.len() as u64,
            },
            sequence: 1,
            first_input_line: 1,
            last_input_line: records.len() as u64,
            batch_sha256: sha256(&exact_bytes),
            exact_bytes,
            records,
        }
    }

    fn record(key: &str, version: &str) -> MigrationRecord {
        let raw_ndjson = format!("{{\"source_system_id\":\"synthetic-system\",\"source_record_key\":\"{key}\",\"source_version\":\"{version}\",\"synthetic\":true}}\n").into_bytes();
        let parsed: serde_json::Value =
            serde_json::from_slice(&raw_ndjson).expect("synthetic JSON fixture is valid");
        let canonical = serde_json::to_vec(&parsed).expect("canonical JSON serialization succeeds");
        MigrationRecord {
            source_system_id: "synthetic-system".into(),
            source_record_key: key.into(),
            source_version: version.into(),
            payload_sha256: sha256(&canonical),
            raw_ndjson,
        }
    }

    #[test]
    fn batch_requires_exact_checksum_scope_and_unique_source_version() {
        assert!(validate_batch(&sample_batch(vec![record("r1", "1")])).is_ok());

        let mut wrong_hash = sample_batch(vec![record("r1", "1")]);
        wrong_hash.batch_sha256 = [9; 32];
        assert_eq!(
            validate_batch(&wrong_hash),
            Err(MigrationError::ChecksumMismatch)
        );

        let duplicate = sample_batch(vec![record("r1", "1"), record("r1", "1")]);
        assert_eq!(
            validate_batch(&duplicate),
            Err(MigrationError::DuplicateRecord)
        );

        let mut wrong_system = sample_batch(vec![record("r1", "1")]);
        wrong_system.records[0].source_system_id = "other-system".into();
        assert_eq!(
            validate_batch(&wrong_system),
            Err(MigrationError::InvalidBatch)
        );

        let mut wrong_row_hash = sample_batch(vec![record("r1", "1")]);
        wrong_row_hash.records[0].payload_sha256 = [9; 32];
        assert_eq!(
            validate_batch(&wrong_row_hash),
            Err(MigrationError::ChecksumMismatch)
        );
    }

    #[test]
    fn reverse_replay_is_a_whole_set_precheck_and_blocks_later_writes() {
        let source = AppliedSource {
            source_system_id: "synthetic-system".into(),
            source_record_key: "r1".into(),
            source_version: "1".into(),
            payload_sha256: [1; 32],
            public_id: "CRUID-synthetic-1".into(),
            source_record_id: "synthetic-source-record".into(),
            receive_version: 2,
            expected_current_sha256: [2; 32],
            expected_current_receive_version: 2,
            source_record_created: true,
            person_created: true,
            previous_receive_version: None,
            introduced_alias_ids: Vec::new(),
            introduced_identifier_ids: Vec::new(),
        };
        let unchanged = CurrentSourceState {
            source_system_id: source.source_system_id.clone(),
            source_record_key: source.source_record_key.clone(),
            payload_sha256: source.expected_current_sha256,
            receive_version: source.expected_current_receive_version,
            has_later_identity_dependency: false,
            relationship_representable: true,
        };
        assert!(
            check_reverse_replay(
                std::slice::from_ref(&source),
                std::slice::from_ref(&unchanged)
            )
            .representable
        );

        let changed = CurrentSourceState {
            payload_sha256: [3; 32],
            receive_version: 3,
            has_later_identity_dependency: true,
            relationship_representable: false,
            ..unchanged
        };
        let check = check_reverse_replay(&[source], &[changed]);
        assert!(!check.representable);
        assert!(
            check
                .refusals
                .iter()
                .any(|r| r.reason == ReverseRefusalReason::LaterSourceChange)
        );
        assert!(
            check
                .refusals
                .iter()
                .any(|r| r.reason == ReverseRefusalReason::LaterIdentityDependency)
        );
        assert!(
            check
                .refusals
                .iter()
                .any(|r| r.reason == ReverseRefusalReason::RelationshipNotRepresentable)
        );
    }

    #[test]
    fn migration_requires_its_explicit_permission_and_keeps_registry_scope() {
        let registrar = AccessContext::from_authorized_identity(
            ActorRef {
                kind: ActorKind::Human,
                id: "synthetic-operator".into(),
                issuer_url: Some("https://idp.invalid".into()),
            },
            vec![ScopeGrant {
                registry_id: "synthetic-registry".into(),
                organization_id: "synthetic-organization".into(),
                record_ids: None,
                roles: BTreeSet::from([Role::Registrar]),
                permissions: BTreeSet::new(),
            }],
            "synthetic-request".into(),
            None,
            None,
        )
        .unwrap();
        let scope = ResourceScope {
            registry_id: "synthetic-registry".into(),
            organization_id: "synthetic-organization".into(),
            record_id: None,
        };
        assert!(authorize(Some(&registrar), Permission::SourceWrite, &scope).is_ok());
        assert_eq!(
            authorize(Some(&registrar), Permission::MigrationWrite, &scope),
            Err(AccessDenied::PermissionMissing)
        );
        let migration_operator = AccessContext::from_authorized_identity(
            ActorRef {
                kind: ActorKind::Human,
                id: "synthetic-migration-operator".into(),
                issuer_url: Some("https://idp.invalid".into()),
            },
            vec![ScopeGrant {
                registry_id: "synthetic-registry".into(),
                organization_id: "synthetic-organization".into(),
                record_ids: None,
                roles: BTreeSet::new(),
                permissions: BTreeSet::from([Permission::MigrationWrite]),
            }],
            "synthetic-migration-request".into(),
            None,
            None,
        )
        .unwrap();
        assert!(
            authorize(
                Some(&migration_operator),
                Permission::MigrationWrite,
                &scope
            )
            .is_ok()
        );
        assert_eq!(
            authorize(
                Some(&migration_operator),
                Permission::MigrationWrite,
                &ResourceScope {
                    organization_id: "other-organization".into(),
                    ..scope.clone()
                }
            ),
            Err(AccessDenied::OutOfScope)
        );
        assert_eq!(
            authorize(None, Permission::MigrationWrite, &scope),
            Err(AccessDenied::Unauthenticated)
        );
    }

    #[test]
    fn canonical_utf8_payload_digest_matches_offline_stager_fixture() {
        let row = serde_json::json!({
            "source_system_id": "synthetic-his",
            "source_record_key": "src-1",
            "source_version": "1",
            "public_id": "CRUID-synthetic-1",
            "cruid": "CRUID-synthetic-1",
            "assertions": [
                {"field_path":"name.original","original_value":"ຄຳ ຕົວຢ່າງ","language_tag":"lo","script":"Lao"},
                {"field_path":"birthDate","original_value":"1984-06","partial_date_precision":"month"}
            ],
            "identifiers": [{"authority":"urn:synthetic:clinic","type":"MRN","value":"0007","authoritative":true}],
            "relationships": []
        });
        let canonical = serde_json::to_vec(&row).unwrap();
        assert_eq!(
            sha256(&canonical),
            [
                0x28, 0xa6, 0xcc, 0x7d, 0x50, 0xdd, 0x54, 0x15, 0x8b, 0xb5, 0xf1, 0x93, 0x05, 0x49,
                0x6b, 0xe3, 0x76, 0x47, 0x67, 0x20, 0x08, 0xbb, 0xeb, 0xa1, 0x3c, 0xd7, 0x46, 0xc5,
                0xd2, 0xce, 0xb9, 0xb1,
            ]
        );
    }
}
