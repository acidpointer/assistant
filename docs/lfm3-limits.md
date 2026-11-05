# LFM3-1.7B: limits and operating range

Status: research reference, verified against published sources and repository
code on 2026-11-05. Existing local measurements are identified below; no new
inference evaluation was performed for this document.

LFM3-1.7B accepts up to **262,144 tokens of total context**. System instructions,
user messages, conversation history, tools and generated output share that
capacity. There is no published separate system-prompt or user-message limit.
The useful operating range must be established for the intended workload: it
depends on instruction complexity, retrieval quality, runtime settings,
quantization and available memory.

This reference separates published model properties, application limits,
measured results and unanswered questions. It does not establish LFM3 as the
project's final model choice. See the [API contract](llm-api.md) for runtime
behavior and the [cache guide](kv-cache-baseline.md) for native configuration.

## One shared token budget

The published [model configuration](https://huggingface.co/LiquidAI/LFM3-1.7B/blob/main/config.json)
sets `max_position_embeddings` to `262144`. The
[chat template](https://huggingface.co/LiquidAI/LFM3-1.7B/blob/main/chat_template.jinja)
does not assign independent length budgets to message roles. It combines the
initial system message with advertised tool definitions in the system block.

| Component | Length constraint |
| --- | --- |
| System instructions | Share the total context; no separate published maximum |
| Individual user message | Shares the total context; no separate published maximum |
| History and supporting text | Share the total context |
| Tool definitions, calls and results | Share the total context |
| Generated reasoning and final answer | Must fit the remaining context; no separate architectural output ceiling is documented |

For each model evaluation:

```text
rendered input tokens + generated output tokens <= configured context capacity
```

The rendered input includes role markers, tool schemas and all retained messages.
Later tool continuations must fit their expanded input again, including the
assistant's tool call and the returned result.

Count tokens with the loaded model's tokenizer. UTF-8 bytes, characters and words
are different units; their relationship varies with language, formatting and
content. The published 256K window also does not automatically configure the
runtime to allocate that capacity.

## Capacity does not establish reliable instruction following

There is no published maximum reliable persona length, number of simultaneous
instructions, or number of advertised tools in the evidence reviewed here.
Reading a long document, retrieving one fact and following a detailed character
description are different tasks. Their limits must be evaluated separately.

Liquid reports the following scores in its
[model-card evaluation](https://huggingface.co/LiquidAI/LFM3-1.7B#performance):

| Evaluation | Reported score |
| --- | ---: |
| IFEval | 96.66 |
| IFBench | 73.03 |
| RULER at 128K | 65.56 |

These are vendor-reported benchmark scores, not success probabilities for an
arbitrary request. The evaluations used BF16 inference in vLLM, with settings
varying by benchmark. They do not establish equivalent quality for a particular
GGUF, cache precision, sampler or application prompt. In particular, the long
context result does not establish accurate use of every detail throughout 256K.

## Current repository limits

These are source defaults, not an inspection of a running daemon. CLI flags and
library configuration can override them. Byte limits remain independent of the
token budget.

| Boundary | Default or enforced limit | Source |
| --- | ---: | --- |
| Daemon context | 8,192 tokens | [`cli.rs`](../crates/assistant-daemon/src/cli.rs), `Args::ctx_size` |
| Daemon output per model turn | 4,096 sampled tokens | `Args::max_tokens` |
| Output across an interaction, including tool continuations | 8,192 sampled tokens | `Args::run_max_tokens` |
| Library context when constructed directly | 4,096 tokens | [`llm.rs`](../crates/assistant/src/engine/llm.rs), `LLMConfig::new` |
| Library output per model turn | 512 sampled tokens | `LLMConfig::new` |
| One application/WebUI user input | 16 KiB of UTF-8 text | [`application/mod.rs`](../crates/assistant/src/application/mod.rs), `INPUT_BYTES` |
| Combined runner input accounting | 256 KiB | [`chat/types.rs`](../crates/assistant/src/chat/types.rs), `RunLimits::input_bytes` |
| One tool result | 16 KiB | `RunLimits::tool_result_bytes` |
| Retained working messages | 128 messages | `RunLimits::working_messages` |
| Model turns / tool calls per interaction | 8 / 8 | `RunLimits` |
| Rendered chat prompt before tokenization | 1 MiB | [`engine/context.rs`](../crates/assistant/src/engine/context.rs), `prepare_chat` |

Sampled-token ceilings include reasoning and tool protocol, not only visible
answer text. Runner byte accounting covers the system text, supporting context
and working messages with protocol allowances; it is not a token count or the
exact size of the final rendered prompt. Advertised tool schemas also consume
the rendered token budget.

### What happens when input approaches capacity

The worker first tries to reserve the requested output allowance. It removes
old complete exchanges until input plus that allowance fits. System text,
supporting context and the current interaction remain intact.

When no old exchange can be removed, the worker reduces the output allowance to
the remaining capacity. If the retained input fills or exceeds capacity, it
returns `EngineError::Budget`. A reduced allowance is reported through
`limit_reason = "context_capacity"`. See
[`runtime/worker.rs`](../crates/assistant/src/runtime/worker.rs), `prepare_input`
and `begin`.

With the daemon's 8K context and 4K output ceiling, the initial reservation leaves
roughly 4K for the entire rendered input. A long system prompt or large tool
catalog therefore displaces recent history even though the model advertises
256K. Raising context capacity alone does not remove the separate byte limits.

## Memory cost of long context

LFM3 has 13 short-convolution layers, five sliding-window attention layers with a
256-token window, and six full-attention layers. The window applies only to the
sliding layers; it does not restrict the whole model to 256 tokens. See Liquid's
[architecture description](https://www.liquid.ai/blog/lfm3).

The full-attention layers retain growing KV state. From the published
configuration, their FP16 payload per token is:

```text
6 layers * 2 (K and V) * 4 KV heads * 128 dimensions * 2 bytes = 12 KiB
```

At 262,144 tokens, those six layers alone require approximately **3 GiB** of
FP16 KV payload. This is a calculation, not a measured allocation. It excludes
weights, sliding-window and convolution state, padding, compute buffers and
runtime overhead. Full SWA allocation reserves additional storage; compact SWA
reduces that storage without removing the global caches.

The existing [local cache baseline](kv-cache-baseline.md#measured-results-and-default-decision)
measured these allocations at 32K capacity:

| FP16 cache profile | Context allocation | Compute allocation |
| --- | ---: | ---: |
| Full SWA | 704.609 MiB | 410.051 MiB |
| Compact SWA | 392.109 MiB | 347.551 MiB |

These measurements used Q6_K model weights on an NVIDIA RTX A500 Laptop GPU.
Weights are additional and shared across contexts; the table is not peak VRAM.
The 32K capacity tests used 1,024-token prompts, so they do not establish
near-full 32K quality or latency. Longer retained inputs also increase prompt
processing work, particularly when a changed prefix prevents cache reuse.

## Other known limits and unverified behavior

The [model card](https://huggingface.co/LiquidAI/LFM3-1.7B) recommends retrieval or
search for knowledge-heavy tasks and discourages complex multistep reasoning.
It also notes that disabling thinking can be inconsistent, especially on
reasoning-heavy tasks. Its template enables thinking by default; this project's
CLI and library defaults request it disabled.

Liquid recommends `temperature=0.1`, `top_k=50` and `repetition_penalty=1.1` for
general chat and tools. The current engine uses greedy decoding after any chat
grammar. Sampling differences should be recorded in model-quality comparisons;
neither setting guarantees correctness.

Ukrainian is absent from the model's list of 16 supported languages. That leaves
its quality unverified rather than establishing that it cannot use the language.
The reviewed evidence also does not establish sustained character fidelity,
behavior around intimate adult subjects, or combined persona-and-tool accuracy.

Existing [local quality fixtures](cache-quality-evaluation.md#recorded-first-device-reference)
passed 12/17 cases with FP16 or Q8 cache and 10/17 with Q4 cache. These are small,
exact-answer development tests, not an overall accuracy estimate. Cache precision
is separate from weight quantization; these results must not be described as a
comparison of FP16 and Q4 model weights.

## Establishing an operating range

A proposed first evaluation should vary system prompts of 256, 1,024 and 4,096
tokens against total rendered inputs of 4K, 8K, 16K and 32K, with output space
reserved separately. These are experiment points, not recommended or validated
limits. Where instructions, tools and reserved output cannot fit together,
record a budget rejection rather than scoring a model-quality failure.

Vary prompt length and instruction count independently. Added biography text
tests a different capability from added behavioral rules. At each size, check
persona consistency, instruction omissions, factual recall, clarification and
tool correctness in the same conversation. Include facts at different positions
and corrections to earlier information.

Record the exact model artifact, template, tokenizer, weight and cache precision,
sampler, thinking mode and effective output allowance. Report correctness beside
first-response latency and peak memory. This establishes a usable range for the
assistant's planned behavior rather than treating advertised capacity as a
quality guarantee.
