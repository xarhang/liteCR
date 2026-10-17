# Security

liteCR is experimental and has not been approved for real patient data or production use. Use synthetic data in isolated Beta environments. No security certification or production-version support policy is claimed.

## Reporting

Do not include patient data, tokens, cookies, credentials, database URLs or exports in public issues. If private vulnerability reporting is enabled for this repository, use its GitHub Security tab. Otherwise ask maintainers for a private channel before sharing sensitive details; none is published here yet.

Provide the affected version/commit, prerequisites, a minimal synthetic reproduction and the observed failure. General bug reports can use public issues after removing sensitive information.

## Deployment boundaries

Use an unprivileged runtime account, separate migration credentials, scoped application grants, private PostgreSQL access and an HTTPS same-origin gateway. Follow [deployment](docs/DEPLOYMENT.md), [access](docs/contracts/ACCESS.md) and [backup/restore](docs/runbooks/backup-restore.md). Local tests do not establish another deployment's security.
