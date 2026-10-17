# Documentation

Start with [Beta scope](BETA.md) and [local setup](BETA-SETUP.md).

Then follow [synthetic usage](USAGE.md). For the documentation website, see [GitHub Pages](GITHUB-PAGES.md). Read the [AGPLv3+ license notice](../LICENSING.md) before contributing or distributing.

- [API](API.md) and [access](contracts/ACCESS.md)
- [Architecture](ARCHITECTURE.md), [backend decision](adr/001-rust-backend.md), [worker decision](adr/002-first-party-postgres-worker.md)
- [Themes](THEMES.md)
- [Deployment](DEPLOYMENT.md) and [installer](../tools/install/INSTALL.md)
- [Database roles](runbooks/database-roles.md), [backup/restore](runbooks/backup-restore.md), [upgrade/rollback](runbooks/upgrade-rollback.md), [migration operations](runbooks/migration-operator.md), [diagnostics](runbooks/redacted-diagnostics.md)
- [Migration package](migration/V1-PACKAGE.md)
- [FHIR adapter](integration/FHIR-R4-BASELINE.md) and [profile notes](contracts/fhir-r4-profile.md)

Internal plans, dated validation reports and workstation-specific profiling harnesses are retained locally and excluded from Git tracking. Public limits remain in the Beta guide; excluding reports does not establish a passed release gate.
