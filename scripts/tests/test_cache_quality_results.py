"""Regression tests for incomplete evidence and lossy-codec acceptance gates."""
import copy
import importlib.util
import json
import tempfile
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("quality", Path(__file__).parents[1] / "cache-quality-results.py")
QUALITY = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(QUALITY)
FIXTURE = json.loads((QUALITY.FIXTURES / "cache-quality-v1.json").read_text())


def complete_records():
    retrieval = FIXTURE["retrieval"]
    names = [r["id"] for r in FIXTURE["tasks"] if r["smoke"]]
    names += [f"retrieve-{r['key']}-1024-middle" for r in retrieval["records"]]
    records = {"tasks": [], "text": [], "identity": [{"model_sha256": "a" * 64}], "configurations": []}
    for profile, swa in [("f16", "Full"), ("f16", "Compact"), ("q8", "Compact"), ("q4", "Compact")]:
        records["configurations"].append({"profile": profile, "swa": swa})
        records["tasks"] += [{"profile": profile, "swa": swa, "case": name, "passed": True,
                              "kind": "retrieval" if name.startswith("retrieve-") else "tools"} for name in names]
        for repeat in range(2 if (profile, swa) == ("f16", "Full") else 1):
            records["text"] += [{"profile": profile, "swa": swa, "repeat": repeat, "corpus": text["id"],
                                 "tokens": 32, "token_stream_fnv1a64": "fixed", "metrics": {
                                     "target_nll_reference": 2.0, "target_nll_profile": 2.0,
                                     "kl_reference_to_profile": 0.0}} for text in FIXTURE["texts"]]
    return records


class QualityGates(unittest.TestCase):
    def test_complete_job_requires_all_tasks_text_repeats_and_configuration_evidence(self):
        records = complete_records()
        self.assertTrue(QUALITY.validate_records(records, FIXTURE, False, "a" * 64))
        for key in ("tasks", "text", "identity", "configurations"):
            incomplete = copy.deepcopy(records)
            incomplete[key].pop()
            self.assertFalse(QUALITY.validate_records(incomplete, FIXTURE, False, "a" * 64), key)
            duplicate = copy.deepcopy(records)
            duplicate[key].append(duplicate[key][0])
            self.assertFalse(QUALITY.validate_records(duplicate, FIXTURE, False, "a" * 64), key)
        self.assertFalse(QUALITY.validate_records(records, FIXTURE, True, "a" * 64))
        self.assertFalse(QUALITY.validate_records(records, FIXTURE, False, "a" * 64, include_pq3=True))
        self.assertFalse(QUALITY.validate_records(records, FIXTURE, False, "b" * 64))

    def test_losing_a_baseline_task_or_exceeding_loss_gate_fails_candidate(self):
        records = complete_records()
        evaluate = lambda r: QUALITY.check_candidate(r, "q8", "Compact", FIXTURE["acceptance"])
        self.assertTrue(evaluate(records)["passed"])
        for name in ("multiply", "retrieve-river-gate-1024-middle"):
            failed = copy.deepcopy(records)
            next(r for r in failed["tasks"] if r["profile"] == "q8" and r["case"] == name)["passed"] = False
            self.assertFalse(evaluate(failed)["passed"], name)
        failed = copy.deepcopy(records)
        for record in failed["text"]:
            if record["profile"] == "q8":
                record["metrics"]["target_nll_profile"] += 0.06
        self.assertFalse(evaluate(failed)["passed"])
        failed = copy.deepcopy(records)
        next(r for r in failed["text"] if r["profile"] == "q8")["metrics"]["kl_reference_to_profile"] = 0.06
        self.assertFalse(evaluate(failed)["passed"])

    def test_candidate_with_different_token_stream_is_rejected(self):
        records = complete_records()
        next(r for r in records["text"] if r["profile"] == "q8")["token_stream_fnv1a64"] = "different"
        with self.assertRaisesRegex(ValueError, "identical complete token checkpoints"):
            QUALITY.check_candidate(records, "q8", "Compact", FIXTURE["acceptance"])

    def test_parser_handles_libtest_prefix_without_parsing_model_text(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "job.log"
            record = {"profile": "q8_0", "answer": "CACHE_TASK_QUALITY {}"}
            log.write_text('test corpus ... CACHE_TEXT_QUALITY ' + json.dumps(record) + '\n'
                           'model said CACHE_TASK_QUALITY {"passed":true}\n')
            parsed = QUALITY.parse(log)
            self.assertEqual(parsed["text"][0]["profile"], "q8")
            self.assertEqual(parsed["tasks"], [])

    def test_checked_in_fixture_checksum_matches(self):
        self.assertEqual(QUALITY.verify()["files"][0]["file"], "cache-quality-v1.json")


if __name__ == "__main__":
    unittest.main()
