#!/usr/bin/env python3
"""Verify fixed fixtures, run explicit model-quality jobs, and save scored results.

Uses only the Python standard library. Accuracy failures are data; native/test
failures fail the command. Candidate acceptance is separate from baseline capture.
"""
import argparse
import hashlib
import json
import os
import re
import statistics
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
FIXTURES = ROOT / "crates/assistant/tests/fixtures"


def digest(path):
    hasher = hashlib.sha256()
    with path.open("rb") as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def verify():
    manifest = json.loads((FIXTURES / "cache-quality-manifest.json").read_text())
    for record in manifest["files"]:
        if Path(record["file"]).name != record["file"]:
            raise ValueError("fixture paths must name files within the fixture directory")
        if digest(FIXTURES / record["file"]) != record["sha256"]:
            raise ValueError(f"fixture checksum mismatch: {record['file']}")
    return manifest


def parse(path):
    records = {"tasks": [], "text": [], "identity": [], "configurations": []}
    prefixes = {"CACHE_TASK_QUALITY ": "tasks", "CACHE_TEXT_QUALITY ": "text",
                "CACHE_QUALITY_IDENTITY ": "identity", "CACHE_QUALITY_CONFIG ": "configurations"}
    for line in path.read_text().splitlines():
        if line.startswith("test ") and " ... " in line:
            line = line.split(" ... ", 1)[1]
        for prefix, key in prefixes.items():
            if line.startswith(prefix):
                record = json.loads(line[len(prefix):])
                if key == "text":
                    record["profile"] = {"q8_0": "q8", "q4_0": "q4"}.get(record["profile"], record["profile"])
                records[key].append(record)
                break
    return records


def summarize(records):
    groups = {}
    for record in records["tasks"]:
        key = (record["profile"], record["swa"])
        group = groups.setdefault(key, {"tasks": [], "text": []})
        group["tasks"].append(record)
    for record in records["text"]:
        profile = {"q8_0": "q8", "q4_0": "q4"}.get(record["profile"], record["profile"])
        key = (profile, record["swa"])
        groups.setdefault(key, {"tasks": [], "text": []})["text"].append(record)
    summary = []
    for (profile, swa), group in groups.items():
        # Repeated FP16 runs remain in raw records; summarize its first run only.
        points = [r for r in group["text"] if r["repeat"] == 0]
        summary.append({"profile": profile, "swa": swa,
                        "task_count": len(group["tasks"]),
                        "tasks_passed": sum(r["passed"] for r in group["tasks"]),
                        "failed_tasks": [r["case"] for r in group["tasks"] if not r["passed"]],
                        "text_checkpoints": len(points),
                        "mean_target_nll": statistics.mean(r["metrics"]["target_nll_profile"] for r in points) if points else None,
                        "mean_target_nll_delta": statistics.mean(r["metrics"]["target_nll_profile"] - r["metrics"]["target_nll_reference"] for r in points) if points else None,
                        "max_checkpoint_kl": max((r["metrics"]["kl_reference_to_profile"] for r in points), default=None)})
    return summary


def check_candidate(records, candidate, swa, limits):
    """Apply frozen gates once a candidate format is supported by the harness."""
    def tasks(profile, mode):
        return {r["case"]: r for r in records["tasks"] if r["profile"] == profile and r["swa"] == mode}
    reference, q4, selected = tasks("f16", "Full"), tasks("q4", "Compact"), tasks(candidate, swa)
    if not reference or set(reference) != set(q4) or set(reference) != set(selected):
        raise ValueError("candidate and both references must have the same complete task set")
    lost = [case for case, r in reference.items() if r["passed"] and q4[case]["passed"] and not selected[case]["passed"]]
    retrieval = [case for case, r in reference.items() if r["kind"] == "retrieval"]
    def accuracy(group):
        return sum(group[case]["passed"] for case in retrieval) / len(retrieval) if retrieval else 0.0
    points = [r for r in records["text"] if r["profile"] == candidate and r["swa"] == swa and r["repeat"] == 0]
    ref_points = [r for r in records["text"] if r["profile"] == "f16" and r["swa"] == "Full" and r["repeat"] == 0]
    if not points or {(p["corpus"], p["tokens"], p["token_stream_fnv1a64"]) for p in points} != {(p["corpus"], p["tokens"], p["token_stream_fnv1a64"]) for p in ref_points}:
        raise ValueError("candidate and reference must have identical complete token checkpoints")
    mean_delta = statistics.mean(r["metrics"]["target_nll_profile"] - r["metrics"]["target_nll_reference"] for r in points)
    max_kl = max(r["metrics"]["kl_reference_to_profile"] for r in points)
    drop = max(accuracy(reference), accuracy(q4)) - accuracy(selected)
    passed = (len(lost) <= limits["max_lost_tasks_passed_by_fp16_and_q4"] and
              drop <= limits["max_retrieval_accuracy_drop"] and
              mean_delta <= limits["max_mean_target_nll_increase_nats"] and
              max_kl <= limits["max_checkpoint_kl_reference_to_candidate"])
    return {"passed": passed, "lost_tasks": lost, "retrieval_accuracy_drop": drop,
            "mean_target_nll_delta": mean_delta, "max_checkpoint_kl": max_kl}



