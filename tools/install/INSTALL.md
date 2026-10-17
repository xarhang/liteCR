# Native Linux installation quick guide

This experimental guide targets Ubuntu 24.04 x86_64 with PostgreSQL 18. Use an isolated environment and synthetic data for Beta evaluation. Clean-host installation, deployed OIDC/TLS behavior, and independent recovery still need validation; other operating systems and PostgreSQL majors are not implied to be supported.

The runtime requires the bundled `litecr` binary, systemd, PostgreSQL 18, an existing OIDC provider, and an operator-managed same-origin HTTPS gateway. Node, npm, Rust, Docker, Caddy, PostgreSQL, and Keycloak are not installed by the runtime installer. Caddy configuration is an optional example only. The installer creates the `litecr` and `litecr-migrate` OS service accounts and installs the API/migration/activation unit templates; it does not install or configure PostgreSQL, create database roles, configure an IdP, gateway, DNS, certificates, firewall, or backup destination. The operator/DBA must complete those prerequisites first.

## Clean-host preparation and first-install order

Use this sequence for a new host. It documents the intended manual procedure; a clean native host installation has not yet been exercised.

1. Provision an Ubuntu 24.04 x86_64 host with systemd and the approved organization security baseline. Ensure root can install files under `/opt`, `/etc`, and `/var`; the installer checks these directories are root-owned and not writable by group/other. Reserve sufficient disk space: preflight requires `floor(archive_bytes/512) + 524288` KiB free under `/opt` (about twice the archive size plus 256 MiB); also reserve space for the expanded release and operational headroom. Database and backup capacity are managed separately.
2. Install and start the operator-managed PostgreSQL 18 service. Pin the exact PostgreSQL 18 patch release in the deployment record and verify it on the target host. Keep the listener private. Have a DBA create a dedicated empty `litecr` database, the separate `litecr_migrator` owner/login and `litecr_runtime` login, and set their credentials through the organization's approved secret workflow. Do not place credentials in SQL, command arguments, history, or source control. See `share/docs/database-roles.md` for role attributes, bootstrap SQL, and the grant procedure. That example also provisions `litecr_worker`; it is optional and must not be enabled until the worker release gates are accepted. The DBA, not the installer, decides how to provision roles and database access.
3. Configure an existing OIDC client and operator-managed same-origin HTTPS gateway. Set the issuer discovery URL reachable from the liteCR host; the installer fetches `/.well-known/openid-configuration` over TLS. The gateway must proxy the browser/API origin to the API's loopback bind, and the configured redirect URI must equal `<web-origin>/api/v1/access/oidc/callback`.
4. Create the runtime and migration input files from the bundled examples, replacing every placeholder. For a first install, keep them in a root-only staging directory such as `/root/litecr-install/`, owned by root and mode `0600`; the installer requires existing protected input files, but creates `/etc/litecr/` and installs protected copies with the final `litecr`/`litecr-migrate` group ownership and mode `0640`. Do not pre-create the destination files on first install. For upgrades, pass the already-installed files only when their contents exactly match the configuration supplied to the installer; changed configuration must be applied as a separately reviewed operation. URL-encode reserved characters in PostgreSQL URL credentials. Do not source either file as shell code.
5. Transfer an owner-approved release archive and adjacent `.sha256` file through the approved channel into a root-owned staging directory, verify provenance/checksum by organizational policy, and stage them mode `0644` or more restrictive. The checksum sidecar only confirms archive integrity against that sidecar; it does not establish publisher identity. Keep the release artifact and metadata for recovery. Verify the sidecar and extract the archive into a separate root-only directory so you can run the bundled installer; the installer then independently checks archive and payload checksums. Replace the placeholder below with the exact approved archive name; do not select a candidate based only on its filename or local test report:
   ```sh
   release_name='litecr-REPLACE_WITH_APPROVED_VERSION-linux-x86_64'
   sudo sha256sum --check --strict "/srv/litecr/${release_name}.tar.gz.sha256"
   sudo install -d -o root -g root -m 0700 /srv/litecr/unpacked
   sudo tar --no-same-owner -xzf "/srv/litecr/${release_name}.tar.gz" -C /srv/litecr/unpacked
   ```
6. Run the extracted installer with `--dry-run` using the same bundle, checksum, and protected configuration paths that will be used for installation (add `--dry-run` to the command shown below). This checks local/systemd/configuration prerequisites, OIDC TLS discovery, archive checksums, and runtime database reachability. A missing schema or incompatible schema does not block staging; exact schema compatibility is checked by the activation service after the explicit migration. It makes no service-file changes. Resolve any failure before proceeding.
7. Run the installer from the extracted approved release using the command below, without `--dry-run`. It stages the immutable release and systemd templates, records it as `/opt/litecr/pending`, and does not migrate or start services. On the first install there is no active `/opt/litecr/current` yet.
8. From the protected migration environment, start `litecr-migrate.service` once and inspect its systemd result. If using the documented `litecr_runtime` role, apply the pending release's runtime grants using the `litecr_migrator` credential, then start `litecr-activate.service`. Activation runs the candidate release's doctor with the runtime credential and changes `/opt/litecr/current` only on exact schema compatibility. Do not enable/start the API before activation succeeds.
9. Enable/start the API, verify both health endpoints through the approved gateway path, then run the authenticated synthetic workflow checks and confirm audit/outbox behavior. Before real data, establish the approved backup configuration and complete an isolated restore rehearsal. The installer does not configure backups or measure RPO/RTO.

