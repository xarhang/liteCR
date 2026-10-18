# Documentation

liteCR is a patient identity registry built with Rust, React and PostgreSQL. It retains source assertions, compares possible duplicates and supports human review of identity decisions. This documentation covers the experimental Beta; evaluate it with synthetic data in an isolated environment.

## Get started

| Your task | Start here |
|---|---|
| Understand what is available and what remains unverified | [Beta scope and known limits](BETA.md) |
| Build, configure a database and connect an IdP | [Local setup](BETA-SETUP.md) |
| Register invented records, search, match and review | [Usage walkthrough](USAGE.md) |
| Understand request routes and authentication | [API overview](API.md) |

Setup requires an OIDC provider and manually provisioned scoped grants. There is no default administrator or first-user wizard. A successful login does not grant registry access by itself.

## Understand and integrate

- [API](API.md) and [access](contracts/ACCESS.md)
- [Architecture](ARCHITECTURE.md), [backend decision](adr/001-rust-backend.md), [worker decision](adr/002-first-party-postgres-worker.md)
- [Themes and interface requirements](THEMES.md)
- [Migration package](migration/V1-PACKAGE.md)
- [FHIR adapter](integration/FHIR-R4-BASELINE.md) and [profile notes](contracts/fhir-r4-profile.md)

OpenCR interoperability is unverified. The FHIR adapter covers a limited Patient surface; these guides do not establish compatibility with a particular external deployment.

## Operate an evaluation environment

- [Deployment boundaries](DEPLOYMENT.md) and [installer](../tools/install/INSTALL.md)
- [Database roles](runbooks/database-roles.md): separate runtime, migration and worker permissions
- [Backup and restore](runbooks/backup-restore.md): capture and validate recoverable data
- [Upgrade and rollback](runbooks/upgrade-rollback.md): plan maintenance and recovery
- [Migration operations](runbooks/migration-operator.md) and [redacted diagnostics](runbooks/redacted-diagnostics.md)

## Work on the project

Read [contributing](../CONTRIBUTING.md), [security reporting](../SECURITY.md), [support](../SUPPORT.md) and the [AGPLv3+ license notice](../LICENSING.md). For this documentation site's build and publishing instructions, see [GitHub Pages](GITHUB-PAGES.md).
