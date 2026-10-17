"""Synthetic command-level checks; never connects to a database or Restic repo."""

from __future__ import annotations

import hashlib
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
BACKUP = ROOT / "tools/operations/backup-postgres.sh"
RESTORE = ROOT / "tools/operations/restore-postgres-isolated.sh"
CLEANUP_TEST = ROOT / "tests/operations/test_disposable_cluster_cleanup.sh"
BLOCKED_LIBPQ_ENVIRONMENT = (
    "PGHOST", "PGHOSTADDR", "PGPORT", "PGDATABASE", "PGUSER", "PGPASSWORD",
    "PGSSLNEGOTIATION", "PGREQUIREAUTH", "PGSSLSNI", "PGREQUIREPEER", "PGGSSLIB",
    "PGSERVICE", "PGSERVICEFILE", "PGOPTIONS", "PGCONNECT_TIMEOUT", "PGSSLMODE",
    "PGREQUIRESSL", "PGSSLCERT", "PGSSLKEY", "PGSSLROOTCERT", "PGSSLCRL",
    "PGSSLCRLDIR", "PGSSLCOMPRESSION", "PGSSLCERTMODE", "PGSSLMINPROTOCOLVERSION",
    "PGSSLMAXPROTOCOLVERSION", "PGSSLKEYLOGFILE", "PGCHANNELBINDING", "PGGSSENCMODE",
    "PGTARGETSESSIONATTRS", "PGLOADBALANCEHOSTS", "PGCLIENTENCODING", "PGREALM",
    "PGGSSDELEGATION", "PGKRBSRVNAME", "PGDATESTYLE", "PGTZ", "PGGEQO",
    "PGSYSCONFDIR", "PGLOCALEDIR", "PGAPPNAME",
)