For the schema grant step, do not use `. /etc/litecr/migration.env`: environment files are data and must not be executed by a shell. In a root login shell, load only the expected URL as a literal value, without printing it, then run the bundled procedure:

```sh
sudo -i
while IFS= read -r line; do
  case "$line" in ''|\#*) continue ;; esac
  case "$line" in
    LITECR_MIGRATION_DATABASE_URL=*) export LITECR_MIGRATION_DATABASE_URL="${line#*=}" ;;
    LITECR_DATABASE_MAX_CONNECTIONS=*) ;;
    *) echo 'Unexpected migration environment key'; exit 1 ;;
  esac
done < /etc/litecr/migration.env
bash /opt/litecr/pending/share/tools/operations/grant-runtime-database-access.sh
unset LITECR_MIGRATION_DATABASE_URL
exit
```

The shell snippet only accepts the two keys permitted by the installer, treats the URL as a literal string, and does not display it. The grant script requires `psql` and Python 3 on the host; install these from the approved OS packages before this step. The migration URL itself must identify the database owner used by `litecr migrate`.

Release archives are built for Linux x86_64. The build host needs Rust, Node.js/npm, and standard archive/checksum tools. Set `SOURCE_DATE_EPOCH` to the release source revision's Unix timestamp in seconds before running `build-release.sh`; it is required and has no implicit default. It fixes the SPDX creation time, and the SPDX namespace is a SHA-256 digest of the release inputs, including source revision status. Release metadata records a full Git object ID only when the build runs from a clean Git worktree. A dirty tree or workspace without Git metadata remains buildable but records a null source revision and an explicit status; this is useful for smoke builds, not source-pinned release evidence. The field is descriptive metadata, not a signed build attestation or independent reproducibility proof. The source license is `AGPL-3.0-or-later`, recorded in Cargo metadata and the repository `LICENSE`. Release metadata marked `review_required` is not distribution approval. Record copyright attribution and required notices, review the full locked dependency and bundled-asset inventory, and rebuild with clean source provenance before considering distribution. SBOM and dependency metadata do not grant distribution rights.

## Source snapshot artifact

When `build-release.sh` sees a clean Git worktree, it also writes a companion `litecr-<version>-source-<commit-prefix>.tar.gz` and SHA-256 sidecar from that exact commit. The packager refuses a dirty tree, a revision other than `HEAD`, or replacement of an existing source archive. Builds without clean Git provenance remain available for smoke testing and do not produce this companion snapshot. The source archive is a versioned, integrity-checkable source handoff; this build step does not publish a source URL or expose the archive to remote users. Keep the distribution status at `review_required` until required license, copyright, attribution, and notice reviews are complete.

## Prepare protected configuration

The first-install inputs are the separately protected runtime and migration files staged before invoking the installer; the installer copies them to `/etc/litecr/` with ownership `root:litecr` and `root:litecr-migrate`, mode `0640`. The installed runtime file is readable by the API service account `litecr` and contains the runtime PostgreSQL credential and existing OIDC client configuration. The installed migration file is readable by `litecr-migrate` and contains the distinct schema-migration credential, which the API cannot read. For upgrades, see the byte-for-byte configuration check in the installation sequence.

Keep the files owned by root, mode `0640`, with their group set to the corresponding service group. Use HTTPS issuer/origin/redirect URIs, `LITECR_ENV=production`, and `LITECR_COOKIE_SECURE=true`. No credential is generated by the installer. It refuses unknown or duplicate keys and does not source the files as shell code. Write the database URL and OIDC values as unquoted `KEY=value` entries for preflight parsing; URL-encode reserved characters in PostgreSQL URL credentials. The installer does not echo these values.

The binary commands are:

```sh
litecr version --json
litecr doctor
litecr migrate
litecr serve
```

`version --json` is offline and prints only application/schema versions. `doctor` checks runtime DB connectivity and schema compatibility. `migrate` requires `LITECR_MIGRATION_DATABASE_URL` and never falls back to the runtime credential. The API does not run migrations at startup. An optional `worker` command and gated systemd template are included, but the installer does not install or enable the worker; follow the worker-role and release gates before any operator setup.

## Offline client-registry migration

The release includes an offline Python stager at `share/tools/migration/litecr_migrate.py`. Run it on a protected operator workstation with Python 3.11+; it has no database access. Validate and stage the exact source manifest/stream, review the redacted quarantine report, and continue only when the package has no quarantined rows or exact replay duplicates. Use `--batch-bytes 8388608` so staged batches fit the authenticated API limit. Historical relationship rows are currently refused by the stager.

