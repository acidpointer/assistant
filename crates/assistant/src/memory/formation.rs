//! Bounded staged proposition formation over one host-attributed user source.
//! Internal inference owns isolated native passes; the host resolves identities.
//! This module never commits or promotes proposals. Its caller owns a fenced job
//! and rechecks the source/read set. Failures retain phase, observed work and
//! redacted partial output; native bounds never masquerade as storage errors.

use super::{
    ExtractionAccounting, ExtractionChanges, MemoryError, MemoryId, MemoryJobUsage, MemoryResult,
    MemoryText, SourceRecord, SourceRole,
    extraction::{self, Domain, Proposal},
};
use crate::{
    chat::ChatError,
    engine::{Cancellation, FinishReason, GenerationMetrics, Usage},
    runtime::{InternalFailure, InternalInference, InternalLimits},
};
use std::time::{Duration, Instant};

/// One bounded stage, without personal text or storage-selected identifiers.
#[derive(Debug, Clone, Copy, serde::Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum FormationStage {
    /// Select person names from the offered source.
    Names,
    /// Classify source-local family relationships.
    Family,
    /// Extract source-local occupations.
    Occupation,
    /// Resolve host identities and typed evidence.
    Resolve,
}
impl std::fmt::Display for FormationStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Names => "names",
            Self::Family => "family",
            Self::Occupation => "occupation",
            Self::Resolve => "resolve",
        })
    }
}
/// Last observed output is diagnostic data, never a validated proposal.
#[derive(Debug)]
pub struct FormationOutput {
    /// Producing stage; it may precede the failed stage.
    pub stage: FormationStage,
    /// Untrusted JSON, redacted by Debug.
    pub json: MemoryText,
    /// Whether the pass reached EOG rather than a partial boundary.
    pub complete: bool,
}
/// Failure causes stay typed rather than being converted into storage outages.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FormationCause {
    /// Host structural/semantic validation or resource failure.
    #[error(transparent)]
    Validation(#[from] MemoryError),
    /// Native inference, including its original limit/deadline cause.
    #[error(transparent)]
    Inference(#[from] InternalFailure),
}
/// Failed source with observed accounting. An incomplete native boundary must
/// retain the caller's conservative reservation until it is safely reconciled.
#[derive(Debug, thiserror::Error)]
#[error("source formation failed during {stage}")]
pub struct FormationFailure {
    /// Stage whose validation/evaluation failed.
    pub stage: FormationStage,
    /// Original cause for trusted host diagnostics.
    #[source]
    pub cause: FormationCause,
    /// Observed tokens/time, including unsuccessful output.
    pub accounting: ExtractionAccounting,
    /// False if work may have continued beyond the observed native boundary.
    pub measurement_complete: bool,
    /// Redacted source-scoped output for explicit authorized diagnostics.
    pub output: Option<FormationOutput>,
}
impl FormationFailure {
    /// Sanitized diagnostic reason, without private native error prose.
    #[must_use]
    pub fn code(&self) -> &'static str {
        match &self.cause {
            FormationCause::Validation(MemoryError::InvalidInput(reason)) => reason,
            FormationCause::Validation(MemoryError::Capacity) => "root_capacity",
            FormationCause::Validation(_) => "validation_failure",
            FormationCause::Inference(failure) => match &failure.source {
                ChatError::Deadline => "inference_deadline",
                ChatError::Cancelled => "inference_cancelled",
                ChatError::Limit(reason) => reason,
                ChatError::Closed => "inference_closed",
                _ => "native_inference_failure",
            },
        }
    }
}
/// Examined source and structurally validated proposal, before qualification.
#[derive(Debug)]
pub struct FormationReport {
    /// Host-resolved propositions, not yet committed.
    pub changes: ExtractionChanges,
    /// Actual completed native token/time sums.
    pub usage: MemoryJobUsage,
    /// All dispatched passes, including unsuccessful/repair passes.
    pub passes: u8,
}

struct Work {
    stage: FormationStage,
    usage: MemoryJobUsage,
    passes: u8,
    measured: bool,
    output: Option<FormationOutput>,
}
impl Work {
    fn account(&mut self, usage: &Usage, metrics: Option<&GenerationMetrics>) -> MemoryResult<()> {
        self.usage.input_tokens = self
            .usage
            .input_tokens
            .checked_add(u32::try_from(usage.input_tokens).map_err(|_| MemoryError::Capacity)?)
            .ok_or(MemoryError::Capacity)?;
        self.usage.output_tokens = self
            .usage
            .output_tokens
            .checked_add(u32::try_from(usage.generated_tokens).map_err(|_| MemoryError::Capacity)?)
            .ok_or(MemoryError::Capacity)?;
        let Some(metrics) = metrics else {
            self.measured = false;
            return Ok(());
        };
        let nanos = metrics
            .prefill_ns
            .checked_add(metrics.generation_ns)
            .ok_or(MemoryError::Capacity)?;
        let millis = u32::try_from(nanos.div_ceil(1_000_000)).map_err(|_| MemoryError::Capacity)?;
        self.usage.native_millis = self
            .usage
            .native_millis
            .checked_add(millis)
            .ok_or(MemoryError::Capacity)?;
        Ok(())
    }
    fn failure(self, cause: FormationCause) -> FormationFailure {
        FormationFailure {
            stage: self.stage,
            cause,
            accounting: ExtractionAccounting {
                usage: self.usage,
                passes: self.passes,
            },
            measurement_complete: self.measured,
            output: self.output,
        }
    }
}

