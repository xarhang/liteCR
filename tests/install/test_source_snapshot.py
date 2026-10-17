"""Test clean, revision-bound source snapshot packaging with temporary Git repos."""

from __future__ import annotations

import hashlib
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
PACKAGER = ROOT / "tools/install/package-source-snapshot.sh"
VERSION = "0.1.9"


class SourceSnapshotTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="litecr-source-snapshot-")
        self.root = Path(self.temp.name)
        self.repo = self.root / "repo"
        required = (
            "Cargo.toml", "Cargo.lock", "LICENSE", "apps/web/package.json",
            "apps/web/package-lock.json", "crates/domain/Cargo.toml",
            "crates/infrastructure/migrations/0001_init.sql",
            "tools/install/build-release.sh", "tools/install/collect-third-party-texts.mjs",
            "tools/migration/litecr_migrate.py",
        )
        for relative in required:
            path = self.repo / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text(f"fixture: {relative}\n", encoding="utf-8")
        subprocess.run(["git", "init", "--quiet", str(self.repo)], check=True)
        subprocess.run(["git", "-C", str(self.repo), "add", "."], check=True)
        subprocess.run(
            ["git", "-C", str(self.repo), "-c", "user.name=liteCR test",
             "-c", "user.email=litecr-test@example.invalid", "-c", "commit.gpgsign=false",
             "commit", "--quiet", "-m", "source snapshot fixture"],
            check=True,
        )
        self.revision = subprocess.check_output(
            ["git", "-C", str(self.repo), "rev-parse", "HEAD"], text=True
        ).strip()

    def tearDown(self) -> None:
        self.temp.cleanup()

    def run_packager(self, output: Path, revision: str | None = None):
        return subprocess.run(
            ["bash", str(PACKAGER), "--repo", str(self.repo), "--revision", revision or self.revision,
             "--version", VERSION, "--output", str(output)],
            text=True, capture_output=True, check=False,
        )

    def test_clean_commit_produces_revision_named_source_archive_and_checksum(self) -> None:
        name = f"litecr-{VERSION}-source-{self.revision[:12]}.tar.gz"
        output = self.root / name
        result = self.run_packager(output)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertTrue(output.is_file())
        digest = hashlib.sha256(output.read_bytes()).hexdigest()
        self.assertEqual((self.root / f"{name}.sha256").read_text(), f"{digest}  {name}\n")
        with tarfile.open(output, "r:gz") as archive:
            paths = set(archive.getnames())
        prefix = f"litecr-{VERSION}-source-{self.revision[:12]}"
        self.assertIn(f"{prefix}/LICENSE", paths)
        self.assertIn(f"{prefix}/Cargo.lock", paths)
        self.assertIn(f"{prefix}/tools/install/build-release.sh", paths)
        second_output_dir = self.root / "second"
        second_output_dir.mkdir()
        second_output = second_output_dir / name
        second = self.run_packager(second_output)
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assertEqual(hashlib.sha256(second_output.read_bytes()).hexdigest(), digest)

    def test_dirty_worktree_and_non_head_revision_fail_without_output(self) -> None:
        output = self.root / f"litecr-{VERSION}-source-{self.revision[:12]}.tar.gz"
        (self.repo / "LICENSE").write_text("modified fixture\n", encoding="utf-8")
        dirty = self.run_packager(output)
        self.assertNotEqual(dirty.returncode, 0)
        self.assertIn("DIRTY_WORKTREE", dirty.stderr)
        self.assertFalse(output.exists())
        (self.repo / "LICENSE").write_text("fixture: LICENSE\n", encoding="utf-8")
        non_head = self.run_packager(output, "0" * 40)
        self.assertNotEqual(non_head.returncode, 0)
        self.assertIn("HEAD_REVISION_MISMATCH", non_head.stderr)
        self.assertFalse(output.exists())

    def test_existing_archive_is_never_replaced(self) -> None:
        name = f"litecr-{VERSION}-source-{self.revision[:12]}.tar.gz"
        output = self.root / name
        output.write_bytes(b"preserve existing file")
        result = self.run_packager(output)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("OUTPUT_EXISTS", result.stderr)
        self.assertEqual(output.read_bytes(), b"preserve existing file")


if __name__ == "__main__":
    unittest.main()
