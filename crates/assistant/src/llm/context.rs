use std::time::Instant;

use llama_cpp::prelude::*;

use crate::llm::find_stop;

use super::LLMModel;

#[derive(Debug)]
pub enum StreamCmd {
    Continue,
    Stop,
}

pub trait TokenSink: Send + Sync {
    fn emit(&mut self, msg: &str);
}

#[derive(Debug)]
pub enum FinishReason {
    EndOfGeneration,
    TokenLimit,
    Stop,
}

#[derive(Debug)]
pub struct Generation {
    text: String,
    duration_ms: u128,
    tokens_generated: usize,
    finish_reason: FinishReason,
}

#[derive(Debug)]
pub struct LLMContext<'a> {
    llama_ctx: LlamaContext<'a>,
    llm: &'a super::LLMModel,
}

impl<'a> LLMContext<'a> {
    pub fn new(llama_ctx: LlamaContext<'a>, llm: &'a LLMModel) -> Self {
        Self { llama_ctx, llm }
    }

    pub fn generate<S: TokenSink>(
        &mut self,
        tokens: &[LlamaToken],
        sink: &mut S,
        chat: Option<&ChatParams>,
    ) -> super::LLMResult<Generation> {
        if tokens.is_empty() {
            return Err(super::LLMError::NoTokensError);
        }

        let ctx = &mut self.llama_ctx;

        // Clear KV cache.
        // This is costly operation, which resets cached attention state
        // for previous token sequence.
        ctx.clear_kv_cache();

        let mut batch = LlamaBatch::new(ctx.n_batch() as usize, 1)?;
        let mut position = 0_i32;

        for chunk in tokens.chunks(ctx.n_batch() as usize) {
            batch.clear();

            for &token in chunk {
                // &[0] - Sequence 0 is our single conversation
                // position - token index in our conversation
                // logits - Only last prompt token needs output logits.

                let logits = position as usize == tokens.len() - 1;

                batch.add(token, position, &[0], logits)?;
                position += 1;
            }
            // This will update cache and compute logits (scores over the vocabulary)
            // for requested output positions
            ctx.decode(&mut batch)?;
        }

        let mut chain = Vec::new();
        if let Some(grammar) =
            chat.and_then(|rendered| rendered.grammar_sampler(self.llm.model_ref()))
        {
            chain.push(grammar);
        }

        chain.push(LlamaSampler::greedy());

        let sampler = LlamaSampler::chain_simple(chain);

        let special = if chat.is_some() {
            Special::Tokenize
        } else {
            Special::Plaintext
        };

        let mut detokenizer = StreamDetokenizer::new(self.llm.model_ref(), special);
        let stops: Vec<String> = chat
            .map(|c| serde_json::from_str(&c.additional_stops_json))
            .transpose()?
            .unwrap_or_default();

        let start_time = Instant::now();

        // Pending buffer is specific buffer to chek for STOP sequences
        // in pending piece for streaming
        let mut pending = String::new();

        let mut text = String::new();
        let mut finish = FinishReason::TokenLimit;
        let mut generated = 0;

        for _ in 0..self.llm.get_token_limit() {
            let token = sampler.try_sample(ctx, -1)?;
            if self.llm.model_ref().is_eog_token(token) {
                finish = FinishReason::EndOfGeneration;
                break;
            }

            generated += 1;

            let piece = match detokenizer.push(token) {
                Ok(piece) => piece,
                Err(err) => {
                    continue;
                }
            };

            text.push_str(&piece);
            pending.push_str(&piece);

            if let Some(offset) = find_stop(&pending, &stops) {
                sink.emit(&pending[..offset]);
                text.truncate(text.len() - pending.len() + offset);
                pending.clear();
                finish = FinishReason::Stop;
            } else {
            }

            batch.clear();
            batch.add(token, position, &[0], true)?;
            ctx.decode(&mut batch)?;
            position += 1;
        }

        let tail = detokenizer.finish()?;
        text.push_str(&tail);

        let duration_ms = start_time.elapsed().as_millis();

        Ok(Generation {
            text,
            tokens_generated: generated,
            duration_ms,
            finish_reason: finish,
        })
    }
}
