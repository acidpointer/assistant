#pragma once

#include <stddef.h>
#include <stdint.h>

#include "ggml-backend.h" // ggml_backend_buffer_t
#include "llama.h"

#ifdef __cplusplus
extern "C" {
#endif

#ifdef LLAMA_RS_TURBOQUANT
// CPU reference only. Exactly 128 floats / 50 encoded bytes. Inputs to encode
// and dot must be finite with block norm <= 65504. No graph rotation is applied.
// Output is unchanged on error; shared status/TLS errors contain failures.
// Versioned owned snapshot. execution: 0=other codec, 1=CPU reference,
// 2=required Vulkan. Counts cover successfully synchronized GPU submissions;
// reading this record adds neither synchronization nor device readback.
struct llama_pq3_graph_info_v1 {
    uint32_t abi_version;
    uint32_t row_version;
    uint32_t graph_version;
    uint32_t transform_version;
    uint32_t execution;
    uint32_t path_mask;
    uint32_t block_values;
    uint32_t block_bytes;
    uint64_t indexed_writes;
    uint64_t attention_operations;
};
int32_t llama_pq3_graph_inspect_guarded(const struct llama_context * ctx, struct llama_pq3_graph_info_v1 * out);

int32_t llama_pq3_encode_block_guarded(const float * input, size_t count, uint8_t * output, size_t bytes);
int32_t llama_pq3_decode_block_guarded(const uint8_t * input, size_t bytes, float * output, size_t count);
int32_t llama_pq3_dot_q8_guarded(const uint8_t * input, size_t bytes, const float * query, size_t count, float * output);
#endif

struct llama_memory_breakdown_entry {
    char    buft_name[128];
    size_t  model;
    size_t  context;
    size_t  compute;
};

// Owned identities of devices with actual non-host allocations. This is not the
// list of available/selected backend devices. A missing PCI id remains empty.
struct llama_gpu_allocation_v1 {
    char name[128];
    char device_id[128];
    uint64_t allocated_bytes;
};
// ctx may be null to inspect resident weights before any context exists. If
// supplied, it must belong to model and remain confined to the calling thread.
int32_t llama_gpu_allocations_collect_v1(const struct llama_model * model,
        const struct llama_context * ctx, struct llama_gpu_allocation_v1 * out,
        size_t capacity, size_t * required);

// Fallible size-query/fill API. Never returns a partial report as success.
// required is mandatory; ctx must remain live and confined to the calling thread.
int32_t llama_memory_breakdown_collect_v2(
        const struct llama_context * ctx,
        struct llama_memory_breakdown_entry * out,
        size_t capacity,
        size_t * required);

// Change attention policy without exceptions crossing FFI; unsupported codec changes fail before mutation.
int32_t llama_context_set_causal_guarded(struct llama_context * ctx, bool causal);

// Complete pending work with the shared exception/TLS error contract.
int32_t llama_context_synchronize_guarded(struct llama_context * ctx);

// Tokenizers can throw (e.g. SPM metadata without byte fallback). Status uses
// shared TLS errors; count keeps llama_tokenize's signed required-size contract.
// On failure count is zero and callers must not consume the token buffer.
int32_t llama_tokenize_guarded(const struct llama_vocab * vocab,
        const char * text, int32_t text_length, llama_token * tokens,
        int32_t capacity, bool add_special, bool parse_special, int32_t * count);

// Owned scalar snapshots of the pinned C++ model/cache internals. No pointers
// or C++ containers cross this ABI. kind: 0=no cache, 1=full, 2=SWA, 3=recurrent.
struct llama_cache_layer_info {
    uint32_t layer;
    uint32_t kind;
    uint32_t kv_heads;
    uint32_t key_head_dim;
    uint32_t value_head_dim;
    uint32_t window;
    char device_name[128];
};

struct llama_cache_state_info {
    uint32_t kind;
    uint32_t cells;
    uint32_t layers;
    uint32_t streams;
    int32_t key_type;
    int32_t value_type;
    uint32_t key_rotation;
    uint32_t value_rotation;
};

struct llama_context_cache_info {
    uint32_t context_size;
    uint32_t batch_size;
    uint32_t microbatch_size;
    uint32_t sequences;
    uint32_t flash_attention;
    uint32_t offload_kqv;
    uint32_t op_offload;
    uint32_t inspection_available;
};

int32_t llama_model_cache_layers_collect(
        const struct llama_model * model,
        struct llama_cache_layer_info * out,
        size_t capacity,
        size_t * required,
        uint32_t * requires_equal_kv);

int32_t llama_context_cache_collect(
        const struct llama_context * ctx,
        struct llama_context_cache_info * info,
        struct llama_cache_state_info * out,
        size_t capacity,
        size_t * required);

// Count the backend buffers a state write of `mem` would touch.
//
// `llama_memory_get_buffers` returns `std::vector<ggml_backend_buffer_t>` by
// value, which bindgen cannot represent — it degrades the signature to
// `-> u8`, so calling the raw binding is an ABI mismatch. Hence this.
//
// Writes up to `max_out` buffer pointers to `out` (may be NULL to query the
// count) and returns the total the memory reports, which can exceed `max_out`.
size_t llama_memory_buffers_collect(
        llama_memory_t          mem,
        ggml_backend_buffer_t * out,
        size_t                  max_out);

// Flatten llama_get_memory_breakdown() into `out` (max `max_out` entries).
// Returns the number of entries written.
size_t llama_memory_breakdown_collect(
        const struct llama_context *          ctx,
        struct llama_memory_breakdown_entry * out,
        size_t                                max_out);

struct common_device_memory_flat_entry {
    int64_t total;
    int64_t free;
    size_t  model;
    size_t  context;
    size_t  compute;
};

// Flatten common_get_device_memory_data() into `out` (max `max_out` entries).
// Writes hyper-parameters to the optional out-pointers when non-null.
// Returns the number of entries written, or (size_t)-1 on error.
size_t common_device_memory_collect(
        const char *                           path_model,
        const struct llama_model_params *      mparams,
        const struct llama_context_params *      cparams,
        enum ggml_log_level                      log_level,
        struct common_device_memory_flat_entry * out,
        size_t                                   max_out,
        uint32_t *                               hp_ngl,
        uint32_t *                               hp_n_ctx_train,
        uint32_t *                               hp_n_expert);

// Guarded wrappers over the `llama_quant_*` preview API.
//
// `llama_quant_model_from_metadata` and `llama_quant_init` throw on input they
// reject (an unknown architecture, a model they cannot quantize). Letting that
// unwind into Rust aborts the process with "Rust cannot catch foreign
// exceptions", so both are wrapped to return NULL instead.

struct llama_model * llama_quant_model_from_metadata_guarded(
        const struct llama_quant_model_desc * desc);

struct quantize_state_impl * llama_quant_init_guarded(
        const struct llama_model *                  model,
        const struct llama_model_quantize_params *  params);

// Returns 0 on success, non-zero if the underlying call threw.
int32_t llama_quant_compute_types_guarded(
        struct quantize_state_impl * qs,
        enum llama_ftype             ftype,
        struct ggml_tensor **        tensors,
        enum ggml_type *             result_types,
        size_t                       n_tensors);

// Returns 1 if quantizable, 0 if not, -1 if the underlying call threw.
int32_t llama_quant_tensor_allows_quantization_guarded(
        const struct quantize_state_impl * qs,
        const struct ggml_tensor *         tensor);

#ifdef __cplusplus
}
#endif
