//! Read-only inspection and classification of legacy governance JSONL.
//!
//! This module deliberately has no daemon or filesystem-state owner.  It reads
//! bytes supplied by the caller, keeps each source line verbatim, and returns a
//! deterministic report.  A later migration adapter can use the report to
//! archive, adapt, reset, or stop for operator input without replaying a
//! source journal while it is being inspected.

#[allow(unused_imports)]
pub use crate::server::global_state::{
    MigrationIdentityRebindReceipt, MigrationReceiptSet, MigrationRuntimeRebindReceipt,
    MigrationWriterReceipt,
};
use crate::server::state::Event;
use serde::de::{DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The disposition of a source record in a future migration transaction.
#[derive(Debug, Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum MappingClass {
    /// The record has enough immutable identity and lifecycle evidence to be
    /// referenced directly.  Direct does not authorize active replay.
    Direct,
    /// The record is useful historical input but needs a schema or semantic
    /// conversion before it can be referenced by the new epoch.
    Adapt,
    /// The record describes state that must be re-bound in a fresh epoch.
    Reset,
    /// The record cannot be safely interpreted without preserving the exact
    /// failure and obtaining an operator decision.  Scope mismatches use this
    /// class because the source may belong to another project and cannot be
    /// safely rebound by this migration.
    Unknown,
}

/// Controller action for one inspected source.  This is intentionally
/// separate from `MappingClass`: the classifier describes what was observed,
/// while the controller decides whether the source may be replayed, adapted,
/// preserved only as evidence, or rebuilt in a new epoch.
#[derive(Debug, Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceDisposition {
    DirectReplay,
    AdaptReconcile,
    ArchiveOnly,
    RebuildRequired,
}

impl SourceDisposition {
    pub fn from_mapping_class(classification: MappingClass) -> Self {
        match classification {
            MappingClass::Direct => Self::DirectReplay,
            MappingClass::Adapt => Self::AdaptReconcile,
            MappingClass::Reset => Self::RebuildRequired,
            MappingClass::Unknown => Self::ArchiveOnly,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DirectReplay => "direct_replay",
            Self::AdaptReconcile => "adapt_reconcile",
            Self::ArchiveOnly => "archive_only",
            Self::RebuildRequired => "rebuild_required",
        }
    }
}

/// Project admission is a project-level gate and must not be inferred from a
/// single source record or confused with the migration transaction phase.
#[derive(Debug, Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectAdmission {
    Verified,
    ResetRequired,
    NeedsOperator,
    Aborted,
}

impl ProjectAdmission {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
            Self::ResetRequired => "reset_required",
            Self::NeedsOperator => "needs_operator",
            Self::Aborted => "aborted",
        }
    }
}

/// Durable lifecycle states for the migration transaction boundary.
///
/// These values describe the transaction owned by this module.  They are
/// deliberately separate from the legacy `MigrationRecord::phase` string so
/// that an old `verified` row cannot be mistaken for a verified target epoch.
#[derive(Debug, Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MigrationPhase {
    Planned,
    SnapshotCaptured,
    ReplayVerified,
    Rebound,
    Applied,
    Verified,
    ResetRequired,
    NeedsOperator,
    Aborted,
}

impl MigrationPhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::SnapshotCaptured => "snapshot_captured",
            Self::ReplayVerified => "replay_verified",
            Self::Rebound => "rebound",
            Self::Applied => "applied",
            Self::Verified => "verified",
            Self::ResetRequired => "reset_required",
            Self::NeedsOperator => "needs_operator",
            Self::Aborted => "aborted",
        }
    }
}

/// Typed failures at the migration transaction boundary.
///
/// The variants intentionally retain the first failed field/boundary.  A
/// caller can expose the error to an operator without converting an unknown
/// or reset-required source into a successful apply.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum MigrationContractError {
    Missing(&'static str),
    Invalid {
        field: &'static str,
        reason: String,
    },
    DigestMismatch {
        field: &'static str,
        expected: String,
        observed: String,
    },
    EpochMismatch {
        field: &'static str,
        expected: u64,
        observed: u64,
    },
    RevisionMismatch {
        field: &'static str,
        expected: u64,
        observed: u64,
    },
    AdmissionBlocked {
        admission: ProjectAdmission,
    },
    PrefixBlocked {
        line: Option<usize>,
        exact_error: String,
    },
    ReceiptRejected {
        reason: String,
    },
}

impl MigrationContractError {
    fn missing(field: &'static str) -> Self {
        Self::Missing(field)
    }

    fn invalid(field: &'static str, reason: impl Into<String>) -> Self {
        Self::Invalid {
            field,
            reason: reason.into(),
        }
    }

    fn digest_mismatch(
        field: &'static str,
        expected: impl Into<String>,
        observed: impl Into<String>,
    ) -> Self {
        Self::DigestMismatch {
            field,
            expected: expected.into(),
            observed: observed.into(),
        }
    }
}

impl fmt::Display for MigrationContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing(field) => write!(f, "MIGRATION_CONTRACT_MISSING:{field}"),
            Self::Invalid { field, reason } => {
                write!(f, "MIGRATION_CONTRACT_INVALID:{field}:{reason}")
            }
            Self::DigestMismatch {
                field,
                expected,
                observed,
            } => write!(
                f,
                "MIGRATION_DIGEST_MISMATCH:{field}:expected={expected}:observed={observed}"
            ),
            Self::EpochMismatch {
                field,
                expected,
                observed,
            } => write!(
                f,
                "MIGRATION_EPOCH_MISMATCH:{field}:expected={expected}:observed={observed}"
            ),
            Self::RevisionMismatch {
                field,
                expected,
                observed,
            } => write!(
                f,
                "MIGRATION_REVISION_MISMATCH:{field}:expected={expected}:observed={observed}"
            ),
            Self::AdmissionBlocked { admission } => {
                write!(f, "MIGRATION_APPLY_REJECTED:{}", admission.as_str())
            }
            Self::PrefixBlocked { line, exact_error } => {
                write!(f, "MIGRATION_PREFIX_BLOCKED:line={line:?}:{exact_error}")
            }
            Self::ReceiptRejected { reason } => {
                write!(f, "MIGRATION_RECEIPT_REJECTED:{reason}")
            }
        }
    }
}

impl Error for MigrationContractError {}

/// An immutable source/archive snapshot receipt.
///
/// This value does not write an archive.  It binds the source digest and the
/// archive digest to the same migration, while allowing the archive to contain
/// the source stream plus the immutable migration evidence collected around
/// it.  The owner that creates the archive must separately prove that both
/// referenced byte streams are immutable.  `verify_source_digest` is the
/// read-side check used before any target operation.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImmutableSnapshot {
    pub migration_id: String,
    pub source_project_id: String,
    pub source_epoch: Option<u64>,
    pub source_digest: String,
    pub archive_ref: String,
    pub archive_digest: String,
    pub captured_revision: u64,
}

impl ImmutableSnapshot {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        migration_id: impl Into<String>,
        source_project_id: impl Into<String>,
        source_digest: impl Into<String>,
        archive_ref: impl Into<String>,
        archive_digest: impl Into<String>,
        captured_revision: u64,
    ) -> Result<Self, MigrationContractError> {
        let snapshot = Self {
            migration_id: migration_id.into(),
            source_project_id: source_project_id.into(),
            source_epoch: None,
            source_digest: source_digest.into(),
            archive_ref: archive_ref.into(),
            archive_digest: archive_digest.into(),
            captured_revision,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub fn with_source_epoch(mut self, source_epoch: Option<u64>) -> Self {
        self.source_epoch = source_epoch;
        self
    }

    pub fn validate(&self) -> Result<(), MigrationContractError> {
        validate_contract_identifier("migration_id", &self.migration_id)?;
        validate_contract_identifier("source_project_id", &self.source_project_id)?;
        validate_contract_identifier("source_digest", &self.source_digest)?;
        validate_contract_reference("archive_ref", &self.archive_ref)?;
        validate_contract_identifier("archive_digest", &self.archive_digest)?;
        if let Some(source_epoch) = self.source_epoch {
            if source_epoch == 0 {
                return Err(MigrationContractError::invalid(
                    "source_epoch",
                    "must be non-zero when present",
                ));
            }
        }
        Ok(())
    }

    pub fn verify_source_digest(&self, observed: &str) -> Result<(), MigrationContractError> {
        self.validate()?;
        if self.source_digest == observed {
            Ok(())
        } else {
            Err(MigrationContractError::digest_mismatch(
                "source_digest",
                self.source_digest.clone(),
                observed,
            ))
        }
    }
}

/// Compatibility name used by migration manifests and archive owners.
pub type SnapshotReceipt = ImmutableSnapshot;

/// Target epoch allocation and the source revision it is fenced against.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetEpoch {
    pub source_epoch: Option<u64>,
    pub target_epoch: u64,
    pub expected_active_revision: u64,
}

impl TargetEpoch {
    pub fn new(
        source_epoch: Option<u64>,
        target_epoch: u64,
        expected_active_revision: u64,
    ) -> Result<Self, MigrationContractError> {
        let epoch = Self {
            source_epoch,
            target_epoch,
            expected_active_revision,
        };
        epoch.validate()?;
        Ok(epoch)
    }

    pub fn validate(&self) -> Result<(), MigrationContractError> {
        if let Some(source_epoch) = self.source_epoch {
            if source_epoch == 0 {
                return Err(MigrationContractError::invalid(
                    "source_epoch",
                    "must be non-zero when present",
                ));
            }
        }
        if self.target_epoch == 0 {
            return Err(MigrationContractError::invalid(
                "target_epoch",
                "must be non-zero",
            ));
        }
        if self.source_epoch == Some(self.target_epoch) {
            return Err(MigrationContractError::invalid(
                "source_epoch",
                "must differ from target_epoch",
            ));
        }
        Ok(())
    }

    pub fn validate_against(
        &self,
        active_epoch: u64,
        active_revision: u64,
    ) -> Result<(), MigrationContractError> {
        self.validate()?;
        if let Some(source_epoch) = self.source_epoch {
            if source_epoch != active_epoch {
                return Err(MigrationContractError::EpochMismatch {
                    field: "source_epoch",
                    expected: source_epoch,
                    observed: active_epoch,
                });
            }
        }
        if self.target_epoch <= active_epoch {
            return Err(MigrationContractError::invalid(
                "target_epoch",
                format!(
                    "must be greater than active_epoch {active_epoch}, observed {}",
                    self.target_epoch
                ),
            ));
        }
        if self.expected_active_revision != active_revision {
            return Err(MigrationContractError::RevisionMismatch {
                field: "expected_active_revision",
                expected: self.expected_active_revision,
                observed: active_revision,
            });
        }
        Ok(())
    }

    /// Validate a transaction after its target epoch is active.  Allocation
    /// uses [`Self::validate_against`], which requires a strictly newer target;
    /// apply admission observes the committed target and therefore requires
    /// equality with the active epoch.
    pub fn validate_committed_against(
        &self,
        active_epoch: u64,
        active_revision: u64,
    ) -> Result<(), MigrationContractError> {
        self.validate()?;
        if self.target_epoch != active_epoch {
            return Err(MigrationContractError::EpochMismatch {
                field: "target_epoch",
                expected: active_epoch,
                observed: self.target_epoch,
            });
        }
        if self.expected_active_revision != active_revision {
            return Err(MigrationContractError::RevisionMismatch {
                field: "expected_active_revision",
                expected: self.expected_active_revision,
                observed: active_revision,
            });
        }
        Ok(())
    }
}

/// Compatibility name used by callers that call the epoch allocation an
/// epoch descriptor.
pub type EpochDescriptor = TargetEpoch;

/// One source record in a verified replay prefix.  Raw bytes remain owned by
/// the source/archive; only stable identity and digest evidence is carried in
/// the replay receipt.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedPrefixRecord {
    pub line_number: usize,
    pub record_id: String,
    pub sequence: u64,
    pub digest: String,
}

impl VerifiedPrefixRecord {
    fn validate(&self) -> Result<(), MigrationContractError> {
        if self.line_number == 0 {
            return Err(MigrationContractError::invalid(
                "verified_prefix.line_number",
                "must be non-zero",
            ));
        }
        if self.sequence == 0 {
            return Err(MigrationContractError::invalid(
                "verified_prefix.sequence",
                "must be non-zero",
            ));
        }
        validate_contract_identifier("verified_prefix.record_id", &self.record_id)?;
        validate_contract_identifier("verified_prefix.digest", &self.digest)
    }
}

/// Evidence for the only source records eligible for direct replay.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedPrefix {
    pub source_digest: String,
    pub prefix_digest: String,
    pub records: Vec<VerifiedPrefixRecord>,
    pub complete: bool,
    pub stop_line: Option<usize>,
    pub stop_error: Option<String>,
    /// The exact source bytes covered by `prefix_digest`.  This is an
    /// in-memory verification witness: it is deliberately omitted from the
    /// serialized contract so a deserialized prefix must be rebound through
    /// `verify_against_report` or `verify_against_bytes` before apply.
    #[serde(skip)]
    raw_prefix_bytes: Option<Vec<u8>>,
}