class OperationsScriptsTest(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="litecr-ops-test-")
        self.root = Path(self.temp.name)
        self.bin = self.root / "bin"
        self.bin.mkdir(mode=0o700)
        self.log = self.root / "commands.log"
        self.config = self.root / "config"
        self.config.write_text(
            "LITECR_PG_DATABASE=synthetic_registry\n"
            "LITECR_PG_USER=litecr_test\n"
            f"LITECR_BACKUP_DIR={self.root / 'backups'}\n"
            "LITECR_RELEASE_ID=synthetic-test-release\n"
            "LITECR_MIN_FREE_KB=1\n",
            encoding="utf-8",
        )
        self.config.chmod(0o600)
        self.install_mock("pg_dump", r'''#!/usr/bin/env bash
printf 'pg_dump %s\n' "$*" >> "$OPS_TEST_LOG"
printf 'libpq PGHOST=%s PGPORT=%s PGPASSFILE=%s\n' "${PGHOST:-}" "${PGPORT:-}" "${PGPASSFILE:-}" >> "$OPS_TEST_LOG"
if [[ "${1:-}" == --version ]]; then echo 'pg_dump (PostgreSQL) 16.15'; exit 0; fi
[[ "${OPS_FAIL_DUMP:-}" != 1 ]] || exit 8
out=""
while (($#)); do if [[ "$1" == --file=* ]]; then out="${1#--file=}"; fi; shift; done
printf 'synthetic custom archive\n' > "$out"
''')
        self.install_mock("psql", r'''#!/usr/bin/env bash
printf 'psql %s\n' "$*" >> "$OPS_TEST_LOG"
case "${*: -1}" in
  *'schema_meta'*)
    count=$(grep -c "^psql .*schema_meta" "$OPS_TEST_LOG" || true)
    if [[ "$count" -le 1 ]]; then echo "${OPS_SCHEMA_VERSION_BEFORE:-${OPS_SCHEMA_VERSION:-9}}"; else echo "${OPS_SCHEMA_VERSION_AFTER:-${OPS_SCHEMA_VERSION:-9}}"; fi
    ;;
  *'SHOW server_version'*)
    count=$(grep -c "^psql .*SHOW server_version" "$OPS_TEST_LOG" || true)
    if [[ "$count" -le 1 ]]; then echo "${OPS_SERVER_VERSION_BEFORE:-${OPS_SERVER_VERSION:-16.15 (Ubuntu 16.15-0ubuntu0.24.04.1)}}"; else echo "${OPS_SERVER_VERSION_AFTER:-${OPS_SERVER_VERSION:-16.15 (Ubuntu 16.15-0ubuntu0.24.04.1)}}"; fi
    ;;
  *'system_identifier'*) echo "${OPS_SYSTEM_IDENTIFIER:-1234567890123456789}" ;;
  *'pg_database'*) echo "${OPS_TARGET_EXISTS:-}" ;;
  *) exit 9 ;;
esac
''')
        self.install_mock("pg_restore", r'''#!/usr/bin/env bash
printf 'pg_restore %s\n' "$*" >> "$OPS_TEST_LOG"
if [[ "${1:-}" == --list && "${OPS_INVALID_BACKUP_ARCHIVE:-}" == 1 ]]; then exit 8; fi
[[ "${OPS_FAIL_RESTORE:-}" != 1 ]] || exit 8
''')
        self.install_mock("createdb", r'''#!/usr/bin/env bash
printf 'createdb %s\n' "$*" >> "$OPS_TEST_LOG"
''')
        self.install_mock("restic", r'''#!/usr/bin/env bash
printf 'restic %s\n' "$*" >> "$OPS_TEST_LOG"
''')

    def tearDown(self) -> None:
        self.temp.cleanup()

    def install_mock(self, name: str, body: str) -> None:
        path = self.bin / name
        path.write_text(body, encoding="utf-8")
        path.chmod(0o700)

    def run_script(self, script: Path, *args: str, extra_env: dict[str, str] | None = None):
        env = os.environ.copy()
        for key in (*BLOCKED_LIBPQ_ENVIRONMENT, "PGPASSFILE", "RESTIC_REPOSITORY", "RESTIC_PASSWORD_FILE"):
            env.pop(key, None)
        env.update({
            "PATH": f"{self.bin}{os.pathsep}{env['PATH']}",
            "LITECR_CONFIG_FILE": str(self.config),
            "OPS_TEST_LOG": str(self.log),
        })
        if extra_env:
            env.update(extra_env)
        return subprocess.run(["bash", str(script), *args], env=env, text=True, capture_output=True)

    def test_backup_creates_private_dump_manifest_and_checksum(self) -> None:
        result = self.run_script(BACKUP, "--archive-only")
        self.assertEqual(result.returncode, 0, result.stderr)
        backup_dir = self.root / "backups"
        dump = next(backup_dir.glob("*.dump"))
        manifest = dump.with_suffix(".manifest")
        checksum = dump.with_suffix(".sha256")
        success = dump.with_suffix(".success")
        self.assertIn("format=litecr-backup-v1", manifest.read_text())
        self.assertIn("format=litecr-backup-success-v1", success.read_text())
        self.assertIn("schema_version=9", manifest.read_text())
        self.assertIn("postgres_server_version=16.15 (Ubuntu 16.15-0ubuntu0.24.04.1)", manifest.read_text())
        self.assertEqual(dump.stat().st_mode & 0o777, 0o600)
        self.assertEqual(manifest.stat().st_mode & 0o777, 0o600)
        self.assertEqual(success.stat().st_mode & 0o777, 0o600)
        digest = hashlib.sha256(dump.read_bytes()).hexdigest()
        self.assertIn(digest, checksum.read_text())
        self.assertNotIn("password", result.stdout.lower() + result.stderr.lower())

    def test_backup_uses_explicit_endpoint_and_protected_password_file(self) -> None:
        passfile = self.root / "backup.pgpass"
        passfile.write_text("localhost:5432:synthetic_registry:litecr_test:synthetic-secret\n", encoding="utf-8")
        passfile.chmod(0o600)
        self.config.write_text(
            self.config.read_text()
            + "LITECR_PGHOST=127.0.0.1\n"
            + "LITECR_PGPORT=5432\n",
            encoding="utf-8",
        )
        self.config.chmod(0o600)

        result = self.run_script(BACKUP, extra_env={"PGPASSFILE": str(passfile)})
        self.assertEqual(result.returncode, 0, result.stderr)
        log = self.log.read_text()
        self.assertIn(f"libpq PGHOST=127.0.0.1 PGPORT=5432 PGPASSFILE={passfile}", log)
        self.assertNotIn("synthetic-secret", log + result.stdout + result.stderr)

    def test_low_space_fails_before_dump(self) -> None:
        text = self.config.read_text().replace("LITECR_MIN_FREE_KB=1", "LITECR_MIN_FREE_KB=999999999999")
        self.config.write_text(text)
        result = self.run_script(BACKUP)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("INSUFFICIENT_FREE_SPACE", result.stderr)
        self.assertFalse(self.log.exists())

    def test_backup_rejects_inherited_libpq_settings_before_database_commands(self) -> None:
        for variable in BLOCKED_LIBPQ_ENVIRONMENT:
            with self.subTest(variable=variable):
                self.log.unlink(missing_ok=True)
                result = self.run_script(BACKUP, extra_env={variable: "inherited-value"})
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("LIBPQ_ENVIRONMENT_OVERRIDE", result.stderr)
                self.assertFalse(self.log.exists(), "psql/pg_dump must not run with inherited libpq settings")
                backup_dir = self.root / "backups"
                self.assertFalse(backup_dir.exists(), "backup artifacts must not be created")

    def test_backup_rejects_schema_change_across_dump_snapshot(self) -> None:
        result = self.run_script(BACKUP, extra_env={"OPS_SCHEMA_VERSION_BEFORE": "16", "OPS_SCHEMA_VERSION_AFTER": "17"})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("SCHEMA_CHANGED_DURING_BACKUP", result.stderr)
        backup_dir = self.root / "backups"
        self.assertEqual(list(backup_dir.glob("*.dump")), [])
        self.assertEqual(list(backup_dir.glob("*.manifest")), [])
        self.assertEqual(list(backup_dir.glob("*.sha256")), [])

    def test_backup_manifest_records_stable_schema_and_server_versions(self) -> None:
        result = self.run_script(BACKUP)
        self.assertEqual(result.returncode, 0, result.stderr)
        log = self.log.read_text()
        self.assertEqual(log.count("schema_meta"), 2)
        self.assertEqual(log.count("SHOW server_version"), 2)
        manifest = next((self.root / "backups").glob("*.manifest")).read_text()
        self.assertIn("schema_version=9", manifest)
        self.assertIn("postgres_server_version=16.15 (Ubuntu 16.15-0ubuntu0.24.04.1)", manifest)

    def test_failed_dump_does_not_publish_completed_artifacts(self) -> None:
        result = self.run_script(BACKUP, extra_env={"OPS_FAIL_DUMP": "1"})
        self.assertNotEqual(result.returncode, 0)
        backup_dir = self.root / "backups"
        self.assertEqual(list(backup_dir.glob("*.dump")), [])
        self.assertEqual(list(backup_dir.glob("*.manifest")), [])

    def test_backup_temp_allocation_failures_clean_earlier_files_only(self) -> None:
        backup_dir = self.root / "backups"
        backup_dir.mkdir(mode=0o700)
        unrelated = backup_dir / "operator-note.txt"
        unrelated.write_text("preserve this unrelated file\n", encoding="utf-8")
        unrelated.chmod(0o600)

        count_file = self.root / "mktemp-count"
        self.install_mock("mktemp", r'''#!/usr/bin/env bash
set -euo pipefail
count=0
if [[ -f "$OPS_MKTEMP_COUNT_FILE" ]]; then count="$(cat -- "$OPS_MKTEMP_COUNT_FILE")"; fi
count=$((count + 1))
printf '%s\n' "$count" > "$OPS_MKTEMP_COUNT_FILE"
if [[ "$count" == "$OPS_FAIL_MKTEMP_AT" ]]; then exit 73; fi
exec /usr/bin/mktemp "$@"
''')

        # Fail each temp allocation in turn. The EXIT trap must already be
        # active and must remove successful earlier allocations only.
        for failure_index in range(1, 5):
            with self.subTest(failed_allocation=failure_index):
                count_file.unlink(missing_ok=True)
                result = self.run_script(
                    BACKUP,
                    extra_env={
                        "OPS_FAIL_MKTEMP_AT": str(failure_index),
                        "OPS_MKTEMP_COUNT_FILE": str(count_file),
                    },
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(count_file.read_text(encoding="utf-8").strip(), str(failure_index))
                self.assertEqual(list(backup_dir.glob(".litecr-*")), [])
                self.assertEqual(list(backup_dir.glob("*.dump")), [])
                self.assertEqual(list(backup_dir.glob("*.manifest")), [])
                self.assertEqual(list(backup_dir.glob("*.sha256")), [])
                self.assertEqual(list(backup_dir.glob("*.success")), [])
                self.assertEqual(unrelated.read_text(encoding="utf-8"), "preserve this unrelated file\n")
                self.assertEqual(unrelated.stat().st_mode & 0o777, 0o600)

    def test_backup_publication_rename_failures_clean_only_this_runs_files(self) -> None:
        backup_dir = self.root / "backups"
        backup_dir.mkdir(mode=0o700)
        unrelated = backup_dir / "operator-note.txt"
        unrelated.write_text("preserve this unrelated file\n", encoding="utf-8")
        unrelated.chmod(0o600)

        count_file = self.root / "mv-count"
        self.install_mock("mv", r'''#!/usr/bin/env bash
set -euo pipefail
count=0
if [[ -f "$OPS_MV_COUNT_FILE" ]]; then count="$(cat -- "$OPS_MV_COUNT_FILE")"; fi
count=$((count + 1))
printf '%s\n' "$count" > "$OPS_MV_COUNT_FILE"
if [[ "$count" == "$OPS_FAIL_MV_AT" ]]; then exit 73; fi
exec /usr/bin/mv "$@"
''')

        # The four moves publish dump, manifest, checksum and completion marker.
        # Fail each operation independently and verify cleanup never broadens to
        # regular files that this invocation did not create.
        for failure_index in range(1, 5):
            with self.subTest(failed_move=failure_index):
                count_file.unlink(missing_ok=True)
                result = self.run_script(
                    BACKUP,
                    extra_env={
                        "OPS_FAIL_MV_AT": str(failure_index),
                        "OPS_MV_COUNT_FILE": str(count_file),
                    },
                )
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(count_file.read_text(encoding="utf-8").strip(), str(failure_index))
                self.assertEqual(list(backup_dir.glob("*.dump")), [])
                self.assertEqual(list(backup_dir.glob("*.manifest")), [])
                self.assertEqual(list(backup_dir.glob("*.sha256")), [])
                self.assertEqual(list(backup_dir.glob("*.success")), [])
                self.assertEqual(list(backup_dir.glob(".litecr-*")), [])
                self.assertEqual(unrelated.read_text(encoding="utf-8"), "preserve this unrelated file\n")
                self.assertEqual(unrelated.stat().st_mode & 0o777, 0o600)

    def test_backup_rejects_unreadable_archive_before_publishing_artifacts(self) -> None:
        result = self.run_script(BACKUP, extra_env={"OPS_INVALID_BACKUP_ARCHIVE": "1"})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ARCHIVE_INVALID", result.stderr)
        backup_dir = self.root / "backups"
        self.assertEqual(list(backup_dir.glob("*.dump")), [])
        self.assertEqual(list(backup_dir.glob("*.manifest")), [])
        self.assertEqual(list(backup_dir.glob("*.sha256")), [])
        self.assertIn("pg_restore --list", self.log.read_text())

    def test_duplicate_config_key_is_rejected(self) -> None:
        self.config.write_text(self.config.read_text() + "LITECR_PG_USER=other\n")
        self.config.chmod(0o600)
        result = self.run_script(BACKUP)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("CONFIG_KEY_DUPLICATE", result.stderr)

    def synthetic_dump(self) -> Path:
        backup_dir = self.root / "restore-input"
        backup_dir.mkdir(mode=0o700, exist_ok=True)
        dump = backup_dir / "synthetic.dump"
        dump.write_bytes(b"synthetic archive bytes\n")
        digest = hashlib.sha256(dump.read_bytes()).hexdigest()
        (backup_dir / "synthetic.manifest").write_text(
            "format=litecr-backup-v1\n"
            "created_utc=2026-01-01T00:00:00Z\n"
            "database=synthetic_registry\n"
            "release_id=synthetic-test-release\n"
            "schema_version=9\n"
            "postgres_server_version=16.15\n"
            "pg_dump_version=pg_dump PostgreSQL 16.15\n"
            "dump_file=synthetic.dump\n"
            f"dump_bytes={dump.stat().st_size}\n"
            f"sha256={digest}\n",
            encoding="utf-8",
        )
        (backup_dir / "synthetic.sha256").write_text(f"{digest}  synthetic.dump\n", encoding="utf-8")
        (backup_dir / "synthetic.success").write_text(
            "format=litecr-backup-success-v1\n"
            "backup_id=synthetic\n"
            "dump_file=synthetic.dump\n"
            f"sha256={digest}\n"
            "completed_utc=2026-01-01T00:00:00Z\n",
            encoding="utf-8",
        )
        for path in (dump, backup_dir / "synthetic.manifest", backup_dir / "synthetic.sha256", backup_dir / "synthetic.success"):
            path.chmod(0o600)
        return dump

    def restore(self, dump: Path, extra_env: dict[str, str] | None = None):
        marker = self.root / "isolation-marker"
        marker.write_text(
            "format=litecr-isolated-postgres-target-v2\n"
            "system_identifier=1234567890123456789\n",
            encoding="utf-8",
        )
        marker.chmod(0o600)
        return self.restore_with_marker(dump, marker, extra_env)

    def restore_with_marker(self, dump: Path, marker: Path, extra_env: dict[str, str] | None = None):
        config = self.root / "restore-config"
        config.write_text("LITECR_PG_USER=litecr_test\n", encoding="utf-8")
        config.chmod(0o600)
        env = {"LITECR_CONFIG_FILE": str(config)}
        env.update(extra_env or {})
        return self.run_script(
            RESTORE, "--dump", str(dump), "--target-db", "litecr_restore_test",
            "--isolation-marker", str(marker), extra_env=env,
        )

    def test_restore_requires_isolated_local_target_and_single_transaction(self) -> None:
        result = self.restore(self.synthetic_dump())
        self.assertEqual(result.returncode, 0, result.stderr)
        log = self.log.read_text()
        self.assertIn("createdb", log)
        self.assertIn("--single-transaction", log)
        self.assertIn("--exit-on-error", log)
        self.assertIn("database=litecr_restore_test schema_version=9", result.stdout)

    def test_restore_refuses_existing_database_before_creation(self) -> None:
        result = self.restore(self.synthetic_dump(), {"OPS_TARGET_EXISTS": "1"})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("TARGET_DATABASE_ALREADY_EXISTS", result.stderr)
        self.assertNotIn("createdb", self.log.read_text())

    def test_restore_refuses_a_different_postgres_cluster_before_database_creation(self) -> None:
        result = self.restore(self.synthetic_dump(), {"OPS_SYSTEM_IDENTIFIER": "9876543210987654321"})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("TARGET_CLUSTER_IDENTITY_MISMATCH", result.stderr)
        log = self.log.read_text()
        self.assertIn("system_identifier", log)
        self.assertNotIn("createdb", log)

    def test_restore_rejects_legacy_or_malformed_isolation_marker_before_database_access(self) -> None:
        dump = self.synthetic_dump()
        marker = self.root / "isolation-marker"
        marker.write_text("litecr-isolated-postgres-target-v1\n", encoding="utf-8")
        marker.chmod(0o600)
        result = self.restore_with_marker(dump, marker)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("ISOLATION_MARKER_CONTENT", result.stderr)
        self.assertFalse(self.log.exists())

    def test_restore_requires_completed_backup_marker_before_database_access(self) -> None:
        dump = self.synthetic_dump()
        dump.with_suffix(".success").unlink()
        result = self.restore(dump)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("SIDECAR_MISSING", result.stderr)
        self.assertFalse(self.log.exists())

    def test_restore_rejects_inconsistent_or_malformed_success_marker_before_database_access(self) -> None:
        cases = {
            "format": ("format=wrong-format", "SUCCESS_MARKER_FORMAT"),
            "duplicate": ("backup_id=synthetic\nbackup_id=other", "SUCCESS_MARKER_KEY_DUPLICATE"),
            "unknown": ("unexpected=value", "SUCCESS_MARKER_KEY_UNKNOWN"),
            "backup_id": ("backup_id=other", "SUCCESS_MARKER_BACKUP_ID"),
            "dump_file": ("dump_file=other.dump", "SUCCESS_MARKER_DUMP_FILE"),
            "sha256": ("sha256=" + "0" * 64, "SUCCESS_MARKER_CHECKSUM"),
            "timestamp": ("completed_utc=not-a-timestamp", "SUCCESS_MARKER_TIMESTAMP"),
        }
        for name, (replacement, error_code) in cases.items():
            with self.subTest(name=name):
                dump = self.synthetic_dump()
                marker = dump.with_suffix(".success")
                content = marker.read_text(encoding="utf-8")
                field = replacement.split("=", 1)[0]
                lines = [line for line in content.splitlines() if not line.startswith(field + "=")]
                marker.write_text("\n".join([*lines, *replacement.splitlines()]) + "\n", encoding="utf-8")
                marker.chmod(0o600)
                self.log.unlink(missing_ok=True)
                result = self.restore(dump)
                self.assertNotEqual(result.returncode, 0)
                self.assertIn(error_code, result.stderr)
                self.assertFalse(self.log.exists(), "invalid completion marker must be rejected before PostgreSQL access")

    def test_restore_rejects_unprotected_success_marker_before_database_access(self) -> None:
        dump = self.synthetic_dump()
        marker = dump.with_suffix(".success")
        marker.chmod(0o644)
        result = self.restore(dump)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("BACKUP_INPUT_MODE", result.stderr)
        self.assertFalse(self.log.exists())

    def test_restore_rejects_inherited_libpq_settings_before_database_commands(self) -> None:
        dump = self.synthetic_dump()
        for variable in BLOCKED_LIBPQ_ENVIRONMENT:
            with self.subTest(variable=variable):
                self.log.unlink(missing_ok=True)
                result = self.restore(dump, {variable: "inherited-value"})
                self.assertNotEqual(result.returncode, 0)
                self.assertIn("LIBPQ_ENVIRONMENT_OVERRIDE", result.stderr)
                self.assertFalse(self.log.exists(), "restore must not contact PostgreSQL with inherited libpq settings")

    def test_restore_refuses_remote_database_host(self) -> None:
        result = self.restore(self.synthetic_dump(), {"PGHOST": "database.example.invalid"})
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("LIBPQ_ENVIRONMENT_OVERRIDE", result.stderr)
        self.assertFalse(self.log.exists())

    def test_restore_refuses_modified_dump_before_database_access(self) -> None:
        dump = self.synthetic_dump()
        dump.write_bytes(dump.read_bytes() + b"tampered")
        dump.chmod(0o600)
        result = self.restore(dump)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("DUMP_SIZE_MISMATCH", result.stderr)
        self.assertFalse(self.log.exists())

    def test_transfer_mode_requires_explicit_encrypted_repo_configuration(self) -> None:
        result = self.run_script(BACKUP, "--transfer")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("RESTIC_REPOSITORY", result.stderr)
        self.assertNotIn("restic ", self.log.read_text())

    def test_transfer_includes_completed_backup_marker(self) -> None:
        password_file = self.root / "restic-password"
        password_file.write_text("synthetic recovery secret\n", encoding="utf-8")
        password_file.chmod(0o600)
        result = self.run_script(
            BACKUP,
            "--transfer",
            extra_env={"RESTIC_REPOSITORY": str(self.root / "restic-repo"), "RESTIC_PASSWORD_FILE": str(password_file)},
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        log = self.log.read_text()
        success = next((self.root / "backups").glob("*.success"))
        self.assertIn(str(success), log)

    def test_disposable_cluster_cleanup_preserves_data_when_stop_fails(self) -> None:
        result = subprocess.run(
            ["bash", str(CLEANUP_TEST)], text=True, capture_output=True, timeout=10
        )
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("Disposable PostgreSQL cleanup failure/success cases passed.", result.stdout)


if __name__ == "__main__":
    unittest.main()
