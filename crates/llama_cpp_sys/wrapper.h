// Only include the public headers that ship in llama.cpp/include/.
// llama-grammar.h and llama-sampler.h live in src/ (internal) — everything
// we need from them is already re-exported through llama.h.
#include "llama.h"
#include "common.h"
#include "fit.h"
#include "mtp_shim.h"
#include "shim_support.h"
#include "ext_shim.h"
#include "chat_shim.h"
#include "common_shim.h"

#ifdef LLAMA_RS_TURBOQUANT
#include "ggml-cpu.h"
#endif

// llama-ext.h lives in src/ but exports LLAMA_API entry points (pre-norm
// embeddings setter/getters, memory-breakdown, etc). Pull it in so bindgen
// emits the corresponding extern fns.
#include "llama-ext.h"

// Vulkan lifecycle declarations are additive; callers must enable the native
// Vulkan backend before referencing these symbols.
#include "ggml-vulkan.h"

#ifdef RPC_SUPPORT
#include "ggml-rpc.h"
#endif

#ifdef MTMD_SUPPORT
#include "mtmd.h"
#include "mtmd-helper.h"
#endif
