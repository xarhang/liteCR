"""Exercise the installer mutation path inside a disposable Linux mount namespace.

These tests never mount over host paths in the parent namespace. They skip on
hosts where unprivileged user/mount namespaces are unavailable.
"""

from __future__ import annotations

import hashlib
import os
from pathlib import Path
import shutil
import subprocess
import tarfile
import tempfile
import unittest


ROOT = Path(__file__).resolve().parents[2]
INSTALLER = ROOT / "tools/install/install.sh"


class InstallerStagingTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls) -> None:
        if os.name != "posix" or not shutil.which("unshare"):
            raise unittest.SkipTest("Linux user and mount namespaces are required")
        probe = subprocess.run(
            ["unshare", "-Urnm", "true"], capture_output=True, text=True, check=False
        )
        if probe.returncode != 0:
            raise unittest.SkipTest("unprivileged user/mount namespaces are unavailable")

    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory(prefix="litecr-install-stage-")
        self.root = Path(self.temp.name)
        self.isolated = self.root / "root"
        for path in ("etc/systemd/system", "etc/alternatives", "var/tmp", "var/lib", "opt", "run/lock"):
            (self.isolated / path).mkdir(parents=True, exist_ok=True)
        (self.isolated / "etc/alternatives/awk").symlink_to("/usr/bin/gawk")
        (self.isolated / "etc/passwd").write_text("root:x:0:0:root:/root:/bin/sh\n", encoding="utf-8")
        (self.isolated / "etc/group").write_text("root:x:0:\n", encoding="utf-8")
        self.bin = self.root / "bin"
        self.bin.mkdir()
        self.log = self.root / "calls.log"
        self.config_stat_overrides: dict[str, str] = {}
        self.write_shims()
        self.runtime_env = self.root / "runtime.env"
        self.runtime_env.write_text(
            "LITECR_DATABASE_URL=postgresql://synthetic:synthetic@127.0.0.1/litecr\n"
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
        self.bundle = self.make_bundle()
        self.sidecar = self.bundle.with_name(self.bundle.name + ".sha256")

    def tearDown(self) -> None:
        self.temp.cleanup()

    def write_shim(self, name: str, source: str) -> None:
        path = self.bin / name
        path.write_text(source, encoding="utf-8")
        path.chmod(0o755)

    def write_shims(self, fail_activation: bool = False, fail_copy: bool = False) -> None:
        self.write_shim(
            "stat",
            "#!/usr/bin/python3\n"
            "import os, sys\n"
            "args=sys.argv[1:]; target=args[-1]; fmt=args[args.index('-c')+1] if '-c' in args else ''\n"
            "if target == '/run/lock/litecr-release' and fmt == '%U:%G:%a': print('root:litecr-migrate:750')\n"
            "elif target == '/run/lock/litecr-release/lock' and fmt == '%U:%G:%a': print('root:litecr-migrate:660')\n"
            "elif target == '/etc/litecr/litecr.env' and fmt == '%U:%G:%a': print(os.environ.get('LITECR_TEST_RUNTIME_CONFIG_STAT','root:litecr:640'))\n"
            "elif target == '/etc/litecr/migration.env' and fmt == '%U:%G:%a': print(os.environ.get('LITECR_TEST_MIGRATION_CONFIG_STAT','root:litecr-migrate:640'))\n"
            "else: os.execv('/usr/bin/stat',['stat',*args])\n",
        )
        self.write_shim(
            "id",
            "#!/bin/sh\n"
            "if [ \"$1\" = -u ]; then echo 0; exit 0; fi\n"
            "echo \"id $*\" >> \"$LITECR_TEST_LOG\"; exit 1\n",
        )
        self.write_shim(
            "getent",
            "#!/bin/sh\necho \"getent $*\" >> \"$LITECR_TEST_LOG\"; exit 2\n",
        )
        for name in ("groupadd", "useradd", "chown"):
            self.write_shim(
                name,
                f"#!/bin/sh\necho \"{name} $*\" >> \"$LITECR_TEST_LOG\"; exit 0\n",
            )
        self.write_shim(
            "install",
            "#!/usr/bin/python3\n"
            "import os, sys\n"
            "args=sys.argv[1:]; out=[]; i=0\n"
            "while i < len(args):\n"
            "  if args[i] in ('-o','-g'): i += 2; continue\n"
            "  out.append(args[i]); i += 1\n"
            "with open(os.environ['LITECR_TEST_LOG'],'a') as f: f.write('install '+' '.join(args)+'\\n')\n"
            "os.execv('/usr/bin/install',['install',*out])\n",
        )
        self.write_shim(
            "systemctl",
            "#!/bin/sh\n"
            "if [ \"$1\" = --version ]; then echo systemd-test; exit 0; fi\n"
            "echo \"systemctl $*\" >> \"$LITECR_TEST_LOG\"; exit 0\n",
        )
        self.write_shim("curl", "#!/bin/sh\nexit 0\n")
        mv_body = (
            "#!/bin/sh\n"
            "echo \"mv $*\" >> \"$LITECR_TEST_LOG\"\n"
        )
        if fail_activation:
            mv_body += (
                "case \"$*\" in *'/opt/litecr/pending') exit 73;; esac\n"
            )
        mv_body += "exec /usr/bin/mv \"$@\"\n"
        self.write_shim("mv", mv_body)
        cp_body = "#!/bin/bash\n"
        if fail_copy:
            cp_body += (
                "args=(\"$@\"); src=\"${args[${#args[@]}-2]}\"; dest=\"${args[${#args[@]}-1]}\"\n"
                "mkdir -p -- \"$dest/bin\"\n"
                "/usr/bin/cp -- \"$src/bin/litecr\" \"$dest/bin/litecr\"\n"
                "exit 73\n"
            )
        else:
            cp_body += "exec /usr/bin/cp \"$@\"\n"
        self.write_shim("cp", cp_body)

    def make_bundle(self, version: str = "0.1.0", payload_marker: str = "") -> Path:
        folder = self.root / f"litecr-{version}-linux-x86_64"
        (folder / "bin").mkdir(parents=True, exist_ok=True)
        for subdir in ("share/templates", "share/metadata", "share/tools/operations"):
            (folder / subdir).mkdir(parents=True, exist_ok=True)
        binary = folder / "bin/litecr"
        binary.write_text(
            "#!/bin/sh\n"
            f"# synthetic payload marker: {payload_marker}\n"
            f"if [ \"$1 $2\" = 'version --json' ]; then printf '%s\\n' '{{\"application_version\":\"{version}\",\"schema_version\":11}}'; exit 0; fi\n"
            "if [ \"$1\" = doctor ]; then exit \"${LITECR_TEST_DOCTOR_STATUS:-0}\"; fi\n"
            "exit 2\n",
            encoding="utf-8",
        )
        binary.chmod(0o755)
        (folder / "share/metadata/version.json").write_text(
            f'{{"application_version":"{version}","schema_version":11}}\n', encoding="utf-8"
        )
        for unit in ("litecr-api.service", "litecr-migrate.service", "litecr-activate.service"):
            shutil.copyfile(ROOT / "infra/release" / unit, folder / "share/templates" / unit)
        helper = folder / "share/tools/operations/activate-pending-release.sh"
        shutil.copyfile(ROOT / "tools/operations/activate-pending-release.sh", helper)
        helper.chmod(0o755)
        for name in (
            "grant-runtime-database-access.sh",
            "grant-worker-database-access.sh",
            "backup-postgres.sh",
            "restore-postgres-isolated.sh",
        ):
            tool = folder / "share/tools/operations" / name
            shutil.copyfile(ROOT / "tools/operations" / name, tool)
            tool.chmod(0o755)
        (folder / "INSTALL.md").write_text("synthetic installer fixture\n", encoding="utf-8")
        (folder / "SHA256SUMS").unlink(missing_ok=True)
        sums = []
        for path in sorted(p for p in folder.rglob("*") if p.is_file()):
            sums.append(f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.relative_to(folder).as_posix()}\n")
        (folder / "SHA256SUMS").write_text("".join(sums), encoding="utf-8")
        archive = self.root / f"litecr-{version}-linux-x86_64.tar.gz"
        with tarfile.open(archive, "w:gz") as tar:
            tar.add(folder, arcname=folder.name)
        digest = hashlib.sha256(archive.read_bytes()).hexdigest()
        sidecar = archive.with_name(archive.name + ".sha256")
        sidecar.write_text(f"{digest}  {archive.name}\n", encoding="utf-8")
        archive.chmod(0o600)
        sidecar.chmod(0o600)
        return archive

    def run_install(
        self, fail_activation: bool = False, backup_verified: bool | None = None,
        fail_copy: bool = False,
    ) -> subprocess.CompletedProcess[str]:
        self.write_shims(fail_activation=fail_activation, fail_copy=fail_copy)
        args = [
            "--bundle", str(self.bundle), "--checksum", str(self.sidecar),
            "--runtime-env-file", str(self.runtime_env), "--migration-env-file", str(self.migration_env),
        ]
        if backup_verified is True or (
            backup_verified is None and (self.isolated / "opt/litecr/current").is_symlink()
        ):
            args.append("--backup-verified")
        mount_script = r'''
set -eu
fixture=$1; installer=$2; log=$3; shift 3
mount --make-rprivate /
mount --bind "$fixture/etc" /etc
mount --bind "$fixture/var" /var
mount --bind "$fixture/opt" /opt
mount --bind "$fixture/run" /run
export PATH="$LITECR_TEST_BIN:$PATH" LITECR_TEST_LOG="$log"
for command in uname stat awk tar sha256sum cmp find df getent id useradd groupadd install ln mv readlink systemctl curl mktemp grep chown chmod cp flock mkdir; do
  command -v "$command" >/dev/null 2>&1 || echo "TEST_MISSING_COMMAND=$command" >&2
done
bash "$installer" "$@"
'''
        env = os.environ.copy()
        env["LITECR_TEST_BIN"] = str(self.bin)
        env["LITECR_TEST_LOG"] = str(self.log)
        for config_name, stat_value in self.config_stat_overrides.items():
            env[f"LITECR_TEST_{config_name.upper()}_CONFIG_STAT"] = stat_value
        return subprocess.run(
            ["unshare", "-Urnm", "bash", "-c", mount_script, "--", str(self.isolated), str(INSTALLER), str(self.log), *args],
            text=True, capture_output=True, env=env, check=False,
        )

    def assert_staged(self, current_target: str | None = None) -> None:
        target = self.isolated / "opt/litecr/releases/0.1.0"
        self.assertTrue(target.is_dir())
        self.assertEqual(target.stat().st_mode & 0o777, 0o755)
        pending = self.isolated / "opt/litecr/pending"
        self.assertTrue(pending.is_symlink())
        self.assertEqual(os.readlink(pending), "/opt/litecr/releases/0.1.0")
        for name in ("backup-postgres.sh", "restore-postgres-isolated.sh"):
            self.assertTrue((target / "share/tools/operations" / name).is_file())
        for name in (
            "activate-pending-release.sh",
            "grant-runtime-database-access.sh",
            "grant-worker-database-access.sh",
        ):
            self.assertTrue((target / "share/tools/operations" / name).stat().st_mode & 0o111)
        current = self.isolated / "opt/litecr/current"
        if current_target is None:
            self.assertFalse(current.exists())
        else:
            self.assertTrue(current.is_symlink())
            self.assertEqual(os.readlink(current), current_target)
        runtime = self.isolated / "etc/litecr/litecr.env"
        migration = self.isolated / "etc/litecr/migration.env"
        self.assertEqual(runtime.read_bytes(), self.runtime_env.read_bytes())
        self.assertEqual(migration.read_bytes(), self.migration_env.read_bytes())
        self.assertEqual(runtime.stat().st_mode & 0o777, 0o640)
        self.assertEqual(migration.stat().st_mode & 0o777, 0o640)
        self.assertTrue((self.isolated / "etc/systemd/system/litecr-api.service").is_file())
        self.assertTrue((self.isolated / "etc/systemd/system/litecr-migrate.service").is_file())

    def test_mutating_install_stages_protected_files_and_retry_is_idempotent(self) -> None:
        first = self.run_install()
        self.assertEqual(first.returncode, 0, first.stderr)
        self.assertIn("Migrations were not run", first.stdout)
        self.assert_staged()
        prior_sums = (self.isolated / "opt/litecr/releases/0.1.0/SHA256SUMS").read_bytes()
        second = self.run_install()
        self.assertEqual(second.returncode, 0, second.stderr)
        self.assert_staged()
        self.assertEqual((self.isolated / "opt/litecr/releases/0.1.0/SHA256SUMS").read_bytes(), prior_sums)
        calls = self.log.read_text(encoding="utf-8")
        self.assertIn("chown litecr:litecr /var/lib/litecr", calls)
        self.assertIn("chown litecr-migrate:litecr-migrate /var/lib/litecr-migrate", calls)
        self.assertIn("systemctl daemon-reload", calls)
        self.assertNotIn("systemctl start", calls)
        self.assertNotIn("systemctl enable", calls)

    def test_retry_refuses_existing_release_with_damaged_payload(self) -> None:
        installed = self.run_install()
        self.assertEqual(installed.returncode, 0, installed.stderr)
        target = self.isolated / "opt/litecr/releases/0.1.0"
        binary = target / "bin/litecr"
        binary.write_text("#!/bin/sh\nexit 99\n", encoding="utf-8")

        retried = self.run_install()

        self.assertNotEqual(retried.returncode, 0)
        self.assertIn("RELEASE_TARGET_CHECKSUM_INVALID", retried.stderr)
        self.assertEqual(os.readlink(self.isolated / "opt/litecr/pending"), "/opt/litecr/releases/0.1.0")
        self.assertEqual(binary.read_text(encoding="utf-8"), "#!/bin/sh\nexit 99\n")

    def test_same_version_different_archive_cannot_replace_immutable_release(self) -> None:
        installed = self.run_install()
        self.assertEqual(installed.returncode, 0, installed.stderr)
        target = self.isolated / "opt/litecr/releases/0.1.0"
        original_binary = (target / "bin/litecr").read_bytes()
        original_manifest = (target / "SHA256SUMS").read_bytes()
        original_pending = os.readlink(self.isolated / "opt/litecr/pending")

        # Rebuild the archive with identical version metadata but a different
        # payload. This models accidentally reusing a published version tag.
        self.bundle = self.make_bundle(version="0.1.0", payload_marker="different-build")
        self.sidecar = self.bundle.with_name(self.bundle.name + ".sha256")
        retried = self.run_install()

        self.assertNotEqual(retried.returncode, 0)
        self.assertIn("RELEASE_ALREADY_DIFFERS", retried.stderr)
        self.assertEqual((target / "bin/litecr").read_bytes(), original_binary)
        self.assertEqual((target / "SHA256SUMS").read_bytes(), original_manifest)
        self.assertEqual(os.readlink(self.isolated / "opt/litecr/pending"), original_pending)

    def test_activation_failure_leaves_staged_files_and_retry_completes(self) -> None:
        failed = self.run_install(fail_activation=True)
        self.assertEqual(failed.returncode, 73, failed.stderr)
        self.assertFalse((self.isolated / "opt/litecr/current").exists())
        self.assertTrue((self.isolated / "opt/litecr/releases/0.1.0/SHA256SUMS").is_file())
        self.assertTrue((self.isolated / "etc/litecr/litecr.env").is_file())
        self.assertFalse((self.isolated / "etc/systemd/system/litecr-api.service").exists())
        retried = self.run_install()
        self.assertEqual(retried.returncode, 0, retried.stderr)
        self.assert_staged()
        calls = self.log.read_text(encoding="utf-8")
        self.assertNotIn("systemctl start", calls)
        self.assertNotIn("systemctl enable", calls)

    def test_interrupted_release_copy_is_cleaned_and_retry_stages_atomically(self) -> None:
        failed = self.run_install(fail_copy=True)

        self.assertEqual(failed.returncode, 73, failed.stderr)
        releases = self.isolated / "opt/litecr/releases"
        self.assertFalse((releases / "0.1.0").exists())
        self.assertEqual(list(releases.glob(".0.1.0.*")), [])
        self.assertFalse((self.isolated / "opt/litecr/pending").exists())

        retried = self.run_install()

        self.assertEqual(retried.returncode, 0, retried.stderr)
        self.assert_staged()
        self.assertEqual(list(releases.glob(".0.1.0.*")), [])

    def test_upgrade_stages_pending_release_without_retargeting_current(self) -> None:
        initial = self.run_install()
        self.assertEqual(initial.returncode, 0, initial.stderr)
        current = self.isolated / "opt/litecr/current"
        current.symlink_to("/opt/litecr/releases/0.1.0")

        self.bundle = self.make_bundle(version="0.2.0")
        self.sidecar = self.bundle.with_name(self.bundle.name + ".sha256")
        upgraded = self.run_install(backup_verified=True)

        self.assertEqual(upgraded.returncode, 0, upgraded.stderr)
        self.assertEqual(os.readlink(current), "/opt/litecr/releases/0.1.0")
        self.assertEqual(
            os.readlink(self.isolated / "opt/litecr/pending"), "/opt/litecr/releases/0.2.0"
        )
        self.assertTrue((self.isolated / "opt/litecr/releases/0.1.0/SHA256SUMS").is_file())
        self.assertTrue((self.isolated / "opt/litecr/releases/0.2.0/SHA256SUMS").is_file())

    def test_upgrade_replaces_only_the_previous_release_migration_unit(self) -> None:
        initial = self.run_install()
        self.assertEqual(initial.returncode, 0, initial.stderr)
        current = self.isolated / "opt/litecr/current"
        current.symlink_to("/opt/litecr/releases/0.1.0")
        old_unit = (
            "[Service]\nUser=litecr-migrate\n"
            "ExecStart=/opt/litecr/current/litecr migrate\n"
        )
        active_template = (
            self.isolated / "opt/litecr/releases/0.1.0/share/templates/litecr-migrate.service"
        )
        active_template.write_text(old_unit, encoding="utf-8")
        active_template.chmod(0o644)
        installed_unit = self.isolated / "etc/systemd/system/litecr-migrate.service"
        installed_unit.write_text(old_unit, encoding="utf-8")

        self.bundle = self.make_bundle(version="0.2.0")
        self.sidecar = self.bundle.with_name(self.bundle.name + ".sha256")
        upgraded = self.run_install(backup_verified=True)

        self.assertEqual(upgraded.returncode, 0, upgraded.stderr)
        self.assertEqual(installed_unit.read_text(), (ROOT / "infra/release/litecr-migrate.service").read_text())
        self.assertEqual(os.readlink(current), "/opt/litecr/releases/0.1.0")

    def test_activation_doctor_failure_preserves_active_release_then_success_switches(self) -> None:
        installed = self.run_install()
        self.assertEqual(installed.returncode, 0, installed.stderr)
        mount_script = r'''
set -eu
fixture=$1; helper=$2; log=$3; status=$4
mount --make-rprivate /
mount --bind "$fixture/var" /var
mount --bind "$fixture/opt" /opt
export PATH="$LITECR_TEST_BIN:$PATH" LITECR_TEST_LOG="$log"
export LITECR_DATABASE_URL=postgresql://synthetic:synthetic@127.0.0.1/litecr
export LITECR_TEST_DOCTOR_STATUS="$status"
bash "$helper"
'''

        def activate(status: str) -> subprocess.CompletedProcess[str]:
            env = os.environ.copy()
            env["LITECR_TEST_BIN"] = str(self.bin)
            env["LITECR_TEST_LOG"] = str(self.log)
            return subprocess.run(
                [
                    "unshare", "-Urnm", "bash", "-c", mount_script, "--",
                    str(self.isolated), str(ROOT / "tools/operations/activate-pending-release.sh"),
                    str(self.log), status,
                ],
                text=True, capture_output=True, env=env, check=False,
            )

        failed = activate("2")
        self.assertNotEqual(failed.returncode, 0)
        self.assertIn("DATABASE_SCHEMA_INCOMPATIBLE", failed.stderr)
        self.assertFalse((self.isolated / "opt/litecr/current").exists())

        succeeded = activate("0")
        self.assertEqual(succeeded.returncode, 0, succeeded.stderr)
        self.assertEqual(
            os.readlink(self.isolated / "opt/litecr/current"), "/opt/litecr/releases/0.1.0"
        )

    def test_systemd_migration_and_activation_units_use_pending_and_require_success(self) -> None:
        api = (ROOT / "infra/release/litecr-api.service").read_text()
        migration = (ROOT / "infra/release/litecr-migrate.service").read_text()
        activation = (ROOT / "infra/release/litecr-activate.service").read_text()
        self.assertIn("/opt/litecr/current/bin/litecr serve", api)
        self.assertIn("/opt/litecr/pending/bin/litecr migrate", migration)
        self.assertNotIn("/opt/litecr/current", migration)
        self.assertIn("Conflicts=litecr-api.service litecr-worker.service", migration)
        self.assertIn("After=network-online.target litecr-api.service litecr-worker.service", migration)
        self.assertIn("Requires=litecr-migrate.service", activation)
        self.assertIn("activate-pending-release.sh", activation)

    def test_upgrade_doctor_failure_preserves_old_current_then_retry_advances(self) -> None:
        installed = self.run_install()
        self.assertEqual(installed.returncode, 0, installed.stderr)

        def activate(status: str) -> subprocess.CompletedProcess[str]:
            mount_script = r'''
set -eu
fixture=$1; helper=$2; log=$3; status=$4
mount --make-rprivate /
mount --bind "$fixture/var" /var
mount --bind "$fixture/opt" /opt
export PATH="$LITECR_TEST_BIN:$PATH" LITECR_TEST_LOG="$log"
export LITECR_DATABASE_URL=postgresql://synthetic:synthetic@127.0.0.1/litecr
export LITECR_TEST_DOCTOR_STATUS="$status"
bash "$helper"
'''
            env = os.environ.copy()
            env["LITECR_TEST_BIN"] = str(self.bin)
            env["LITECR_TEST_LOG"] = str(self.log)
            return subprocess.run(
                [
                    "unshare", "-Urnm", "bash", "-c", mount_script, "--",
                    str(self.isolated), str(ROOT / "tools/operations/activate-pending-release.sh"),
                    str(self.log), status,
                ],
                text=True, capture_output=True, env=env, check=False,
            )

        first_activation = activate("0")
        self.assertEqual(first_activation.returncode, 0, first_activation.stderr)
        current = self.isolated / "opt/litecr/current"
        self.assertEqual(os.readlink(current), "/opt/litecr/releases/0.1.0")

        self.bundle = self.make_bundle(version="0.2.0")
        self.sidecar = self.bundle.with_name(self.bundle.name + ".sha256")
        upgrade = self.run_install(backup_verified=True)
        self.assertEqual(upgrade.returncode, 0, upgrade.stderr)
        self.assertEqual(os.readlink(current), "/opt/litecr/releases/0.1.0")

        failed = activate("2")
        self.assertNotEqual(failed.returncode, 0)
        self.assertEqual(os.readlink(current), "/opt/litecr/releases/0.1.0")
        succeeded = activate("0")
        self.assertEqual(succeeded.returncode, 0, succeeded.stderr)
        self.assertEqual(os.readlink(current), "/opt/litecr/releases/0.2.0")

    def test_dangling_current_symlink_still_requires_backup_confirmation(self) -> None:
        current = self.isolated / "opt/litecr/current"
        current.parent.mkdir(parents=True)
        current.symlink_to("/opt/litecr/releases/missing")

        result = self.run_install(backup_verified=False)

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("BACKUP_CONFIRMATION_REQUIRED", result.stderr)
        self.assertFalse((self.isolated / "opt/litecr/releases/0.1.0").exists())

    def test_symlinked_install_root_is_refused_before_writing_outside_opt(self) -> None:
        escape = self.isolated / "var/tmp/litecr-escape"
        (self.isolated / "opt/litecr").symlink_to("../var/tmp/litecr-escape")

        result = self.run_install()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("INSTALL_PATH_NOT_DIRECTORY", result.stderr)
        self.assertFalse(escape.exists())

    def test_symlinked_config_directory_is_refused_before_copying_secrets(self) -> None:
        escape = self.isolated / "var/tmp/litecr-config-escape"
        (self.isolated / "etc/litecr").symlink_to("../var/tmp/litecr-config-escape")

        result = self.run_install()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("INSTALL_PATH_NOT_DIRECTORY", result.stderr)
        self.assertFalse(escape.exists())

    def test_symlinked_existing_config_files_are_refused_even_when_contents_match(self) -> None:
        config_dir = self.isolated / "etc/litecr"
        config_dir.mkdir()
        cases = (
            ("litecr.env", self.runtime_env),
            ("migration.env", self.migration_env),
        )
        for name, source in cases:
            with self.subTest(name=name):
                config = config_dir / name
                config.symlink_to(source)

                result = self.run_install()

                self.assertNotEqual(result.returncode, 0)
                self.assertIn("CONFIG_FILE_NOT_REGULAR", result.stderr)
                self.assertTrue(config.is_symlink())
                self.assertFalse((self.isolated / "opt/litecr/releases/0.1.0").exists())
                config.unlink()

    def test_existing_config_files_require_exact_owner_group_and_mode(self) -> None:
        config_dir = self.isolated / "etc/litecr"
        config_dir.mkdir()
        cases = (
            ("runtime", "litecr.env", self.runtime_env, "root:root:640"),
            ("runtime", "litecr.env", self.runtime_env, "root:litecr:644"),
            ("runtime", "litecr.env", self.runtime_env, "operator:litecr:640"),
            ("migration", "migration.env", self.migration_env, "root:root:640"),
            ("migration", "migration.env", self.migration_env, "root:litecr-migrate:644"),
            ("migration", "migration.env", self.migration_env, "operator:litecr-migrate:640"),
        )
        for config_name, name, source, bad_stat in cases:
            with self.subTest(name=name, stat=bad_stat):
                config = config_dir / name
                config.write_bytes(source.read_bytes())
                self.config_stat_overrides[config_name] = bad_stat

                result = self.run_install()

                self.assertNotEqual(result.returncode, 0)
                self.assertIn("CONFIG_FILE_OWNERSHIP_OR_MODE", result.stderr)
                self.assertFalse((self.isolated / "opt/litecr/releases/0.1.0").exists())
                self.config_stat_overrides.pop(config_name)
                config.unlink()

    def test_group_writable_install_root_is_refused(self) -> None:
        install_root = self.isolated / "opt/litecr"
        install_root.mkdir()
        install_root.chmod(0o775)

        result = self.run_install()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("INSTALL_PATH_WRITABLE", result.stderr)
        self.assertFalse((install_root / "releases/0.1.0").exists())

    def test_symlinked_service_data_directories_are_refused_before_chown(self) -> None:
        for name in ("litecr", "litecr-migrate"):
            with self.subTest(name=name):
                escape = self.isolated / f"var/tmp/{name}-data-escape"
                data_dir = self.isolated / f"var/lib/{name}"
                data_dir.symlink_to(f"../tmp/{name}-data-escape")

                result = self.run_install()

                self.assertNotEqual(result.returncode, 0)
                self.assertIn("INSTALL_PATH_NOT_DIRECTORY", result.stderr)
                self.assertFalse(escape.exists())
                data_dir.unlink()

    def test_group_writable_service_data_directory_is_refused(self) -> None:
        data_dir = self.isolated / "var/lib/litecr"
        data_dir.mkdir()
        data_dir.chmod(0o775)

        result = self.run_install()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("INSTALL_PATH_WRITABLE", result.stderr)
        self.assertFalse(self.log.exists(), "refusal must happen before any install command shim runs")

    def test_symlinked_existing_release_target_is_refused(self) -> None:
        release_fixture = self.root / "litecr-0.1.0-linux-x86_64"
        escape = self.isolated / "var/tmp/litecr-release-escape"
        escape.mkdir()
        shutil.copyfile(release_fixture / "SHA256SUMS", escape / "SHA256SUMS")
        release_parent = self.isolated / "opt/litecr/releases"
        release_parent.mkdir(parents=True)
        target = release_parent / "0.1.0"
        target.symlink_to("../../../var/tmp/litecr-release-escape")

        result = self.run_install()

        self.assertNotEqual(result.returncode, 0)
        self.assertIn("RELEASE_TARGET_NOT_DIRECTORY", result.stderr)
        self.assertTrue(target.is_symlink())
        self.assertFalse((self.isolated / "opt/litecr/current").exists())


if __name__ == "__main__":
    unittest.main()