impl VerifiedPrefix {
    /// Derive a prefix from the existing read-only classifier.  The method
    /// stops at the first record that is not direct and retains its exact
    /// error.  It never changes the report or attempts replay.
    pub fn from_report(report: &InspectionReport) -> Result<Self, MigrationContractError> {
        let mut records = Vec::new();
        let mut prefix_bytes = Vec::new();
        let mut expected_sequence = None;
        let mut stop_line = None;
        let mut stop_error = None;

        for record in &report.records {
            let Some(record_id) = record.record_id.as_ref() else {
                stop_line = Some(record.line_number);
                stop_error = record.exact_error.clone();
                break;
            };
            let Some(sequence) = record.sequence else {
                stop_line = Some(record.line_number);
                stop_error = record.exact_error.clone();
                break;
            };
            if sequence == 0 {
                stop_line = Some(record.line_number);
                stop_error = Some("INVALID_SEQUENCE:0".to_owned());
                break;
            }
            if record.classification != MappingClass::Direct || record.exact_error.is_some() {
                stop_line = Some(record.line_number);
                stop_error = record
                    .exact_error
                    .clone()
                    .or_else(|| Some("RECORD_NOT_DIRECT".to_owned()));
                break;
            }
            if let Some(expected) = expected_sequence {
                if sequence != expected {
                    stop_line = Some(record.line_number);
                    stop_error = Some(if sequence > expected {
                        format!("SEQUENCE_GAP:expected={expected}:observed={sequence}")
                    } else {
                        format!("SEQUENCE_NON_MONOTONIC:expected={expected}:observed={sequence}")
                    });
                    break;
                }
            }
            expected_sequence = sequence.checked_add(1);
            if expected_sequence.is_none() {
                stop_line = Some(record.line_number);
                stop_error = Some("SEQUENCE_OVERFLOW".to_owned());
                break;
            }
            records.push(VerifiedPrefixRecord {
                line_number: record.line_number,
                record_id: record_id.clone(),
                sequence,
                digest: record.digest.clone(),
            });
            prefix_bytes.extend_from_slice(&record.raw_bytes);
        }

        if stop_line.is_none() {
            if let Some(issue) = report.issues.first() {
                stop_line = issue.line_number;
                stop_error = Some(issue.exact_error.clone());
            }
        }

        let prefix = Self {
            source_digest: report.source_digest.clone(),
            prefix_digest: digest_bytes(&prefix_bytes),
            complete: stop_line.is_none() && stop_error.is_none() && report.issues.is_empty(),
            records,
            stop_line,
            stop_error,
            raw_prefix_bytes: Some(prefix_bytes),
        };
        prefix.validate()?;
        Ok(prefix)
    }

    pub fn validate(&self) -> Result<(), MigrationContractError> {
        validate_contract_identifier("verified_prefix.source_digest", &self.source_digest)?;
        validate_contract_identifier("verified_prefix.prefix_digest", &self.prefix_digest)?;
        if self.records.is_empty() {
            if self.complete {
                return Err(MigrationContractError::missing("verified_prefix.records"));
            }
            if self.stop_error.is_none() {
                return Err(MigrationContractError::invalid(
                    "verified_prefix.stop",
                    "an incomplete empty prefix must retain the exact stop error",
                ));
            }
        }
        let mut expected_sequence = None;
        let mut previous_line = None;
        for record in &self.records {
            record.validate()?;
            if let Some(expected) = expected_sequence {
                if record.sequence != expected {
                    return Err(MigrationContractError::invalid(
                        "verified_prefix.records",
                        format!(
                            "sequence is not contiguous: expected {expected}, observed {}",
                            record.sequence
                        ),
                    ));
                }
            }
            if let Some(previous) = previous_line {
                if record.line_number <= previous {
                    return Err(MigrationContractError::invalid(
                        "verified_prefix.records",
                        format!(
                            "line numbers must increase: previous {previous}, observed {}",
                            record.line_number
                        ),
                    ));
                }
            }
            previous_line = Some(record.line_number);
            expected_sequence = Some(record.sequence.checked_add(1).ok_or_else(|| {
                MigrationContractError::invalid("verified_prefix.sequence", "must not overflow")
            })?);
        }
        if self.complete != (self.stop_line.is_none() && self.stop_error.is_none()) {
            return Err(MigrationContractError::invalid(
                "verified_prefix.complete",
                "complete must be true only when no stop error exists",
            ));
        }
        if self.stop_line.is_some() && self.stop_error.is_none() {
            return Err(MigrationContractError::invalid(
                "verified_prefix.stop",
                "stop_error is required when stop_line is supplied",
            ));
        }
        let Some(raw_prefix_bytes) = self.raw_prefix_bytes.as_deref() else {
            return Err(MigrationContractError::invalid(
                "verified_prefix.evidence",
                "must be rebound against the source report or bytes",
            ));
        };
        if digest_bytes(raw_prefix_bytes) != self.prefix_digest {
            return Err(MigrationContractError::digest_mismatch(
                "verified_prefix.prefix_digest",
                digest_bytes(raw_prefix_bytes),
                self.prefix_digest.clone(),
            ));
        }
        validate_prefix_bytes_against_records(raw_prefix_bytes, &self.records)?;
        Ok(())
    }

    /// Rebind serialized or caller-provided prefix metadata to one inspected
    /// source report.  Every field and every covered line must match the
    /// report-derived prefix before the in-memory byte witness is installed.
    pub fn verify_against_report(
        &mut self,
        report: &InspectionReport,
    ) -> Result<(), MigrationContractError> {
        let expected = Self::from_report(report)?;
        if self.source_digest != expected.source_digest {
            return Err(MigrationContractError::digest_mismatch(
                "verified_prefix.source_digest",
                expected.source_digest,
                self.source_digest.clone(),
            ));
        }
        if self.prefix_digest != expected.prefix_digest {
            return Err(MigrationContractError::digest_mismatch(
                "verified_prefix.prefix_digest",
                expected.prefix_digest,
                self.prefix_digest.clone(),
            ));
        }
        if self.records != expected.records {
            return Err(MigrationContractError::invalid(
                "verified_prefix.records",
                "do not match the report-derived direct prefix",
            ));
        }
        if self.complete != expected.complete
            || self.stop_line != expected.stop_line
            || self.stop_error != expected.stop_error
        {
            return Err(MigrationContractError::invalid(
                "verified_prefix.stop",
                "does not match the report-derived stop boundary",
            ));
        }
        self.raw_prefix_bytes = expected.raw_prefix_bytes;
        self.validate()
    }

    /// Inspect and bind one exact source byte stream.  No filesystem or
    /// daemon operation is performed; the caller remains the source owner.
    pub fn verify_against_bytes(
        &mut self,
        source_bytes: &[u8],
    ) -> Result<(), MigrationContractError> {
        let report = inspect_jsonl(source_bytes);
        self.verify_against_report(&report)
    }

    pub fn verify_source_digest(&self, observed: &str) -> Result<(), MigrationContractError> {
        self.validate()?;
        if self.source_digest == observed {
            Ok(())
        } else {
            Err(MigrationContractError::digest_mismatch(
                "verified_prefix.source_digest",
                self.source_digest.clone(),
                observed,
            ))
        }
    }

    pub fn last_sequence(&self) -> Option<u64> {
        self.records.last().map(|record| record.sequence)
    }

    pub fn record_count(&self) -> usize {
        self.records.len()
    }
}

/// Receipt produced by the pure apply gate.  It is evidence of validation,
/// not a journal commit; callers still need the resident writer to persist it.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationApplyReceipt {
    pub migration_id: String,
    pub source_project_id: String,
    pub target_epoch: u64,
    pub source_snapshot_digest: String,
    pub verified_prefix_digest: String,
    pub writer_operation_id: String,
}

/// Pure migration transaction gate.  It can be constructed and validated in
/// a copied fixture; no method here opens a live source, freezes a daemon,
/// creates an archive, allocates a target sequence, or rebinds a runtime.
#[derive(Debug, Clone, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MigrationTransaction {
    pub migration_id: String,
    pub source_project_id: String,
    pub project_admission: ProjectAdmission,
    pub phase: MigrationPhase,
    pub snapshot: Option<ImmutableSnapshot>,
    pub target_epoch: Option<TargetEpoch>,
    pub verified_prefix: Option<VerifiedPrefix>,
    pub receipts: Option<MigrationReceiptSet>,
}

impl MigrationTransaction {
    pub fn new(
        migration_id: impl Into<String>,
        source_project_id: impl Into<String>,
    ) -> Result<Self, MigrationContractError> {
        let transaction = Self {
            migration_id: migration_id.into(),
            source_project_id: source_project_id.into(),
            project_admission: ProjectAdmission::NeedsOperator,
            phase: MigrationPhase::Planned,
            snapshot: None,
            target_epoch: None,
            verified_prefix: None,
            receipts: None,
        };
        transaction.validate_identity()?;
        Ok(transaction)
    }

