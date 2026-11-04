#include "ext_shim.h"

#include "fit.h"
#include "ggml-backend.h"
#include "llama-ext.h"
#include "llama-context.h"
#include "llama-model.h"
#include "llama-memory-hybrid.h"
#include "llama-memory-hybrid-iswa.h"
#include "shim_support_impl.hpp"

#include <cstring>
#include <vector>
#include <stdexcept>
#include <limits>

#ifdef LLAMA_RS_TURBOQUANT
#include "ggml-quants.h"
#include "ggml-cpu.h"
#ifdef LLAMA_RS_VULKAN
#include "ggml-vulkan.h"
#endif
#include <cmath>

namespace {
void validate_pq3_input(const float * input, size_t count) {
    if (!input || count != QK_PQ3_0) throw std::invalid_argument("PQ3 reference requires exactly 128 floats");
    double squares = 0.0;
    for (size_t i = 0; i < count; ++i) {
        if (!std::isfinite(input[i])) throw std::invalid_argument("PQ3 reference input must be finite");
        squares += double(input[i]) * input[i];
    }
    if (std::sqrt(squares) > 65504.0) throw std::invalid_argument("PQ3 block norm exceeds finite FP16 range");
}
block_pq3_0 read_pq3_block(const uint8_t * input, size_t bytes) {
    if (!input || bytes != sizeof(block_pq3_0)) throw std::invalid_argument("PQ3 reference requires exactly 50 bytes");
    // Bytes have no alignment guarantee. Keep native half/struct access local.
    block_pq3_0 block{};
    std::memcpy(&block, input, sizeof(block));
    const float norm = ggml_fp16_to_fp32(block.d);
    if (!std::isfinite(norm) || std::signbit(norm)) throw std::invalid_argument("PQ3 block has an invalid norm");
    if (norm == 0.0f) {
        for (uint8_t byte : block.qs) {
            if (byte) throw std::invalid_argument("PQ3 zero block is not canonical");
        }
    }
    return block;
}
}

int32_t llama_pq3_graph_inspect_guarded(const llama_context * ctx, llama_pq3_graph_info_v1 * out) {
    return llama_shim::guard([&]() -> int32_t {
        if (!ctx || !out) return LLAMA_SHIM_INVALID_ARG;
        llama_pq3_graph_info_v1 result{};
        result.abi_version = 1;
        result.row_version = ggml_pq3_reference_abi_v1();
        result.graph_version = llama_pq3_graph_abi_v1();
        result.block_values = ggml_blck_size(GGML_TYPE_PQ3_0);
        result.block_bytes = ggml_type_size(GGML_TYPE_PQ3_0);
        if (llama_pq3_context_uses_codec(ctx)) {
            result.transform_version = 1;
            result.execution = ctx->get_cparams().op_offload ? 2 : 1;
#if defined(LLAMA_RS_VULKAN) && defined(GGML_PQ3_VULKAN_OBSERVATION_ABI)
            auto * scheduler = ctx->get_sched();
            for (int i=0;i<ggml_backend_sched_get_n_backends(scheduler);++i) {
                auto * backend = ggml_backend_sched_get_backend(scheduler,i);
                if (!ggml_backend_is_vk(backend)) continue;
                ggml_pq3_vulkan_observation observed{};
                if (!ggml_backend_vk_pq3_observe_v1(backend,&observed)) throw std::runtime_error("PQ3 Vulkan observation ABI unavailable");
                if (UINT64_MAX-result.indexed_writes < observed.indexed_writes || UINT64_MAX-result.attention_operations < observed.attention_operations) throw std::overflow_error("PQ3 observation counters overflow");
                result.indexed_writes += observed.indexed_writes;
                result.attention_operations += observed.attention_operations;
                result.path_mask |= observed.path_mask;
            }
#endif
        }
        *out=result;
        return LLAMA_SHIM_OK;
    });
}

