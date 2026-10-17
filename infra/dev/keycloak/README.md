# Disposable local OIDC fixture

This is a Docker-free, local-only Keycloak fixture for integration tests. It contains only a synthetic realm, one synthetic browser client and one synthetic user. It is not a production deployment recipe, bootstrap mechanism, or source of liteCR permissions.

## Fixed fixture contract

| Setting | Value |
| --- | --- |
| Keycloak version | `26.8.0` |
| Issuer | `http://127.0.0.1:18081/realms/litecr` |
| OIDC client ID | `litecr-web` (confidential; generated client secret is external state) |
| Redirect URI | `http://127.0.0.1:18080/api/v1/access/oidc/callback` |
| PKCE | S256 |
| Synthetic user | `registrar.synthetic` |
| Synthetic subject | `00000000-0000-4000-8000-000000000042` |
| App listener | `127.0.0.1:18080` (started separately) |

The application must separately provision `(actor_kind='human', external_subject='00000000-0000-4000-8000-000000000042')` and an explicit, scoped grant in the isolated liteCR test database. The realm role `registrar` is only a fixture label and is not consumed as a liteCR grant. A sample SQL file is provided at `fixture-access-grant.sql`; apply it only to the disposable test DB after the access migration. It must never be added to normal production bootstrap.

## Prerequisites and verified download

Run from Linux or WSL Bash with OpenJDK 25, `unzip`, `sha256sum`, `ss` (iproute2), and `curl`. The official [Keycloak 26.8.0 getting-started guide](https://www.keycloak.org/getting-started/getting-started-zip) specifies OpenJDK 25 and this release archive. Download the official ZIP from [Keycloak downloads](https://www.keycloak.org/downloads) or the [official Keycloak GitHub 26.8.0 release](https://github.com/keycloak/keycloak/releases/tag/26.8.0).

Place it at `infra/dev/keycloak/.runtime/keycloak-26.8.0.zip` and verify its SHA-256 before extraction:

```bash
mkdir -p infra/dev/keycloak/.runtime
sha256sum infra/dev/keycloak/.runtime/keycloak-26.8.0.zip
```

Expected SHA-256 (from the official release asset listing):

```text
7ed1de3fda2598369262613bf682aab7e233d80a38c405e91588f7a7454370a1
```

The `start.sh` helper checks the same digest before extraction and refuses a mismatch. Download is deliberately manual so no dependency or executable is fetched implicitly by starting the fixture.

## Start and configure

The Keycloak documentation warns that dev mode under WSL binds to `0.0.0.0` by default. The helper explicitly overrides this with `--http-host=127.0.0.1`, and similarly binds the management listener to loopback. It enables health endpoints so Keycloak opens the management interface; no metrics endpoint is enabled. Startup fails unless `ss` reports both ports 18081 and 19001 only on IPv4 loopback, accepting either `127.0.0.1:<port>` or the equivalent IPv4-mapped address `[::ffff:127.0.0.1]:<port>`. Wildcard and non-loopback bindings are rejected. See the official [configuration options](https://www.keycloak.org/server/all-config), [management interface guide](https://www.keycloak.org/server/management-interface), and [realm import guide](https://www.keycloak.org/server/importExport).

Store the disposable bootstrap administrator username/password in a WSL-home file outside the Windows-mounted workspace, for example `~/.litecr-dev/keycloak-fixture.env`. Create the directory as mode `0700`, the file as mode `0600`, and do not commit or copy it under `/mnt/d` (chmod is not enforced there). Keep the generated client secret and synthetic test-user password in the same WSL-home secret file or a secret manager. Source the environment file with shell tracing disabled, run the helper, then unset the variables:

```bash
install -d -m 700 "$HOME/.litecr-dev"
chmod 700 "$HOME/.litecr-dev"
chmod 600 "$HOME/.litecr-dev/keycloak-fixture.env"
set +x
set -a
. "$HOME/.litecr-dev/keycloak-fixture.env"
set +a
bash infra/dev/keycloak/scripts/start.sh
unset KC_BOOTSTRAP_ADMIN_USERNAME KC_BOOTSTRAP_ADMIN_PASSWORD
```

The helper imports `realm/litecr-realm.json` into Keycloak's local `data/import` directory. Keycloak's startup import skips a realm that already exists, so changes to the JSON do not overwrite local state on restart. Reset that state explicitly using the reset steps below.

In the Keycloak admin console at `http://127.0.0.1:18081/admin/`, log in with the disposable administrator. For client `litecr-web`, open **Credentials** and copy its generated secret directly into the local app's environment. Keep it only in the WSL-home secret file above or a secret manager; do not place it under `.runtime/`, paste it into command logs/screenshots, or commit it. Set a disposable password for `registrar.synthetic` through **Users → Credentials**, then store it with the other fixture credentials outside the repository. No user password or client secret is committed. Do not enable direct access grants; the app uses authorization code + PKCE.

Then independently provision the matching synthetic identity/grant in the isolated liteCR test database using `fixture-access-grant.sql`. The database must not contain real or shared data. Set local app configuration to the table values above. No web-origin wildcard is configured; browser login is backend-mediated.

## Verify, stop, and reset

After the service has been started by an authorized integration run, check the socket binding:

```bash
bash infra/dev/keycloak/scripts/verify-listener.sh
curl --fail --silent http://127.0.0.1:18081/realms/litecr/.well-known/openid-configuration
```

After the integrated liteCR server is running on `127.0.0.1:18080` with the client secret loaded from a WSL-home-only file, run the repeatable synthetic browser-flow check:

```bash
python3 infra/dev/keycloak/scripts/test-flow.py
```

The script reads only `LITECR_FIXTURE_USER_PASSWORD` from `~/.litecr-dev/keycloak-fixture.env`, requires that file to be mode `0600` or stricter, restricts every flow URL to `127.0.0.1`, and prints only pass/fail status. Python's test cookie jar permits Keycloak's Secure cookies over this HTTP loopback fixture; this behavior is confined to the test harness and does not change browser or production cookie settings.

The verification helper requires listeners on ports 18081 and 19001 and accepts only `127.0.0.1:<port>` or the kernel's equivalent IPv4-mapped loopback address. Do not expose either listener to LAN, WSL host forwarding, a tunnel, or a public network. Do not run this fixture on a machine where loopback port forwarding is made externally reachable.

Stop only the PID recorded by this fixture:

```bash
bash infra/dev/keycloak/scripts/stop.sh
```

After stop, the reset helper removes only the marked installation's `data/` directory, after an exact typed confirmation. This clears the dev database, imported realm, admin account and generated client secret; the next start imports the synthetic realm again. Re-enter fresh disposable user/client credentials and re-apply the SQL grant as needed.

```bash
bash infra/dev/keycloak/scripts/reset-data.sh
```

Logs, PID, archive, extracted installation, realm state and discovery response live under ignored `.runtime/`; this directory contains no credentials. Credentials belong under the WSL home in a mode-0600 file or in a secret manager. Delete `.runtime/` manually after stopping if you also want to remove the downloaded archive and extracted Keycloak distribution. Never include `.runtime/` in a support bundle.

## Limits

This fixture does not by itself verify application callback/session/CSRF/logout behavior, negative-token cases, cross-organization policy, or production readiness. Its HTTP issuer is safe only for local isolated integration tests; do not reuse the settings outside loopback. Test these flows against the actual API before relying on the fixture for application behavior.