    pub fn validate(&self) -> Result<(), MigrationContractError> {
        self.validate_identity()?;
        if self.phase == MigrationPhase::ResetRequired
            && self.project_admission != ProjectAdmission::ResetRequired
        {
            return Err(MigrationContractError::invalid(
                "phase",
                "reset_required phase must carry reset_required project admission",
            ));
        }

        if let Some(snapshot) = self.snapshot.as_ref() {
            snapshot.validate()?;
            if snapshot.migration_id != self.migration_id {
                return Err(MigrationContractError::invalid(
                    "snapshot.migration_id",
                    "does not match migration transaction",
                ));
            }
            if snapshot.source_project_id != self.source_project_id {
                return Err(MigrationContractError::invalid(
                    "snapshot.source_project_id",
                    "does not match migration transaction",
                ));
            }
        }

        if let Some(target_epoch) = self.target_epoch.as_ref() {
            target_epoch.validate()?;
            if let Some(snapshot) = self.snapshot.as_ref() {
                if snapshot.source_epoch != target_epoch.source_epoch {
                    return Err(match (target_epoch.source_epoch, snapshot.source_epoch) {
                        (Some(expected), observed) => MigrationContractError::EpochMismatch {
                            field: "target_epoch.source_epoch",
                            expected,
                            observed: observed.unwrap_or_default(),
                        },
                        (None, Some(observed)) => MigrationContractError::invalid(
                            "target_epoch.source_epoch",
                            format!("snapshot source epoch {observed} is not bound to the target"),
                        ),
                        (None, None) => MigrationContractError::invalid(
                            "target_epoch.source_epoch",
                            "source epoch values differ",
                        ),
                    });
                }
                if snapshot.source_epoch == Some(target_epoch.target_epoch) {
                    return Err(MigrationContractError::invalid(
                        "target_epoch.target_epoch",
                        "must differ from the snapshot source epoch",
                    ));
                }
            }
        }

        if let Some(prefix) = self.verified_prefix.as_ref() {
            prefix.validate()?;
            if let Some(snapshot) = self.snapshot.as_ref() {
                if prefix.source_digest != snapshot.source_digest {
                    return Err(MigrationContractError::digest_mismatch(
                        "verified_prefix.source_digest",
                        snapshot.source_digest.clone(),
                        prefix.source_digest.clone(),
                    ));
                }
            }
        }

        if let Some(receipts) = self.receipts.as_ref() {
            let snapshot = self
                .snapshot
                .as_ref()
                .ok_or_else(|| MigrationContractError::missing("snapshot"))?;
            let target_epoch = self
                .target_epoch
                .as_ref()
                .ok_or_else(|| MigrationContractError::missing("target_epoch"))?;
            receipts
                .validate_for(
                    &self.migration_id,
                    &self.source_project_id,
                    &snapshot.source_digest,
                    target_epoch.target_epoch,
                )
                .map_err(|error| MigrationContractError::ReceiptRejected {
                    reason: error.to_string(),
                })?;
            if let Some(writer) = receipts.writer.as_ref() {
                if writer.source_epoch != snapshot.source_epoch {
                    return Err(MigrationContractError::invalid(
                        "receipts.writer.source_epoch",
                        format!(
                            "does not match snapshot source epoch {:?}",
                            snapshot.source_epoch
                        ),
                    ));
                }
            }
        }

        match self.phase {
            MigrationPhase::SnapshotCaptured if self.snapshot.is_none() => {
                return Err(MigrationContractError::missing("snapshot"));
            }
            MigrationPhase::ReplayVerified if self.verified_prefix.is_none() => {
                return Err(MigrationContractError::missing("verified_prefix"));
            }
            MigrationPhase::Rebound if self.receipts.is_none() => {
                return Err(MigrationContractError::missing("receipts"));
            }
            MigrationPhase::Applied | MigrationPhase::Verified => {
                for (field, present) in [
                    ("snapshot", self.snapshot.is_some()),
                    ("target_epoch", self.target_epoch.is_some()),
                    ("verified_prefix", self.verified_prefix.is_some()),
                    ("receipts", self.receipts.is_some()),
                ] {
                    if !present {
                        return Err(MigrationContractError::missing(field));
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Validate every precondition required before an apply side effect.
    /// `reset_required`, `needs_operator`, and `aborted` are terminal stops;
    /// they are rejected before receipt or source validation is attempted.
    /// Validate every precondition against the currently active epoch and
    /// revision before an apply side effect is admitted.
    pub fn validate_for_apply(
        &self,
        active_epoch: u64,
        active_revision: u64,
    ) -> Result<(), MigrationContractError> {
        self.validate_identity()?;
        if self.project_admission != ProjectAdmission::Verified {
            return Err(MigrationContractError::AdmissionBlocked {
                admission: self.project_admission,
            });
        }
        self.validate()?;
        if !matches!(
            self.phase,
            MigrationPhase::Rebound | MigrationPhase::Applied | MigrationPhase::Verified
        ) {
            return Err(MigrationContractError::invalid(
                "phase",
                format!("{} cannot be applied", self.phase.as_str()),
            ));
        }
        for (field, present) in [
            ("snapshot", self.snapshot.is_some()),
            ("target_epoch", self.target_epoch.is_some()),
            ("verified_prefix", self.verified_prefix.is_some()),
            ("receipts", self.receipts.is_some()),
        ] {
            if !present {
                return Err(MigrationContractError::missing(field));
            }
        }
        self.target_epoch
            .as_ref()
            .expect("target epoch presence checked above")
            .validate_committed_against(active_epoch, active_revision)?;
        Ok(())
    }

    /// Return a typed apply receipt without mutating source or target state.
    /// The active epoch and revision are required so the target cannot be
    /// reused or moved backward between validation and apply admission.
    pub fn apply(
        &self,
        active_epoch: u64,
        active_revision: u64,
    ) -> Result<MigrationApplyReceipt, MigrationContractError> {
        self.validate_for_apply(active_epoch, active_revision)?;
        let snapshot = self
            .snapshot
            .as_ref()
            .ok_or_else(|| MigrationContractError::missing("snapshot"))?;
        let target_epoch = self
            .target_epoch
            .as_ref()
            .ok_or_else(|| MigrationContractError::missing("target_epoch"))?;
        let prefix = self
            .verified_prefix
            .as_ref()
            .ok_or_else(|| MigrationContractError::missing("verified_prefix"))?;
        if !prefix.complete {
            return Err(MigrationContractError::PrefixBlocked {
                line: prefix.stop_line,
                exact_error: prefix
                    .stop_error
                    .clone()
                    .unwrap_or_else(|| "VERIFIED_PREFIX_INCOMPLETE".to_owned()),
            });
        }
        let receipts = self
            .receipts
            .as_ref()
            .ok_or_else(|| MigrationContractError::missing("receipts"))?;
        let writer_operation_id = receipts
            .writer
            .as_ref()
            .map(|writer| writer.operation_id.as_str().to_owned())
            .ok_or_else(|| MigrationContractError::missing("receipts.writer"))?;
        Ok(MigrationApplyReceipt {
            migration_id: self.migration_id.clone(),
            source_project_id: self.source_project_id.clone(),
            target_epoch: target_epoch.target_epoch,
            source_snapshot_digest: snapshot.source_digest.clone(),
            verified_prefix_digest: prefix.prefix_digest.clone(),
            writer_operation_id,
        })
    }

    fn validate_identity(&self) -> Result<(), MigrationContractError> {
        validate_contract_identifier("migration_id", &self.migration_id)?;
        validate_contract_identifier("source_project_id", &self.source_project_id)
    }
}

fn validate_contract_identifier(
    field: &'static str,
    value: &str,
) -> Result<(), MigrationContractError> {
    if value.trim().is_empty() {
        return Err(MigrationContractError::missing(field));
    }
    if value
        .chars()
        .any(|character| character.is_control() || character.is_whitespace())
    {
        return Err(MigrationContractError::invalid(
            field,
            "must not contain whitespace or control characters",
        ));
    }
    Ok(())
}

fn validate_contract_reference(
    field: &'static str,
    value: &str,
) -> Result<(), MigrationContractError> {
    if value.trim().is_empty() {
        return Err(MigrationContractError::missing(field));
    }
    if value.chars().any(char::is_control) {
        return Err(MigrationContractError::invalid(
            field,
            "must not contain control characters",
        ));
    }
    Ok(())
}

/// Bind a serialized prefix receipt to the exact source bytes that it claims
/// to cover.  The receipt only carries stable record metadata, so the bytes
/// must be inspected again before the prefix can pass validation or apply.
fn validate_prefix_bytes_against_records(
    raw_prefix_bytes: &[u8],
    expected_records: &[VerifiedPrefixRecord],
) -> Result<(), MigrationContractError> {
    let report = inspect_jsonl(raw_prefix_bytes);
    if report.records.len() != expected_records.len() {
        return Err(MigrationContractError::invalid(
            "verified_prefix.records",
            format!(
                "source prefix contains {} records, expected {}",
                report.records.len(),
                expected_records.len()
            ),
        ));
    }

    for (expected, observed) in expected_records.iter().zip(&report.records) {
        if observed.line_number != expected.line_number {
            return Err(MigrationContractError::invalid(
                "verified_prefix.records.line_number",
                format!(
                    "expected {}, observed {}",
                    expected.line_number, observed.line_number
                ),
            ));
        }
        if observed.record_id.as_deref() != Some(expected.record_id.as_str()) {
            return Err(MigrationContractError::invalid(
                "verified_prefix.records.record_id",
                format!(
                    "expected {}, observed {:?}",
                    expected.record_id, observed.record_id
                ),
            ));
        }
        if observed.sequence != Some(expected.sequence) {
            return Err(MigrationContractError::invalid(
                "verified_prefix.records.sequence",
                format!(
                    "expected {}, observed {:?}",
                    expected.sequence, observed.sequence
                ),
            ));
        }

        let observed_digest = digest_bytes(&observed.raw_bytes);
        if observed_digest != expected.digest {
            return Err(MigrationContractError::digest_mismatch(
                "verified_prefix.records.digest",
                expected.digest.clone(),
                observed_digest,
            ));
        }
        if observed.classification != MappingClass::Direct || observed.exact_error.is_some() {
            return Err(MigrationContractError::PrefixBlocked {
                line: Some(observed.line_number),
                exact_error: observed
                    .exact_error
                    .clone()
                    .unwrap_or_else(|| "RECORD_NOT_DIRECT".to_owned()),
            });
        }
    }
    Ok(())
}

impl MappingClass {
    fn combine(self, other: Self) -> Self {
        self.max(other)
    }
}

/// Optional context supplied by the migration owner.  The context is used for
/// validation only; it never changes the preserved source bytes.
#[derive(Debug, Clone, Default, Eq, PartialEq)]
pub struct InspectOptions {
    pub source_path: Option<PathBuf>,
    pub canonical_project_cwd: Option<PathBuf>,
}

/// One deterministic issue found while inspecting a source or replay shape.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct HistoryIssue {
    pub line_number: Option<usize>,
    pub classification: MappingClass,
    pub exact_error: String,
    pub first_failed_boundary: String,
}

/// A parsed source line with the original bytes and line digest retained.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct RecordInspection {
    pub line_number: usize,
    pub raw_bytes: Vec<u8>,
    /// SHA-256 of this complete source line, including its line ending when
    /// one was present in the source stream.
    pub digest: String,
    pub record_id: Option<String>,
    pub sequence: Option<u64>,
    pub entity_id: Option<String>,
    pub event: Option<String>,
    pub owner: Option<String>,
    pub cwd: Option<String>,
    pub worktree: Option<String>,
    pub base_commit: Option<String>,
    pub status: Option<String>,
    pub classification: MappingClass,
    pub source_disposition: SourceDisposition,
    pub exact_error: Option<String>,
    pub first_failed_boundary: Option<String>,
}

/// Read-only result for one source JSONL stream.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct InspectionReport {
    pub source_path: Option<PathBuf>,
    /// SHA-256 of the exact inspected byte stream.  A migration manifest may
    /// use this as its source snapshot digest after the caller records the
    /// corresponding source path and project context.
    pub source_digest: String,
    pub byte_len: usize,
    pub line_count: usize,
    pub final_newline: bool,
    pub classification: MappingClass,
    pub records: Vec<RecordInspection>,
    pub issues: Vec<HistoryIssue>,
}

/// A source digest mismatch is a migration admission failure, not a retryable
/// parse error.
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct DigestDrift {
    pub expected: String,
    pub observed: String,
}

impl fmt::Display for DigestDrift {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "DIGEST_DRIFT:expected={}:observed={}",
            self.expected, self.observed
        )
    }
}

impl Error for DigestDrift {}

impl InspectionReport {
    /// Verify that the bytes inspected are the bytes admitted by the caller.
    pub fn verify_digest(&self, expected: &str) -> Result<(), DigestDrift> {
        if self.source_digest == expected {
            Ok(())
        } else {
            Err(DigestDrift {
                expected: expected.to_owned(),
                observed: self.source_digest.clone(),
            })
        }
    }

    pub fn has_error(&self, code: &str) -> bool {
        self.issues.iter().any(|issue| {
            issue.exact_error == code || issue.exact_error.starts_with(&format!("{code}:"))
        }) || self.records.iter().any(|record| {
            record
                .exact_error
                .as_deref()
                .is_some_and(|error| error == code || error.starts_with(&format!("{code}:")))
        })
    }
}

/// Inspect and classify a JSONL byte stream with default context.
pub fn inspect_jsonl(bytes: &[u8]) -> InspectionReport {
    inspect_jsonl_with_options(bytes, &InspectOptions::default())
}

/// Alias used by callers that want to emphasize the classification step.
pub fn classify_jsonl(bytes: &[u8]) -> InspectionReport {
    inspect_jsonl(bytes)
}

/// Inspect and classify a JSONL byte stream without writing to its source or
/// to any target state.
pub fn inspect_jsonl_with_options(bytes: &[u8], options: &InspectOptions) -> InspectionReport {
    let source_digest = digest_bytes(bytes);
    let final_newline = bytes.last() == Some(&b'\n');
    let lines: Vec<&[u8]> = if bytes.is_empty() {
        Vec::new()
    } else {
        bytes.split_inclusive(|byte| *byte == b'\n').collect()
    };
    let line_count = lines.len();
    let mut records = Vec::new();
    let mut issues = Vec::new();

    if bytes.is_empty() {
        issues.push(issue(None, MappingClass::Unknown, "EMPTY_SOURCE", "source"));
    }
    if let Some(canonical_cwd) = options.canonical_project_cwd.as_ref() {
        if !canonical_cwd.is_absolute() {
            issues.push(issue(
                None,
                MappingClass::Unknown,
                format!("INVALID_CANONICAL_PROJECT_CWD:{}", canonical_cwd.display()),
                "source.context",
            ));
        }
    }

    for (index, raw_bytes) in lines.iter().enumerate() {
        let line_number = index + 1;
        let content = trim_line_ending(raw_bytes);
        if content.iter().all(u8::is_ascii_whitespace) {
            let exact_error = "EMPTY_LINE";
            issues.push(issue(
                Some(line_number),
                MappingClass::Unknown,
                exact_error,
                "parse",
            ));
            records.push(invalid_record(
                line_number,
                raw_bytes.to_vec(),
                exact_error,
                "parse",
            ));
            continue;
        }

        let value = match serde_json::from_slice::<Value>(content) {
            Ok(value) => value,
            Err(error) => {
                let (code, boundary) = if line_number == line_count {
                    ("MALFORMED_JSON_TAIL", "parse.tail")
                } else {
                    ("MALFORMED_JSON_MIDDLE", "parse.middle")
                };
                issues.push(issue(
                    Some(line_number),
                    MappingClass::Unknown,
                    format!("{code}:{error}"),
                    boundary,
                ));
                records.push(invalid_record(
                    line_number,
                    raw_bytes.to_vec(),
                    format!("{code}:{error}"),
                    boundary,
                ));
                continue;
            }
        };

        if !value.is_object() {
            let exact_error = "INVALID_RECORD_SHAPE";
            issues.push(issue(
                Some(line_number),
                MappingClass::Unknown,
                exact_error,
                "schema",
            ));
            records.push(invalid_record(
                line_number,
                raw_bytes.to_vec(),
                exact_error,
                "schema",
            ));
            continue;
        }

        let mut record = inspect_record(line_number, raw_bytes.to_vec(), content, &value, options);
        if record.record_id.is_none() {
            record.classification = record.classification.combine(MappingClass::Adapt);
            record.exact_error = record
                .exact_error
                .or_else(|| Some("LEGACY_RECORD_ID_ABSENT".to_owned()));
            record.first_failed_boundary = record
                .first_failed_boundary
                .or_else(|| Some("schema".to_owned()));
        }
        if record.sequence.is_none() {
            record.classification = record.classification.combine(MappingClass::Adapt);
            record.exact_error = record
                .exact_error
                .or_else(|| Some("LEGACY_SEQUENCE_ABSENT".to_owned()));
            record.first_failed_boundary = record
                .first_failed_boundary
                .or_else(|| Some("schema".to_owned()));
        }
        records.push(record);
    }

    if !bytes.is_empty() && !final_newline {
        issues.push(issue(
            line_count.checked_sub(1).map(|_| line_count),
            MappingClass::Unknown,
            "MISSING_FINAL_NEWLINE",
            "source.integrity",
        ));
    }

    mark_duplicate_record_ids(&mut records, &mut issues);
    mark_duplicate_registrations(&mut records, &mut issues);
    mark_sequence_issues(&mut records, &mut issues);
    for record in &mut records {
        record.source_disposition = SourceDisposition::from_mapping_class(record.classification);
    }

    let has_valid_record = records.iter().any(|record| !is_parse_invalid(record));
    if has_valid_record
        && records
            .iter()
            .filter(|record| !is_parse_invalid(record))
            .all(|record| record.sequence.is_none())
    {
        issues.push(issue(
            None,
            MappingClass::Adapt,
            "LEGACY_SEQUENCE_ABSENT",
            "schema",
        ));
    }

    let mut classification = if records.is_empty() {
        MappingClass::Unknown
    } else {
        records
            .iter()
            .map(|record| record.classification)
            .fold(MappingClass::Direct, MappingClass::combine)
    };
    classification = issues
        .iter()
        .map(|issue| issue.classification)
        .fold(classification, MappingClass::combine);

    InspectionReport {
        source_path: options.source_path.clone(),
        source_digest,
        byte_len: bytes.len(),
        line_count,
        final_newline,
        classification,
        records,
        issues,
    }
}