int32_t llama_pq3_encode_block_guarded(const float * input, size_t count, uint8_t * output, size_t bytes) {
    return llama_shim::guard([&]() -> int32_t {
        if (!output || bytes != sizeof(block_pq3_0)) return LLAMA_SHIM_INVALID_ARG;
        validate_pq3_input(input, count);
        block_pq3_0 block{};
        quantize_row_pq3_0_ref(input, &block, QK_PQ3_0);
        // Check rounded scale too; do not publish an infinite native block.
        if (!std::isfinite(ggml_fp16_to_fp32(block.d))) throw std::invalid_argument("PQ3 norm rounded outside finite FP16 range");
        std::memcpy(output, &block, sizeof(block));
        return LLAMA_SHIM_OK;
    });
}
int32_t llama_pq3_decode_block_guarded(const uint8_t * input, size_t bytes, float * output, size_t count) {
    return llama_shim::guard([&]() -> int32_t {
        if (!output || count != QK_PQ3_0) return LLAMA_SHIM_INVALID_ARG;
        const auto block = read_pq3_block(input, bytes);
        float values[QK_PQ3_0];
        dequantize_row_pq3_0(&block, values, QK_PQ3_0);
        std::memcpy(output, values, sizeof(values));
        return LLAMA_SHIM_OK;
    });
}
int32_t llama_pq3_dot_q8_guarded(const uint8_t * input, size_t bytes, const float * query, size_t count, float * output) {
    return llama_shim::guard([&]() -> int32_t {
        if (!output) return LLAMA_SHIM_INVALID_ARG;
        const auto block = read_pq3_block(input, bytes);
        validate_pq3_input(query, count);
        block_q8_0 q8[QK_PQ3_0 / QK8_0];
        quantize_row_q8_0_ref(query, q8, QK_PQ3_0);
        float result;
        if (ggml_pq3_cpu_reference_abi_v1() != 1) throw std::runtime_error("unsupported PQ3 CPU ABI");
        const auto * traits = ggml_get_type_traits_cpu(GGML_TYPE_PQ3_0);
        if (!traits->vec_dot || traits->vec_dot_type != GGML_TYPE_Q8_0 || traits->nrows != 1) throw std::runtime_error("incomplete PQ3 CPU traits");
        traits->vec_dot(QK_PQ3_0, &result, 0, &block, 0, q8, 0, 1);
        *output = result;
        return LLAMA_SHIM_OK;
    });
}
#endif

namespace {
// Fixed ABI strings must be complete: truncation could merge device identities.
void copy_cache_name(char (&out)[128], const char * name) {
    if (!name || std::strlen(name) >= sizeof(out)) {
        throw std::runtime_error("invalid or oversized native buffer/device name");
    }
    std::memcpy(out, name, std::strlen(name) + 1);
}

uint32_t rotation_size(const std::vector<ggml_tensor *> & rotations) {
    uint32_t size = 0;
    for (const auto * tensor : rotations) {
        if (tensor) {
            if (size && size != tensor->ne[0]) {
                throw std::runtime_error("inconsistent cache rotation dimensions");
            }
            size = static_cast<uint32_t>(tensor->ne[0]);
        }
    }
    return size;
}

void append_cache(std::vector<llama_cache_state_info> & result, llama_kv_cache * cache, uint32_t kind) {
    if (!cache) return;
    const auto ids = cache->get_layer_ids();
    // Native type_k/type_v index the first layer; an empty SWA arena is valid.
    if (ids.empty()) return;
    result.push_back({kind, cache->get_size(), static_cast<uint32_t>(ids.size()),
        cache->get_n_stream(), static_cast<int32_t>(cache->type_k()),
        static_cast<int32_t>(cache->type_v()), rotation_size(cache->get_k_rot()),
        rotation_size(cache->get_v_rot())});
}

void append_swa(std::vector<llama_cache_state_info> & result, llama_kv_cache_iswa * cache) {
    append_cache(result, cache->get_base(), 1);
    append_cache(result, cache->get_swa(), 2);
}
} // namespace

int32_t llama_context_synchronize_guarded(llama_context * ctx) {
    return llama_shim::guard([&]() -> int32_t {
        if (!ctx) return LLAMA_SHIM_INVALID_ARG;
        llama_synchronize(ctx);
        return LLAMA_SHIM_OK;
    });
}

