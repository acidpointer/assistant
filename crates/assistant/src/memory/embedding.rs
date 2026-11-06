//! Resident E5 CPU worker. Weights and context live on one native owner thread;
//! only bounded text commands and normalized owned vectors cross its boundary.
//! Foreground queries have a separate reserved queue and precede indexing work.
//! Startup verifies the selected artifact before loading; it never downloads.
//! Explicit shutdown cancels queued work and joins native ownership.

use super::{MemoryError, MemoryResult, MemoryText};
use crate::{
    engine::{Cancellation, ModelEngine},
    pipeline::BoxFuture,
};
use llama_cpp::{
    context::params::{LlamaContextParams, LlamaPoolingType},
    llama_batch::LlamaBatch,
    model::{
        AddBos, LlamaModel,
        params::{LlamaModelParams, kv_overrides::ParamOverrideValue},
    },
};
use sha2::{Digest, Sha256};
use std::{io::Read, num::NonZeroU32, path::PathBuf, thread};
use tokio::sync::{mpsc, oneshot};

/// Immutable identity of the owner-selected publisher artifact. Configuration
/// binds the path to these bytes rather than guessing compatibility by filename.
pub const E5_Q8_SHA256: &str = "0a34067a40f25d3149b36885faa62bee0e5284d0f9edc102acfc00e115d953e8";
/// E5 output width; serialized vectors use little-endian f32 after L2 normalization.
pub const E5_DIMENSIONS: usize = 384;
/// Derived-index identity includes artifact, normalization, prefixes and literal
/// window policy. Older coverage never masquerades as coverage for this policy.
pub const E5_INDEX_ID: &str = "0a34067a40f25d3149b36885faa62bee0e5284d0f9edc102acfc00e115d953e8:ws1:passage384-overlap64:l2f32";

/// Startup-only path to the selected E5 `Q8_0` GGUF. Debug omits the local path.
#[derive(Clone)]
pub struct EmbeddingConfig {
    path: PathBuf,
}
impl std::fmt::Debug for EmbeddingConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EmbeddingConfig([private path])")
    }
}
impl EmbeddingConfig {
    /// Select the downloaded cstr multilingual E5 small `Q8_0` artifact.
    #[must_use]
    pub fn e5_q8(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
    fn verify(&self) -> MemoryResult<()> {
        let mut file = std::fs::File::open(&self.path)?;
        if file.metadata()?.len() != 131_624_960 {
            return Err(MemoryError::InvalidInput("E5 artifact size"));
        }
        let mut hash = Sha256::new();
        let mut buffer = vec![0; 65_536];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hash.update(&buffer[..count]);
        }
        if format!("{:x}", hash.finalize()) != E5_Q8_SHA256 {
            return Err(MemoryError::InvalidInput("E5 artifact checksum"));
        }
        Ok(())
    }
}