/// Read a source file and inspect its bytes.  This function has no migration
/// side effect; the caller decides where an archive or target should go.
pub fn inspect_path(path: &Path, options: &InspectOptions) -> io::Result<InspectionReport> {
    let bytes = fs::read(path)?;
    let mut effective = options.clone();
    effective.source_path = Some(path.to_path_buf());
    Ok(inspect_jsonl_with_options(&bytes, &effective))
}

/// A stable SHA-256 digest suitable for snapshot and drift checks.
pub fn digest_bytes(bytes: &[u8]) -> String {
    let mut state = [
        0x6a09e667_u32,
        0xbb67ae85,
        0x3c6ef372,
        0xa54ff53a,
        0x510e527f,
        0x9b05688c,
        0x1f83d9ab,
        0x5be0cd19,
    ];
    let mut padded = Vec::with_capacity((bytes.len() + 72) / 64 * 64);
    padded.extend_from_slice(bytes);
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&((bytes.len() as u64) * 8).to_be_bytes());

    for chunk in padded.chunks_exact(64) {
        let mut words = [0_u32; 64];
        for (index, word) in words[..16].iter_mut().enumerate() {
            let offset = index * 4;
            *word = u32::from_be_bytes([
                chunk[offset],
                chunk[offset + 1],
                chunk[offset + 2],
                chunk[offset + 3],
            ]);
        }
        for index in 16..64 {
            let s0 = words[index - 15].rotate_right(7)
                ^ words[index - 15].rotate_right(18)
                ^ (words[index - 15] >> 3);
            let s1 = words[index - 2].rotate_right(17)
                ^ words[index - 2].rotate_right(19)
                ^ (words[index - 2] >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }

        let mut working = state;
        for (index, constant) in SHA256_CONSTANTS.iter().enumerate() {
            let s1 = working[4].rotate_right(6)
                ^ working[4].rotate_right(11)
                ^ working[4].rotate_right(25);
            let choose = (working[4] & working[5]) ^ ((!working[4]) & working[6]);
            let temp1 = working[7]
                .wrapping_add(s1)
                .wrapping_add(choose)
                .wrapping_add(*constant)
                .wrapping_add(words[index]);
            let s0 = working[0].rotate_right(2)
                ^ working[0].rotate_right(13)
                ^ working[0].rotate_right(22);
            let majority =
                (working[0] & working[1]) ^ (working[0] & working[2]) ^ (working[1] & working[2]);
            let temp2 = s0.wrapping_add(majority);
            working[7] = working[6];
            working[6] = working[5];
            working[5] = working[4];
            working[4] = working[3].wrapping_add(temp1);
            working[3] = working[2];
            working[2] = working[1];
            working[1] = working[0];
            working[0] = temp1.wrapping_add(temp2);
        }
        for (target, value) in state.iter_mut().zip(working) {
            *target = target.wrapping_add(value);
        }
    }

    let mut hex = String::with_capacity(64);
    for word in state {
        use std::fmt::Write;
        write!(&mut hex, "{word:08x}").expect("writing to String cannot fail");
    }
    format!("sha256:{hex}")
}

const SHA256_CONSTANTS: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn inspect_record(
    line_number: usize,
    raw_bytes: Vec<u8>,
    raw_content: &[u8],
    value: &Value,
    options: &InspectOptions,
) -> RecordInspection {
    let event = string_at(value, &["ev", "type", "event"]);
    let record_id = string_at(value, &["record_id", "event_id", "entry_id"]);
    let sequence = integer_at(value, &["sequence", "seq"]);
    let entity_id = first_nested_string(
        value,
        &[
            (&["task"][..], &["id"][..]),
            (&["worker"][..], &["id"][..]),
            (&["msg"][..], &["id"][..]),
            (&["migration"][..], &["id"][..]),
            (&["subscription"][..], &["id"][..]),
        ],
    )
    .or_else(|| string_at(value, &["message_id", "task_id", "worker_id", "msg_id"]));
    let owner = first_nested_string(
        value,
        &[
            (&["task"][..], &["owner"][..]),
            (&["worker"][..], &["owner"][..]),
        ],
    )
    .or_else(|| string_at(value, &["owner", "created_by", "worker_id"]));
    let cwd = first_nested_string(
        value,
        &[
            (&["task"][..], &["cwd"][..]),
            (&["worker"][..], &["cwd"][..]),
        ],
    )
    .or_else(|| string_at(value, &["cwd", "project_cwd", "canonical_project_cwd"]));
    let worktree = first_nested_string(
        value,
        &[(&["task"][..], &["worktree_path", "worktree"][..])],
    )
    .or_else(|| string_at(value, &["worktree_path", "worktree"]));
    let base_commit = first_nested_string(value, &[(&["task"][..], &["base_commit"][..])])
        .or_else(|| string_at(value, &["base_commit"]));
    let status = first_nested_string(value, &[(&["task"][..], &["status"][..])])
        .or_else(|| string_at(value, &["status", "phase"]));

    let is_task = event
        .as_deref()
        .is_some_and(|name| name.starts_with("Task"))
        || value.get("task").is_some();
    let event_error = event_schema_error(raw_content, value);
    let mut classification = if record_id.is_some() && sequence.is_some() && event_error.is_none() {
        MappingClass::Direct
    } else {
        MappingClass::Adapt
    };
    let mut exact_error = event_error;
    let mut first_failed_boundary = exact_error.as_ref().map(|_| "schema".to_owned());
    if exact_error.is_some() {
        classification = MappingClass::Unknown;
    }

    if is_task {
        if let Some(status_name) = status.as_deref() {
            if !is_known_task_status(status_name) {
                classification = MappingClass::Unknown;
                exact_error = Some(format!("UNKNOWN_TASK_STATUS:{status_name}"));
                first_failed_boundary = Some("schema".to_owned());
            }
        }
    }
    if is_task && status.as_deref() == Some("available") {
        classification = classification.combine(MappingClass::Adapt);
        if exact_error.is_none() {
            exact_error = Some("DEPRECATED_TASK_STATUS:available".to_owned());
        }
        if first_failed_boundary.is_none() {
            first_failed_boundary = Some("schema".to_owned());
        }
    }

    if is_task {
        if owner.is_none() {
            classification = MappingClass::Unknown;
            if exact_error.is_none() {
                exact_error = Some("MISSING_OWNER".to_owned());
            }
            if first_failed_boundary.is_none() {
                first_failed_boundary = Some("ownership".to_owned());
            }
        } else if let Some(canonical_cwd) = options
            .canonical_project_cwd
            .as_deref()
            .filter(|cwd| cwd.is_absolute())
        {
            if let Some(record_cwd) = cwd.as_deref() {
                if !cwd_matches_project(canonical_cwd, record_cwd) {
                    classification = MappingClass::Unknown;
                    exact_error = Some(scope_error(
                        "CWD_OUTSIDE_PROJECT_SCOPE",
                        canonical_cwd,
                        record_cwd,
                    ));
                    first_failed_boundary = Some("scope".to_owned());
                }
            }
            if let Some(record_worktree) = worktree.as_deref() {
                if !worktree_matches_project(canonical_cwd, record_worktree) {
                    classification = MappingClass::Unknown;
                    exact_error = Some(scope_error(
                        "WORKTREE_OUTSIDE_PROJECT_SCOPE",
                        canonical_cwd,
                        record_worktree,
                    ));
                    first_failed_boundary = Some("binding".to_owned());
                }
            }
        }

        if owner.is_some() && is_active_status(status.as_deref()) {
            // The canonical context validates the record; it never supplies
            // missing binding evidence from the source record.
            let has_cwd = cwd.is_some();
            if worktree.is_none() || base_commit.is_none() || !has_cwd {
                classification = classification.combine(MappingClass::Reset);
                if exact_error.is_none() {
                    exact_error = Some(missing_binding_error(
                        worktree.is_some(),
                        base_commit.is_some(),
                        has_cwd,
                    ));
                }
                if first_failed_boundary.is_none() {
                    first_failed_boundary = Some("binding".to_owned());
                }
            }
        } else if is_terminal_status(status.as_deref())
            && (worktree.is_none() || base_commit.is_none() || cwd.is_none())
        {
            classification = classification.combine(MappingClass::Adapt);
            if exact_error.is_none() {
                exact_error = Some("LEGACY_BINDING_EVIDENCE_ABSENT".to_owned());
            }
            if first_failed_boundary.is_none() {
                first_failed_boundary = Some("binding".to_owned());
            }
        }
    } else if let Some(canonical_cwd) = options
        .canonical_project_cwd
        .as_deref()
        .filter(|cwd| cwd.is_absolute())
    {
        if let Some(record_cwd) = cwd.as_deref() {
            if !cwd_matches_project(canonical_cwd, record_cwd) {
                classification = MappingClass::Unknown;
                exact_error = Some(scope_error(
                    "CWD_OUTSIDE_PROJECT_SCOPE",
                    canonical_cwd,
                    record_cwd,
                ));
                first_failed_boundary = Some("scope".to_owned());
            }
        }
        if let Some(record_worktree) = worktree.as_deref() {
            if !worktree_matches_project(canonical_cwd, record_worktree) {
                classification = MappingClass::Unknown;
                exact_error = Some(scope_error(
                    "WORKTREE_OUTSIDE_PROJECT_SCOPE",
                    canonical_cwd,
                    record_worktree,
                ));
                first_failed_boundary = Some("binding".to_owned());
            }
        }
    }

    RecordInspection {
        line_number,
        digest: digest_bytes(&raw_bytes),
        raw_bytes,
        record_id,
        sequence,
        entity_id,
        event,
        owner,
        cwd,
        worktree,
        base_commit,
        status,
        classification,
        source_disposition: SourceDisposition::from_mapping_class(classification),
        exact_error,
        first_failed_boundary,
    }
}

fn mark_duplicate_record_ids(records: &mut [RecordInspection], issues: &mut Vec<HistoryIssue>) {
    let mut seen = BTreeMap::<String, usize>::new();
    for index in 0..records.len() {
        let Some(record_id) = records[index].record_id.clone() else {
            continue;
        };
        if let Some(previous_index) = seen.insert(record_id.clone(), index) {
            let error = format!("DUPLICATE_RECORD_ID:{record_id}");
            mark_record(
                &mut records[previous_index],
                MappingClass::Unknown,
                &error,
                "replay",
            );
            mark_record(&mut records[index], MappingClass::Unknown, &error, "replay");
            issues.push(issue(
                Some(records[index].line_number),
                MappingClass::Unknown,
                error,
                "replay",
            ));
        }
    }
}

fn mark_duplicate_registrations(records: &mut [RecordInspection], issues: &mut Vec<HistoryIssue>) {
    let mut seen = BTreeMap::<String, usize>::new();
    for index in 0..records.len() {
        if records[index].event.as_deref() != Some("Registered") {
            continue;
        }
        let Some(entity_id) = records[index].entity_id.clone() else {
            continue;
        };
        if let Some(previous_index) = seen.insert(entity_id.clone(), index) {
            let error = format!("DUPLICATE_REGISTRATION:{entity_id}");
            mark_record(
                &mut records[previous_index],
                MappingClass::Reset,
                &error,
                "identity",
            );
            mark_record(&mut records[index], MappingClass::Reset, &error, "identity");
            issues.push(issue(
                Some(records[index].line_number),
                MappingClass::Reset,
                error,
                "identity",
            ));
        }
    }
}