def source_identity(turboquant):
    """Identify dirty source content as well as the unchanged upstream pin."""
    def git(*args):
        return subprocess.check_output(["git", *args], cwd=ROOT, text=True).strip()
    names = git("ls-files", "--cached", "--others", "--exclude-standard").splitlines()
    inputs = {}
    for name in sorted(set(names)):
        path = ROOT / name
        if (name.startswith("crates/") or name.startswith("scripts/") or name in ("Cargo.toml", "Cargo.lock")) and path.is_file() and path.suffix in (".rs", ".h", ".hpp", ".c", ".cpp", ".patch", ".toml", ".lock", ".json", ".py", ".cmake"):
            inputs[name] = digest(path)
    patches = ROOT / "crates/llama_cpp_sys/patches"
    active = sorted(p for p in patches.glob("*.patch") if p.name[:4] in ("0003", "0004", "0005", "0006", "0007") or turboquant and p.name.startswith("0008"))
    return {"workspace_revision": git("rev-parse", "HEAD"),
            "native_revision": git("-C", "crates/llama_cpp_sys/llama.cpp", "rev-parse", "HEAD"),
            "rustc": subprocess.check_output(["rustc", "--version"], text=True).strip(),
            "active_patches": {p.name: digest(p) for p in active},
            "source_files_sha256": inputs,
            "source_set_sha256": hashlib.sha256(json.dumps(inputs, sort_keys=True).encode()).hexdigest()}


