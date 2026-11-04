#!/usr/bin/env python3
"""Summarize local cache JSONL, numerical evaluations, or scheduler traces.

Only the Python standard library is needed. Inputs remain local; summaries
include their SHA256 so a published table can identify its source artifact.
Scheduler assignments show placement, not a GPU profiler's execution timeline.
"""
import argparse
import hashlib
import json
import re
import statistics
from collections import Counter, defaultdict
from pathlib import Path


def spread(values):
    return {"median": statistics.median(values), "min": min(values), "max": max(values)}


def benchmark(text):
    rows = []
    for line in text.splitlines():
        record = json.loads(line)
        if record["status"] != "ok":
            rows.append({k: record[k] for k in ("status", "profile", "context")})
            continue
        if not record["timing_valid_for_comparison"]:
            raise ValueError("trace-enabled timings cannot be benchmark baselines")
        warm = [t for t in record["trials"] if t["temperature"] == "warm"]
        memory = record["allocated"]["memory"]
        rows.append({
            "status": "ok", "profile": record["profile"],
            "context": record["allocated"]["effective"]["context_size"],
            "microbatch": record["allocated"]["effective"]["microbatch_size"],
            "input_tokens": warm[0]["input_tokens"], "warm_trials": len(warm),
            "prefill_tokens_per_second": spread([t["prefill_tokens_per_second"] for t in warm]),
            "decode_tokens_per_second": spread([t["decode_tokens_per_second"] for t in warm]),
            "time_to_first_text_seconds": spread([t["time_to_first_text_seconds"] for t in warm if t["time_to_first_text_seconds"] is not None]) if any(t["time_to_first_text_seconds"] is not None for t in warm) else None,
            "kv_payload_estimate_bytes": record["allocated"]["kv_payload_estimate_bytes"],
            "context_bytes": sum(m["context_bytes"] for m in memory),
            "compute_bytes": sum(m["compute_bytes"] for m in memory),
            "model_bytes": sum(m["model_bytes"] for m in memory),
            "peak_device_bytes": record["device_peak"],
            "model_sha256": record["identity"]["model_sha256"],
            "corpus_sha256": record.get("corpus_sha256"),
            "prompt_text_sha256": record.get("prompt_text_sha256"),
            "codec": record["allocated"]["effective"].get("codec"),
        })
    return rows


def quality(text):
    groups = defaultdict(list)
    for line in text.splitlines():
        if line.startswith("CACHE_QUALITY "):
            record = json.loads(line.removeprefix("CACHE_QUALITY "))
            key = (record["key"], record["value"], record["swa"], record["rotation"], record["microbatch"])
            groups[key].append(record)
    if not groups:
        raise ValueError("no CACHE_QUALITY records found")
    return [{"key": k[0], "value": k[1], "swa": k[2], "rotation": k[3], "microbatch": k[4],
             "checkpoints": len(rows), "max_logit_delta": max(r["logit_max"] for r in rows),
             "max_logit_rms": max(r["logit_rms"] for r in rows),
             "max_kl_reference_to_profile": max(r["distribution"]["kl_reference_to_profile"] for r in rows),
             "top1_agreement": sum(r["distribution"]["top1_agrees"] for r in rows) / len(rows),
             "mean_target_nll_delta": statistics.mean(r["distribution"]["target_nll_profile"] - r["distribution"]["target_nll_reference"] for r in rows)}
            for k, rows in groups.items()]


def placement(text, required):
    backend = None
    counts = Counter()
    for line in text.splitlines():
        split = re.search(r"## SPLIT #\d+: (\S+)", line)
        if split:
            backend = split[1]
        node = re.search(r"node #\s*\d+ \(([^)]+)\):", line)
        if node and node[1].strip() in ("SET_ROWS", "FLASH_ATTN"):
            if backend is None:
                raise ValueError("node has no owning scheduler split")
            counts[(node[1].strip(), backend)] += 1
    if {op for op, _ in counts} != {"SET_ROWS", "FLASH_ATTN"}:
        raise ValueError("cache write and flash-attention assignments are both required (GGML_SCHED_DEBUG=2)")
    if required and any(not backend.startswith(required) for _, backend in counts):
        raise ValueError(f"cache/attention assignment outside required backend {required}: {counts}")
    return [{"op": op, "backend": backend, "scheduled_nodes": count} for (op, backend), count in sorted(counts.items())]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("mode", choices=("benchmark", "quality", "placement"))
    parser.add_argument("artifact", type=Path)
    parser.add_argument("--require-backend", choices=("Vulkan", "CPU"))
    args = parser.parse_args()
    data = args.artifact.read_bytes()
    text = data.decode("utf-8")
    result = placement(text, args.require_backend) if args.mode == "placement" else {"benchmark": benchmark, "quality": quality}[args.mode](text)
    print(json.dumps({"schema": 1, "artifact_sha256": hashlib.sha256(data).hexdigest(), "results": result}, indent=2))


if __name__ == "__main__":
    main()