fn mark_sequence_issues(records: &mut [RecordInspection], issues: &mut Vec<HistoryIssue>) {
    let has_sequence = records.iter().any(|record| record.sequence.is_some());
    if !has_sequence {
        return;
    }

    let mut expected = None;
    let mut exhausted = false;
    for record in records {
        if is_parse_invalid(record) {
            continue;
        }
        if exhausted {
            mark_record(record, MappingClass::Unknown, "SEQUENCE_OVERFLOW", "replay");
            issues.push(issue(
                Some(record.line_number),
                MappingClass::Unknown,
                "SEQUENCE_OVERFLOW",
                "replay",
            ));
            continue;
        }
        let Some(sequence) = record.sequence else {
            mark_record(record, MappingClass::Unknown, "MISSING_SEQUENCE", "replay");
            issues.push(issue(
                Some(record.line_number),
                MappingClass::Unknown,
                "MISSING_SEQUENCE",
                "replay",
            ));
            continue;
        };
        let Some(next) = expected else {
            expected = sequence.checked_add(1);
            if expected.is_none() {
                exhausted = true;
                mark_record(record, MappingClass::Unknown, "SEQUENCE_OVERFLOW", "replay");
                issues.push(issue(
                    Some(record.line_number),
                    MappingClass::Unknown,
                    "SEQUENCE_OVERFLOW",
                    "replay",
                ));
            }
            continue;
        };
        if sequence != next {
            let error = if sequence > next {
                format!("SEQUENCE_GAP:expected={next}:observed={sequence}")
            } else {
                format!("SEQUENCE_NON_MONOTONIC:expected={next}:observed={sequence}")
            };
            mark_record(record, MappingClass::Unknown, &error, "replay");
            issues.push(issue(
                Some(record.line_number),
                MappingClass::Unknown,
                error,
                "replay",
            ));
        }
        expected = sequence.checked_add(1);
        if expected.is_none() {
            exhausted = true;
            mark_record(record, MappingClass::Unknown, "SEQUENCE_OVERFLOW", "replay");
            issues.push(issue(
                Some(record.line_number),
                MappingClass::Unknown,
                "SEQUENCE_OVERFLOW",
                "replay",
            ));
        }
    }
}

fn invalid_record(
    line_number: usize,
    raw_bytes: Vec<u8>,
    exact_error: impl Into<String>,
    first_failed_boundary: impl Into<String>,
) -> RecordInspection {
    RecordInspection {
        line_number,
        digest: digest_bytes(&raw_bytes),
        raw_bytes,
        record_id: None,
        sequence: None,
        entity_id: None,
        event: None,
        owner: None,
        cwd: None,
        worktree: None,
        base_commit: None,
        status: None,
        classification: MappingClass::Unknown,
        source_disposition: SourceDisposition::ArchiveOnly,
        exact_error: Some(exact_error.into()),
        first_failed_boundary: Some(first_failed_boundary.into()),
    }
}

fn is_parse_invalid(record: &RecordInspection) -> bool {
    record
        .exact_error
        .as_deref()
        .is_some_and(|error| error == "EMPTY_LINE" || error.starts_with("MALFORMED_JSON_"))
}

fn event_schema_error(raw_content: &[u8], value: &Value) -> Option<String> {
    if let Some(error) = duplicate_json_key_error(raw_content) {
        return Some(error);
    }
    let Some(event_value) = value.get("ev") else {
        return Some("EVENT_ABSENT".to_owned());
    };
    let event_name = event_value.as_str();
    // Validate the canonical event against the original bytes.  Parsing the
    // Value first is still useful for extracting legacy fields, but Value
    // collapses duplicate object keys and therefore cannot be the schema
    // authority for a migration decision.
    match serde_json::from_slice::<Event>(raw_content) {
        Ok(_) => None,
        Err(error) => {
            let detail = error.to_string();
            if let Some(event_name) = event_name {
                if detail.contains("unknown variant") {
                    Some(format!("UNKNOWN_EVENT:{event_name}"))
                } else {
                    Some(format!("INVALID_EVENT:{event_name}:{detail}"))
                }
            } else {
                Some(format!("INVALID_EVENT:ev:{detail}"))
            }
        }
    }
}

fn duplicate_json_key_error(raw_content: &[u8]) -> Option<String> {
    let mut deserializer = serde_json::Deserializer::from_slice(raw_content);
    match deserializer.deserialize_any(DuplicateKeyVisitor) {
        Ok(()) => None,
        Err(error) => {
            let detail = error.to_string();
            let stable_detail = detail
                .split(" at line ")
                .next()
                .unwrap_or(detail.as_str())
                .to_owned();
            if stable_detail.starts_with("DUPLICATE_JSON_KEY:") {
                Some(stable_detail)
            } else {
                Some(format!("DUPLICATE_KEY_SCAN_FAILED:{stable_detail}"))
            }
        }
    }
}

struct DuplicateKeyVisitor;

impl<'de> Visitor<'de> for DuplicateKeyVisitor {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut keys = BTreeMap::new();
        while let Some(key) = map.next_key::<String>()? {
            if keys.insert(key.clone(), ()).is_some() {
                return Err(serde::de::Error::custom(format!(
                    "DUPLICATE_JSON_KEY:{key}"
                )));
            }
            map.next_value_seed(DuplicateValueSeed)?;
        }
        Ok(())
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while sequence.next_element_seed(DuplicateValueSeed)?.is_some() {}
        Ok(())
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(())
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(())
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(())
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(())
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(())
    }

    fn visit_string<E>(self, _value: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(())
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Ok(())
    }
}

struct DuplicateValueSeed;

impl<'de> DeserializeSeed<'de> for DuplicateValueSeed {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(DuplicateKeyVisitor)
    }
}

fn is_known_task_status(status: &str) -> bool {
    matches!(
        status,
        "available"
            | "assigned"
            | "working"
            | "blocked"
            | "waiting"
            | "verifying"
            | "reviewed"
            | "delivered"
            | "accepted"
            | "rework"
            | "merged"
            | "closed"
            | "cancelled"
    )
}

fn cwd_matches_project(canonical_cwd: &Path, record_cwd: &str) -> bool {
    let record_cwd = Path::new(record_cwd);
    record_cwd.is_absolute() && normalize_path(record_cwd) == normalize_path(canonical_cwd)
}

fn worktree_matches_project(canonical_cwd: &Path, record_worktree: &str) -> bool {
    let raw = Path::new(record_worktree);
    if raw
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return false;
    }
    let candidate = if raw.is_absolute() {
        raw.to_path_buf()
    } else {
        let relative = raw.strip_prefix(Path::new("./")).unwrap_or(raw);
        canonical_cwd.join(relative)
    };
    let candidate = normalize_path(&candidate);
    let playground = normalize_path(&canonical_cwd.join("playground"));
    candidate.starts_with(&playground) && candidate != playground
}

fn normalize_path(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
}

fn scope_error(kind: &str, canonical_cwd: &Path, observed: &str) -> String {
    format!(
        "{kind}:expected={}:observed={observed}",
        canonical_cwd.display()
    )
}

fn mark_record(record: &mut RecordInspection, class: MappingClass, error: &str, boundary: &str) {
    record.classification = record.classification.combine(class);
    if record.exact_error.is_none() {
        record.exact_error = Some(error.to_owned());
    }
    if record.first_failed_boundary.is_none() {
        record.first_failed_boundary = Some(boundary.to_owned());
    }
}

fn issue(
    line_number: Option<usize>,
    classification: MappingClass,
    exact_error: impl Into<String>,
    first_failed_boundary: impl Into<String>,
) -> HistoryIssue {
    HistoryIssue {
        line_number,
        classification,
        exact_error: exact_error.into(),
        first_failed_boundary: first_failed_boundary.into(),
    }
}

fn missing_binding_error(has_worktree: bool, has_base: bool, has_cwd: bool) -> String {
    let mut missing = Vec::new();
    if !has_worktree {
        missing.push("worktree");
    }
    if !has_base {
        missing.push("base_commit");
    }
    if !has_cwd {
        missing.push("cwd");
    }
    format!("MISSING_BINDING_EVIDENCE:{}", missing.join(","))
}

fn is_active_status(status: Option<&str>) -> bool {
    status.is_none_or(|status| {
        matches!(
            status,
            "assigned"
                | "working"
                | "blocked"
                | "waiting"
                | "verifying"
                | "reviewed"
                | "delivered"
                | "accepted"
                | "rework"
                | "cleanup_pending"
        )
    })
}

fn is_terminal_status(status: Option<&str>) -> bool {
    status.is_some_and(|status| matches!(status, "closed" | "merged" | "cancelled"))
}

fn trim_line_ending(raw_bytes: &[u8]) -> &[u8] {
    let without_lf = raw_bytes.strip_suffix(b"\n").unwrap_or(raw_bytes);
    without_lf.strip_suffix(b"\r").unwrap_or(without_lf)
}

fn string_at(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(as_non_empty_string))
}

fn integer_at(value: &Value, keys: &[&str]) -> Option<u64> {
    keys.iter().find_map(|key| {
        value.get(*key).and_then(|candidate| {
            candidate
                .as_u64()
                .or_else(|| candidate.as_str().and_then(|text| text.parse().ok()))
        })
    })
}

fn first_nested_string(value: &Value, paths: &[(&[&str], &[&str])]) -> Option<String> {
    paths.iter().find_map(|(parents, keys)| {
        parents.iter().find_map(|parent| {
            value
                .get(*parent)
                .and_then(|nested| string_at(nested, keys))
        })
    })
}

