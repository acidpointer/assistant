# Deterministic cache quality evaluation

Status: experimental evaluation harness, 2026-11-04. This supplements the
[cache preparation baseline](kv-cache-baseline.md) before new codecs are usable.
It exercises existing FP16/full, FP16/compact, Q8/compact and Q4/compact profiles.
Enabling `turboquant` currently tests build compatibility only; it does not
select PQ3 inference.

## Fixed inputs and scoring

[`cache-quality-v1.json`](../crates/assistant/tests/fixtures/cache-quality-v1.json)
contains four authored prose/code texts, five short tasks and a retrieval
recipe. The [manifest](../crates/assistant/tests/fixtures/cache-quality-manifest.json)
records SHA256 and FNV1a64. Python verifies SHA256 before running; the default
Rust structural test verifies FNV and fixture consistency. No dataset, model or
dependency download is implicit.

Task evaluation uses the public resident runtime and calculator, with a typed
fixed-clock test tool returning `Thu Jan 15 12:34:56 UTC 2026`. Application tool
policy is unchanged. Success requires the expected answer and actual successful
tool receipts for required tools; arithmetic checks include the returned value.
Retrieval and literal-reading answers must match exactly after trimming
whitespace and simple quote/Markdown wrappers. Echoing the needle during an
unfinished explanation fails. The existing two-tool model failure is retained
as a scored case. Live-clock
integration tests remain separate.

Retrieval inserts one known key/value record among indexed deterministic
filler at the beginning, middle or end. Filler count is chosen using the actual
GGUF tokenizer to fit 1024 or 3500 user tokens, within a 4096-token context.
Actual prepared token counts are recorded. The full job has 17 cases/profile:
five short tasks and twelve retrieval cases. Smoke has seven: five tasks and
two middle-position 1024-token retrieval cases. Context/history is cleared
between independent cases. A quality failure is recorded as data; a native,
model, configuration or execution error fails the evaluation command.

Numerical evaluation feeds the exact same token sequences for all profiles,
with checkpoints every 32 tokens and at the final target. At each checkpoint
it records reference-to-profile KL divergence, target negative log likelihood
(NLL), top-token agreement and token-stream identity. FP16/full is repeated
twice to expose backend variation. The small texts test representative syntax
and vocabulary, not long-context language quality. Retrieval supplies the
separate context-length dimension.

## Reproduce and inspect

```sh
python3 scripts/cache-quality-results.py verify
python3 scripts/cache-quality-results.py run --model /path/to/model.gguf \
  --output /tmp/cache-quality.json --full --require-vulkan --turboquant
python3 scripts/cache-quality-results.py check /tmp/cache-quality.json \
  --candidate f16 --swa Full
```

Omit `--full` for smoke or `--require-vulkan` for CPU-compatible evaluation.
`--turboquant` compiles the experimental CPU reference alongside the existing
cache profiles. The runner hashes the model, sets explicit required-fixture
flags, runs task and numerical jobs separately, and saves raw logs plus scored
JSON, including model/source/active-patch identities and executable/log hashes.
Missing models and missing requested Vulkan execution fail required jobs;
default workspace tests skip these optional model evaluations cleanly. The
Vulkan requirement checks observed weight-device allocations; it does not
replace the separate operator-placement audit or prove every operation runs on
the GPU. Diagnostics retain `gpu_placement: not_observed` when no trace exists.

The Rust entry points are
[`tests/cache_quality.rs`](../crates/assistant/tests/cache_quality.rs) and
[`context/cache_tests.rs`](../crates/assistant/src/engine/context/cache_tests.rs).
Without the runner, supply `ASSISTANT_QUALITY_MODEL`, its SHA256 in
`ASSISTANT_QUALITY_MODEL_SHA256`, and `ASSISTANT_QUALITY_REQUIRE_MODEL=1`.
`ASSISTANT_QUALITY_FULL=1` and `ASSISTANT_QUALITY_REQUIRE_VULKAN=1` request the
corresponding modes. Hash verification belongs to the runner; the Rust job
records the supplied identity rather than reading a multi-gigabyte model twice.

## Gates declared before PQ3 results

The versioned fixture fixes these experimental candidate gates:

| Measure | Gate |
| --- | --- |
| Mean target NLL increase against FP16/full | ≤ 0.05 nats |
| Maximum checkpoint KL from FP16/full | ≤ 0.05 |
| Cases passed by both FP16/full and compact Q4 | No lost cases |
| Retrieval accuracy against the better of FP16/full and Q4 | No decrease |
| Throughput regression | Investigate > 5%; measured by the separate benchmark |

The checker requires complete matching task sets and identical numerical
checkpoints/token identities. Repeated FP16 variation remains in raw records;
it must be inspected before attributing a tiny change to compression. A profile
failing a declared gate remains a failed experiment. Do not adjust thresholds
after observing PQ3 simply to pass it.

These are small, inspectable development fixtures, not a statistical adoption
study. They do not establish tool reliability, broad model quality, 32K behavior,
or AMD/Intel coverage. Promotion still requires broader corpora/tasks,
long-context evaluation, memory/latency measurements and actual vendor jobs in
the [implementation plan](turboquant-implementation-plan.md).

Regression tests for artifact completeness, parser boundaries and acceptance
gates run with `PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts/tests -v`.

## Recorded first-device reference

The [versioned artifact](benchmarks/cache-quality-2026-11-04.json) records the
feature-enabled Vulkan run on NVIDIA RTX A500 Laptop GPU, driver 615.78.08,
with the same model SHA256 as the preparation benchmark. It preserves all case
scores, 85 numerical records, diagnostics and model/source/patch/binary/log
identities. Each profile ran 17 tasks, including twelve retrieval cases.
Exact-answer failures remain visible in the table and raw case records. This
fixed-clock result does not erase the earlier live-clock two-tool failure.

| Profile | Tasks | Mean target NLL delta | Maximum checkpoint KL |
| --- | ---: | ---: | ---: |
| f16/Full | 12/17 | 0.000000 | 0.000000 |
| f16/Compact | 12/17 | 0.000000 | 0.000000 |
| q8/Compact | 12/17 | -0.003896 | 0.000398 |
| q4/Compact | 10/17 | 0.044142 | 0.075786 |

Each numerical run has 17 checkpoints across four texts. The repeated FP16/full
run's maximum KL was 0. Q4's worst KL exceeds the declared 0.05
experimental candidate gate; its passing tasks do not override that failure.
Q4 also loses two exact-answer retrieval cases relative to FP16/full in this
fixture. The artifact records the failed verdict without changing thresholds. No PQ3
candidate exists yet, and these debug runs are not performance measurements.
