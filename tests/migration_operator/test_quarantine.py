import hashlib
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
STAGER = ROOT / "tools" / "migration" / "litecr_migrate.py"
TOOL = ROOT / "tools" / "migration" / "litecr_quarantine.py"


class QuarantineDispositionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix="litecr-quarantine-test-")
        self.root = Path(self.temp.name)
        self.manifest = self.root / "manifest.json"
        self.stream = self.root / "records.ndjson"
        self.stage = self.root / "stage"

    def tearDown(self):
        self.temp.cleanup()

    @staticmethod
    def row(key="src-a", value="ຄຳ ຕົວຢ່າງ"):
        return {
            "source_system_id": "synthetic-his",
            "source_record_key": key,
            "source_version": "1",
            "assertions": [{"field_path": "name.original", "original_value": value, "language_tag": "lo", "script": "Lao"}],
            "identifiers": [],
            "relationships": [],
        }

    def package(self, physical_lines):
        stream = b"".join(physical_lines)
        self.stream.write_bytes(stream)
        manifest = {
            "format": "litecr-migration/v1",
            "snapshot_id": "synthetic-snapshot",
            "source_system_id": "synthetic-his",
            "target_registry_id": "synthetic-registry",
            "target_organization_id": "synthetic-org",
            "exported_at": "2026-10-09T00:00:00Z",
            "record_count": len(physical_lines),
            "stream_bytes": len(stream),
            "stream_sha256": hashlib.sha256(stream).hexdigest(),
        }
        self.manifest.write_text(json.dumps(manifest, separators=(",", ":")), encoding="utf-8")

    def stage_package(self):
        result = subprocess.run([sys.executable, str(STAGER), "stage", "--manifest", str(self.manifest), "--stream", str(self.stream), "--output", str(self.stage)], capture_output=True, text=True)
        self.assertIn(result.returncode, (0, 2), result.stderr)

    def run_prepare(self, action, output, *extra, manifest=None, stream=None, stage=None):
        return subprocess.run([
            sys.executable, str(TOOL), "prepare", "--action", action,
            "--manifest", str(manifest or self.manifest), "--stream", str(stream or self.stream),
            "--stage-dir", str(stage or self.stage), "--output", str(output),
            "--reason", "synthetic reviewed change", "--idempotency-key", "test-disposition-1", *map(str, extra),
        ], capture_output=True, text=True)

    def test_corrected_resubmission_preserves_parent_and_binds_hashes(self):
        original_bad = b'{not-json\n'
        good = (json.dumps(self.row("good"), ensure_ascii=False, separators=(",", ":")) + "\n").encode()
        self.package([original_bad, good])
        self.stage_package()
        corrected = self.root / "corrections.json"
        row = self.row("corrected")
        corrected.write_text(json.dumps([{"line": 1, "row": row}], ensure_ascii=False), encoding="utf-8")
        original_bytes = self.stream.read_bytes()
        output = self.root / "derived"
        result = self.run_prepare("corrected_resubmission", output, "--corrections", corrected)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.stream.read_bytes(), original_bytes)
        derived_lines = (output / "records.ndjson").read_bytes().splitlines(keepends=True)
        self.assertEqual(derived_lines[1], good)
        request = json.loads((output / "disposition-request.json").read_text())
        self.assertEqual(request["action"], "corrected_resubmission")
        self.assertEqual(request["corrections"][0]["original_payload_sha256"], hashlib.sha256(original_bad).hexdigest())
        self.assertEqual(request["corrections"][0]["corrected_payload_sha256"], hashlib.sha256(derived_lines[0]).hexdigest())

    def test_missing_final_line_feed_can_be_repaired_without_changing_parent(self):
        unterminated = json.dumps(self.row("final"), ensure_ascii=False, separators=(",", ":")).encode()
        self.package([unterminated])
        self.stage_package()
        corrected = self.root / "corrections.json"
        corrected.write_text(json.dumps([{"line": 1, "row": self.row("final")}], ensure_ascii=False), encoding="utf-8")
        original = self.stream.read_bytes()
        output = self.root / "terminated"
        result = self.run_prepare("corrected_resubmission", output, "--corrections", corrected)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.stream.read_bytes(), original)
        self.assertTrue((output / "records.ndjson").read_bytes().endswith(b"\n"))

    def test_oversized_physical_row_is_one_repairable_quarantine(self):
        oversized = b"x" * (1024 * 1024 + 2) + b"\n"
        self.package([oversized])
        self.stage_package()
        report = json.loads((self.stage / "quarantine.json").read_text())
        self.assertEqual(report["exceptions"], [{
            "line": 1, "code": "ROW_TOO_LARGE", "payload_sha256": hashlib.sha256(oversized).hexdigest(),
        }])
        corrected = self.root / "corrections.json"
        corrected.write_text(json.dumps([{"line": 1, "row": self.row("fixed")}], ensure_ascii=False), encoding="utf-8")
        result = self.run_prepare("corrected_resubmission", self.root / "bounded", "--corrections", corrected)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(len((self.root / "bounded" / "records.ndjson").read_bytes()),
                         len((json.dumps(self.row("fixed"), ensure_ascii=False, sort_keys=True, separators=(",", ":")) + "\n").encode()))

    def test_mixed_mechanical_quarantine_and_exact_replay_are_resolved_together(self):
        bad = b"{invalid\n"
        good = (json.dumps(self.row("same"), ensure_ascii=False, separators=(",", ":")) + "\n").encode()
        self.package([bad, good, good])
        self.stage_package()
        corrections = self.root / "corrections.json"
        corrections.write_text(json.dumps([{"line": 1, "row": self.row("repaired")}], ensure_ascii=False), encoding="utf-8")
        corrected_dir = self.root / "corrected"
        first = self.run_prepare("corrected_resubmission", corrected_dir, "--corrections", corrections)
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertEqual(len((corrected_dir / "records.ndjson").read_bytes().splitlines()), 2)
        request = json.loads((corrected_dir / "disposition-request.json").read_text())
        self.assertEqual([item["quarantine_code"] for item in request["corrections"]], [
            "ROW_INVALID_UTF8_OR_JSON", "EXACT_REPLAY_DUPLICATE",
        ])
        derived_stage = self.root / "corrected-stage"
        staged = subprocess.run([sys.executable, str(STAGER), "stage", "--manifest", str(corrected_dir / "manifest.json"), "--stream", str(corrected_dir / "records.ndjson"), "--output", str(derived_stage)], capture_output=True, text=True)
        self.assertEqual(staged.returncode, 0, staged.stderr)
        checkpoint = json.loads((derived_stage / "checkpoint.json").read_text())
        self.assertEqual(checkpoint["quarantined_records"], 0)
        self.assertEqual(checkpoint["exact_replay_duplicates"], 0)

    def test_deferred_action_refuses_symlink_output_directory(self):
        bad = self.row("relationship")
        bad["relationships"] = [{"kind": "link", "target_source_system_id": "synthetic-his", "target_source_record_key": "missing"}]
        self.package([(json.dumps(bad, ensure_ascii=False, separators=(",", ":")) + "\n").encode()])
        self.stage_package()
        target = self.root / "outside"
        target.mkdir()
        output = self.root / "linked-output"
        output.symlink_to(target, target_is_directory=True)
        result = self.run_prepare("deferred", output)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((target / "disposition-request.json").exists())

    def test_duplicate_acknowledgement_removes_only_exact_physical_replay(self):
        first = (json.dumps(self.row("same"), ensure_ascii=False, separators=(",", ":")) + "\n").encode()
        middle = (json.dumps(self.row("other"), ensure_ascii=False, separators=(",", ":")) + "\n").encode()
        self.package([first, middle, first])
        self.stage_package()
        original = self.stream.read_bytes()
        output = self.root / "deduplicated"
        result = self.run_prepare("duplicate_acknowledged", output)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(self.stream.read_bytes(), original)
        self.assertEqual((output / "records.ndjson").read_bytes(), first + middle)
        request = json.loads((output / "disposition-request.json").read_text())
        self.assertEqual(request["corrections"][0]["quarantine_code"], "EXACT_REPLAY_DUPLICATE")

    def test_tampered_duplicate_physical_line_fails_closed(self):
        first = (json.dumps(self.row("same"), ensure_ascii=False, separators=(",", ":")) + "\n").encode()
        altered = first.replace(b"\"name.original\"", b"\"name.changed\"")
        self.package([first, altered])
        self.stage_package()
        output = self.root / "tampered"
        result = self.run_prepare("duplicate_acknowledged", output)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse((output / "manifest.json").exists())

    def test_relationship_quarantine_can_only_be_deferred(self):
        bad = self.row("relationship")
        bad["relationships"] = [{"kind": "link", "target_source_system_id": "synthetic-his", "target_source_record_key": "missing"}]
        raw = (json.dumps(bad, ensure_ascii=False, separators=(",", ":")) + "\n").encode()
        self.package([raw])
        self.stage_package()
        corrections = self.root / "corrections.json"
        corrections.write_text(json.dumps([{"line": 1, "row": self.row("relationship")}]), encoding="utf-8")
        denied = self.run_prepare("corrected_resubmission", self.root / "blocked", "--corrections", corrections)
        self.assertNotEqual(denied.returncode, 0)
        deferred = self.run_prepare("deferred", self.root / "deferred")
        self.assertEqual(deferred.returncode, 0, deferred.stderr)
        body = json.loads((self.root / "deferred" / "disposition-request.json").read_text())
        self.assertEqual(body["action"], "deferred")
        self.assertIsNone(body["replacement_manifest_sha256"])


if __name__ == "__main__":
    unittest.main()
