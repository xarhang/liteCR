# Local Beta setup

Use an empty, disposable database and synthetic identities. These instructions describe source evaluation, not production installation.

## 1. Build

Install Rust 1.85 or newer, a Node version supported by `apps/web/package.json`, and PostgreSQL. PostgreSQL 18 is the native deployment target; other CI entries are compatibility checks, not a support promise.

```sh
cd apps/web
npm ci
npm run build
cd ../..
cargo build --locked -p litecr-server
```

## 2. Prepare the database

Follow the [development database instructions](../infra/dev/README.md). Set `LITECR_DATABASE_URL` and `LITECR_MIGRATION_DATABASE_URL`. Using one database owner is acceptable only for a disposable development database; deployed service roles are described in [database roles](runbooks/database-roles.md).

```sh
cargo run --locked -p litecr-server -- migrate
cargo run --locked -p litecr-server -- doctor
```

Do not point migrations or integration tests at shared or patient data.

## 3. Configure OIDC

Use a provider you manage. The optional [Keycloak fixture](../infra/dev/keycloak/README.md) uses loopback HTTP and synthetic users; its credentials and downloaded runtime remain local. Match issuer, client ID/secret, web origin and redirect URI exactly. The API serves the embedded UI on the same origin.

Copy the repository `.env.example` to ignored `.env` and replace the OIDC placeholder with the generated local client secret. Keep the file private. Its development cookie settings are only for loopback; see [deployment](DEPLOYMENT.md) for HTTPS configuration.

## 4. Provision application access

OIDC login alone grants no registry access. An operator must create an active `litecr.access_identity` and scoped `litecr.access_grant` rows for the exact issuer/subject. There is no first-admin command. Inspect the [access model](contracts/ACCESS.md), [schema](../crates/infrastructure/migrations/0002_access.sql) and [role permissions](../crates/application/src/access/context.rs).

For the disposable Keycloak fixture, the synthetic subject is `00000000-0000-4000-8000-000000000042`; use it only with its fixture issuer. A scoped `registrar` role allows registration/matching. Person search additionally requires explicit `person:search`; the registrar role alone does not grant it. Review operations need a `reviewer` grant and the applicable case assignment. Limit grants to one test registry/organization. Do not grant administrator or cross-organization access merely to bypass an error.

Manual provisioning is an onboarding limitation. Operators must review the exact inserts and scope; test fixtures are examples, not production bootstrap scripts.

## 5. Start and exercise

```sh
cargo run --locked -p litecr-server -- serve
```

Open the configured origin, sign in and select the granted registry/organization. Register invented records, retain their public IDs for exact search, request matching and inspect evidence before review decisions. Verify an ungranted organization is denied. Report reproducible failures without sensitive data.