/// Form source-local propositions under one root deadline/native allowance.
/// Qualification is separate from structural validation; no graph write occurs.
/// # Errors
/// Carries the stage and original validation/native error. An unmeasured failure
/// cannot release the caller's durable conservative reservation.
pub async fn extract_source(
    inference: &InternalInference,
    source: &SourceRecord,
    principal: MemoryId,
    cancellation: Cancellation,
) -> Result<FormationReport, FormationFailure> {
    let mut work = Work {
        stage: FormationStage::Names,
        usage: MemoryJobUsage::default(),
        passes: 0,
        measured: true,
        output: None,
    };
    let result = extract(inference, source, principal, cancellation, &mut work).await;
    match result {
        Ok(changes) => Ok(FormationReport {
            changes,
            usage: work.usage,
            passes: work.passes,
        }),
        Err(cause) => Err(work.failure(cause)),
    }
}
async fn extract(
    inference: &InternalInference,
    source: &SourceRecord,
    principal: MemoryId,
    cancellation: Cancellation,
    work: &mut Work,
) -> Result<ExtractionChanges, FormationCause> {
    if source.role != SourceRole::User {
        return Err(MemoryError::InvalidInput("formation source role").into());
    }
    let text = source.source.text.as_str();
    let deadline = Instant::now() + Duration::from_secs(120);
    let names = infer(
        inference,
        extraction::people_task(text)?,
        cancellation.clone(),
        deadline,
        work,
        128,
    )
    .await?;
    let people = extraction::parse_people(names.as_str(), text)?;
    let mut proposal = Proposal { facts: Vec::new() };
    for domain in [Domain::Family, Domain::Occupation] {
        if domain == Domain::Family && people.is_empty() {
            continue;
        }
        work.stage = match domain {
            Domain::Family => FormationStage::Family,
            Domain::Occupation => FormationStage::Occupation,
        };
        let output = infer(
            inference,
            extraction::task_with_people(text, domain, &people)?,
            cancellation.clone(),
            deadline,
            work,
            512,
        )
        .await?;
        proposal
            .facts
            .extend(extraction::parse_with_people(output.as_str(), text, domain, &people)?.facts);
    }
    work.stage = FormationStage::Resolve;
    Ok(proposal.resolve(principal, &source.source)?)
}
async fn infer(
    inference: &InternalInference,
    task: extraction::ExtractionTask,
    cancellation: Cancellation,
    deadline: Instant,
    work: &mut Work,
    output_cap: usize,
) -> Result<MemoryText, FormationCause> {
    let input = 4096_u32
        .checked_sub(work.usage.input_tokens)
        .ok_or(MemoryError::Capacity)?
        .min(1024);
    let output = 1024_u32
        .checked_sub(work.usage.output_tokens)
        .ok_or(MemoryError::Capacity)?
        .min(512);
    if input == 0 || output == 0 || work.passes >= 4 {
        return Err(MemoryError::Capacity.into());
    }
    work.passes += 1;
    let limits = InternalLimits {
        input_tokens: usize::try_from(input).map_err(|_| MemoryError::Capacity)?,
        output_tokens: usize::try_from(output)
            .map_err(|_| MemoryError::Capacity)?
            .min(output_cap),
        deadline,
    };
    let result = match inference.extract_bounded(task, cancellation, limits).await {
        Ok(result) => result,
        Err(failure) => {
            work.measured = false;
            work.account(&failure.usage, failure.metrics.as_ref())?;
            if let Ok(json) = &failure.partial_json {
                work.output = Some(FormationOutput {
                    stage: work.stage,
                    json: json.clone(),
                    complete: false,
                });
            }
            return Err(failure.into());
        }
    };
    work.account(&result.usage, result.metrics.as_ref())?;
    work.output = Some(FormationOutput {
        stage: work.stage,
        json: result.json.clone(),
        complete: result.finish_reason == FinishReason::EndOfGeneration,
    });
    if !work.measured {
        return Err(MemoryError::InvalidInput("unmeasured formation").into());
    }
    if result.finish_reason != FinishReason::EndOfGeneration
        || work.usage.input_tokens > 4096
        || work.usage.output_tokens > 1024
        || work.usage.native_millis > 120_000
    {
        return Err(MemoryError::InvalidInput("incomplete formation").into());
    }
    Ok(result.json)
}
