//! Read-only inspection and classification of legacy governance JSONL.
//!
//! This module deliberately has no daemon or filesystem-state owner.  It reads
//! bytes supplied by the caller, keeps each source line verbatim, and returns a
//! deterministic report.  A later migration adapter can use the report to
//! archive, adapt, reset, or stop for operator input without replaying a
//! source journal while it is being inspected.

use crate::server::state::Event;
use serde::de::{DeserializeSeed, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::Value;
use std::collections::BTreeMap;
use std::error::Error;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The disposition of a source record in a future migration transaction.
#[derive(Debug, Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
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
#[derive(Debug, Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
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
#[derive(Debug, Clone, Copy, Eq, Ord, PartialEq, PartialOrd)]
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
}
