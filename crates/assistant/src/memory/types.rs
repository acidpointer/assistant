//! Owned memory commands and evidence. Identifiers and sequence numbers validate
//! at construction/deserialization. Private text has redacted Debug output.
//! UTC milliseconds are supplied by the host, allowing deterministic replay.

use super::{MemoryError, MemoryResult};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::{fmt, path::PathBuf};

/// Stable random host identity, independent of a daemon or native context.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MemoryId([u8; 16]);

impl MemoryId {
    /// Generate a random 128-bit identity.
    /// # Errors
    /// Returns a sanitized error if operating-system entropy is unavailable.
    pub fn new() -> MemoryResult<Self> {
        let mut bytes = [0; 16];
        getrandom::fill(&mut bytes).map_err(|_| MemoryError::Unavailable)?;
        Ok(Self(bytes))
    }

    /// Parse exactly 32 lowercase hexadecimal digits.
    /// # Errors
    /// Rejects malformed, uppercase or truncated identities.
    pub fn parse(text: &str) -> MemoryResult<Self> {
        if text.len() != 32
            || !text
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(MemoryError::InvalidInput("identity"));
        }
        let mut bytes = [0; 16];
        for (i, pair) in text.as_bytes().chunks_exact(2).enumerate() {
            let digit = |b| if b <= b'9' { b - b'0' } else { b - b'a' + 10 };
            bytes[i] = (digit(pair[0]) << 4) | digit(pair[1]);
        }
        Ok(Self(bytes))
    }
}

impl fmt::Display for MemoryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}
impl fmt::Debug for MemoryId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
impl Serialize for MemoryId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.to_string())
    }
}
impl<'de> Deserialize<'de> for MemoryId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::parse(&String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// Positive SQLite-compatible version, serialized as a decimal string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Revision(i64);
impl Revision {
    /// Validate a positive signed-64-bit version.
    /// # Errors
    /// Rejects zero and negative versions.
    pub fn new(value: i64) -> MemoryResult<Self> {
        if value <= 0 {
            return Err(MemoryError::InvalidInput("revision"));
        }
        Ok(Self(value))
    }
    /// `SQLite` representation.
    #[must_use]
    pub fn get(self) -> i64 {
        self.0
    }
}
impl TryFrom<String> for Revision {
    type Error = MemoryError;
    fn try_from(value: String) -> MemoryResult<Self> {
        if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) || value.starts_with('0')
        {
            return Err(MemoryError::InvalidInput("revision"));
        }
        Self::new(
            value
                .parse()
                .map_err(|_| MemoryError::InvalidInput("revision"))?,
        )
    }
}
impl From<Revision> for String {
    fn from(value: Revision) -> Self {
        value.0.to_string()
    }
}

/// Bounded private UTF-8 content. Debug never prints the contents.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct MemoryText(String);
impl MemoryText {
    /// Validate nonempty text up to 128 KiB. Command-specific limits also apply.
    /// # Errors
    /// Rejects empty, NUL-containing and oversized input.
    pub fn new(value: impl Into<String>) -> MemoryResult<Self> {
        let value = value.into();
        if value.trim().is_empty() || value.len() > 131_072 || value.contains('\0') {
            return Err(MemoryError::InvalidInput("text"));
        }
        Ok(Self(value))
    }
    /// Read authorized content explicitly.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Debug for MemoryText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MemoryText([redacted])")
    }
}
impl TryFrom<String> for MemoryText {
    type Error = MemoryError;
    fn try_from(value: String) -> MemoryResult<Self> {
        Self::new(value)
    }
}
impl From<MemoryText> for String {
    fn from(value: MemoryText) -> Self {
        value.0
    }
}

