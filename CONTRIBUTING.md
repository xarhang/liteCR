# Contributing

Use synthetic data and a dedicated development database. Keep credentials, patient records, exports and local runtime files out of commits and reports. Read the [Beta limits](docs/BETA.md) and [architecture](docs/ARCHITECTURE.md).

## Checks

Contributions use [AGPL-3.0-or-later](LICENSING.md). Submit only material you have the right to contribute and identify third-party licenses. No DCO sign-off or cryptographic signature is required. Submit a focused pull request with reproduction steps and actual check results. Discuss substantial changes before implementing a new architecture or public contract.

Keep discussions respectful and focused on the work. Do not publish another person's private information. Maintainers may remove abusive or sensitive content. A separate confidential conduct-reporting contact has not been designated.

See the [community conduct policy](CODE_OF_CONDUCT.md) and [support guidance](SUPPORT.md).

Build web assets before Rust checks because the server embeds them:

```sh
cd apps/web
npm ci
npm test
npm run check:catalog
npm run build
cd ../..
cargo fmt --all -- --check
cargo check --workspace --locked
cargo test --workspace --locked -- --test-threads=1
cargo clippy --workspace --all-targets --locked -- -D warnings
```

Configure `LITECR_TEST_DATABASE_URL` for PostgreSQL integration tests, using only a disposable test database. Some tests skip without required environment settings; a run without them is not complete integration evidence. See [local setup](docs/BETA-SETUP.md) and the CI workflows.

Preserve original source values, assigning-authority identifier semantics and identity separation. Matching produces evidence; it must not silently merge people. Registry mutations retain authorization, current-version, audit and outbox checks. Add meaningful regression tests for changes to those boundaries.

Keep pull requests focused. Explain the problem, resulting behavior, validation and limitations. Avoid unsupported accuracy, security, interoperability or performance claims. Use [SECURITY.md](SECURITY.md) for security reports.
