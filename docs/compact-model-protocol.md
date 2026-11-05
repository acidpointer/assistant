# Compact model protocols for memory and tools

Status: research and tokenizer experiment, 2026-11-05. No application or native
code changed. Generation accuracy, latency and training requirements remain
unmeasured. This proposal complements the [memory design](memory-system-plan.md)
and [LFM3 operating-range research](lfm3-limits.md).

The [implementation specification](memory-implementation-spec.md) selects named
JSON as v1's internal proposal format. Compact codecs remain a separately gated
optimization; they are not prerequisites for delivering the complete system.

## Conclusion

A compact domain-specific protocol is worth evaluating for isolated memory
jobs. The strongest starting point is a small vocabulary of meaningful operations,
short scoped references and host-supplied structure. A universal symbolic language
is a substantially riskier hypothesis. Short characters are not necessarily
short token sequences, and fewer generated tokens do not establish lower cost
per correct operation.

The local tokenizer experiment confirms that reducing repeated structure can
save tokens. It also shows that replacing `search` with `S` need not save anything.
Keep typed Rust operations and canonical records; experiment with their model-facing
representation. Treat the representation as a versioned codec, not the memory
database schema, a programming language or an authorization mechanism.

## Research evidence and its limits

| Primary source | Relevant result | Implication for this project |
| --- | --- | --- |
| [Lost in Space](https://arxiv.org/abs/2502.14969), Hamilton and Mimno | Across four model families and several classification tasks, conventional output formats and leading-whitespace tokens affected accuracy; smaller models were particularly sensitive | Benchmark complete token contexts and familiar labels, not just character lengths. This does not prove a particular LFM3 tool syntax works |
| [StructureBench](https://ijcai.org/proceedings/2026/267) | Separates syntax from semantic correctness across on-device models; constraints can preserve syntax while reducing semantic accuracy on some small-model/complex-grammar combinations | A parser pass is not a correct memory operation. Keep argument meaning, evidence and abstention as separate gates |
| [TOON project documentation](https://raw.githubusercontent.com/toon-format/toon/main/packages/toon/README.md) | Provides a compact JSON-data representation with tabular forms; identifies uniform records as a favorable case and nested/irregular records as less favorable | Compare compact result tables for host-to-model input. Its reported savings do not establish output-generation reliability or LFM3 latency |
| [ToolkenGPT](https://arxiv.org/abs/2305.11554) | Represents tools using learned tool embeddings and predicts tool tokens, followed by argument completion | The “one symbol means a tool” idea has a training-based precedent. Learned token semantics are different from inventing punctuation in a prompt |
| [LFM3 chat template](https://huggingface.co/LiquidAI/LFM3-1.7B/blob/main/chat_template.jinja) | Renders tool calls with function/parameter tags and advertises tool definitions in the prompt | Compare against the actual native template, not an assumed JSON wire representation |

These are primary-source references, not evidence that the proposed protocol has
passed this daemon's tests. No cited result establishes that a short legend can
teach a 1.7B or smaller model an arbitrary high-density language reliably.

## What can actually be compressed

Separate four opportunities:

1. **Serialization:** remove repeated keys, wrappers and predictable metadata.
   Positional tuples, function notation and column headers can help.
2. **References:** refer to a source/entity/query already supplied by the host
   instead of copying its text, UUID or full structure into the response.
3. **Decision scope:** let host code choose the job kind and perform deterministic
   work. Ask the model for uncertain fields or a bounded selection only.
4. **Reasoning/generation policy:** reduce unnecessary explanation or thinking
   for simple jobs, but evaluate task accuracy. Compression of tool syntax does
   not automatically shorten internal reasoning or remove model turns.

The first three need no new model vocabulary. Actual special tokens require
compatible tokenizer/model embeddings and learned semantics; adding a tokenizer
entry alone does not teach the model what it should mean. Fine-tuning a stable
ASCII protocol is a separate intermediate experiment before vocabulary changes.

Characters, token IDs and semantic concepts are different units. Existing words
can be single tokens; Unicode symbols may split into multiple tokens. Whitespace
and neighboring punctuation affect segmentation. Token IDs are specific to the
artifact and cannot be used as a portable protocol across models.

Do not compress factual prose or persona instructions into opaque shorthand by
default. Preserve readable evidence, modality, attribution and temporal conditions.
A compressed reference should resolve to that content, not replace it with an
unexplained number that the model has to guess.

## Local tokenizer experiment

Used the existing `llama-tokenize` binary against the locally available LFM3-1.7B
Q6_K derivative. This is tokenizer-only inspection, not an evaluation of official
LFM3 weights or of the derivative's behavior. The tool initializes a vocabulary-only
model; its context warning is recorded in the artifact and no generation ran.

The [raw measurements](benchmarks/compact-protocol-tokens-2026-11-05.json) include
exact input strings, token IDs, flags, model SHA-256, executable SHA-256, embedded
tokenizer metadata hash and chat-template hash. No machine-local model path is
recorded. The checked-out upstream pin is
`a34de8618b25dfc32ee43749db47d767a9f0a25d`; the executable hash identifies the
pre-existing binary actually used, without asserting it was rebuilt this turn.

| Representation of the same search intent | Tokens |
| --- | ---: |
| Native function/parameter-tag body from the embedded template | 25 |
| `{"name":"memory.search","arguments":{"query":"project blockers","limit":5}}` | 18 |
| `{"op":"search","q":"project blockers","k":5}` | 15 |
| `memory.search(query="project blockers",limit=5)` | 12 |
| `["search","project blockers",5]` | 9 |
| `search("project blockers",5)` | 8 |
| `S "project blockers" 5` | 8 |
| `?"project blockers"#5` | 7 |

The 25-to-8 reduction is **68% of this isolated body**, not 68% end-to-end
latency or total interaction tokens. No full prompt, protocol legend, examples,
thinking, end-of-generation tokens, response correctness or retries are included.
Native-body counting enables special-token parsing; ordinary text cases disable
it. The local embedded template was inspected and uses the same function/parameter
tag structure described above. A full generated token trace may tokenize text
differently from re-encoding its final string, so record actual sampled IDs later.

Additional observations:

- `search` and `follow` are each one token here. Their single-letter aliases
  cannot beat that standalone cost. `supersede` is two tokens, so the answer is
  vocabulary-dependent.
- `🔍` is one token while `∃` is two. Neither count establishes comprehension.
- `search(q1,5)` is six tokens and `S q1 5` is five, but both require an offered
  query-reference table. The table and its interpretation are additional input.
- A fixed search job can emit `["project blockers",5]` in seven tokens without
  generating its operation name at all.
- A two-record JSON result example took 31 tokens versus 23 for a table with
  one header. This small example does not establish a general format ranking.
- The example symbolic-protocol legend took 47 tokens; the readable function
  legend took 44. A short payload can require a longer explanation.

Reproduce a sample using the existing tokenizer executable, supplied model and
the recorded text via stdin, with `--ids --no-bos --no-escape` and
`--no-parse-special` unless that sample records `parse_special: true`. Count the
returned token-ID list. Use the recorded hashes to distinguish artifacts; do not
assume every model named LFM3 has an identical tokenizer/template.

## The proposed protocol experiment

Start with a deliberately small job-specific protocol. For example, an isolated
recall planner receives a legend, the current query and bounded scoped handles:

```text
search("query", limit)   search memory; limit 1..10
follow(cN, relation)      follow an offered claim handle and allowed relation
none                     no further investigation needed
```

Use meaningful names first. Compare a JSON tuple encoding before implementing a
new parser: `["search","project blockers",5]` is only one token longer than the
function example. It preserves ordinary JSON escaping and can use the existing
binding's schema-to-grammar path. This is a useful baseline even if a dedicated
syntax later wins.

For a fixed extraction job the operation is already known. Generate bounded
tuples of subject, predicate, object and source handle rather than repeating a
complete tool envelope. Positional fields need a fixed schema with explicit
representations for unknown values, negation, qualifications and multiple sources.
Never achieve a shorter answer by deleting those semantics.

The host supplies principal, operation/job identity, permissions, source revisions,
idempotency keys and persistence policy. The model does not need to regenerate
them. A local handle maps to a complete host record with those properties.
Handle tables are immutable for a generation, scoped to that operation and
versioned with the prompt; reject unknown, stale or wrong-kind references.
Do not reassign a handle while its previous meaning remains in working context.

An even smaller experiment is **candidate selection**. The host offers a few
complete eligible proposals and asks for one handle or `none`. This can turn a
long generation into a classification decision. It is appropriate only where
the candidate set can be constructed correctly: missing candidates impose a
quality ceiling, and the cost of generating/describing them must be counted.
Preserve a bounded open-query path when selection is insufficient.

Keep the codebook stable per protocol version and job type. Do not invent a new
random alphabet every turn; that adds interpretation cost and destroys learned
regularity. Extension installation may add operations through a validated
registry, but advertise only the small task-relevant subset. Avoid another model
round trip solely to select a tool subset unless its measured benefit pays for it.

## Grammar, parser and execution contracts

The pinned binding already exposes `ChatApplyParams::with_grammar` and
`with_json_schema` in [`chat.rs`](../crates/llama_cpp/src/chat.rs).
[`prepare_chat`](../crates/assistant/src/engine/context.rs) constructs rendered
input, tokenization and parser state together; generation uses the prepared
chat grammar. [`prepare_input`](../crates/assistant/src/runtime/worker.rs) currently
wires chat/tools, not a general internal-job codec contract.

A prototype must preserve this coherence. Add a typed internal output contract
through the shared preparation path, with protocol version, schema/grammar,
matching parser, allowed handles, limits and termination rules. Do not attach a
compact grammar to the native tool parser and assume it understands a different
language. Native tool markers, lazy grammar triggers, thinking and prefilled
prefixes require explicit compatibility. Begin with isolated jobs that cannot
execute ordinary tools, leaving conversational tool calls on the native path.

Pipeline:

```text
bounded task + evidence + handles
    -> model under selected output grammar
    -> complete-message parse
    -> typed proposal
    -> scope / reference / revision / semantic validation
    -> existing authorized host operation or memory transaction
```

The codec never evaluates generated SQL, Rust, shell text or arbitrary functions.
Use fixed opcodes, fixed arities, bounded lists/strings/numbers and an explicit
`none`/unresolved alternative. Strings need a real escaping rule; quoted evidence
containing delimiters must remain data. Reject trailing commands, incomplete
output, duplicate/conflicting edits, invalid reference types and token-limit
termination. No side effects occur while parsing a streaming prefix.

Constrained decoding can restrict syntax and even enumerate allowed handles,
but it cannot prove that an allowed handle is the correct evidence. Keep memory's
semantic validation, revision checks and authorization exactly as specified in
the memory plan. “Secret” notation is not a security boundary or protection
against prompt injection.

Forcing a token with grammar still normally requires decoding it in the current
engine. Grammar does not automatically skip all predictable syntax. Host-filled
fields remove generation work directly; fast-forwarding fixed spans is a separate
native scheduling/cache experiment with its own correctness requirements.

## Economics and break-even measurement

Optimize **cost per correct completed job**, with latency and correctness shown
separately. For a bounded job including any permitted retries:

```text
time = prompt preparation + uncached prefill + all decoding
       + grammar/parser/validation + tool/storage time + retry overhead
```

Also record rendered input, evaluated prefill, sampled output, cached-prefix
length and native context memory. Existing engine metrics distinguish prefill
and generation time; use actual measurements rather than assuming input and
output tokens have equal cost.

For a simple comparison, savings require the avoided decoding time to exceed
extra prefill, grammar work and expected retry/failure cost. A longer legend may
pay off across repeated jobs if its exact token prefix is reused; it is not free
on cold starts, context eviction or prefix changes. Frequent handle-table/schema
changes can prevent reuse. Benchmark cold and warm cases independently.

As an illustrative arithmetic example only: a 40-token additional legend and
an eight-token per-job payload saving require more than five uses to reduce the
sum of token counts if the legend is charged once. This ignores unequal
prefill/decode costs, templates, retries and cache behavior; it is not a speed
prediction. A single extra failed model pass can outweigh several small savings.

Compress host-produced tool results separately from model outputs. Uniform tables
and selective field projection can reduce input without teaching the model to
generate a new language. Keep authoritative full records outside the prompt and
provide detail on demand. Do not omit sources or uncertainty to improve a token
metric. Profile compilation should similarly provide only relevant effective
behavior, rather than serializing the entire persona history into every job.

## Controlled evaluation and adoption decision

Compare these arms on identical semantic jobs:

| Arm | Purpose |
| --- | --- |
| Native model tool protocol | Real conversational baseline, including its template and schema |
| Compact named JSON | Familiar syntax with reduced duplication |
| JSON tuples for fixed jobs | Separate positional/schema savings from inventing a language |
| Readable function protocol | Test familiar compact syntax |
| Letter/symbol protocol | Test whether the last few tokens justify reduced interpretability |
| Host candidate selection | Measure scope reduction, including candidate recall and input overhead |

Use a held-out set covering search, graph following, evidence extraction,
correction proposals, no-op/abstention, conflicting sources and malformed data.
Include English/Ukrainian strings, quotes/newlines, unfamiliar names, reordered
handle tables, unseen combinations, truncation and adversarial instructions in
evidence. Keep developer and held-out examples separate. Require all encodings
to represent the same semantics; otherwise label them as different tasks.

First compare codecs with identical host-defaulted fields and reference tables;
then separately ablate removal of metadata, references and candidate selection.
This prevents attributing a host-side simplification to punctuation. Count every
legend, example, tool description, result and repair prompt.

Run the available LFM3 artifact first, then each smaller candidate independently.
Record model/tokenizer/template hashes, quantization, sampler, thinking policy,
grammar, codec and prompt versions. Benchmark the production greedy setting and
any proposed sampler separately. Grammar construction, output termination and
thinking tokens are part of the measured workload. Syntax tests use fake output;
real-model tests are needed for meaning and latency.

Report exact operation/argument correctness, evidence precision, abstention,
structural validity, retries, unprocessed tails, total tokens, p50/p95 latency and
resource use. Count silent semantic errors even when parsing succeeds. Use paired
case outcomes and uncertainty intervals; do not choose a winner from a tiny
average token reduction. Establish a task-specific noninferiority margin before
examining held-out results. Wrong-target mutation/authority violations are
hard failures, not acceptable averages.

Proposed promotion gate: all deterministic codec/validation fixtures pass,
semantic quality meets the preregistered baseline requirement, and measured
total latency improves materially (initial experiment target: at least 15%) at
the same workload and retry allowance. Publish output-token reduction separately.
If tuple JSON matches a symbolic codec's quality and cost, prefer the simpler
existing parsing path. No speed/quality gate has been run in this research.

If prompt-only compact protocols miss quality requirements, preserve native
tools and evaluate a narrowly fine-tuned protocol using ordinary vocabulary.
Only consider learned special tool tokens after stable semantics, training data,
conversion support and cross-version maintenance justify them. The most ambitious
version of the user's idea is technically plausible, but it is model training
and deployment work, not a free gain from a short language legend.