/// Idempotency namespace and key, supplied by the host/client rather than a model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "(String, String)", into = "(String, String)")]
pub struct OperationKey {
    namespace: String,
    key: String,
}
impl OperationKey {
    /// Construct a bounded opaque request identity.
    /// # Errors
    /// Rejects empty, oversized or non-ASCII identifiers.
    pub fn new(namespace: impl Into<String>, key: impl Into<String>) -> MemoryResult<Self> {
        let value = Self {
            namespace: namespace.into(),
            key: key.into(),
        };
        if [&value.namespace, &value.key].into_iter().any(|s| {
            s.is_empty()
                || s.len() > 128
                || !s
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        }) {
            return Err(MemoryError::InvalidInput("operation key"));
        }
        Ok(value)
    }
    pub(super) fn parts(&self) -> (&str, &str) {
        (&self.namespace, &self.key)
    }
}
impl TryFrom<(String, String)> for OperationKey {
    type Error = MemoryError;
    fn try_from(v: (String, String)) -> MemoryResult<Self> {
        Self::new(v.0, v.1)
    }
}
impl From<OperationKey> for (String, String) {
    fn from(v: OperationKey) -> Self {
        (v.namespace, v.key)
    }
}

/// Startup-only store settings. The data directory must be private to its owner.
#[derive(Clone)]
pub struct MemoryConfig {
    pub(super) directory: PathBuf,
    pub(super) quota_bytes: u64,
    pub(super) pending_job_limit: u32,
}
impl MemoryConfig {
    /// Use an explicit host-selected directory and the v1 two-GiB soft quota.
    #[must_use]
    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
            quota_bytes: 2 * 1024 * 1024 * 1024,
            pending_job_limit: 10_000,
        }
    }
    /// Set a quota, retaining at least 64 MiB of emergency headroom.
    /// # Errors
    /// Rejects quotas smaller than 128 MiB or outside `SQLite`'s signed range.
    pub fn with_quota(mut self, bytes: u64) -> MemoryResult<Self> {
        if bytes < 128 * 1024 * 1024 || bytes > i64::MAX as u64 {
            return Err(MemoryError::InvalidInput("store quota"));
        }
        self.quota_bytes = bytes;
        Ok(self)
    }
    /// Bound optional pending formation without rejecting saved literal input.
    /// # Errors
    /// Rejects zero or limits above the v1 hard bound of 10,000 jobs.
    pub fn with_pending_job_limit(mut self, limit: u32) -> MemoryResult<Self> {
        if !(1..=10_000).contains(&limit) {
            return Err(MemoryError::InvalidInput("pending jobs"));
        }
        self.pending_job_limit = limit;
        Ok(self)
    }
}
impl fmt::Debug for MemoryConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MemoryConfig")
            .field("directory", &"[redacted]")
            .field("quota_bytes", &self.quota_bytes)
            .field("pending_job_limit", &self.pending_job_limit)
            .finish()
    }
}

/// A committed command and its stable result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryReceipt<T> {
    /// Durable mutation identity.
    pub operation_id: MemoryId,
    /// Commit visibility boundary.
    pub sequence: Revision,
    /// The original command's result.
    pub value: T,
    /// True when the original committed receipt was returned for a repeated key.
    pub replayed: bool,
}

/// A literal experience preserved independently of successful extraction.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Source {
    /// Stable episode identity.
    pub id: MemoryId,
    /// Immutable source version.
    pub revision: Revision,
    /// Original words, including their qualifications.
    pub text: MemoryText,
    /// Whether the owner explicitly requested preservation.
    pub preserved: bool,
}

/// Host-authored entity categories, including ordinary personal knowledge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum EntityKind {
    /// A human.
    Person,
    /// The persistent assistant.
    Agent,
    /// An organization.
    Organization,
    /// An ongoing project.
    Project,
    /// A desired outcome.
    Goal,
    /// An explicit promise or task.
    Commitment,
    /// An unresolved question.
    Question,
    /// A city, region or other location.
    Place,
    /// A profession or role.
    Occupation,
    /// A concept outside the narrower categories.
    Topic,
}

