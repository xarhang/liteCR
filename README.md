# liteCR

liteCR is an experimental patient identity registry built with Rust, React and PostgreSQL.

**Beta evaluation uses synthetic data in an isolated environment.** Real-world matching accuracy, production capacity and deployment security have not been established. Read the [Beta limits](docs/BETA.md).

## Features

- Source-record registration with original names, identifiers and provenance.
- Scoped person lookup and exact public-ID/alias search.
- Advisory matching and human review of duplicate, link, merge and reversal decisions.
- OIDC login, scoped permissions, audit and outbox records.
- Localization, country-profile and appearance settings.
- A limited authenticated FHIR R4 Patient adapter with explicitly configured create.

Matching scores are evidence for review, not proof of identity. MCP and an application memory candidate index are not available. External OpenCR/client compatibility is unverified.

## Build

Use Rust 1.85 or newer and a Node version supported by apps/web/package.json (Node 22.12 or newer in the supported release lines). Build web assets before compiling the server:

```sh
cd apps/web
npm ci
npm run build
cd ../..
cargo build --locked -p litecr-server
```

Running also requires PostgreSQL, OIDC and manual scoped user grants. There is no default administrator or first-user wizard. Follow [local setup](docs/BETA-SETUP.md); keep credentials out of Git.

After setup, follow the [usage guide](docs/USAGE.md) for synthetic registration, search, matching and review.

## Documentation

[Documentation index](docs/README.md), [API](docs/API.md), [deployment](docs/DEPLOYMENT.md), [installer](tools/install/INSTALL.md), [contributing](CONTRIBUTING.md), [security reporting](SECURITY.md).

[License details](LICENSING.md), [community conduct](CODE_OF_CONDUCT.md), [support](SUPPORT.md).

Read the [documentation website](https://xarhang.github.io/liteCR/) for the product overview, setup, usage and API guides, or [build the site locally](docs/GITHUB-PAGES.md). The registry itself requires the server, database and IdP. Source code is available at [xarhang/liteCR](https://github.com/xarhang/liteCR).

## License

[AGPL-3.0-or-later](LICENSE). Keep dependency licenses and attribution files with distributed builds. Local review archives are not approved release downloads.