fn as_non_empty_string(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_disposition_is_orthogonal_to_observed_mapping_class() {
        assert_eq!(
            SourceDisposition::from_mapping_class(MappingClass::Direct).as_str(),
            "direct_replay"
        );
        assert_eq!(
            SourceDisposition::from_mapping_class(MappingClass::Adapt).as_str(),
            "adapt_reconcile"
        );
        assert_eq!(
            SourceDisposition::from_mapping_class(MappingClass::Reset).as_str(),
            "rebuild_required"
        );
        assert_eq!(
            SourceDisposition::from_mapping_class(MappingClass::Unknown).as_str(),
            "archive_only"
        );
    }

    #[test]
    fn project_admission_names_are_stable_contract_values() {
        assert_eq!(ProjectAdmission::Verified.as_str(), "verified");
        assert_eq!(ProjectAdmission::ResetRequired.as_str(), "reset_required");
        assert_eq!(ProjectAdmission::NeedsOperator.as_str(), "needs_operator");
        assert_eq!(ProjectAdmission::Aborted.as_str(), "aborted");
    }

    fn direct_line(id: &str, sequence: u64) -> String {
        format!(
            "{{\"record_id\":\"{id}\",\"sequence\":{sequence},\"ev\":\"TaskUpdated\",\"task\":{{\"id\":\"task-{id}\",\"owner\":\"peer\",\"created_by\":\"master\",\"cwd\":\"/repo\",\"worktree_path\":\"playground/{id}\",\"base_commit\":\"base-{id}\",\"status\":\"working\",\"created_ms\":1,\"updated_ms\":1}}}}\n"
        )
    }

    fn canonical_options() -> InspectOptions {
        InspectOptions {
            source_path: Some(PathBuf::from("/repo/.agent-collab/server/journal.jsonl")),
            canonical_project_cwd: Some(PathBuf::from("/repo")),
        }
    }

    fn canonical_event_bytes(
        event: Event,
        record_id: Option<&str>,
        sequence: Option<u64>,
        final_newline: bool,
    ) -> Vec<u8> {
        let mut value = serde_json::to_value(event).expect("canonical event serializes");
        if let Some(record_id) = record_id {
            value["record_id"] = Value::String(record_id.into());
        }
        if let Some(sequence) = sequence {
            value["sequence"] = Value::Number(sequence.into());
        }

        let mut bytes = serde_json::to_vec(&value).expect("canonical event value serializes");
        if final_newline {
            bytes.push(b'\n');
        }
        bytes
    }

    #[test]
    fn valid_prefix_retains_raw_bytes_and_deterministic_digest() {
        let bytes = direct_line("one", 1).into_bytes();
        let report = inspect_jsonl(&bytes);
        assert_eq!(report.classification, MappingClass::Direct);
        assert_eq!(report.records.len(), 1);
        assert_eq!(report.records[0].line_number, 1);
        assert_eq!(report.records[0].raw_bytes, bytes);
        assert_eq!(report.records[0].digest, digest_bytes(&bytes));
        assert!(report.issues.is_empty());
        assert!(report.verify_digest(&report.source_digest).is_ok());
    }

    #[test]
    fn canonical_event_without_legacy_identity_is_adapted() {
        let bytes = canonical_event_bytes(
            Event::WakeAttempted {
                ids: vec!["message-1".into()],
                attempted_ms: 42,
            },
            None,
            None,
            true,
        );
        let report = inspect_jsonl(&bytes);

        assert_eq!(report.classification, MappingClass::Adapt);
        assert_eq!(report.records[0].classification, MappingClass::Adapt);
        assert!(report.has_error("LEGACY_RECORD_ID_ABSENT"));
        assert!(report.has_error("LEGACY_SEQUENCE_ABSENT"));
        assert!(!report.has_error("INVALID_EVENT"));
    }

    #[test]
    fn concatenated_canonical_events_fail_jsonl_framing_closed() {
        let first = canonical_event_bytes(
            Event::WakeAttempted {
                ids: vec!["message-1".into()],
                attempted_ms: 42,
            },
            Some("event-1"),
            Some(1),
            false,
        );
        let second = canonical_event_bytes(
            Event::WakeAttempted {
                ids: vec!["message-2".into()],
                attempted_ms: 43,
            },
            Some("event-2"),
            Some(2),
            false,
        );
        let mut bytes = first;
        bytes.extend_from_slice(&second);
        bytes.push(b'\n');

        let report = inspect_jsonl(&bytes);

        assert_eq!(report.classification, MappingClass::Unknown);
        assert!(report.has_error("MALFORMED_JSON_TAIL"));
    }

    #[test]
    fn canonical_event_without_final_newline_is_unknown() {
        let bytes = canonical_event_bytes(
            Event::WakeAttempted {
                ids: vec!["message-1".into()],
                attempted_ms: 42,
            },
            Some("event-1"),
            Some(1),
            false,
        );
        let report = inspect_jsonl(&bytes);

        assert_eq!(report.classification, MappingClass::Unknown);
        assert!(report.has_error("MISSING_FINAL_NEWLINE"));
        assert!(!report.has_error("INVALID_EVENT"));
    }

    #[test]
    fn duplicate_event_tag_is_unknown_even_when_value_collapses_it() {
        let bytes = br#"{"record_id":"event-1","sequence":1,"ev":"FutureEvent","ev":"WakeAttempted","ids":["message-1"],"attempted_ms":42}
"#;
        let report = inspect_jsonl(bytes);

        assert_eq!(report.classification, MappingClass::Unknown);
        assert!(report.has_error("DUPLICATE_JSON_KEY:ev"));
    }

    #[test]
    fn duplicate_canonical_event_field_is_unknown() {
        let bytes = br#"{"record_id":"event-1","sequence":1,"ev":"WakeAttempted","ids":["message-1"],"ids":["message-2"],"attempted_ms":42}
"#;
        let report = inspect_jsonl(bytes);

        assert_eq!(report.classification, MappingClass::Unknown);
        assert!(report.has_error("DUPLICATE_JSON_KEY:ids"));
    }

    #[test]
    fn duplicate_envelope_key_is_unknown_even_when_event_is_valid() {
        let bytes = br#"{"record_id":"event-1","record_id":"event-2","sequence":1,"ev":"WakeAttempted","ids":["message-1"],"attempted_ms":42}
"#;
        let report = inspect_jsonl(bytes);

        assert_eq!(report.classification, MappingClass::Unknown);
        assert!(report.has_error("DUPLICATE_JSON_KEY:record_id"));
    }

    #[test]
    fn empty_jsonl_line_is_unknown() {
        let report = inspect_jsonl(b"\n");

        assert_eq!(report.classification, MappingClass::Unknown);
        assert!(report.has_error("EMPTY_LINE"));
    }

    #[test]
    fn canonical_cwd_accepts_bound_worktree_context() {
        let bytes = direct_line("one", 1);
        let report = inspect_jsonl_with_options(bytes.as_bytes(), &canonical_options());
        assert_eq!(report.classification, MappingClass::Direct);
        assert_eq!(
            report.source_path,
            Some(PathBuf::from("/repo/.agent-collab/server/journal.jsonl"))
        );
    }

    #[test]
    fn foreign_cwd_is_unknown_even_when_binding_fields_are_present() {
        let bytes = b"{\"record_id\":\"task-1\",\"sequence\":1,\"ev\":\"TaskUpdated\",\"task\":{\"id\":\"task-1\",\"owner\":\"peer\",\"created_by\":\"master\",\"cwd\":\"/other\",\"worktree_path\":\"playground/task-1\",\"base_commit\":\"base\",\"status\":\"working\",\"created_ms\":1,\"updated_ms\":1}}\n";
        let report = inspect_jsonl_with_options(bytes, &canonical_options());
        assert_eq!(report.classification, MappingClass::Unknown);
        assert_eq!(
            report.records[0].exact_error.as_deref(),
            Some("CWD_OUTSIDE_PROJECT_SCOPE:expected=/repo:observed=/other")
        );
        assert_eq!(
            report.records[0].first_failed_boundary.as_deref(),
            Some("scope")
        );
    }

    #[test]
    fn foreign_worktree_is_unknown_even_when_cwd_matches() {
        let bytes = b"{\"record_id\":\"task-1\",\"sequence\":1,\"ev\":\"TaskUpdated\",\"task\":{\"id\":\"task-1\",\"owner\":\"peer\",\"created_by\":\"master\",\"cwd\":\"/repo\",\"worktree_path\":\"/other/playground/task-1\",\"base_commit\":\"base\",\"status\":\"working\",\"created_ms\":1,\"updated_ms\":1}}\n";
        let report = inspect_jsonl_with_options(bytes, &canonical_options());
        assert_eq!(report.classification, MappingClass::Unknown);
        assert_eq!(
            report.records[0].exact_error.as_deref(),
            Some("WORKTREE_OUTSIDE_PROJECT_SCOPE:expected=/repo:observed=/other/playground/task-1")
        );
        assert_eq!(
            report.records[0].first_failed_boundary.as_deref(),
            Some("binding")
        );
    }

    #[test]
    fn foreign_registered_cwd_is_not_direct() {
        let bytes = b"{\"record_id\":\"worker-1\",\"sequence\":1,\"ev\":\"Registered\",\"worker\":{\"id\":\"peer\",\"token\":\"token\",\"pane\":\"%1\",\"cwd\":\"/other\",\"registered_ms\":1}}\n";
        let report = inspect_jsonl_with_options(bytes, &canonical_options());
        assert_eq!(report.classification, MappingClass::Unknown);
        assert!(report.has_error("CWD_OUTSIDE_PROJECT_SCOPE"));
    }

    #[test]
    fn canonical_context_does_not_fill_missing_active_task_cwd() {
        let bytes = b"{\"record_id\":\"task-1\",\"sequence\":1,\"ev\":\"TaskUpdated\",\"task\":{\"id\":\"task-1\",\"owner\":\"peer\",\"created_by\":\"master\",\"worktree_path\":\"playground/task-1\",\"base_commit\":\"base\",\"status\":\"working\",\"created_ms\":1,\"updated_ms\":1}}\n";
        let report = inspect_jsonl_with_options(bytes, &canonical_options());
        assert_eq!(report.classification, MappingClass::Reset);
        assert_eq!(report.records[0].cwd, None);
        assert_eq!(
            report.records[0].exact_error.as_deref(),
            Some("MISSING_BINDING_EVIDENCE:cwd")
        );
        assert_eq!(
            report.records[0].first_failed_boundary.as_deref(),
            Some("binding")
        );
    }

    #[test]
    fn non_absolute_canonical_context_is_unknown() {
        let options = InspectOptions {
            source_path: None,
            canonical_project_cwd: Some(PathBuf::from("repo")),
        };
        let report = inspect_jsonl_with_options(direct_line("one", 1).as_bytes(), &options);
        assert_eq!(report.classification, MappingClass::Unknown);
        assert!(report.has_error("INVALID_CANONICAL_PROJECT_CWD"));
    }

    #[test]
    fn unknown_or_missing_event_cannot_be_direct() {
        let unknown =
            inspect_jsonl(b"{\"record_id\":\"one\",\"sequence\":1,\"ev\":\"FutureEvent\"}\n");
        assert_eq!(unknown.classification, MappingClass::Unknown);
        assert_eq!(
            unknown.records[0].exact_error.as_deref(),
            Some("UNKNOWN_EVENT:FutureEvent")
        );

        let missing = inspect_jsonl(b"{\"record_id\":\"one\",\"sequence\":1}\n");
        assert_eq!(missing.classification, MappingClass::Unknown);
        assert_eq!(
            missing.records[0].exact_error.as_deref(),
            Some("EVENT_ABSENT")
        );
    }

    #[test]
    fn sent_without_message_payload_is_unknown() {
        let report = inspect_jsonl(b"{\"record_id\":\"sent-1\",\"sequence\":1,\"ev\":\"Sent\"}\n");
        assert_eq!(report.classification, MappingClass::Unknown);
        assert_eq!(report.records[0].classification, MappingClass::Unknown);
        assert!(report.records[0]
            .exact_error
            .as_deref()
            .is_some_and(|error| error.starts_with("INVALID_EVENT:Sent:")));
        assert_eq!(
            report.records[0].first_failed_boundary.as_deref(),
            Some("schema")
        );
    }

    #[test]
    fn sent_with_non_object_message_payload_is_unknown() {
        let report = inspect_jsonl(
            b"{\"record_id\":\"sent-1\",\"sequence\":1,\"ev\":\"Sent\",\"msg\":null}\n",
        );
        assert_eq!(report.classification, MappingClass::Unknown);
        assert!(report.records[0]
            .exact_error
            .as_deref()
            .is_some_and(|error| error.starts_with("INVALID_EVENT:Sent:")));
    }

    #[test]
    fn sent_with_incomplete_message_payload_is_unknown() {
        let report = inspect_jsonl(
            b"{\"record_id\":\"sent-1\",\"sequence\":1,\"ev\":\"Sent\",\"msg\":{}}\n",
        );
        assert_eq!(report.classification, MappingClass::Unknown);
        assert!(report.records[0]
            .exact_error
            .as_deref()
            .is_some_and(|error| error.starts_with("INVALID_EVENT:Sent:")));
    }

    #[test]
    fn valid_sent_message_payload_can_be_direct() {
        let report = inspect_jsonl(
            b"{\"record_id\":\"sent-1\",\"sequence\":1,\"ev\":\"Sent\",\"msg\":{\"id\":\"message-1\",\"from\":\"peer-a\",\"to\":\"peer-b\",\"type\":\"notify\",\"subject\":\"subject\",\"body\":\"body\",\"in_reply_to\":null,\"created_ms\":1,\"state\":\"pending\"}}\n",
        );
        assert_eq!(report.classification, MappingClass::Direct);
        assert_eq!(report.records[0].classification, MappingClass::Direct);
        assert_eq!(report.records[0].entity_id.as_deref(), Some("message-1"));
    }

    #[test]
    fn sent_without_optional_reply_reference_can_be_direct() {
        let report = inspect_jsonl(
            b"{\"record_id\":\"sent-optional-1\",\"sequence\":1,\"ev\":\"Sent\",\"msg\":{\"id\":\"message-optional-1\",\"from\":\"peer-a\",\"to\":\"peer-b\",\"type\":\"notify\",\"subject\":\"subject\",\"body\":\"body\",\"created_ms\":1,\"state\":\"pending\"}}\n",
        );
        assert_eq!(report.classification, MappingClass::Direct);
        assert_eq!(report.records[0].classification, MappingClass::Direct);
        assert!(report.records[0].exact_error.is_none());
    }

    #[test]
    fn sent_accepts_legacy_wakeup_aliases_when_schema_is_complete() {
        let report = inspect_jsonl(
            b"{\"record_id\":\"sent-alias-1\",\"sequence\":1,\"ev\":\"Sent\",\"msg\":{\"id\":\"message-alias-1\",\"from\":\"peer-a\",\"to\":\"peer-b\",\"type\":\"notify\",\"subject\":\"subject\",\"body\":\"body\",\"in_reply_to\":null,\"created_ms\":1,\"state\":\"pending\",\"nudge_count\":2,\"last_nudge_ms\":3}}\n",
        );
        assert_eq!(report.classification, MappingClass::Direct);
        assert!(report.records[0].exact_error.is_none());
    }

    #[test]
    fn sent_rejects_wrong_type_wakeup_count_alias() {
        let report = inspect_jsonl(
            b"{\"record_id\":\"sent-alias-bad-count\",\"sequence\":1,\"ev\":\"Sent\",\"msg\":{\"id\":\"message-alias-bad-count\",\"from\":\"peer-a\",\"to\":\"peer-b\",\"type\":\"notify\",\"subject\":\"subject\",\"body\":\"body\",\"in_reply_to\":null,\"created_ms\":1,\"state\":\"pending\",\"nudge_count\":\"2\"}}\n",
        );
        assert_eq!(report.classification, MappingClass::Unknown);
        assert!(report.records[0]
            .exact_error
            .as_deref()
            .is_some_and(|error| error.starts_with("INVALID_EVENT:Sent:")));
    }

    #[test]
    fn sent_rejects_wrong_type_last_wakeup_alias() {
        let report = inspect_jsonl(
            b"{\"record_id\":\"sent-alias-bad-time\",\"sequence\":1,\"ev\":\"Sent\",\"msg\":{\"id\":\"message-alias-bad-time\",\"from\":\"peer-a\",\"to\":\"peer-b\",\"type\":\"notify\",\"subject\":\"subject\",\"body\":\"body\",\"in_reply_to\":null,\"created_ms\":1,\"state\":\"pending\",\"last_nudge_ms\":\"3\"}}\n",
        );
        assert_eq!(report.classification, MappingClass::Unknown);
        assert!(report.records[0]
            .exact_error
            .as_deref()
            .is_some_and(|error| error.starts_with("INVALID_EVENT:Sent:")));
    }

    #[test]
    fn registered_without_worker_payload_is_unknown() {
        let report = inspect_jsonl(
            b"{\"record_id\":\"worker-missing\",\"sequence\":1,\"ev\":\"Registered\"}\n",
        );
        assert_eq!(report.classification, MappingClass::Unknown);
        assert_eq!(report.records[0].classification, MappingClass::Unknown);
        assert!(report.records[0]
            .exact_error
            .as_deref()
            .is_some_and(|error| error.starts_with("INVALID_EVENT:Registered:")));
        assert_eq!(
            report.records[0].first_failed_boundary.as_deref(),
            Some("schema")
        );
    }

    #[test]
    fn registered_with_wrong_worker_type_is_unknown() {
        let report = inspect_jsonl(
            b"{\"record_id\":\"worker-wrong-type\",\"sequence\":1,\"ev\":\"Registered\",\"worker\":[] }\n",
        );
        assert_eq!(report.classification, MappingClass::Unknown);
        assert!(report.records[0]
            .exact_error
            .as_deref()
            .is_some_and(|error| error.starts_with("INVALID_EVENT:Registered:")));
    }

    #[test]
    fn registered_with_invalid_worker_object_is_unknown() {
        let report = inspect_jsonl(
            b"{\"record_id\":\"worker-invalid-object\",\"sequence\":1,\"ev\":\"Registered\",\"worker\":{}}\n",
        );
        assert_eq!(report.classification, MappingClass::Unknown);
        assert!(report.records[0]
            .exact_error
            .as_deref()
            .is_some_and(|error| error.starts_with("INVALID_EVENT:Registered:")));
    }

    #[test]
    fn complete_registered_payload_can_be_direct() {
        let report = inspect_jsonl(
            b"{\"record_id\":\"worker-valid\",\"sequence\":1,\"ev\":\"Registered\",\"worker\":{\"id\":\"peer\",\"token\":\"token\",\"pane\":\"%1\",\"cwd\":\"/repo\",\"registered_ms\":1}}\n",
        );
        assert_eq!(report.classification, MappingClass::Direct);
        assert_eq!(report.records[0].classification, MappingClass::Direct);
        assert!(report.records[0].exact_error.is_none());
    }

    #[test]
    fn delivered_without_ids_is_unknown() {
        let report = inspect_jsonl(
            b"{\"record_id\":\"delivered-missing-ids\",\"sequence\":1,\"ev\":\"Delivered\"}\n",
        );
        assert_eq!(report.classification, MappingClass::Unknown);
        assert!(report.records[0]
            .exact_error
            .as_deref()
            .is_some_and(|error| error.starts_with("INVALID_EVENT:Delivered:")));
    }

    #[test]
    fn acked_without_ids_is_unknown() {
        let report = inspect_jsonl(
            b"{\"record_id\":\"acked-missing-ids\",\"sequence\":1,\"ev\":\"Acked\"}\n",
        );
        assert_eq!(report.classification, MappingClass::Unknown);
        assert!(report.records[0]
            .exact_error
            .as_deref()
            .is_some_and(|error| error.starts_with("INVALID_EVENT:Acked:")));
    }

    #[test]
    fn unknown_schema_failure_survives_active_binding_reset() {
        let bytes = b"{\"record_id\":\"task-1\",\"sequence\":1,\"ev\":\"FutureTaskEvent\",\"task\":{\"id\":\"task-1\",\"owner\":\"peer\",\"status\":\"working\"}}\n";
        let report = inspect_jsonl(bytes);
        assert_eq!(report.classification, MappingClass::Unknown);
        assert_eq!(report.records[0].classification, MappingClass::Unknown);
        assert_eq!(
            report.records[0].exact_error.as_deref(),
            Some("UNKNOWN_EVENT:FutureTaskEvent")
        );
        assert_eq!(
            report.records[0].first_failed_boundary.as_deref(),
            Some("schema")
        );
    }

    #[test]
    fn scope_failure_survives_active_binding_reset() {
        let bytes = b"{\"record_id\":\"task-1\",\"sequence\":1,\"ev\":\"TaskUpdated\",\"task\":{\"id\":\"task-1\",\"owner\":\"peer\",\"created_by\":\"master\",\"cwd\":\"/other\",\"worktree_path\":\"playground/task-1\",\"base_commit\":\"base\",\"status\":\"working\",\"created_ms\":1,\"updated_ms\":1}}\n";
        let report = inspect_jsonl_with_options(bytes, &canonical_options());
        assert_eq!(report.classification, MappingClass::Unknown);
        assert_eq!(report.records[0].classification, MappingClass::Unknown);
        assert_eq!(
            report.records[0].exact_error.as_deref(),
            Some("CWD_OUTSIDE_PROJECT_SCOPE:expected=/repo:observed=/other")
        );
        assert_eq!(
            report.records[0].first_failed_boundary.as_deref(),
            Some("scope")
        );
    }

    #[test]
    fn unknown_task_status_cannot_be_direct() {
        let bytes = b"{\"record_id\":\"task-1\",\"sequence\":1,\"ev\":\"TaskUpdated\",\"task\":{\"id\":\"task-1\",\"owner\":\"peer\",\"created_by\":\"master\",\"cwd\":\"/repo\",\"worktree_path\":\"playground/task-1\",\"base_commit\":\"base\",\"status\":\"future\",\"created_ms\":1,\"updated_ms\":1}}\n";
        let report = inspect_jsonl(bytes);
        assert_eq!(report.classification, MappingClass::Unknown);
        assert_eq!(
            report.records[0].exact_error.as_deref(),
            Some("UNKNOWN_TASK_STATUS:future")
        );
    }

    #[test]
    fn maximum_sequence_overflow_is_fail_closed() {
        let bytes = format!(
            "{}{}",
            direct_line("max-a", u64::MAX),
            direct_line("max-b", u64::MAX)
        );
        let report = inspect_jsonl(bytes.as_bytes());
        assert_eq!(report.classification, MappingClass::Unknown);
        assert!(report.has_error("SEQUENCE_OVERFLOW"));
        assert!(report
            .records
            .iter()
            .all(|record| record.classification == MappingClass::Unknown));
    }

    #[test]
    fn malformed_middle_preserves_valid_prefix_and_suffix() {
        let bytes = format!(
            "{}{{bad json}}\n{}",
            direct_line("one", 1),
            direct_line("two", 2)
        );
        let report = inspect_jsonl(bytes.as_bytes());
        assert_eq!(report.classification, MappingClass::Unknown);
        assert_eq!(report.records.len(), 3);
        assert_eq!(report.records[0].line_number, 1);
        assert_eq!(report.records[1].line_number, 2);
        assert_eq!(report.records[1].raw_bytes, b"{bad json}\n");
        assert_eq!(report.records[2].line_number, 3);
        assert!(report.has_error("MALFORMED_JSON_MIDDLE"));
    }

    #[test]
    fn malformed_records_do_not_create_replay_errors() {
        let bytes = format!(
            "{}{{broken}}\n{}",
            direct_line("one", 1),
            direct_line("two", 2)
        );
        let report = inspect_jsonl(bytes.as_bytes());
        assert!(report.has_error("MALFORMED_JSON_MIDDLE"));
        assert!(!report.has_error("MISSING_SEQUENCE"));
        assert_eq!(report.records[1].classification, MappingClass::Unknown);
    }

    #[test]
    fn malformed_tail_and_missing_newline_are_explicit() {
        let bytes = format!("{}{{\"record_id\":\"tail\"", direct_line("one", 1));
        let report = inspect_jsonl(bytes.as_bytes());
        assert_eq!(report.classification, MappingClass::Unknown);
        assert_eq!(report.records.len(), 2);
        assert_eq!(report.records[1].line_number, 2);
        assert_eq!(report.records[1].raw_bytes, b"{\"record_id\":\"tail\"");
        assert!(report.has_error("MALFORMED_JSON_TAIL"));
        assert!(report.has_error("MISSING_FINAL_NEWLINE"));
    }

    #[test]
    fn valid_json_that_is_not_an_object_is_not_a_record() {
        let report = inspect_jsonl(b"[]\n");
        assert_eq!(report.classification, MappingClass::Unknown);
        assert_eq!(report.records.len(), 1);
        assert_eq!(report.records[0].raw_bytes, b"[]\n");
        assert_eq!(
            report.records[0].exact_error.as_deref(),
            Some("INVALID_RECORD_SHAPE")
        );
        assert!(report.has_error("INVALID_RECORD_SHAPE"));
    }

    #[test]
    fn duplicate_record_ids_fail_closed() {
        let first = direct_line("same", 1);
        let second = direct_line("same", 2);
        let report = inspect_jsonl(format!("{first}{second}").as_bytes());
        assert_eq!(report.classification, MappingClass::Unknown);
        assert!(report.has_error("DUPLICATE_RECORD_ID"));
        assert!(report
            .records
            .iter()
            .all(|record| record.classification == MappingClass::Unknown));
    }

    #[test]
    fn sequence_gap_is_not_replayable() {
        let bytes = format!("{}{}", direct_line("one", 1), direct_line("three", 3));
        let report = inspect_jsonl(bytes.as_bytes());
        assert_eq!(report.classification, MappingClass::Unknown);
        assert!(report.has_error("SEQUENCE_GAP"));
        assert_eq!(report.records[1].classification, MappingClass::Unknown);
    }

    #[test]
    fn unknown_owner_is_operator_input() {
        let bytes = b"{\"record_id\":\"task-1\",\"sequence\":1,\"ev\":\"TaskUpdated\",\"task\":{\"id\":\"task-1\",\"status\":\"working\",\"created_by\":\"master\",\"cwd\":\"/repo\",\"worktree_path\":\"playground/task-1\",\"base_commit\":\"base\",\"created_ms\":1,\"updated_ms\":1}}\n";
        let report = inspect_jsonl(bytes);
        assert_eq!(report.classification, MappingClass::Unknown);
        assert_eq!(report.records[0].classification, MappingClass::Unknown);
        assert_eq!(
            report.records[0].exact_error.as_deref(),
            Some("INVALID_EVENT:TaskUpdated:missing field `owner`")
        );
    }

    #[test]
    fn active_task_without_binding_is_reset() {
        let bytes = b"{\"record_id\":\"task-1\",\"sequence\":1,\"ev\":\"TaskUpdated\",\"task\":{\"id\":\"task-1\",\"owner\":\"peer\",\"created_by\":\"master\",\"status\":\"working\",\"created_ms\":1,\"updated_ms\":1}}\n";
        let report = inspect_jsonl(bytes);
        assert_eq!(report.classification, MappingClass::Reset);
        assert_eq!(report.records[0].classification, MappingClass::Reset);
        assert_eq!(
            report.records[0].exact_error.as_deref(),
            Some("MISSING_BINDING_EVIDENCE:worktree,base_commit,cwd")
        );
    }

    #[test]
    fn rework_task_without_binding_is_reset() {
        let bytes = b"{\"record_id\":\"task-1\",\"sequence\":1,\"ev\":\"TaskUpdated\",\"task\":{\"id\":\"task-1\",\"owner\":\"peer\",\"created_by\":\"master\",\"status\":\"rework\",\"created_ms\":1,\"updated_ms\":1}}\n";
        let report = inspect_jsonl(bytes);
        assert_eq!(report.classification, MappingClass::Reset);
        assert_eq!(report.records[0].classification, MappingClass::Reset);
        assert_eq!(
            report.records[0].exact_error.as_deref(),
            Some("MISSING_BINDING_EVIDENCE:worktree,base_commit,cwd")
        );
    }

    #[test]
    fn legacy_records_are_adapted_without_inventing_sequence() {
        let bytes = b"{\"ev\":\"Registered\",\"worker\":{\"id\":\"peer\",\"token\":\"token\",\"pane\":\"%1\",\"cwd\":\"/repo\",\"registered_ms\":1}}\n";
        let report = inspect_jsonl(bytes);
        assert_eq!(report.classification, MappingClass::Adapt);
        assert_eq!(report.records[0].classification, MappingClass::Adapt);
        assert!(report.has_error("LEGACY_SEQUENCE_ABSENT"));
    }

    #[test]
    fn duplicate_registration_requires_identity_reset() {
        let bytes = b"{\"ev\":\"Registered\",\"worker\":{\"id\":\"peer\",\"token\":\"token\",\"pane\":\"%1\",\"cwd\":\"/repo\",\"registered_ms\":1}}\n{\"ev\":\"Registered\",\"worker\":{\"id\":\"peer\",\"token\":\"token\",\"pane\":\"%1\",\"cwd\":\"/repo\",\"registered_ms\":2}}\n";
        let report = inspect_jsonl(bytes);
        assert_eq!(report.classification, MappingClass::Reset);
        assert!(report.has_error("DUPLICATE_REGISTRATION"));
        assert!(report
            .records
            .iter()
            .all(|record| record.classification == MappingClass::Reset));
    }

    #[test]
    fn digest_drift_is_reported_exactly() {
        let report = inspect_jsonl(direct_line("one", 1).as_bytes());
        let error = report
            .verify_digest("fnv1a64:0000000000000000")
            .unwrap_err();
        assert!(error
            .to_string()
            .starts_with("DIGEST_DRIFT:expected=fnv1a64:0000000000000000:observed=sha256:"));
    }

    #[test]
    fn sha256_digest_matches_known_vector() {
        assert_eq!(
            digest_bytes(b"abc"),
            "sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn terminal_task_without_binding_is_historical_adapt() {
        let bytes = b"{\"record_id\":\"task-1\",\"sequence\":1,\"ev\":\"TaskUpdated\",\"task\":{\"id\":\"task-1\",\"owner\":\"peer\",\"created_by\":\"master\",\"status\":\"closed\",\"created_ms\":1,\"updated_ms\":1}}\n";
        let report = inspect_jsonl(bytes);
        assert_eq!(report.classification, MappingClass::Adapt);
        assert_eq!(report.records[0].classification, MappingClass::Adapt);
        assert!(report.has_error("LEGACY_BINDING_EVIDENCE_ABSENT"));
    }

    fn valid_snapshot(report: &InspectionReport) -> ImmutableSnapshot {
        ImmutableSnapshot::new(
            "migration-1",
            "project-1",
            report.source_digest.clone(),
            "archive/migration-1",
            report.source_digest.clone(),
            0,
        )
        .expect("snapshot")
    }

    fn valid_receipts(source_digest: &str, target_epoch: u64) -> MigrationReceiptSet {
        use crate::identity::{AgentId, OperationId};

        MigrationReceiptSet {
            writer: Some(
                MigrationWriterReceipt::new(
                    "migration-1",
                    "project-1",
                    None,
                    target_epoch,
                    source_digest,
                    AgentId::new("writer-1").unwrap(),
                    OperationId::new("writer-op-1").unwrap(),
                    7,
                    1,
                    1,
                )
                .unwrap(),
            ),
            identity_rebinds: Vec::new(),
            runtime_rebinds: Vec::new(),
        }
    }

    #[test]
    fn verified_prefix_preserves_the_direct_prefix_and_first_stop() {
        let bytes = format!(
            "{}{{bad json}}\n{}",
            direct_line("one", 1),
            direct_line("two", 2)
        );
        let report = inspect_jsonl(bytes.as_bytes());
        let prefix = VerifiedPrefix::from_report(&report).expect("direct prefix");

        assert_eq!(prefix.records.len(), 1);
        assert_eq!(prefix.records[0].record_id, "one");
        assert_eq!(prefix.records[0].sequence, 1);
        assert_eq!(prefix.stop_line, Some(2));
        assert!(prefix
            .stop_error
            .as_deref()
            .is_some_and(|error| error.starts_with("MALFORMED_JSON_MIDDLE:")));
        assert!(!prefix.complete);
        assert_eq!(prefix.source_digest, report.source_digest);
        assert_eq!(prefix.last_sequence(), Some(1));
    }

    #[test]
    fn first_failed_record_preserves_an_empty_prefix_and_exact_stop() {
        let bytes = b"{bad json}\n";
        let report = inspect_jsonl(bytes);
        let prefix = VerifiedPrefix::from_report(&report).expect("empty incomplete prefix");

        assert!(prefix.records.is_empty());
        assert!(!prefix.complete);
        assert_eq!(prefix.stop_line, Some(1));
        assert!(prefix
            .stop_error
            .as_deref()
            .is_some_and(|error| error.starts_with("MALFORMED_JSON_TAIL:")));

        let mut transaction = MigrationTransaction::new("migration-1", "project-1").unwrap();
        transaction.snapshot = Some(valid_snapshot(&report));
        transaction.target_epoch = Some(TargetEpoch::new(None, 2, 0).unwrap());
        transaction.verified_prefix = Some(prefix);
        transaction.receipts = Some(valid_receipts(&report.source_digest, 2));
        transaction.project_admission = ProjectAdmission::Verified;
        transaction.phase = MigrationPhase::Rebound;

        assert!(matches!(
            transaction.apply(2, 0),
            Err(MigrationContractError::PrefixBlocked {
                line: Some(1),
                exact_error,
            }) if exact_error.starts_with("MALFORMED_JSON_TAIL:")
        ));
    }

    #[test]
    fn serialized_prefix_requires_rebinding_to_exact_source_bytes() {
        let bytes = direct_line("one", 1).into_bytes();
        let report = inspect_jsonl(&bytes);
        let prefix = VerifiedPrefix::from_report(&report).expect("prefix");
        let serialized = serde_json::to_value(&prefix).expect("prefix serializes");
        assert!(serialized.get("raw_prefix_bytes").is_none());

        let mut decoded: VerifiedPrefix = serde_json::from_value(serialized).expect("prefix");
        assert!(matches!(
            decoded.validate(),
            Err(MigrationContractError::Invalid {
                field: "verified_prefix.evidence",
                ..
            })
        ));
        decoded
            .verify_against_bytes(&bytes)
            .expect("exact source bytes rebind the prefix");
        decoded.validate().expect("rebound prefix validates");

        let mut forged = serde_json::to_value(&prefix).expect("prefix serializes");
        forged["prefix_digest"] = Value::String(digest_bytes(b"forged"));
        let mut forged: VerifiedPrefix = serde_json::from_value(forged).expect("forged prefix");
        assert!(matches!(
            forged.verify_against_bytes(&bytes),
            Err(MigrationContractError::DigestMismatch {
                field: "verified_prefix.prefix_digest",
                ..
            })
        ));
    }

    #[test]
    fn reset_required_transaction_rejects_apply_before_side_effects() {
        let report = inspect_jsonl(direct_line("one", 1).as_bytes());
        let snapshot = valid_snapshot(&report);
        let prefix = VerifiedPrefix::from_report(&report).expect("prefix");
        let receipts = valid_receipts(&report.source_digest, 2);
        let mut transaction = MigrationTransaction::new("migration-1", "project-1").unwrap();
        transaction.snapshot = Some(snapshot);
        transaction.target_epoch = Some(TargetEpoch::new(None, 2, 0).unwrap());
        transaction.verified_prefix = Some(prefix);
        transaction.receipts = Some(receipts);
        transaction.project_admission = ProjectAdmission::ResetRequired;

        assert!(matches!(
            transaction.apply(2, 0),
            Err(MigrationContractError::AdmissionBlocked {
                admission: ProjectAdmission::ResetRequired
            })
        ));
    }

    #[test]
    fn migration_apply_requires_snapshot_epoch_prefix_and_receipts() {
        let mut transaction = MigrationTransaction::new("migration-1", "project-1").unwrap();
        transaction.project_admission = ProjectAdmission::Verified;
        transaction.phase = MigrationPhase::Applied;

        assert!(matches!(
            transaction.apply(2, 0),
            Err(MigrationContractError::Missing("snapshot"))
        ));
    }

    #[test]
    fn migration_apply_rejects_an_incomplete_verified_prefix() {
        let bytes = format!(
            "{}{{bad json}}\n{}",
            direct_line("one", 1),
            direct_line("two", 2)
        );
        let report = inspect_jsonl(bytes.as_bytes());
        let mut transaction = MigrationTransaction::new("migration-1", "project-1").unwrap();
        transaction.snapshot = Some(valid_snapshot(&report));
        transaction.target_epoch = Some(TargetEpoch::new(None, 2, 0).unwrap());
        transaction.verified_prefix = Some(VerifiedPrefix::from_report(&report).unwrap());
        transaction.receipts = Some(valid_receipts(&report.source_digest, 2));
        transaction.project_admission = ProjectAdmission::Verified;
        transaction.phase = MigrationPhase::Rebound;

        assert!(matches!(
            transaction.apply(2, 0),
            Err(MigrationContractError::PrefixBlocked {
                line: Some(2),
                exact_error,
            }) if exact_error.starts_with("MALFORMED_JSON_MIDDLE:")
        ));
    }

    #[test]
    fn migration_apply_requires_rebound_phase_and_returns_a_pure_receipt() {
        let report = inspect_jsonl(direct_line("one", 1).as_bytes());
        let mut transaction = MigrationTransaction::new("migration-1", "project-1").unwrap();
        transaction.snapshot = Some(valid_snapshot(&report));
        transaction.target_epoch = Some(TargetEpoch::new(None, 2, 0).unwrap());
        transaction.verified_prefix = Some(VerifiedPrefix::from_report(&report).unwrap());
        transaction.receipts = Some(valid_receipts(&report.source_digest, 2));
        transaction.project_admission = ProjectAdmission::Verified;

        assert!(matches!(
            transaction.apply(2, 0),
            Err(MigrationContractError::Invalid { field: "phase", .. })
        ));

        transaction.phase = MigrationPhase::Rebound;
        assert!(matches!(
            transaction.apply(1, 0),
            Err(MigrationContractError::EpochMismatch {
                field: "target_epoch",
                expected: 1,
                observed: 2,
            })
        ));
        let receipt = transaction.apply(2, 0).expect("complete rebound applies");
        assert_eq!(receipt.migration_id, "migration-1");
        assert_eq!(receipt.source_project_id, "project-1");
        assert_eq!(receipt.target_epoch, 2);
        assert_eq!(receipt.source_snapshot_digest, report.source_digest);
        assert_eq!(receipt.writer_operation_id, "writer-op-1");
    }

    #[test]
    fn snapshot_allows_an_independent_archive_digest() {
        let report = inspect_jsonl(direct_line("one", 1).as_bytes());
        let mut snapshot = valid_snapshot(&report);
        snapshot.archive_digest = "sha256:other-archive".into();

        snapshot
            .validate()
            .expect("archive has independent evidence digest");
        snapshot
            .verify_source_digest(&report.source_digest)
            .expect("source digest remains bound");
    }

    #[test]
    fn source_epoch_zero_is_rejected_and_global_failures_keep_their_error() {
        let report = inspect_jsonl(direct_line("one", 1).as_bytes());
        let mut snapshot = valid_snapshot(&report);
        snapshot.source_epoch = Some(0);
        assert!(matches!(
            snapshot.validate(),
            Err(MigrationContractError::Invalid {
                field: "source_epoch",
                ..
            })
        ));

        let epoch = TargetEpoch {
            source_epoch: Some(0),
            target_epoch: 2,
            expected_active_revision: 0,
        };
        assert!(matches!(
            epoch.validate(),
            Err(MigrationContractError::Invalid {
                field: "source_epoch",
                ..
            })
        ));

        let epoch = TargetEpoch::new(Some(1), 2, 7).unwrap();
        assert!(matches!(
            epoch.validate_against(1, 8),
            Err(MigrationContractError::RevisionMismatch {
                field: "expected_active_revision",
                expected: 7,
                observed: 8,
            })
        ));

        let report = inspect_jsonl_with_options(
            direct_line("one", 1).as_bytes(),
            &InspectOptions {
                canonical_project_cwd: Some(PathBuf::from("relative")),
                ..InspectOptions::default()
            },
        );
        let prefix = VerifiedPrefix::from_report(&report).expect("prefix evidence");
        assert!(!prefix.complete);
        assert_eq!(prefix.stop_line, None);
        assert!(prefix
            .stop_error
            .as_deref()
            .is_some_and(|error| error.starts_with("INVALID_CANONICAL_PROJECT_CWD:")));
        prefix.validate().expect("global stop retains valid shape");
    }

    #[test]
    fn target_epoch_requires_strictly_new_active_epoch_when_source_is_unknown() {
        for active_epoch in [3, 4] {
            let epoch = TargetEpoch::new(None, 3, 7).unwrap();
            assert!(matches!(
                epoch.validate_against(active_epoch, 7),
                Err(MigrationContractError::Invalid {
                    field: "target_epoch",
                    reason,
                }) if reason.contains("greater than active_epoch")
            ));
        }

        let epoch = TargetEpoch::new(None, 4, 7).unwrap();
        epoch
            .validate_against(3, 7)
            .expect("unknown source still advances the active epoch");
    }

    #[test]
    fn migration_transaction_binds_snapshot_and_target_source_epochs() {
        let report = inspect_jsonl(direct_line("one", 1).as_bytes());
        let snapshot = valid_snapshot(&report).with_source_epoch(Some(3));
        let mut transaction = MigrationTransaction::new("migration-1", "project-1").unwrap();
        transaction.snapshot = Some(snapshot);
        transaction.target_epoch = Some(TargetEpoch::new(None, 4, 0).unwrap());

        assert!(matches!(
            transaction.validate(),
            Err(MigrationContractError::Invalid {
                field: "target_epoch.source_epoch",
                ..
            })
        ));

        transaction.target_epoch = Some(TargetEpoch::new(Some(3), 4, 0).unwrap());
        assert!(transaction.validate().is_ok());
    }

    #[test]
    fn migration_transaction_rejects_prefix_digest_drift() {
        let report = inspect_jsonl(direct_line("one", 1).as_bytes());
        let mut snapshot = valid_snapshot(&report);
        snapshot.source_digest = "sha256:other-source".into();
        snapshot.archive_digest = snapshot.source_digest.clone();
        let prefix = VerifiedPrefix::from_report(&report).expect("prefix");
        let mut transaction = MigrationTransaction::new("migration-1", "project-1").unwrap();
        transaction.snapshot = Some(snapshot);
        transaction.target_epoch = Some(TargetEpoch::new(None, 2, 0).unwrap());
        transaction.verified_prefix = Some(prefix);
        transaction.receipts = Some(valid_receipts(&report.source_digest, 2));

        assert!(matches!(
            transaction.validate(),
            Err(MigrationContractError::DigestMismatch {
                field: "verified_prefix.source_digest",
                ..
            })
        ));
    }
}