int32_t llama_gpu_allocations_collect_v1(const llama_model * model,
        const llama_context * ctx, llama_gpu_allocation_v1 * out,
        size_t capacity, size_t * required) {
    return llama_shim::guard([&]() -> int32_t {
        if (!model || !required || (!out && capacity) ||
                (ctx && llama_get_model(ctx) != model)) return LLAMA_SHIM_INVALID_ARG;
        if (model->no_alloc()) throw std::invalid_argument("GPU placement requires an allocated model");
        std::vector<llama_gpu_allocation_v1> result;
        auto append = [&](ggml_backend_buffer_type_t buft, uint64_t bytes) {
            if (!bytes || !buft || ggml_backend_buft_is_host(buft)) return;
            llama_gpu_allocation_v1 item{};
            auto dev = ggml_backend_buft_get_device(buft);
            if (dev) {
                const auto type = ggml_backend_dev_type(dev);
                if (type != GGML_BACKEND_DEVICE_TYPE_GPU && type != GGML_BACKEND_DEVICE_TYPE_IGPU &&
                        type != GGML_BACKEND_DEVICE_TYPE_META) return;
                ggml_backend_dev_props props{};
                ggml_backend_dev_get_props(dev, &props);
                copy_cache_name(item.name, props.description ? props.description : props.name);
                copy_cache_name(item.device_id, props.device_id ? props.device_id : "");
            } else {
                // Unattributable non-host memory must remain visible as unknown;
                // dropping it would manufacture a falsely complete CPU/VRAM total.
                copy_cache_name(item.name, ggml_backend_buft_name(buft));
            }
            item.allocated_bytes = bytes;
            result.push_back(item);
        };
        if (ctx) {
            for (const auto & [buft, usage] : llama_get_memory_breakdown(ctx)) {
                uint64_t bytes = usage.model;
                for (const auto value : {usage.context, usage.compute}) {
                    if (value > std::numeric_limits<uint64_t>::max() - bytes)
                        throw std::overflow_error("GPU allocation accounting overflow");
                    bytes += value;
                }
                append(buft, bytes);
            }
        } else {
            for (const auto & [buft, bytes] : model->memory_breakdown()) append(buft, bytes);
        }
        *required = result.size();
        if (capacity < result.size()) return LLAMA_SHIM_BUFFER_TOO_SMALL;
        for (size_t i = 0; i < result.size(); ++i) out[i] = result[i];
        return LLAMA_SHIM_OK;
    });
}

int32_t llama_memory_breakdown_collect_v2(const llama_context * ctx,
        llama_memory_breakdown_entry * out, size_t capacity, size_t * required) {
    return llama_shim::guard([&]() -> int32_t {
        if (!ctx || !required || (!out && capacity)) return LLAMA_SHIM_INVALID_ARG;
        const auto breakdown = llama_get_memory_breakdown(ctx);
        *required = breakdown.size();
        if (capacity < breakdown.size()) return LLAMA_SHIM_BUFFER_TOO_SMALL;
        size_t i = 0;
        for (const auto & [buft, memory] : breakdown) {
            llama_memory_breakdown_entry entry{};
            copy_cache_name(entry.buft_name, buft ? ggml_backend_buft_name(buft) : "");
            entry.model = memory.model;
            entry.context = memory.context;
            entry.compute = memory.compute;
            out[i++] = entry;
        }
        return LLAMA_SHIM_OK;
    });
}