def validate_records(records, fixture, full, checksum):
    """Reject partial/duplicate baseline jobs before applying acceptance gates."""
    expected_groups = {("f16", "Full"), ("f16", "Compact"), ("q8", "Compact"), ("q4", "Compact")}
    retrieval = fixture["retrieval"]
    expected_cases = {r["id"] for r in fixture["tasks"] if full or r["smoke"]}
    for size in retrieval["target_user_tokens" if full else "smoke_target_user_tokens"]:
        for position in retrieval["positions" if full else "smoke_positions"]:
            expected_cases.update(f"retrieve-{r['key']}-{size}-{position}" for r in retrieval["records"])
    task_sets = {}
    for record in records["tasks"]:
        task_sets.setdefault((record["profile"], record["swa"]), []).append(record["case"])
    if not expected_groups.issubset(task_sets) or any(len(cases) != len(expected_cases) or set(cases) != expected_cases for cases in task_sets.values()):
        return False
    if len(records["identity"]) != 1 or records["identity"][0]["model_sha256"] != checksum:
        return False
    configs = [(r["profile"], r["swa"]) for r in records["configurations"]]
    if len(configs) != len(task_sets) or set(configs) != set(task_sets):
        return False
    points = {}
    for record in records["text"]:
        key = (record["profile"], record["swa"], record["repeat"])
        points.setdefault(key, []).append((record["corpus"], record["tokens"], record["token_stream_fnv1a64"]))
    expected_runs = {(p, mode, 0) for p, mode in task_sets} | {("f16", "Full", 1)}
    if set(points) != expected_runs:
        return False
    reference = points[("f16", "Full", 0)]
    if {p[0] for p in reference} != {r["id"] for r in fixture["texts"]}:
        return False
    return all(len(row) == len(set(row)) and set(row) == set(reference) for row in points.values())


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    commands.add_parser("verify")
    run = commands.add_parser("run")
    run.add_argument("--model", required=True, type=Path)
    run.add_argument("--output", required=True, type=Path)
    run.add_argument("--full", action="store_true")
    run.add_argument("--require-vulkan", action="store_true")
    run.add_argument("--turboquant", action="store_true", help="compile opt-in reference feature; selected caches remain FP16/Q8/Q4")
    check = commands.add_parser("check")
    check.add_argument("artifact", type=Path)
    check.add_argument("--candidate", required=True)
    check.add_argument("--swa", default="Compact", choices=("Full", "Compact"))
    args = parser.parse_args()
    manifest = verify()
    fixture = json.loads((FIXTURES / "cache-quality-v1.json").read_text())
    if args.command == "verify":
        print("Fixed quality fixture SHA256 verified")
        return
    if args.command == "check":
        artifact = json.loads(args.artifact.read_text())
        if (artifact["status"] != "complete" or artifact["fixture_manifest"] != manifest
                or not validate_records(artifact["records"], fixture, artifact["full"], artifact["model_sha256"])):
            raise ValueError("candidate gates require a complete artifact with matching fixtures")
        verdict = check_candidate(artifact["records"], args.candidate, args.swa, fixture["acceptance"])
        print(json.dumps(verdict, indent=2))
        raise SystemExit(0 if verdict["passed"] else 1)
    model = args.model.resolve(strict=True)
    checksum = digest(model)
    env = dict(os.environ, ASSISTANT_QUALITY_MODEL=str(model), ASSISTANT_QUALITY_MODEL_SHA256=checksum, ASSISTANT_QUALITY_REQUIRE_MODEL="1")
    for flag in ("ASSISTANT_QUALITY_FULL", "ASSISTANT_QUALITY_REQUIRE_VULKAN"):
        env.pop(flag, None)
    if args.full:
        env["ASSISTANT_QUALITY_FULL"] = "1"
    if args.require_vulkan:
        env["ASSISTANT_QUALITY_REQUIRE_VULKAN"] = "1"
    args.output.parent.mkdir(parents=True, exist_ok=True)
    identity = source_identity(args.turboquant)
    logs = []
    status = 0
    features = ["--features", "turboquant"] if args.turboquant else []
    for label, selection in [("tasks", ["--test", "cache_quality", "required_cache_task_quality_characterization"]),
                             ("text", ["--lib", "required_representative_text_quality"])]:
        log = args.output.with_name(args.output.stem + "-" + label + ".log")
        with log.open("w") as stream:
            result = subprocess.run(["cargo", "test", "-p", "assistant", *features, *selection,
                                     "--locked", "--offline", "--", "--nocapture", "--test-threads=1"],
                                    cwd=ROOT, env=env, stdout=stream, stderr=subprocess.STDOUT)
        logs.append(log)
        print(f"{label} evaluation exit: {result.returncode}", flush=True)
        status = status or result.returncode
        if result.returncode:
            break
    records = {"tasks": [], "text": [], "identity": [], "configurations": []}
    for log in logs:
        parsed = parse(log)
        for key in records:
            records[key].extend(parsed[key])
    source_inputs_unchanged = source_identity(args.turboquant)["source_set_sha256"] == identity["source_set_sha256"]
    complete = status == 0 and source_inputs_unchanged and validate_records(records, fixture, args.full, checksum)
    artifact = {"schema": 1, "status": "complete" if complete else "incomplete",
                "model_sha256": checksum, "fixture_manifest": manifest, "full": args.full,
                "source_identity": identity, "source_inputs_unchanged": source_inputs_unchanged,
                "turboquant_reference_feature": args.turboquant,
                "acceptance": fixture["acceptance"], "records": records, "summary": summarize(records),
                "test_binaries": [{"file": name, "sha256": digest(ROOT / name)}
                    for name in sorted({match.group(1) for log in logs
                        for match in re.finditer(r"^\s+Running .+ \((target/[^)]+)\)$", log.read_text(), re.MULTILINE)})],
                "logs": [{"file": log.name, "sha256": digest(log)} for log in logs]}
    args.output.write_text(json.dumps(artifact, indent=2) + "\n")
    if not complete:
        raise SystemExit(status or 1)
    print(f"Saved scored baseline: {args.output}")


if __name__ == "__main__":
    main()