/// A named graph node; spelling is retained and does not prove identity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entity {
    /// Stable graph identity.
    pub id: MemoryId,
    /// Host-validated kind.
    pub kind: EntityKind,
    /// Original name; aliases never merge people implicitly.
    pub label: MemoryText,
}

/// Finite graph vocabulary. Every edge still needs qualified source evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Predicate {
    /// Person prefers an item.
    Prefers,
    /// Person is interested in a topic.
    InterestedIn,
    /// Person works on a project.
    WorksOn,
    /// Person belongs to an organization.
    MemberOf,
    /// Person/agent pursues a goal.
    HasGoal,
    /// Goal is blocked by another node.
    BlockedBy,
    /// Project/goal depends on another node.
    DependsOn,
    /// A stated reason relates to an outcome.
    ReasonFor,
    /// Explicit lifecycle status.
    Status,
    /// A commitment's explicit UTC instant.
    DueAt,
    /// An actor asks a question.
    Asks,
    /// Associative navigation; never a factual reasoning premise.
    AssociatedWith,
    /// Person has a sibling. Symmetry is navigation, not a new independent fact.
    SiblingOf,
    /// Person is parent of another person; direction is significant.
    ParentOf,
    /// Person is partner of another person.
    PartnerOf,
    /// Person has a profession.
    HasOccupation,
    /// Person lives at a location.
    LivesIn,
    /// A person or organization owns an item.
    Owns,
    /// An actor uses an item.
    Uses,
}

/// Literal or graph reference. Exactly one representation is serialized.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
#[non_exhaustive]
pub enum ClaimObject {
    /// Another graph node.
    Reference(MemoryId),
    /// A bounded literal value.
    Text(MemoryText),
    /// UTC milliseconds; scheduling additionally requires resolved civil time.
    Timestamp(i64),
}

/// Evidence references an exact quotation in an immutable source revision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimEvidence {
    /// Existing authorized source.
    pub source_id: MemoryId,
    /// Expected immutable version.
    pub source_revision: Revision,
    /// Exact unique text, resolved to UTF-8 byte offsets by the host.
    pub quote: MemoryText,
}

/// Whether words assert a fact or only describe a report/possibility.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ClaimModality {
    /// Direct source assertion, still subject to source attribution.
    Asserted,
    /// Attributed report, not direct testimony of the reported subject.
    Reported,
    /// Hypothetical possibility.
    Hypothetical,
    /// A question, not a positive fact.
    Question,
    /// Authored fiction.
    Fiction,
}

/// Candidate factual content. Construction alone does not prove entailment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimInput {
    /// Subject node.
    pub subject: MemoryId,
    /// Registered relationship.
    pub predicate: Predicate,
    /// Typed object.
    pub object: ClaimObject,
    /// Explicit negation.
    pub negative: bool,
    /// Assertion, report or nonfactual modality.
    pub modality: ClaimModality,
    /// Unknown endpoints remain absent; known intervals are half-open.
    pub valid_from: Option<i64>,
    /// Exclusive end of the known validity interval.
    pub valid_to: Option<i64>,
    /// Conditions must survive recall packing.
    pub conditions: Vec<MemoryText>,
    /// One to four exact source citations.
    pub evidence: Vec<ClaimEvidence>,
}

/// A current or historical assertion version with its original evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claim {
    /// Stable assertion identity, unchanged by correction.
    pub id: MemoryId,
    /// Immutable assertion version.
    pub revision: Revision,
    /// Qualified content.
    pub input: ClaimInput,
    /// When this interpretation was committed.
    pub recorded_from: Revision,
    /// When this interpretation was superseded.
    pub recorded_to: Option<Revision>,
}

/// Literal search results; semantic and graph channels have independent contracts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchBundle {
    /// Read snapshot's committed visibility boundary, if any data has committed.
    pub sequence: Option<Revision>,
    /// Owned source results.
    pub sources: Vec<Source>,
    /// The query stopped at its result/work allowance.
    pub truncated: bool,
}