int32_t llama_model_cache_layers_collect(const llama_model * model,
        llama_cache_layer_info * out, size_t capacity, size_t * required,
        uint32_t * requires_equal_kv) {
    return llama_shim::guard([&]() -> int32_t {
        if (!model || !required || !requires_equal_kv || (!out && capacity)) return LLAMA_SHIM_INVALID_ARG;
        const auto & hp = model->hparams;
        if (hp.vocab_only) {
            llama_shim::set_error("cache topology requires a full model");
            return LLAMA_SHIM_INVALID_ARG;
        }
        *required = hp.n_layer_all;
        *requires_equal_kv = hp.is_mla() || model->arch == LLM_ARCH_DEEPSEEK4 || model->arch == LLM_ARCH_DEEPSEEK41;
        if (capacity < hp.n_layer_all) return LLAMA_SHIM_BUFFER_TOO_SMALL;
        for (uint32_t il = 0; il < hp.n_layer_all; ++il) {
            llama_cache_layer_info entry{};
            entry.layer = il;
            if (hp.is_recr(il)) {
                entry.kind = 3;
            } else if (hp.has_kv(il) && hp.n_head_kv(il)) {
                entry.kind = hp.is_swa(il) ? 2 : 1;
                entry.kv_heads = hp.n_head_kv(il);
                entry.key_head_dim = hp.n_embd_head_k(il);
                entry.value_head_dim = hp.n_embd_head_v(il);
                entry.window = hp.is_swa(il) ? hp.n_swa : 0;
            }
            auto * device = model->dev_layer(il);
            copy_cache_name(entry.device_name, device ? ggml_backend_dev_name(device) : "CPU");
            out[il] = entry;
        }
        return LLAMA_SHIM_OK;
    });
}

int32_t llama_context_cache_collect(const llama_context * ctx, llama_context_cache_info * info,
        llama_cache_state_info * out, size_t capacity, size_t * required) {
    return llama_shim::guard([&]() -> int32_t {
        if (!ctx || !info || !required || (!out && capacity)) return LLAMA_SHIM_INVALID_ARG;
        const auto & cp = ctx->get_cparams();
        *info = {cp.n_ctx, cp.n_batch, cp.n_ubatch, cp.n_seq_max,
            cp.flash_attn, cp.offload_kqv, cp.op_offload, 1};
        const auto & model = ctx->get_model();
        // MLA aliases/omits V storage. Native type_v() unconditionally
        // dereferences layers[0].v, so it is unsafe even for a nonempty arena.
        // Compressed attention needs a separate inspector, not invented K/V
        // geometry or an exception barrier around a null dereference.
        if (model.hparams.is_mla() || model.arch == LLM_ARCH_DEEPSEEK4 || model.arch == LLM_ARCH_DEEPSEEK41) {
            info->inspection_available = 0;
            *required = 0;
            return LLAMA_SHIM_OK;
        }
        std::vector<llama_cache_state_info> result;
        auto * memory = ctx->get_memory();
        if (auto * hybrid = dynamic_cast<llama_memory_hybrid_iswa *>(memory)) {
            append_swa(result, hybrid->get_mem_attn());
        } else if (auto * hybrid = dynamic_cast<llama_memory_hybrid *>(memory)) {
            append_cache(result, hybrid->get_mem_attn(), 1);
        } else if (auto * swa = dynamic_cast<llama_kv_cache_iswa *>(memory)) {
            append_swa(result, swa);
        } else if (auto * kv = dynamic_cast<llama_kv_cache *>(memory)) {
            append_cache(result, kv, 1);
        } else {
            info->inspection_available = 0;
        }
        *required = result.size();
        if (capacity < result.size()) return LLAMA_SHIM_BUFFER_TOO_SMALL;
        for (size_t i = 0; i < result.size(); ++i) out[i] = result[i];
        return LLAMA_SHIM_OK;
    });
}

size_t llama_memory_buffers_collect(
        llama_memory_t          mem,
        ggml_backend_buffer_t * out,
        size_t                  max_out) {
    if (mem == nullptr) {
        return 0;
    }
    const std::vector<ggml_backend_buffer_t> buffers = llama_memory_get_buffers(mem);
    if (out != nullptr) {
        const size_t n = buffers.size() < max_out ? buffers.size() : max_out;
        for (size_t i = 0; i < n; ++i) {
            out[i] = buffers[i];
        }
    }
    return buffers.size();
}

size_t llama_memory_breakdown_collect(
        const struct llama_context *          ctx,
        struct llama_memory_breakdown_entry * out,
        size_t                                max_out) {
    if (!ctx || !out || max_out == 0) {
        return 0;
    }

    const auto breakdown = llama_get_memory_breakdown(ctx);
    size_t     i         = 0;

    for (const auto & [buft, mb] : breakdown) {
        if (i >= max_out) {
            break;
        }

        const char * name = buft ? ggml_backend_buft_name(buft) : "";
        if (!name) {
            name = "";
        }

        std::strncpy(out[i].buft_name, name, sizeof(out[i].buft_name) - 1);
        out[i].buft_name[sizeof(out[i].buft_name) - 1] = '\0';
        out[i].model    = mb.model;
        out[i].context  = mb.context;
        out[i].compute  = mb.compute;
        ++i;
    }

    return i;
}