/// Prefix chosen by the host, never included in retained source text.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub enum EmbeddingPurpose {
    /// Retrieval request.
    Query,
    /// Indexed evidence.
    Passage,
}
/// Normalized, finite model output and actual evaluated token count.
#[derive(Debug, Clone)]
pub struct Embedding {
    values: Vec<f32>,
    /// Includes prefix and special tokens.
    pub tokens: u16,
}
impl Embedding {
    /// Borrow the validated normalized vector.
    #[must_use]
    pub fn values(&self) -> &[f32] {
        &self.values
    }
    pub(super) fn from_values(values: Vec<f32>) -> MemoryResult<Self> {
        if values.len() != E5_DIMENSIONS || !values.iter().all(|v| v.is_finite()) {
            return Err(MemoryError::InvalidInput("embedding shape"));
        }
        let norm = values.iter().map(|v| v * v).sum::<f32>().sqrt();
        if !norm.is_finite() || norm <= f32::EPSILON {
            return Err(MemoryError::InvalidInput("embedding norm"));
        }
        Ok(Self {
            values: values.into_iter().map(|v| v / norm).collect(),
            tokens: 0,
        })
    }
}
#[derive(Debug)]
enum Command {
    Embed {
        text: MemoryText,
        reply: oneshot::Sender<MemoryResult<Embedding>>,
        purpose: EmbeddingPurpose,
    },
    Window {
        text: MemoryText,
        cursor: u32,
        purpose: EmbeddingPurpose,
        reply: oneshot::Sender<MemoryResult<(u32, u32)>>,
    },
}
impl Command {
    fn is_closed(&self) -> bool {
        match self {
            Self::Embed { reply, .. } => reply.is_closed(),
            Self::Window { reply, .. } => reply.is_closed(),
        }
    }
}
/// Cloneable client with no native state or shutdown authority.
#[derive(Debug, Clone)]
pub struct EmbeddingHandle {
    query: mpsc::Sender<Command>,
    passage: mpsc::Sender<Command>,
    stop: Cancellation,
}
impl EmbeddingHandle {
    #[cfg(test)]
    pub(crate) fn for_test() -> Self {
        let (query, mut queries) = mpsc::channel::<Command>(8);
        let (passage, mut passages) = mpsc::channel::<Command>(8);
        tokio::spawn(async move {
            loop {
                let command = tokio::select! {command=queries.recv()=>command,command=passages.recv()=>command};
                let Some(command) = command else {
                    break;
                };
                let mut values = vec![0.0; E5_DIMENSIONS];
                values[0] = 1.0;
                match command {
                    Command::Embed { reply, .. } => {
                        let _ = reply.send(Embedding::from_values(values));
                    }
                    Command::Window {
                        reply,
                        text,
                        cursor,
                        ..
                    } => {
                        let _ = reply.send(passage_window(text.as_str(), cursor, |text| {
                            Ok(text.chars().count())
                        }));
                    }
                }
            }
        });
        Self {
            query,
            passage,
            stop: Cancellation::default(),
        }
    }
    /// Evaluate one complete input (at most 512 tokens including special tokens).
    /// Oversized inputs are rejected, never silently truncated. Queues are bounded
    /// to eight queries and eight passages; dropping a waiter skips unstarted work.
    /// # Errors
    /// Returns capacity/closed, token bounds, native evaluation or invalid output.
    #[must_use]
    pub fn embed(
        &self,
        purpose: EmbeddingPurpose,
        text: MemoryText,
    ) -> BoxFuture<'_, MemoryResult<Embedding>> {
        Box::pin(async move {
            if text.as_str().len() > 16_384 {
                return Err(MemoryError::InvalidInput("embedding input bytes"));
            }
            if self.stop.is_cancelled() {
                return Err(MemoryError::Unavailable);
            }
            let (reply, receiver) = oneshot::channel();
            let sender = match purpose {
                EmbeddingPurpose::Query => &self.query,
                EmbeddingPurpose::Passage => &self.passage,
            };
            sender
                .try_send(Command::Embed {
                    text,
                    reply,
                    purpose,
                })
                .map_err(|error| match error {
                    mpsc::error::TrySendError::Full(_) => MemoryError::Capacity,
                    mpsc::error::TrySendError::Closed(_) => MemoryError::Unavailable,
                })?;
            receiver.await.map_err(|_| MemoryError::Unavailable)?
        })
    }
    /// Locate the next literal UTF-8 span using the resident encoder tokenizer.
    /// The full prefixed input is at most 384 tokens, with up to 64 tokens of
    /// preceding overlap. A separate 16 KiB span limit bounds tokenization work.
    /// # Errors
    /// Rejects invalid cursors, unavailable capacity or unsupported tokenization.
    pub async fn passage_window(&self, text: MemoryText, cursor: u32) -> MemoryResult<(u32, u32)> {
        self.window(text, cursor, EmbeddingPurpose::Passage).await
    }
    pub(crate) async fn query_window(&self, text: MemoryText) -> MemoryResult<u32> {
        if text.as_str().len() > 16_384 {
            return Err(MemoryError::InvalidInput("embedding query bytes"));
        }
        self.window(text, 0, EmbeddingPurpose::Query)
            .await
            .map(|(_, end)| end)
    }
    async fn window(
        &self,
        text: MemoryText,
        cursor: u32,
        purpose: EmbeddingPurpose,
    ) -> MemoryResult<(u32, u32)> {
        if self.stop.is_cancelled() {
            return Err(MemoryError::Unavailable);
        }
        let (reply, receiver) = oneshot::channel();
        let sender = match purpose {
            EmbeddingPurpose::Query => &self.query,
            EmbeddingPurpose::Passage => &self.passage,
        };
        sender
            .try_send(Command::Window {
                text,
                cursor,
                purpose,
                reply,
            })
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => MemoryError::Capacity,
                mpsc::error::TrySendError::Closed(_) => MemoryError::Unavailable,
            })?;
        receiver.await.map_err(|_| MemoryError::Unavailable)?
    }
}
/// Join authority for the dedicated native worker. Drop requests stop; shutdown
/// is required to await destruction of context before model before backend.
#[derive(Debug)]
pub struct EmbeddingWorker {
    handle: EmbeddingHandle,
    thread: Option<thread::JoinHandle<()>>,
}
impl EmbeddingWorker {
    /// Verify and load the selected artifact on its dedicated CPU owner thread.
    /// # Errors
    /// Rejects a different artifact, unavailable executor, or native startup error.
    pub async fn start(
        engine: &'static ModelEngine,
        config: EmbeddingConfig,
    ) -> MemoryResult<Self> {
        let executor =
            tokio::runtime::Handle::try_current().map_err(|_| MemoryError::Unavailable)?;
        let (query, mut queries) = mpsc::channel::<Command>(8);
        let (passage, mut passages) = mpsc::channel::<Command>(8);
        let stop = Cancellation::default();
        let thread_stop = stop.clone();
        let (ready, started) = oneshot::channel();
        let thread = thread::Builder::new()
            .name("assistant-memory-e5".into())
            .spawn(move || {
                let loaded = (|| {
                    config.verify()?;
                    let mut params =
                        std::pin::pin!(LlamaModelParams::default().with_n_gpu_layers(0));
                    // Artifact-specific omissions: tensor token_types.weight is [384,2].
                    // E5 self attention must see the complete sequence. These do not
                    // rewrite the downloaded GGUF or change the pinned native source.
                    params
                        .as_mut()
                        .append_kv_override(
                            c"tokenizer.ggml.token_type_count",
                            ParamOverrideValue::Int(2),
                        )
                        .map_err(|_| MemoryError::InvalidInput("E5 metadata"))?;
                    params
                        .as_mut()
                        .append_kv_override(
                            c"bert.attention.causal",
                            ParamOverrideValue::Bool(false),
                        )
                        .map_err(|_| MemoryError::InvalidInput("E5 metadata"))?;
                    LlamaModel::load_from_file(engine.backend_ref(), &config.path, &params)
                        .map_err(|_| MemoryError::InvalidInput("E5 native loading"))
                })();
                let model = match loaded {
                    Ok(model) => model,
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                if model.n_embd_out() != 384 {
                    let _ = ready.send(Err(MemoryError::InvalidInput("E5 dimension")));
                    return;
                }
                let params = LlamaContextParams::default()
                    .with_n_ctx(NonZeroU32::new(512))
                    .with_n_batch(512)
                    .with_n_ubatch(512)
                    .with_n_threads(2)
                    .with_n_threads_batch(2)
                    .with_embeddings(true)
                    .with_pooling_type(LlamaPoolingType::Mean)
                    .with_offload_kqv(false);
                let Ok(mut context) = model.new_context(engine.backend_ref(), params) else {
                    let _ = ready.send(Err(MemoryError::Unavailable));
                    return;
                };
                if ready.send(Ok(())).is_err() {
                    return;
                }
                loop {
                    // No polling timer or native work when idle. A query can wait for
                    // at most the single bounded passage currently being encoded.
                    let command = executor.block_on(async {
                        tokio::select! { biased;
                            () = thread_stop.cancelled() => None,
                            command = queries.recv() => command,
                            command = passages.recv() => command,
                        }
                    });
                    let Some(command) = command else {
                        break;
                    };
                    if command.is_closed() {
                        continue;
                    }
                    let (text, reply, purpose) = match command {
                        Command::Window {
                            text,
                            cursor,
                            purpose,
                            reply,
                        } => {
                            let result = passage_window(text.as_str(), cursor, |text| {
                                model
                                    .str_to_token(
                                        &encoder_copy(
                                            match purpose {
                                                EmbeddingPurpose::Query => "query: ",
                                                EmbeddingPurpose::Passage => "passage: ",
                                            },
                                            text,
                                        ),
                                        AddBos::Always,
                                    )
                                    .map(|tokens| tokens.len())
                                    .map_err(|_| MemoryError::InvalidInput("E5 tokenization"))
                            });
                            let _ = reply.send(result);
                            continue;
                        }
                        Command::Embed {
                            text,
                            reply,
                            purpose,
                        } => (text, reply, purpose),
                    };
                    let prefix = match purpose {
                        EmbeddingPurpose::Query => "query: ",
                        EmbeddingPurpose::Passage => "passage: ",
                    };
                    let result = (|| {
                        // Normalize only the encoder copy. Literal source text
                        // retains every original byte and evidence offset.
                        let tokens = model
                            .str_to_token(&encoder_copy(prefix, text.as_str()), AddBos::Always)
                            .map_err(|_| MemoryError::InvalidInput("E5 tokenization"))?;
                        if tokens.len() > 512 {
                            return Err(MemoryError::InvalidInput("E5 token limit"));
                        }
                        let token_count =
                            u16::try_from(tokens.len()).map_err(|_| MemoryError::Capacity)?;
                        let mut batch =
                            LlamaBatch::new(512, 1).map_err(|_| MemoryError::Unavailable)?;
                        for (position, token) in tokens.into_iter().enumerate() {
                            batch
                                .add(
                                    token,
                                    i32::try_from(position).map_err(|_| MemoryError::Capacity)?,
                                    &[0],
                                    true,
                                )
                                .map_err(|_| MemoryError::InvalidInput("E5 batch"))?;
                        }
                        // The pinned BERT reports has_encoder=false; decode dispatches
                        // to its noncausal encoder. All tokens fit one physical batch.
                        context
                            .decode(&mut batch)
                            .map_err(|_| MemoryError::Unavailable)?;
                        let row = context
                            .embeddings_seq_ith(0)
                            .map_err(|_| MemoryError::Unavailable)?;
                        let mut result = Embedding::from_values(row.to_vec())?;
                        result.tokens = token_count;
                        Ok(result)
                    })();
                    let _ = reply.send(result);
                }
            })?;
        match started.await {
            Ok(Ok(())) => Ok(Self {
                handle: EmbeddingHandle {
                    query,
                    passage,
                    stop,
                },
                thread: Some(thread),
            }),
            result => {
                stop.cancel();
                tokio::task::spawn_blocking(move || thread.join())
                    .await
                    .map_err(|_| MemoryError::Unavailable)?
                    .map_err(|_| MemoryError::Unavailable)?;
                match result {
                    Ok(Err(error)) => Err(error),
                    _ => Err(MemoryError::Unavailable),
                }
            }
        }
    }
    /// Borrow a cloneable inference client.
    #[must_use]
    pub fn handle(&self) -> EmbeddingHandle {
        self.handle.clone()
    }
    /// Stop accepting work and join the native owner without abandoning inference.
    /// # Errors
    /// Returns an executor failure or worker panic after requesting shutdown.
    pub async fn shutdown(mut self) -> MemoryResult<()> {
        self.handle.stop.cancel();
        if let Some(thread) = self.thread.take() {
            tokio::task::spawn_blocking(move || thread.join())
                .await
                .map_err(|_| MemoryError::Unavailable)?
                .map_err(|_| MemoryError::Unavailable)?;
        }
        Ok(())
    }
}

fn encoder_copy(prefix: &str, text: &str) -> String {
    format!(
        "{prefix}{}",
        text.split_whitespace().collect::<Vec<_>>().join(" ")
    )
}

/// Choose checked boundaries from measured substring counts, never decoded token
/// pieces: normalization/subword pieces need not map one-to-one to literal bytes.
fn passage_window(
    text: &str,
    cursor: u32,
    mut count: impl FnMut(&str) -> MemoryResult<usize>,
) -> MemoryResult<(u32, u32)> {
    let cursor = usize::try_from(cursor).map_err(|_| MemoryError::Capacity)?;
    if cursor >= text.len() || !text.is_char_boundary(cursor) {
        return Err(MemoryError::InvalidInput("embedding cursor"));
    }
    let mut floor = cursor.saturating_sub(8192);
    while !text.is_char_boundary(floor) {
        floor += 1;
    }
    let starts = text[floor..cursor]
        .char_indices()
        .map(|(i, _)| floor + i)
        .chain(std::iter::once(cursor))
        .collect::<Vec<_>>();
    let mut low = 0;
    let mut high = starts.len() - 1;
    while low < high {
        let middle = low + (high - low) / 2;
        if count(&text[starts[middle]..cursor])? <= 64 {
            high = middle;
        } else {
            low = middle + 1;
        }
    }
    let start = starts[low];
    let mut ceiling = text.len().min(start + 16384);
    while !text.is_char_boundary(ceiling) {
        ceiling -= 1;
    }
    let ends = text[cursor..ceiling]
        .char_indices()
        .map(|(i, c)| cursor + i + c.len_utf8())
        .collect::<Vec<_>>();
    let mut low = 0;
    let mut high = ends.len();
    while low < high {
        let middle = low + (high - low) / 2;
        if count(&text[start..ends[middle]])? <= 384 {
            low = middle + 1;
        } else {
            high = middle;
        }
    }
    let end = low
        .checked_sub(1)
        .and_then(|index| ends.get(index))
        .copied()
        .ok_or(MemoryError::InvalidInput("E5 chunk progress"))?;
    // Token counts can change around subword boundaries. Recheck the chosen
    // exact literal substring before reporting coverage or dispatching compute.
    if count(&text[start..end])? > 384 {
        return Err(MemoryError::InvalidInput("E5 chunk tokens"));
    }
    Ok((
        u32::try_from(start).map_err(|_| MemoryError::Capacity)?,
        u32::try_from(end).map_err(|_| MemoryError::Capacity)?,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn measured_windows_cover_unicode_and_restart_at_literal_boundaries() {
        let text = "Привіт, Києве! hello 👋 ".repeat(100);
        let mut cursor = 0;
        let mut chunks = 0;
        while usize::try_from(cursor).expect("cursor") < text.len() {
            let (start, end) = passage_window(&text, cursor, |part| Ok(part.chars().count() + 4))
                .expect("bounded measured window");
            let part = text
                .get(start as usize..end as usize)
                .expect("literal UTF-8 span");
            assert!(part.chars().count() + 4 <= 384);
            assert!(start <= cursor && end > cursor);
            assert!(text[start as usize..cursor as usize].chars().count() + 4 <= 64);
            cursor = end;
            chunks += 1;
        }
        assert!(chunks > 1);
        assert_eq!(cursor as usize, text.len());
        assert!(passage_window(&text, 1, |_| Ok(0)).is_err());
    }
}
impl Drop for EmbeddingWorker {
    fn drop(&mut self) {
        self.handle.stop.cancel();
    }
}
