# ADR-001: Rust backend

Status: implemented architecture.

## Decision

Use Rust with Axum, Tokio, SQLx and PostgreSQL for a modular monolith. Build the React/TypeScript/Vite interface into the native release. API and optional worker are separate processes from one executable. Native Linux services are the deployment target; Docker and a Node/Rust toolchain are not runtime dependencies of a prebuilt bundle.

Use an operator-managed same-origin HTTPS gateway and OIDC provider. Keep identity and matching rules independent of transport and storage libraries. The advisory worker uses the [first-party PostgreSQL queue](002-first-party-postgres-worker.md).

## Consequences

Operators must provide PostgreSQL, authentication, HTTPS and backups. Rust memory safety does not establish authorization correctness, identity accuracy or interoperability. Platform support and deployment acceptance require separate evidence; see [Beta limits](../BETA.md) and [deployment](../DEPLOYMENT.md).
