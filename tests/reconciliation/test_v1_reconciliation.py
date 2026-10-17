"""Independent read-only reconciliations for synthetic litecr-migration/v1 artifacts.

These tests do not share validation/index helpers with the migration CLI and never
connect to PostgreSQL or modify Registry state.
"""

import hashlib
import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
TOOL = ROOT / "tools" / "migration" / "litecr_migrate.py"
FIXTURES = Path(__file__).resolve().parent / "fixtures"


def digest(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def canonical_digest(value: object) -> str:
    encoded = json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
    return digest(encoded)


class IndependentV1Reconciliation(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="litecr-p5c-reconciliation-")
        self.work = Path(self.temp.name)
        self.manifest_path = self.work / "manifest.json"
        self.stream_path = self.work / "records.ndjson"
        shutil.copyfile(FIXTURES / "v1-manifest.json", self.manifest_path)
        shutil.copyfile(FIXTURES / "v1-records.ndjson", self.stream_path)

    def tearDown(self):
        self.temp.cleanup()

    def cli(self, *args):
        return subprocess.run(
            [sys.executable, str(TOOL), *map(str, args)],
            cwd=ROOT,
            capture_output=True,
            text=True,
            check=False,
        )

    @staticmethod
    def read_ndjson(data: bytes) -> list[dict]:
        # This is intentionally a simple independent reconciliation reader, not
        # an import/validation helper from the migration package implementation.
        return [json.loads(line.decode("utf-8")) for line in data.splitlines()]

    def test_manifest_counts_batches_exceptions_and_preserved_source_facts(self):
        manifest_bytes = self.manifest_path.read_bytes()
        stream_bytes = self.stream_path.read_bytes()
        manifest = json.loads(manifest_bytes)
        input_rows = self.read_ndjson(stream_bytes)

        # Independently reconcile declared byte/hash/count values before invoking
        # the offline validator/stager.
        self.assertEqual(manifest["format"], "litecr-migration/v1")
        self.assertEqual(manifest["record_count"], len(input_rows))
        self.assertEqual(manifest["stream_bytes"], len(stream_bytes))
        self.assertEqual(manifest["stream_sha256"], digest(stream_bytes))

        keys = [(row["source_system_id"], row["source_record_key"], row["source_version"]) for row in input_rows]
        self.assertEqual(len(keys), 6)
        self.assertEqual(len(set(keys)), 5)  # One exact source-key/version replay.
        self.assertEqual(len({(system, key) for system, key, _ in keys}), 5)
        expected_unique = {(key[0], key[1], key[2]): row for key, row in zip(keys, input_rows)}
        self.assertEqual(len(expected_unique), 5)
        self.assertEqual(input_rows[0], input_rows[4])
        input_relationships = [relationship for row in input_rows for relationship in row["relationships"]]
        self.assertEqual([item["kind"] for item in input_relationships].count("link"), 1)
        self.assertEqual([item["kind"] for item in input_relationships].count("non_match"), 0)
        self.assertEqual(sum(len(row["identifiers"]) for row in input_rows), 4)
        self.assertEqual(sum(len(row.get("aliases", [])) for row in input_rows), 5)
        alias_occurrences = [alias for row in input_rows for alias in row.get("aliases", [])]
        self.assertEqual(alias_occurrences.count("legacy-syn-001"), 2)  # Replay repeats the same source facts.
        self.assertEqual(set(alias_occurrences), {"legacy-syn-001", "clinic-a:0007", "legacy-syn-003"})

        stage_dir = self.work / "stage"
        result = self.cli(
            "stage", "--manifest", self.manifest_path, "--stream", self.stream_path,
            "--output", stage_dir, "--batch-size", "2",
        )
        self.assertEqual(result.returncode, 2, result.stderr)  # Quarantine keeps the package conditional.
        cli_summary = json.loads(result.stdout)
        checkpoint = json.loads((stage_dir / "checkpoint.json").read_text(encoding="utf-8"))
        quarantine = json.loads((stage_dir / "quarantine.json").read_text(encoding="utf-8"))
        replays = json.loads((stage_dir / "replays.json").read_text(encoding="utf-8"))

        self.assertEqual(cli_summary["manifest_sha256"], digest(manifest_bytes))
        self.assertEqual(checkpoint["stream_sha256"], digest(stream_bytes))
        self.assertEqual(checkpoint["record_count"], 6)
        self.assertEqual(checkpoint["valid_records"], 4)
        self.assertEqual(checkpoint["exact_replay_duplicates"], 1)
        self.assertEqual(checkpoint["quarantined_records"], 1)
        self.assertEqual(checkpoint["committed_batch_sequence"], 0)
        self.assertEqual(checkpoint["staged_records"], 4)
        self.assertEqual(checkpoint["staged_batch_sequence"], 2)

        batch_paths = sorted((stage_dir / "batches").glob("*.ndjson"))
        sidecars = sorted((stage_dir / "batches").glob("*.json"))
        self.assertEqual(len(batch_paths), 2)
        self.assertEqual(len(sidecars), 2)
        staged_raw = b"".join(path.read_bytes() for path in batch_paths)
        staged_rows = self.read_ndjson(staged_raw)
        self.assertEqual(staged_rows, input_rows[:4])
        for sequence, (batch_path, sidecar_path) in enumerate(zip(batch_paths, sidecars), start=1):
            metadata = json.loads(sidecar_path.read_text(encoding="utf-8"))
            batch_bytes = batch_path.read_bytes()
            self.assertEqual(metadata["sequence"], sequence)
            self.assertEqual(metadata["manifest_sha256"], digest(manifest_bytes))
            self.assertEqual(metadata["stream_sha256"], digest(stream_bytes))
            self.assertEqual(metadata["batch_sha256"], digest(batch_bytes))
            self.assertEqual(metadata["record_count"], 2)

        # Reconcile original fields and explicit historical facts without
        # collapsing source records by matching demographics.
        by_key = {row["source_record_key"]: row for row in staged_rows}
        self.assertEqual(set(by_key), {"src-001", "src-002", "src-003", "src-004"})
        self.assertEqual(sum(len(row["identifiers"]) for row in staged_rows), 3)
        self.assertEqual(sum(len(row.get("aliases", [])) for row in staged_rows), 3)
        self.assertEqual(by_key["src-001"]["public_id"], "CRUID-SYN-001")
        self.assertEqual(by_key["src-001"]["cruid"], "CRUID-SYN-001")
        self.assertEqual(by_key["src-001"]["source_version"], "2026-01")
        self.assertEqual(by_key["src-001"]["aliases"], ["legacy-syn-001", "clinic-a:0007"])
        self.assertEqual(by_key["src-001"]["identifiers"][0]["authority"], "urn:synthetic:clinic-a")
        self.assertEqual(by_key["src-001"]["identifiers"][0]["value"], "0007")
        month_date = next(row for row in by_key["src-001"]["assertions"] if row["field_path"] == "birthDate")
        year_date = next(row for row in by_key["src-002"]["assertions"] if row["field_path"] == "birthDate")
        self.assertEqual((month_date["original_value"], month_date["partial_date_precision"]), ("1984-06", "month"))
        self.assertEqual((year_date["original_value"], year_date["partial_date_precision"]), ("1984", "year"))
        staged_relationships = [relationship for row in staged_rows for relationship in row["relationships"]]
        self.assertEqual(staged_relationships, [])
        self.assertEqual(by_key["src-003"]["assertions"], by_key["src-004"]["assertions"])
        self.assertNotEqual(by_key["src-003"]["source_record_key"], by_key["src-004"]["source_record_key"])

        self.assertEqual(quarantine["manifest_sha256"], digest(manifest_bytes))
        self.assertEqual(len(quarantine["exceptions"]), 1)
        exception = quarantine["exceptions"][0]
        self.assertEqual(exception["line"], 6)
        self.assertEqual(exception["code"], "HISTORICAL_RELATIONSHIPS_UNSUPPORTED")
        # Quarantine evidence binds the exact physical source line (including
        # its line ending) so a later reviewed derivative can prove the row it
        # corrected. This is intentionally distinct from the canonical JSON
        # digest used for source-version identity conflict checks.
        physical_lines = stream_bytes.splitlines(keepends=True)
        self.assertEqual(exception["payload_sha256"], digest(physical_lines[5]))
        self.assertEqual(set(exception), {"line", "code", "payload_sha256"})
        self.assertNotIn("source_record_key", exception)  # Quarantine does not expose row content.

        # Independently reconcile the exact replay receipt to its physical input
        # line, first occurrence and manifest. The report must not copy row data.
        self.assertEqual(replays["format"], "litecr-migration/v1")
        self.assertEqual(replays["manifest_sha256"], digest(manifest_bytes))
        self.assertEqual(len(replays["replays"]), 1)
        replay = replays["replays"][0]
        self.assertEqual(replay["line"], 5)
        self.assertEqual(replay["duplicate_of_line"], 1)
        self.assertEqual(replay["code"], "EXACT_REPLAY_DUPLICATE")
        self.assertEqual(replay["payload_sha256"], digest(physical_lines[4]))
        self.assertEqual(set(replay), {"line", "duplicate_of_line", "code", "payload_sha256"})

        # A retry against the same output must bind to the same package and
        # preserve the exact batch/replay artifacts; it must not append rows.
        before_retry = {
            path.relative_to(stage_dir).as_posix(): digest(path.read_bytes())
            for path in sorted(stage_dir.rglob("*"))
            if path.is_file() and path.name != "index.sqlite3"
        }
        retry = self.cli(
            "stage", "--manifest", self.manifest_path, "--stream", self.stream_path,
            "--output", stage_dir, "--batch-size", "2",
        )
        self.assertEqual(retry.returncode, 2, retry.stderr)
        after_retry = {
            path.relative_to(stage_dir).as_posix(): digest(path.read_bytes())
            for path in sorted(stage_dir.rglob("*"))
            if path.is_file() and path.name != "index.sqlite3"
        }
        self.assertEqual(after_retry, before_retry)

    def test_explicit_link_and_non_match_facts_are_refused_not_silently_dropped(self):
        rows = self.read_ndjson(self.stream_path.read_bytes())[:4]
        rows[0] = {**rows[0], "relationships": [{
            "kind": "link", "target_source_system_id": rows[1]["source_system_id"],
            "target_source_record_key": rows[1]["source_record_key"],
        }]}
        rows[2] = {**rows[2], "relationships": [{
            "kind": "non_match", "target_source_system_id": rows[3]["source_system_id"],
            "target_source_record_key": rows[3]["source_record_key"],
        }]}
        stream = b"".join(
            (json.dumps(row, ensure_ascii=False, separators=(",", ":")) + "\n").encode("utf-8")
            for row in rows
        )
        manifest = {
            "format": "litecr-migration/v1", "snapshot_id": "synthetic-relationship-snapshot",
            "source_system_id": "synthetic-opencr-export", "target_registry_id": "synthetic-registry",
            "target_organization_id": "synthetic-organization", "exported_at": "2026-10-08T00:00:00Z",
            "record_count": len(rows), "stream_bytes": len(stream), "stream_sha256": digest(stream),
        }
        self.stream_path.write_bytes(stream)
        self.manifest_path.write_text(json.dumps(manifest, ensure_ascii=False, separators=(",", ":")), encoding="utf-8")
        output = self.work / "relationship-stage"
        result = self.cli("stage", "--manifest", self.manifest_path, "--stream", self.stream_path, "--output", output)

        self.assertEqual(result.returncode, 2, result.stderr)
        checkpoint = json.loads((output / "checkpoint.json").read_text(encoding="utf-8"))
        quarantine = json.loads((output / "quarantine.json").read_text(encoding="utf-8"))
        self.assertEqual(checkpoint["valid_records"], 2)
        self.assertEqual(checkpoint["quarantined_records"], 2)
        self.assertEqual([item["line"] for item in quarantine["exceptions"]], [1, 3])
        self.assertTrue(all(item["code"] == "HISTORICAL_RELATIONSHIPS_UNSUPPORTED" for item in quarantine["exceptions"]))
        staged_rows = self.read_ndjson(b"".join(path.read_bytes() for path in sorted((output / "batches").glob("*.ndjson"))))
        self.assertEqual([row["source_record_key"] for row in staged_rows], ["src-002", "src-004"])

    def test_changed_same_version_is_quarantined_instead_of_counted_as_a_replay(self):
        original_rows = self.read_ndjson(self.stream_path.read_bytes())
        first = original_rows[0]
        changed = {**first, "public_id": "CRUID-SYN-001-CHANGED"}
        companion = original_rows[1]
        rows = [first, changed, first, companion]
        stream = b"".join(
            (json.dumps(row, ensure_ascii=False, separators=(",", ":")) + "\n").encode("utf-8")
            for row in rows
        )
        manifest = {
            "format": "litecr-migration/v1", "snapshot_id": "synthetic-conflict-snapshot",
            "source_system_id": "synthetic-opencr-export", "target_registry_id": "synthetic-registry",
            "target_organization_id": "synthetic-organization", "exported_at": "2026-10-08T00:00:00Z",
            "record_count": len(rows), "stream_bytes": len(stream), "stream_sha256": digest(stream),
        }
        self.stream_path.write_bytes(stream)
        self.manifest_path.write_text(json.dumps(manifest, ensure_ascii=False, separators=(",", ":")), encoding="utf-8")

        output = self.work / "conflict-stage"
        result = self.cli("stage", "--manifest", self.manifest_path, "--stream", self.stream_path, "--output", output)
        self.assertEqual(result.returncode, 2, result.stderr)
        checkpoint = json.loads((output / "checkpoint.json").read_text(encoding="utf-8"))
        quarantine = json.loads((output / "quarantine.json").read_text(encoding="utf-8"))
        self.assertEqual(checkpoint["valid_records"], 1)
        self.assertEqual(checkpoint["exact_replay_duplicates"], 0)
        self.assertEqual(checkpoint["quarantined_records"], 3)
        self.assertEqual([entry["line"] for entry in quarantine["exceptions"]], [1, 2, 3])
        self.assertTrue(
            all(entry["code"] == "SAME_VERSION_CHANGED_PAYLOAD" for entry in quarantine["exceptions"]),
            repr(quarantine["exceptions"]),
        )
        self.assertEqual(checkpoint["staged_records"], 1)

    def test_reverse_precheck_accepts_only_matching_synthetic_version_and_payload(self):
        row = self.read_ndjson(self.stream_path.read_bytes())[0]
        current_row = {**row, "current_receive_version": 9}
        current_path = self.work / "current-synthetic.ndjson"
        journal_path = self.work / "journal-synthetic.ndjson"
        current_bytes = (json.dumps(current_row, ensure_ascii=False, separators=(",", ":")) + "\n").encode("utf-8")
        expected = {"source_system_id": row["source_system_id"], "source_record_key": row["source_record_key"],
                    "expected_current_payload_sha256": canonical_digest(row), "expected_receive_version": 9}
        current_path.write_bytes(current_bytes)
        journal_path.write_text(json.dumps(expected, separators=(",", ":")) + "\n", encoding="utf-8")

        unchanged = self.cli("reverse-check", "--journal", journal_path, "--current", current_path)
        self.assertEqual(unchanged.returncode, 0, unchanged.stderr)
        self.assertEqual(json.loads(unchanged.stdout), {"records_checked": 1, "records_refused": 0, "reverse_representable": True})

        edited_payload = {**current_row, "public_id": "CRUID-SYN-001-EDITED"}
        current_path.write_text(json.dumps(edited_payload, ensure_ascii=False, separators=(",", ":")) + "\n", encoding="utf-8")
        changed_payload = self.cli("reverse-check", "--journal", journal_path, "--current", current_path)
        self.assertEqual(changed_payload.returncode, 3)
        self.assertEqual(json.loads(changed_payload.stdout), {"records_checked": 1, "records_refused": 1, "reverse_representable": False})

        edited = {**current_row, "current_receive_version": 10}
        current_path.write_text(json.dumps(edited, ensure_ascii=False, separators=(",", ":")) + "\n", encoding="utf-8")
        changed = self.cli("reverse-check", "--journal", journal_path, "--current", current_path)
        self.assertEqual(changed.returncode, 3)
        self.assertEqual(json.loads(changed.stdout), {"records_checked": 1, "records_refused": 1, "reverse_representable": False})


if __name__ == "__main__":
    unittest.main()