size_t common_device_memory_collect(
        const char *                           path_model,
        const struct llama_model_params *      mparams,
        const struct llama_context_params *      cparams,
        enum ggml_log_level                      log_level,
        struct common_device_memory_flat_entry * out,
        size_t                                   max_out,
        uint32_t *                               hp_ngl,
        uint32_t *                               hp_n_ctx_train,
        uint32_t *                               hp_n_expert) {
    if (!path_model || !mparams || !cparams || !out || max_out == 0) {
        return (size_t) -1;
    }

    try {
        std::vector<ggml_backend_dev_t> devs;
        uint32_t                          ngl = 0;
        uint32_t                          nct = 0;
        uint32_t                          nex = 0;

        const auto vec = common_get_device_memory_data(
                path_model, mparams, cparams, devs, ngl, nct, nex, log_level);

        if (hp_ngl) {
            *hp_ngl = ngl;
        }
        if (hp_n_ctx_train) {
            *hp_n_ctx_train = nct;
        }
        if (hp_n_expert) {
            *hp_n_expert = nex;
        }

        const size_t n = vec.size() < max_out ? vec.size() : max_out;
        for (size_t i = 0; i < n; ++i) {
            out[i].total   = vec[i].total;
            out[i].free    = vec[i].free;
            out[i].model   = vec[i].model;
            out[i].context = vec[i].context;
            out[i].compute = vec[i].compute;
        }
        return n;
    } catch (...) {
        return (size_t) -1;
    }
}

// ── guarded llama_quant_* wrappers ──────────────────────────────────────────
//
// These entry points throw for input they reject. A C++ exception unwinding
// through `extern "C"` into Rust is undefined behaviour and in practice aborts
// the process, so every one is caught here and reported as a null/error return.

struct llama_model * llama_quant_model_from_metadata_guarded(
        const struct llama_quant_model_desc * desc) {
    if (!desc) {
        return nullptr;
    }
    try {
        return llama_quant_model_from_metadata(desc);
    } catch (...) {
        return nullptr;
    }
}

struct quantize_state_impl * llama_quant_init_guarded(
        const struct llama_model *                 model,
        const struct llama_model_quantize_params * params) {
    if (!model || !params) {
        return nullptr;
    }
    try {
        return llama_quant_init(model, params);
    } catch (...) {
        return nullptr;
    }
}

int32_t llama_quant_compute_types_guarded(
        struct quantize_state_impl * qs,
        enum llama_ftype             ftype,
        struct ggml_tensor **        tensors,
        enum ggml_type *             result_types,
        size_t                       n_tensors) {
    if (!qs || !tensors || !result_types) {
        return -1;
    }
    if (n_tensors == 0) {
        return 0;
    }
    try {
        llama_quant_compute_types(qs, ftype, tensors, result_types, n_tensors);
        return 0;
    } catch (...) {
        return -1;
    }
}

int32_t llama_quant_tensor_allows_quantization_guarded(
        const struct quantize_state_impl * qs,
        const struct ggml_tensor *         tensor) {
    if (!qs || !tensor) {
        return -1;
    }
    try {
        return llama_quant_tensor_allows_quantization(qs, tensor) ? 1 : 0;
    } catch (...) {
        return -1;
    }
}

int32_t llama_context_set_causal_guarded(llama_context * ctx, bool causal) {
    return llama_shim::guard([&]() -> int32_t {
        if (!ctx) return LLAMA_SHIM_INVALID_ARG;
#ifdef LLAMA_RS_TURBOQUANT
        if (!causal && llama_pq3_context_uses_codec(ctx)) throw std::invalid_argument("PQ3 requires causal attention");
#endif
        ctx->set_causal_attn(causal);
        return LLAMA_SHIM_OK;
    });
}