/// Journaled suppression; product output barriers and physical purge are separate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuppressionReceipt {
    /// Suppressed source.
    pub source_id: MemoryId,
    /// Monotonic revocation generation.
    pub deletion_generation: Revision,
}

/// Bounded adjacency traversal results. These edges are not inferred conclusions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphBundle {
    /// Visibility boundary of the read snapshot.
    pub sequence: Option<Revision>,
    /// Current qualified edges encountered during traversal.
    pub claims: Vec<Claim>,
    /// Eligible labels for encountered nodes, with suppressed labels redacted.
    #[serde(default)]
    pub entities: Vec<Entity>,
    /// Navigation provenance, without transitive or causal conclusions.
    #[serde(default)]
    pub paths: Vec<GraphPath>,
    /// Host source roles/times for every supporting quotation, in this snapshot.
    #[serde(default)]
    pub sources: Vec<super::SourceAttribution>,
    /// True if work/result limits stopped the traversal.
    pub truncated: bool,
}

/// One adjacency step, retaining the original supporting assertion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphStep {
    /// Original assertion identity and revision.
    pub claim: MemoryId,
    /// Checked original assertion revision.
    pub revision: Revision,
    /// Node from which navigation examined the assertion.
    pub from: MemoryId,
    /// Reference reached, absent for a literal-object assertion.
    pub to: Option<MemoryId>,
}
/// Bounded provenance for locating an edge, not a logical derivation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GraphPath {
    /// Emitted assertion reached by this trace.
    pub claim: MemoryId,
    /// At most the requested depth; no manufactured inverse assertion.
    pub steps: Vec<GraphStep>,
}

/// A claimed extraction attempt. Private fence fields cannot come from model JSON.
#[derive(Debug, Clone)]
pub struct ExtractionLease {
    pub(crate) job: MemoryId,
    pub(crate) fence: Revision,
    pub(crate) instance: MemoryId,
    pub(crate) generation: i64,
    /// Immutable evidence offered to this attempt.
    pub source: Source,
}

/// Conservative model-work envelope chosen by the trusted formation controller.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum ExtractionBudget {
    /// One pass: at most 1,024 input and 512 sampled output tokens.
    SinglePass,
    /// Up to four passes sharing 4,096 input/1,024 output tokens and 120 seconds.
    StagedRoot,
}
impl ExtractionBudget {
    pub(crate) fn limits(self) -> (i64, i64, i64, i64) {
        match self {
            Self::SinglePass => (1, 1024, 512, 120_000),
            Self::StagedRoot => (4, 4096, 1024, 120_000),
        }
    }
}

/// Actual completed staged work, measured by the host native owner.
#[derive(Debug, Clone, Copy, Serialize)]
pub struct ExtractionAccounting {
    /// All evaluated input, sampled output and native time.
    pub usage: MemoryJobUsage,
    /// All dispatched passes, including failed or repair passes.
    pub passes: u8,
}

/// Accounted native work. Values come from the host inference worker.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct MemoryJobUsage {
    /// Evaluated input tokens, including repeated prefill.
    pub input_tokens: u32,
    /// All sampled output, including unsuccessful/repair output.
    pub output_tokens: u32,
    /// Native execution milliseconds.
    pub native_millis: u32,
}

/// Host-validated proposal application. Jobs do not commit model text directly.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ExtractionChanges {
    /// Host-allocated new entities, committed with their claims.
    pub entities: Vec<Entity>,
    /// At most four source-backed assertions.
    pub claims: Vec<ClaimInput>,
}

/// Terminal extraction result; unresolved evidence remains literally searchable.
#[derive(Debug, Clone, Copy, Serialize)]
#[non_exhaustive]
pub enum ExtractionOutcome {
    /// Complete source examination.
    Complete,
    /// No safe complete interpretation within the attempt allowance.
    Unresolved,
    /// Foreground work interrupted inference; retry keeps charged consumption.
    Deferred,
}