The `litecr migration apply`, `status`, and `reverse` commands call the protected API with a scoped bearer integration credential stored in a private file. Apply verifies the package, runs no-write server validation over every batch before the first commit, then resumes from the authoritative server checkpoint. Reverse requires the exact manifest and explicit SHA-256 confirmation; it withdraws only source rows journaled for that manifest and is not a database snapshot restore. See `share/docs/migration-operator.md` for the command sequence and limits.

The release includes `share/tools/operations/grant-runtime-database-access.sh` and its SQL policy. After the explicit migration step and before starting the API, run this ACL procedure from the protected migration environment. It enforces the documented `litecr_migrator`/`litecr_runtime` owner model and fails closed if the DBA provisioned a different arrangement; it never creates roles or changes passwords. The matching release also includes `backup-postgres.sh` and `restore-postgres-isolated.sh` at `share/tools/operations/`; use those versioned scripts for backup and isolated restore. See `share/docs/database-roles.md` and `share/docs/backup-restore.md` for DBA bootstrap and recovery limitations.

The release also includes the optional advisory worker binary command, `litecr worker`, the worker-only database grant procedure, and example systemd/environment templates. The installer does not install or enable the worker service. Its role and service require an operator-provisioned `litecr_worker` login, a separate root-owned `/etc/litecr/worker.env` readable only by the `litecr-worker` account, and the gates in `share/docs/database-roles.md` and the release contract. Do not create `/etc/litecr/worker.enable` on an operational host until the applicable clean-host, restart/recovery and readiness checks have been accepted.

## Verify and stage a release

Transfer the release archive and adjacent `.sha256` file through the approved channel into a root-owned staging directory. The installer requires both files to be root-owned and non-writable by group/other. Verify artifact provenance according to organizational policy. Ensure a current backup is restorable before upgrading. As root, run:

```sh
release_name='litecr-REPLACE_WITH_APPROVED_VERSION-linux-x86_64'
sudo "/srv/litecr/unpacked/${release_name}/install.sh" \
  --bundle "/srv/litecr/${release_name}.tar.gz" \
  --checksum "/srv/litecr/${release_name}.tar.gz.sha256" \
  --runtime-env-file /root/litecr-install/litecr.env \
  --migration-env-file /root/litecr-install/migration.env
```

If a previous release is installed, also supply `--backup-verified` after the operator has checked the backup outside this script. A `--dry-run` option executes offline/local preflight without changing service files. The installer verifies the archive hash and internal file checksums, version metadata, Linux architecture, systemd, disk headroom, runtime DB reachability (schema compatibility is checked during activation), OIDC discovery over TLS, loopback API binding, and protected configuration before staging immutable versioned files. It preserves previous release directories and leaves `/opt/litecr/current` pointing to the active release. The candidate is selected by `/opt/litecr/pending`; changed systemd units are replaced only when they still match the active release's bundled templates. Locally modified units are refused. The installer never starts migrations or services automatically. `--dry-run` does not prove that migration, activation, service startup, or a clean-host install works.

For a first install, `current` remains absent until migration and the candidate compatibility check succeed. For an upgrade, the old API release remains selected while candidate migration runs. Follow the approved maintenance plan and stop/isolate API writers before migrating. A reboot before migration starts the old release; after migration but before activation, the old release's startup schema check rejects an incompatible schema and does not bind the API listener. After review, explicitly run:

```sh
sudo systemctl start litecr-migrate.service
sudo systemctl status litecr-migrate.service
sudo systemctl start litecr-activate.service
sudo systemctl status litecr-activate.service
```

The migration unit runs the immutable candidate selected by `pending` under a shared release-operation lock. If the database uses the separate `litecr_runtime` role, apply its grants with the pending release's `grant-runtime-database-access.sh` and protected migration credential before activation; see `share/docs/database-roles.md` in the release bundle. The activation unit requires the migration unit, runs the candidate's `litecr doctor` using the runtime database credential, and atomically replaces `current` only if the database schema exactly matches. Migration or doctor failure leaves `current` unchanged. After activation, start the API for first installation or restart it for an upgrade:

```sh
sudo systemctl enable --now litecr-api.service   # first installation
sudo systemctl restart litecr-api.service        # upgrade
```

Verify `/health/live` and `/health/ready` through the approved private/gateway path, then complete authenticated IdP and synthetic workflow smoke tests. An incompatible schema, failed migration, or uncertain data state blocks startup; follow the backup/restore and upgrade/rollback runbooks. PostgreSQL, IdP, TLS, and gateway setup remain operator-managed.

The release includes the embedded UI, embedded SQLx migration runner plus source SQL migration copies, offline migration stager, systemd templates, Caddy example, locked dependency/license metadata, checksums, and runbooks. The installer does not install packages, provision an IdP, modify PostgreSQL listen/firewall settings, transfer backups, or delete data.
