//! OIDC callback/session endpoints and reusable protected-route middleware.

mod router;

pub use router::{
    AccessApiConfig, AccessEnvironment, protected_routes, require_permission, router,
};
