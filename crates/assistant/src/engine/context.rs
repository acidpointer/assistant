use std::time::Instant;

use llama_cpp::prelude::*;

use crate::engine::EngineResult;

use super::{find_stop, holdback_len};

use super::LLMModel;

#[derive(Debug)]
pub enum StreamCmd {
    Continue,
    Stop,
}

pub trait TokenSink: Send + Sync {
    fn emit(&mut self, msg: &str);
}

#[derive(Debug, PartialEq, Eq)]
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
    llm: &'a LLMModel<'a>,
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
    ) -> EngineResult<Generation> {
        if tokens.is_empty() {
            return Err(super::EngineError::NoTokensError);
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
        let max_stop_len = stops.iter().map(String::len).max().unwrap_or_default();

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
                    tracing::error!(target: "engine::context::generate()", "Detokenizer error: {err}");
                    continue;
                }
            };

            text.push_str(&piece);
            pending.push_str(&piece);

            if let Some(offset) = find_stop(&pending, &stops) {
                if offset > 0 {
                    sink.emit(&pending[..offset]);
                }
                text.truncate(text.len() - pending.len() + offset);
                pending.clear();
                finish = FinishReason::Stop;
                break;
            } else {
                // Keep only text that could still become a stop across tokens.
                let ready = pending.len() - holdback_len(&pending, &stops, max_stop_len);
                if ready > 0 {
                    sink.emit(&pending[..ready]);
                    pending.replace_range(..ready, "");
                }
            }

            batch.clear();
            batch.add(token, position, &[0], true)?;
            ctx.decode(&mut batch)?;
            position += 1;
        }

        if finish != FinishReason::Stop {
            let tail = match detokenizer.finish() {
                Ok(tail) => tail,
                Err(DetokenizeError::IncompleteUtf8(bytes))
                    if finish == FinishReason::TokenLimit =>
                {
                    String::from_utf8_lossy(&bytes).into_owned()
                }
                Err(err) => return Err(err.into()),
            };

            text.push_str(&tail);
            pending.push_str(&tail);
            if let Some(offset) = find_stop(&pending, &stops) {
                text.truncate(text.len() - pending.len() + offset);
                pending.truncate(offset);
                finish = FinishReason::Stop;
            }
            // At normal termination an unfinished stop prefix is ordinary text.
            if !pending.is_empty() {
                sink.emit(&pending);
            }
        }

        let duration_ms = start_time.elapsed().as_millis();

        Ok(Generation {
            text,
            tokens_generated: generated,
            duration_ms,
            finish_reason: finish,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::{num::NonZeroU32, path::PathBuf};

    use llama_cpp::prelude::{AddBos, ChatApplyParams, ChatTemplates};

    use super::{FinishReason, TokenSink};
    use crate::engine::{LLMModel, ModelEngine, find_stop};

    #[derive(Default)]
    struct CollectedText(Vec<String>);

    impl TokenSink for CollectedText {
        fn emit(&mut self, msg: &str) {
            self.0.push(msg.to_owned());
        }
    }

    #[test]
    fn generation_stops_across_chunks_and_flushes_unmatched_prefixes() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/test-models/stories260K.gguf");
        if !path.is_file() {
            eprintln!("SKIP: stop-sequence generation test needs stories260K.gguf");
            return;
        }

        ModelEngine::init().expect("initialize backend for stop-sequence test");
        let engine = ModelEngine::get_instance().expect("initialized engine");
        let model = LLMModel::init(
            path,
            engine,
            0,
            64,
            1,
            NonZeroU32::new(256).expect("nonzero context size"),
            false,
            NonZeroU32::new(32).expect("nonzero batch size"),
        )
        .expect("load tiny model");
        let tokens = model
            .model_ref()
            .str_to_token("Once upon a time", AddBos::Always)
            .expect("tokenize fixture prompt");
        let mut context = model.new_context().expect("create fixture context");
        let mut baseline_sink = CollectedText::default();
        let baseline = context
            .generate(&tokens, &mut baseline_sink, None)
            .expect("generate baseline");
        assert_eq!(baseline_sink.0.concat(), baseline.text);
        assert!(baseline_sink.0.len() >= 3, "fixture must span chunks");

        let templates =
            ChatTemplates::from_model(model.model_ref(), Some("{{ messages[0].content }}"))
                .expect("create minimal template for stop metadata");
        let split_stop = baseline_sink.0[..2].concat();
        for stops in [
            vec![split_stop],
            vec![String::new(), baseline_sink.0[1].clone()],
            vec![format!("{} never matches", baseline.text)],
            vec![String::new()],
            Vec::new(),
        ] {
            let mut chat = templates
                .apply(&ChatApplyParams::new(
                    r#"[{"role":"user","content":"fixture"}]"#,
                ))
                .expect("render stop metadata");
            chat.additional_stops_json = serde_json::to_string(&stops).expect("serialize stops");
            let mut sink = CollectedText::default();
            let generation = context
                .generate(&tokens, &mut sink, Some(&chat))
                .expect("generate with stops");
            let stop = find_stop(&baseline.text, &stops);
            assert_eq!(
                generation.text,
                baseline.text[..stop.unwrap_or(baseline.text.len())]
            );
            assert_eq!(sink.0.concat(), generation.text);
            if stop.is_some() {
                assert_eq!(generation.finish_reason, FinishReason::Stop);
                assert!(generation.tokens_generated < baseline.tokens_generated);
            } else {
                assert_eq!(generation.finish_reason, baseline.finish_reason);
            }
        }
    }
}
