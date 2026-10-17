import hashlib
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

TOOL = Path(__file__).resolve().parents[2] / "tools" / "migration" / "litecr_migrate.py"


class MigrationPackageTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="litecr-migration-test-")
        self.root = Path(self.temp.name)
        self.stream = self.root / "records.ndjson"
        self.manifest = self.root / "manifest.json"

    def tearDown(self):
        self.temp.cleanup()

    @staticmethod
    def row(key="src-1", version="1", **extra):
        return {
            "source_system_id": "synthetic-his",
            "source_record_key": key,
            "source_version": version,
            "public_id": "CRUID-synthetic-1",
            "cruid": "CRUID-synthetic-1",
            "assertions": [
                {"field_path": "name.original", "original_value": "ຄຳ ຕົວຢ່າງ", "language_tag": "lo", "script": "Lao"},
                {"field_path": "birthDate", "original_value": "1984-06", "partial_date_precision": "month"},
            ],
            "identifiers": [{"authority": "urn:synthetic:clinic", "type": "MRN", "value": "0007", "authoritative": True}],
            "relationships": [],
            **extra,
        }

    def package(self, rows):
        stream_bytes = b"".join((json.dumps(row, ensure_ascii=False, separators=(",", ":")) + "\n").encode() for row in rows)
        self.stream.write_bytes(stream_bytes)
        manifest = {
            "format": "litecr-migration/v1",
            "snapshot_id": "synthetic-snapshot-20261008",
            "source_system_id": "synthetic-his",
            "target_registry_id": "synthetic-registry",
            "target_organization_id": "synthetic-organization",
            "exported_at": "2026-10-08T00:00:00Z",
            "record_count": len(rows),
            "stream_bytes": len(stream_bytes),
            "stream_sha256": hashlib.sha256(stream_bytes).hexdigest(),
        }
        self.manifest.write_text(json.dumps(manifest, ensure_ascii=False, separators=(",", ":")), encoding="utf-8")
        return manifest

    def run_tool(self, *args):
        return subprocess.run([sys.executable, str(TOOL), *map(str, args)], capture_output=True, text=True)

    def test_stage_preserves_exact_rows_and_is_bound_to_raw_manifest_digest(self):
        rows = [self.row("a"), self.row("b")]
        manifest = self.package(rows)
        out = self.root / "stage"
        result = self.run_tool("stage", "--manifest", self.manifest, "--stream", self.stream, "--output", out, "--batch-size", 1)
        self.assertEqual(result.returncode, 0, result.stderr)
        checkpoint = json.loads((out / "checkpoint.json").read_text())
        self.assertEqual(checkpoint["manifest_sha256"], hashlib.sha256(self.manifest.read_bytes()).hexdigest())
        self.assertEqual(checkpoint["stream_sha256"], manifest["stream_sha256"])
        self.assertEqual(checkpoint["staged_batch_sequence"], 2)
        staged = b"".join(path.read_bytes() for path in sorted((out / "batches").glob("*.ndjson")))
        self.assertEqual(staged, self.stream.read_bytes())
        # Resume validates the journaled batch and repairs an interrupted/corrupt artifact.
        first_batch = out / "batches" / "00000001.ndjson"
        expected_first_batch = first_batch.read_bytes()
        first_batch.write_bytes(b"partial")
        retry_same = self.run_tool("stage", "--manifest", self.manifest, "--stream", self.stream, "--output", out, "--batch-size", 1)
        self.assertEqual(retry_same.returncode, 0, retry_same.stderr)
        self.assertEqual(first_batch.read_bytes(), expected_first_batch)
        # Whitespace changes preserve JSON meaning but invalidate an exact-byte checkpoint.
        changed = self.manifest.read_text().replace('"format":"litecr-migration/v1"', '"format": "litecr-migration/v1"')
        self.manifest.write_text(changed)
        retry = self.run_tool("stage", "--manifest", self.manifest, "--stream", self.stream, "--output", out)
        self.assertEqual(retry.returncode, 1)
        self.assertIn("CHECKPOINT_PACKAGE_MISMATCH", retry.stderr)

    def test_same_source_version_with_changed_payload_quarantines_both_rows(self):
        self.package([self.row("same"), self.row("same", extra={"note": "changed"}), self.row("same")])
        result = self.run_tool("validate", "--manifest", self.manifest, "--stream", self.stream, "--work-dir", self.root / "index")
        self.assertEqual(result.returncode, 2, result.stderr)
        summary = json.loads(result.stdout)
        self.assertEqual(summary["records_valid"], 0)
        self.assertEqual(summary["records_quarantined"], 3)

    def test_exact_replay_is_idempotent_and_dangling_relationship_is_quarantined(self):
        left = self.row("left")
        replay = self.row("left")
        right = self.row("right", relationships=[{"kind": "link", "target_source_system_id": "synthetic-his", "target_source_record_key": "missing"}])
        self.package([left, replay, right])
        result = self.run_tool("stage", "--manifest", self.manifest, "--stream", self.stream, "--output", self.root / "stage")
        self.assertEqual(result.returncode, 2)
        checkpoint = json.loads((self.root / "stage" / "checkpoint.json").read_text())
        self.assertEqual(checkpoint["valid_records"], 1)
        self.assertEqual(checkpoint["exact_replay_duplicates"], 1)
        self.assertEqual(checkpoint["quarantined_records"], 1)
        self.assertEqual(checkpoint["staged_batch_sequence"], 1)

    def test_stream_checksum_fails_before_any_staging(self):
        self.package([self.row()])
        self.stream.write_bytes(self.stream.read_bytes() + b" ")
        output = self.root / "stage"
        result = self.run_tool("stage", "--manifest", self.manifest, "--stream", self.stream, "--output", output)
        self.assertEqual(result.returncode, 1)
        self.assertIn("STREAM_BYTE_COUNT_MISMATCH", result.stderr)
        self.assertFalse((output / "batches").exists())

    def test_stage_refuses_unjournaled_batch_artifacts_on_resume(self):
        self.package([self.row("expected")])
        output = self.root / "stage"
        staged = self.run_tool("stage", "--manifest", self.manifest, "--stream", self.stream, "--output", output)
        self.assertEqual(staged.returncode, 0, staged.stderr)

        batches = output / "batches"
        stray = batches / "00000002.ndjson"
        stray.write_text('{"source_record_key":"stale"}\n', encoding="utf-8")
        resumed = self.run_tool("stage", "--manifest", self.manifest, "--stream", self.stream, "--output", output)
        self.assertEqual(resumed.returncode, 1)
        self.assertIn("UNEXPECTED_BATCH_ARTIFACT", resumed.stderr)
        self.assertTrue(stray.exists())  # Fail closed; do not delete operator-owned artifacts.
        checkpoint = json.loads((output / "checkpoint.json").read_text(encoding="utf-8"))
        self.assertEqual(checkpoint["staged_batch_sequence"], 1)

    def test_canonical_payload_digest_matches_frozen_utf8_fixture(self):
        self.assertEqual(
            __import__("hashlib").sha256(
                json.dumps(self.row("src-1"), sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
            ).hexdigest(),
            "28a6cc7d50dd54158bb5f19305496be37647672008bbeba13cd746c5d2ceb9b1",
        )

    def test_non_jcs_numbers_are_quarantined_instead_of_hashed_differently(self):
        self.package([self.row("fraction", extra={"unmapped_numeric": 1.25})])
        result = self.run_tool("validate", "--manifest", self.manifest, "--stream", self.stream, "--work-dir", self.root / "index")
        self.assertEqual(result.returncode, 2)
        self.assertEqual(json.loads(result.stdout)["records_quarantined"], 1)

    def test_reverse_check_refuses_later_payload_or_version_change(self):
        current = self.root / "current.ndjson"
        journal = self.root / "reverse.ndjson"
        row = self.row("reverse-1")
        current.write_bytes((json.dumps({**row, "current_receive_version": 3}, ensure_ascii=False) + "\n").encode())
        journal.write_bytes((json.dumps({"source_system_id": "synthetic-his", "source_record_key": "reverse-1", "expected_current_payload_sha256": "0" * 64, "expected_receive_version": 3}) + "\n").encode())
        result = self.run_tool("reverse-check", "--journal", journal, "--current", current)
        self.assertEqual(result.returncode, 3)
        self.assertIn('"reverse_representable":false', result.stdout)

    def test_reverse_check_rejects_duplicate_current_source_keys(self):
        current = self.root / "current.ndjson"
        journal = self.root / "reverse.ndjson"
        row = self.row("reverse-duplicate")
        current_row = {**row, "current_receive_version": 3}
        current_bytes = (json.dumps(current_row, ensure_ascii=False) + "\n").encode()
        current.write_bytes(current_bytes + current_bytes)
        journal.write_bytes((json.dumps({
            "source_system_id": "synthetic-his",
            "source_record_key": "reverse-duplicate",
            "expected_current_payload_sha256": hashlib.sha256(
                json.dumps(row, sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode("utf-8")
            ).hexdigest(),
            "expected_receive_version": 3,
        }) + "\n").encode())

        result = self.run_tool("reverse-check", "--journal", journal, "--current", current)

        self.assertEqual(result.returncode, 1)
        self.assertIn("CURRENT_SNAPSHOT_DUPLICATE_SOURCE_KEY", result.stderr)
        self.assertNotIn('"reverse_representable":true', result.stdout)

    def test_reverse_check_rejects_boolean_or_nonpositive_receive_versions(self):
        current = self.root / "current.ndjson"
        journal = self.root / "reverse.ndjson"
        for version in (True, 0, -1):
            with self.subTest(version=version):
                current.write_text(json.dumps({**self.row("reverse-version"), "current_receive_version": version}) + "\n", encoding="utf-8")
                journal.write_text("{}\n", encoding="utf-8")

                result = self.run_tool("reverse-check", "--journal", journal, "--current", current)

                self.assertEqual(result.returncode, 1)
                self.assertIn("CURRENT_SNAPSHOT_INVALID", result.stderr)
                self.assertNotIn('"reverse_representable":true', result.stdout)


if __name__ == "__main__":
    unittest.main()
