//! PostgreSQL-backed access records and OpenID Connect client adapters.

mod postgres;

pub use postgres::{
    OidcProviderClient, OidcProviderConfig, PostgresAccessAuthenticator, PostgresAccessDirectory,
    PostgresAccessSecurityAudit, PostgresAccessSessionRepository, PostgresMcpInvocationAudit,
    PostgresOidcFlowStore,
};
