"""Run the actual offline matcher on a deterministic synthetic stress corpus.

Requires Node, the built litecr-eval binary, and Python jsonschema. Family
reports retain the full held-out candidate population; only queries/labels
are sliced. No production configuration or database is used.
"""
import datetime
import hashlib
import json
import pathlib
import subprocess
import sys
import time

import jsonschema

ROOT = pathlib.Path(__file__).resolve().parents[2]
FAMILIES = ["common-name", "shared-household", "twin", "single-character-error",
            "partial-date", "cross-script", "identifier-collision"]


def validate(data, schema):
    jsonschema.Draft202012Validator(schema).validate(data)
    records = {r["record_id"]: r for r in data["records"]}
    assert len(records) == len(data["records"])
    assert len({q["query_id"] for q in data["queries"]}) == len(data["queries"])
    assert len({p["pair_id"] for p in data["pair_labels"]}) == len(data["pair_labels"])
    assert len({c["gold_cluster_id"] for c in data["clusters"]}) == len(data["clusters"])
    assert sorted(r for c in data["clusters"] for r in c["member_record_ids"]) == sorted(records)
    for cluster in data["clusters"]:
        members = [records[r] for r in cluster["member_record_ids"]]
        assert all(r["gold_cluster_id"] == cluster["gold_cluster_id"] and
                   r["split"] == cluster["split"] for r in members)
    for record in records.values():
        date = record["date_of_birth"]
        if date and date["precision"] == "day":
            datetime.date.fromisoformat(date["original_value"])
    for query in data["queries"]:
        anchor = records[query["query_record_id"]]
        expected = {r["record_id"] for r in records.values()
                    if r["gold_cluster_id"] == anchor["gold_cluster_id"] and
                    r["record_id"] != anchor["record_id"]}
        assert set(query["eligible_match_record_ids"]) == expected
        for rid in query["hard_negative_record_ids"]:
            assert records[rid]["split"] == anchor["split"]
            assert records[rid]["gold_cluster_id"] != anchor["gold_cluster_id"]
        other = records[query["hard_negative_record_ids"][0]]
        family = query["case_tags"][0]
        if family == "twin":
            assert anchor["name_assertions"] == other["name_assertions"]
            assert anchor["date_of_birth"] == other["date_of_birth"]
        if family == "common-name":
            assert anchor["name_assertions"] == other["name_assertions"]
        if family in ("twin", "shared-household"):
            assert anchor["contacts"] == other["contacts"]
            assert anchor["address_token"] == other["address_token"]
        if family == "identifier-collision":
            assert anchor["identifiers"] == other["identifiers"]
        if family == "cross-script":
            positive = records[query["eligible_match_record_ids"][0]]
            assert anchor["name_assertions"][0]["script"] != positive["name_assertions"][0]["script"]
    for pair in data["pair_labels"]:
        left, right = records[pair["left_record_id"]], records[pair["right_record_id"]]
        assert pair["split"] == left["split"] == right["split"]
        assert (left["gold_cluster_id"] == right["gold_cluster_id"]) == (pair["label"] == "match")


def main():
    out = pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else ROOT / "scratch/synthetic-evaluation-20261014").resolve()
    binary = pathlib.Path(sys.argv[2] if len(sys.argv) > 2 else ROOT / "target/debug/litecr-eval").resolve()
    out.mkdir(parents=True, exist_ok=True)
    schema = json.loads((ROOT / "fixtures/matching/schema/dataset.schema.json").read_text())
    summaries = {}
    overall_data = None
    for family in ["overall", *FAMILIES]:
        dataset_path = out / f"{family}.dataset.json"
        report_path = out / f"{family}.report.json"
        command = ["node", str(ROOT / "tools/evaluation/generate_synthetic_cohort.mjs"), f"--out={dataset_path}"]
        if family != "overall":
            command.append(f"--only-family={family}")
        subprocess.run(command, check=True, capture_output=True, text=True)
        data = json.loads(dataset_path.read_text())
        validate(data, schema)
        if family == "overall":
            overall_data = data
        else:
            assert data["records"] == overall_data["records"]
            assert data["clusters"] == overall_data["clusters"]
            assert data["queries"] == [q for q in overall_data["queries"] if family in q["case_tags"]]
        # Repeated generation must be byte-identical before evaluation begins.
        original = dataset_path.read_bytes()
        subprocess.run(command, check=True, capture_output=True, text=True)
        assert dataset_path.read_bytes() == original
        started = time.perf_counter()
        subprocess.run([str(binary), str(dataset_path), "held_out_test", str(report_path)], check=True)
        elapsed = time.perf_counter() - started
        report = json.loads(report_path.read_text())
        summaries[family] = {
            "dataset_sha256": hashlib.sha256(original).hexdigest(),
            "matcher_versions": report["matcher_versions"],
            "queries": report["queries"],
            "positive_candidates": report["positive_candidates"],
            "positive_candidates_retrieved": report["positive_candidates_retrieved"],
            "candidate_recall": report["candidate_recall"],
            "hard_negatives": report["hard_negatives"],
            "hard_negatives_retrieved": report["hard_negatives_retrieved"],
            "returned_candidate_count": report["returned_candidate_count"],
            "queries_truncated": report["queries_truncated"],
            "candidate_count_distribution": report["candidate_count_distribution"],
            "pair_labels": {label: {key: value for key, value in summary.items()
                                    if key != "raw_point_values"}
                            for label, summary in report["pair_labels"].items()},
            "subgroup_metrics": report["subgroup_metrics"],
            "approximate_union": {key: value for key, value in
                                  report["candidate_generation_experiment"]["approximate_union"].items()
                                  if not key.startswith("by_") and key != "missing_attribution"},
            "report_sha256": hashlib.sha256(report_path.read_bytes()).hexdigest(),
            "whole_evaluator_elapsed_seconds": elapsed,
        }
        print(f'{family}: {report["positive_candidates_retrieved"]}/{report["positive_candidates"]} positive candidates; {report["hard_negatives_retrieved"]}/{report["hard_negatives"]} designated hard negatives retrieved', flush=True)
    for key in ("queries", "positive_candidates", "positive_candidates_retrieved",
                "hard_negatives", "hard_negatives_retrieved", "returned_candidate_count"):
        assert sum(summaries[family][key] for family in FAMILIES) == summaries["overall"][key]
    result = {"synthetic_only": True, "seed": 20261014, "population_records": 588,
              "held_out_population_records": 196, "generator_schema_validation": "passed",
              "deterministic_regeneration": "passed", "family_slices_preserve_candidate_population": True,
              "family_totals_reconcile": "passed",
              "source_revision": subprocess.check_output(["git", "rev-parse", "HEAD"], cwd=ROOT, text=True).strip(),
              "source_dirty": bool(subprocess.check_output(["git", "status", "--porcelain"], cwd=ROOT, text=True).strip()),
              "generator_sha256": hashlib.sha256((ROOT / "tools/evaluation/generate_synthetic_cohort.mjs").read_bytes()).hexdigest(),
              "runner_sha256": hashlib.sha256(pathlib.Path(__file__).read_bytes()).hexdigest(),
              "evaluator_binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
              "results": summaries}
    (out / "summary.json").write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n")


if __name__ == "__main__":
    main()
