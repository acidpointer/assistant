//! Real E5 preflight: native mean pooling, output bounds, normalized semantic
//! ranking and batch isolation. Optional local weights keep the default suite
//! portable; ASSISTANT_EMBEDDING_MODEL makes absence a hard qualification failure.

mod support;

use llama_cpp::{
    context::params::{LlamaContextParams, LlamaPoolingType},
    llama_batch::LlamaBatch,
    model::AddBos,
    model::{params::LlamaModelParams, LlamaModel},
    EmbeddingsError,
};
use std::{num::NonZeroU32, path::PathBuf};

#[test]
fn e5_native_embeddings_and_index_guards() {
    let _guard = support::model::llama_guard();
    let explicit = std::env::var_os("ASSISTANT_EMBEDDING_MODEL");
    let path = explicit.as_ref().map_or_else(
        || PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../multilingual-e5-small-q8_0.gguf"),
        PathBuf::from,
    );
    if !path.is_file() {
        assert!(explicit.is_none(), "required E5 artifact absent");
        eprintln!("skip: E5 artifact absent");
        return;
    }
    let backend = support::model::backend();
    let mut model_params = std::pin::pin!(LlamaModelParams::default().with_n_gpu_layers(0));
    // This exact publisher artifact omits these keys; its token_types tensor
    // has two rows. Explicit overrides keep the downloaded bytes unchanged.
    use llama_cpp::model::params::kv_overrides::ParamOverrideValue;
    model_params
        .as_mut()
        .append_kv_override(
            c"tokenizer.ggml.token_type_count",
            ParamOverrideValue::Int(2),
        )
        .expect("token type override");
    model_params
        .as_mut()
        .append_kv_override(c"bert.attention.causal", ParamOverrideValue::Bool(false))
        .expect("noncausal override");
    let model = LlamaModel::load_from_file(backend, &path, &model_params)
        .expect("load selected E5 artifact");
    assert_eq!(model.n_embd_out(), 384);
    // Missing byte-fallback rows previously caused a foreign unwind/abort.
    let mut rejected = Vec::new();
    assert!(model
        .str_to_token_into("passage: \u{0001}", AddBos::Always, &mut rejected)
        .is_err());
    assert!(rejected.is_empty());
    assert!(model
        .str_token_count("passage: \u{0001}", AddBos::Always)
        .is_err());

    let params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(1024))
        .with_n_seq_max(2)
        .with_n_batch(512)
        .with_n_ubatch(512)
        .with_n_threads(2)
        .with_n_threads_batch(2)
        .with_embeddings(true)
        .with_pooling_type(LlamaPoolingType::Mean)
        .with_offload_kqv(false);
    let mut context = model.new_context(backend, params).expect("E5 context");
    assert!(matches!(
        context.embeddings_seq_ith(0),
        Err(EmbeddingsError::InvalidIndex)
    ));
    let texts = [
        "query: Who in my family works with animals?",
        "passage: My sister Lena is a veterinarian.",
        "passage: The database stores financial transactions.",
        "query: Хто в моїй родині працює з тваринами?",
    ];
    let mut vectors = Vec::new();
    for text in texts {
        let tokens = model
            .str_to_token(text, AddBos::Always)
            .expect("tokenize E5 input");
        assert!(tokens.len() <= 512);
        let mut batch = LlamaBatch::new(512, 1).expect("batch");
        for (position, token) in tokens.into_iter().enumerate() {
            batch
                .add(
                    token,
                    i32::try_from(position).expect("position"),
                    &[0],
                    true,
                )
                .expect("entry");
        }
        if model.has_encoder() {
            context.encode(&mut batch).expect("encode E5");
        } else {
            context.decode(&mut batch).expect("decode E5");
        }
        let mut vector = context.embeddings_seq_ith(0).expect("pooled row").to_vec();
        assert_eq!(vector.len(), 384);
        assert!(vector.iter().all(|v| v.is_finite()));
        let norm = vector
            .iter()
            .map(|v| f64::from(*v).powi(2))
            .sum::<f64>()
            .sqrt();
        assert!(norm > 0.0);
        for value in &mut vector {
            *value /= norm as f32;
        }
        vectors.push(vector);
        assert!(matches!(
            context.embeddings_seq_ith(-1),
            Err(EmbeddingsError::InvalidIndex)
        ));
        assert!(matches!(
            context.embeddings_seq_ith(i32::MAX),
            Err(EmbeddingsError::InvalidIndex)
        ));
        assert!(context.embeddings_ith(i32::MAX).is_err());
    }
    let cosine = |a: usize, b: usize| {
        vectors[a]
            .iter()
            .zip(&vectors[b])
            .map(|(a, b)| a * b)
            .sum::<f32>()
    };
    eprintln!(
        "E5 cosine query/relevant={} unrelated={} Ukrainian/relevant={}",
        cosine(0, 1),
        cosine(0, 2),
        cosine(3, 1)
    );
    assert!(
        cosine(0, 1) > cosine(0, 2) + 0.02,
        "semantic retrieval ordering"
    );
    assert!(
        cosine(3, 1) > cosine(3, 2),
        "cross-language retrieval ordering"
    );
    // Same physical batch, separate sequences: no cross-sequence attention.
    let mut mixed = LlamaBatch::new(512, 2).expect("mixed batch");
    for (sequence, text) in texts[..2].iter().enumerate() {
        for (position, token) in model
            .str_to_token(text, AddBos::Always)
            .expect("mixed tokens")
            .into_iter()
            .enumerate()
        {
            mixed
                .add(
                    token,
                    i32::try_from(position).expect("position"),
                    &[i32::try_from(sequence).expect("sequence")],
                    true,
                )
                .expect("mixed entry");
        }
    }
    context.decode(&mut mixed).expect("mixed E5 evaluation");
    for (sequence, expected) in vectors[..2].iter().enumerate() {
        let actual = context
            .embeddings_seq_ith(i32::try_from(sequence).expect("sequence"))
            .expect("mixed row");
        let norm = actual.iter().map(|v| v * v).sum::<f32>().sqrt();
        let similarity = actual
            .iter()
            .zip(expected)
            .map(|(a, b)| a * b / norm)
            .sum::<f32>();
        assert!(similarity > 0.9999, "batch isolation: {similarity}");
    }
    let mut empty = LlamaBatch::new(1, 1).expect("empty batch");
    assert!(if model.has_encoder() {
        context.encode(&mut empty).is_err()
    } else {
        context.decode(&mut empty).is_err()
    });
    assert!(
        matches!(
            context.embeddings_seq_ith(0),
            Err(EmbeddingsError::InvalidIndex)
        ),
        "failed evaluation revokes old rows"
    );
}
