"""Offline preflight tests using synthetic archives and command shims only."""

from __future__ import annotations

import hashlib
import os
from pathlib import Path
import subprocess
import tarfile
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
INSTALLER = ROOT / "tools/install/install.sh"


class InstallerPreflightTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="litecr-install-test-")
        self.root = Path(self.temp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir(mode=0o700)
        self.runtime_env = self.root / "runtime.env"
        self.runtime_env.write_text(
            "LITECR_DATABASE_URL=postgresql://fake:fake@127.0.0.1/litecr\n"
            "LITECR_ENV=production\n"
            "LITECR_OIDC_ISSUER_URL=https://idp.example.invalid/realms/synthetic\n"
            "LITECR_OIDC_CLIENT_ID=synthetic-client\n"
            "LITECR_OIDC_CLIENT_SECRET=synthetic-secret\n"
            "LITECR_WEB_ORIGIN=https://registry.example.invalid\n"
            "LITECR_OIDC_REDIRECT_URI=https://registry.example.invalid/api/v1/access/oidc/callback\n"
            "LITECR_COOKIE_SECURE=true\n",
            encoding="utf-8",
        )
        self.migration_env = self.root / "migration.env"
        self.migration_env.write_text(
            "LITECR_MIGRATION_DATABASE_URL=postgresql://synthetic:synthetic@127.0.0.1/litecr\n",
            encoding="utf-8",
        )
        self.runtime_env.chmod(0o600)
        self.migration_env.chmod(0o600)
        self.install_mock("id", """#!/bin/sh
if [ "$1" = -u ]; then echo 0; else exec /usr/bin/id "$@"; fi
""")
        self.install_mock("systemctl", "#!/bin/sh\nexit 0\n")
        self.install_mock("curl", "#!/bin/sh\nexit 0\n")
        self.install_mock("mktemp", """#!/bin/sh
if [ "$1" = -d ] && [ "$2" = /var/tmp/litecr-install.XXXXXXXX ]; then
  exec /usr/bin/mktemp -d "${TMPDIR:-/tmp}/litecr-install.XXXXXXXX"
fi
exec /usr/bin/mktemp "$@"
""")
        self.install_mock("df", "#!/bin/sh\nprintf 'Filesystem 1024-blocks Used Available Capacity Mounted\\nsynthetic 2097152 0 2097152 0%% /opt\\n'\n")
        self.install_mock("stat", """#!/bin/bash
if [[ "$1" == -c && "$2" == %u ]]; then echo 0; exit 0; fi
exec /usr/bin/stat "$@"
""")

    def tearDown(self) -> None:
        self.temp.cleanup()

    def install_mock(self, name: str, body: str) -> None:
        path = self.bin / name
        path.write_text(body, encoding="utf-8")
        path.chmod(0o700)

    def make_bundle(self, traversal: bool = False, binary_version: str = "0.1.0") -> Path:
        folder = self.root / "litecr-0.1.0-linux-x86_64"
        (folder / "bin").mkdir(parents=True)
        (folder / "share/templates").mkdir(parents=True)
        (folder / "share/metadata").mkdir(parents=True)
        (folder / "share/tools/operations").mkdir(parents=True)
        binary = folder / "bin/litecr"
        binary.write_text(
            r'''#!/bin/sh
if [ "$1 $2" = 'version --json' ]; then echo '{"application_version":"0.1.0","schema_version":9}'; exit 0; fi
if [ "$1" = doctor ]; then
  if [ -n "${LITECR_TEST_DOCTOR_OUTPUT:-}" ]; then printf '\033[31mERROR\033[0m liteCR command failed \033[3merror\033[0m\033[2m=\033[0m%s\n' "$LITECR_TEST_DOCTOR_OUTPUT" >&2; exit 1; fi
  exit 0
fi
exit 2
'''.replace('"application_version":"0.1.0"', f'"application_version":"{binary_version}"'),
            encoding="utf-8",
        )
        binary.chmod(0o700)
        for name in (
            "activate-pending-release.sh",
            "backup-postgres.sh",
            "grant-runtime-database-access.sh",
            "restore-postgres-isolated.sh",
        ):
            tool = folder / "share/tools/operations" / name
            tool.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
            tool.chmod(0o700)
        (folder / "share/metadata/version.json").write_text(
            f'{{"application_version":"{binary_version}","schema_version":9}}\n', encoding="utf-8"
        )
        (folder / "share/templates/litecr-api.service").write_text("[Service]\n", encoding="utf-8")
        (folder / "share/templates/litecr-migrate.service").write_text("[Service]\n", encoding="utf-8")
        (folder / "share/templates/litecr-activate.service").write_text("[Service]\n", encoding="utf-8")
        (folder / "INSTALL.md").write_text("synthetic\n", encoding="utf-8")
        files = sorted(path for path in folder.rglob("*") if path.is_file())
        sums = []
        for path in files:
            digest = hashlib.sha256(path.read_bytes()).hexdigest()
            sums.append(f"{digest}  {path.relative_to(folder).as_posix()}\n")
        (folder / "SHA256SUMS").write_text("".join(sums), encoding="utf-8")
        archive = self.root / "synthetic-release.tar.gz"
        with tarfile.open(archive, "w:gz") as tar:
            if traversal:
                item = tarfile.TarInfo("../escape")
                item.size = 1
                tar.addfile(item, __import__("io").BytesIO(b"x"))
            else:
                tar.add(folder, arcname=folder.name)
        sidecar = self.root / "synthetic-release.tar.gz.sha256"
        output = subprocess.check_output(["sha256sum", archive.name], cwd=self.root, text=True)
        sidecar.write_text(output, encoding="utf-8")
        sidecar.chmod(0o600)
        archive.chmod(0o600)
        return archive

    def run_installer(self, archive: Path, extra_env: dict[str, str] | None = None):
        sidecar = archive.with_name(archive.name + ".sha256")
        env = os.environ.copy()
        env["PATH"] = f"{self.bin}{os.pathsep}{env['PATH']}"
        env.update(extra_env or {})
        return subprocess.run(
            ["bash", str(INSTALLER), "--bundle", str(archive), "--checksum", str(sidecar),
             "--runtime-env-file", str(self.runtime_env), "--migration-env-file", str(self.migration_env),
             "--dry-run"],
            text=True,
            capture_output=True,
            env=env,
        )

    def test_valid_synthetic_bundle_passes_dry_preflight_without_changes(self) -> None:
        result = self.run_installer(self.make_bundle())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("preflight passed", result.stdout)
        self.assertNotIn("synthetic-secret", result.stdout + result.stderr)

    def test_insufficient_disk_space_is_rejected(self) -> None:
        self.install_mock("df", "#!/bin/sh\nprintf 'Filesystem 1024-blocks Used Available Capacity Mounted\\nsynthetic 100 99 1 99%% /opt\\n'\n")
        result = self.run_installer(self.make_bundle())
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("DISK_SPACE", result.stderr)

    def test_current_runtime_template_and_matching_budget_pass_preflight(self) -> None:
        values = dict(line.split("=", 1) for line in self.runtime_env.read_text().splitlines())
        template = ROOT / "infra/release/litecr.env.example"
        for line in template.read_text().splitlines():
            if not line or line.startswith("#"):
                continue
            key, value = line.split("=", 1)
            values.setdefault(key, value)
        values["LITECR_MATCHING_PARALLELISM"] = "4"
        self.runtime_env.write_text("".join(f"{key}={value}\n" for key, value in values.items()))
        self.runtime_env.chmod(0o600)
        result = self.run_installer(self.make_bundle())
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("preflight passed", result.stdout)

    def test_release_directory_must_match_embedded_binary_version(self) -> None:
        result = self.run_installer(self.make_bundle(binary_version="0.1.1"))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("RELEASE_VERSION_MISMATCH", result.stderr)
        self.assertNotIn("preflight passed", result.stdout)

    def test_only_explicit_missing_or_older_schema_errors_pass_preflight(self) -> None:
        messages = (
            "liteCR schema is not installed; run litecr migrate",
            "database schema 19 is older than application schema 20; run litecr migrate",
        )
        archive = self.make_bundle()
        for message in messages:
            with self.subTest(message=message):
                result = self.run_installer(
                    archive, {"LITECR_TEST_DOCTOR_OUTPUT": message}
                )
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertIn("preflight passed", result.stdout)

    def test_permission_or_newer_schema_diagnostics_fail_preflight(self) -> None:
        archive = self.make_bundle()
        for message in (
            "could not query liteCR schema metadata; verify database permissions and health",
            "database schema 21 is newer than application schema 20; use a compatible application release",
            "prefix liteCR schema is not installed; run litecr migrate",
        ):
            with self.subTest(message=message):
                result = self.run_installer(
                    archive, {"LITECR_TEST_DOCTOR_OUTPUT": message}
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("DATABASE_UNREACHABLE", result.stderr)

    def test_unrecognized_doctor_failure_still_fails_closed(self) -> None:
        result = self.run_installer(
            self.make_bundle(), {"LITECR_TEST_DOCTOR_OUTPUT": "database connection refused"}
        )
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("DATABASE_UNREACHABLE", result.stderr)

    def test_tampered_archive_fails_outer_checksum(self) -> None:
        archive = self.make_bundle()
        archive.write_bytes(archive.read_bytes() + b"tamper")
        result = self.run_installer(archive)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("BUNDLE_CHECKSUM_INVALID", result.stderr)

    def test_checksum_sidecar_requires_exactly_one_canonical_record(self) -> None:
        archive = self.make_bundle()
        sidecar = archive.with_name(archive.name + ".sha256")
        valid_record = sidecar.read_text(encoding="utf-8").strip()
        invalid_sidecars = (
            valid_record + "\n" + valid_record + "\n",
            valid_record + "\n\n",
            valid_record.replace("  ", " ", 1) + "\n",
            valid_record.rsplit("  ", 1)[0] + "  another-archive.tar.gz\n",
        )
        for content in invalid_sidecars:
            with self.subTest(sidecar=content):
                sidecar.write_text(content, encoding="utf-8")
                sidecar.chmod(0o600)
                result = self.run_installer(archive)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("CHECKSUM_SIDECAR_INVALID", result.stderr)

    def test_archive_path_traversal_is_rejected(self) -> None:
        result = self.run_installer(self.make_bundle(traversal=True))
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ARCHIVE_PATH", result.stderr)

    def test_unknown_environment_key_is_rejected_without_source_execution(self) -> None:
        self.runtime_env.write_text(
            self.runtime_env.read_text() + "UNKNOWN_KEY=$(touch /tmp/litecr-should-not-exist)\n",
            encoding="utf-8",
        )
        self.runtime_env.chmod(0o600)
        result = self.run_installer(self.make_bundle())
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("RUNTIME_ENV_KEY", result.stderr)
        self.assertFalse(Path("/tmp/litecr-should-not-exist").exists())

    def test_installer_refuses_public_api_bind(self) -> None:
        self.runtime_env.write_text(self.runtime_env.read_text() + "LITECR_BIND=0.0.0.0:8080\n", encoding="utf-8")
        self.runtime_env.chmod(0o600)
        result = self.run_installer(self.make_bundle())
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("API_BIND_NOT_PRIVATE", result.stderr)

    def test_world_readable_environment_file_is_refused(self) -> None:
        self.runtime_env.chmod(0o644)
        result = self.run_installer(self.make_bundle())
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("PROTECTED_FILE_MODE", result.stderr)


if __name__ == "__main__":
    unittest.main()
